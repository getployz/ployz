//! Once per daemon start, before this Machine writes to the Cluster store, find out
//! whether a power cut lost commits that its peers already hold, and if so move its
//! writes to a new Corrosion actor.
//!
//! Corrosion broadcasts a commit before SQLite syncs it. A power cut can then drop this
//! Machine's last commits while peers keep them. Corrosion never sends a Machine its own
//! actor's changes back, and peers drop a version number they already hold, so the next
//! writes here would reuse those numbers and never reach a peer. Corrosion's own answer
//! for a store behind its peers is `restore`, which gives it a new actor and keeps the
//! data. The old actor then syncs like any peer's, so the lost versions come back.
//!
//! Only the kernel losing its page cache loses a commit, so a start after a clean stop,
//! or after the daemon alone crashed, checks nothing.

use std::{
    collections::{BTreeMap, BTreeSet},
    fs, io,
    net::{IpAddr, SocketAddr},
    path::Path,
    time::Duration,
};

use futures_util::future::join_all;
use ployz_core::{LocalMachinePhase, MACHINE_API_PORT};
use tokio::time::Instant;

use crate::{
    boot_pass::{actor_key, alone, target_versions},
    corrosion::{Error, RunningCorrosion, wait_for_catch_up},
    filesystem::atomic_write,
    machine::RecordOwner,
    network::NetworkPlane,
};

/// How long boot waits for every peer to answer. When the whole Cluster boots at once,
/// no peer answers until this passes, since each one waits here before serving.
const ASK_FOR: Duration = Duration::from_secs(20);
const PEER_TIMEOUT: Duration = Duration::from_secs(2);
const PEER_RETRY: Duration = Duration::from_secs(1);
const CATCH_UP_FOR: Duration = Duration::from_secs(30);

/// What the peers' stores hold of this Machine's own actor.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum OwnTail {
    /// No answering peer holds a version of it past this store's head.
    Current { head: i64, peer_head: i64 },
    /// A peer holds versions past this store's head, which a power cut lost here.
    Lost {
        actor: String,
        head: i64,
        peer_head: i64,
    },
}

pub(crate) fn own_tail(actor: &str, head: i64, answers: &[BTreeMap<String, i64>]) -> OwnTail {
    let key = actor_key(actor);
    let held = answers
        .iter()
        .flat_map(|versions| versions.iter())
        .filter(|(peer_actor, _)| actor_key(peer_actor) == key)
        .max_by_key(|(_, version)| **version);
    match held {
        Some((peer_actor, &peer_head)) if peer_head > head => OwnTail::Lost {
            actor: peer_actor.clone(),
            head,
            peer_head,
        },
        held => OwnTail::Current {
            head,
            peer_head: held.map_or(0, |(_, version)| *version),
        },
    }
}

/// Written once ployzd's own writes are on disk at a graceful stop, removed at start.
const STOPPED_CLEANLY: &str = "corrosion-stopped-cleanly";
/// The kernel boot this daemon last started in.
const STARTED_IN_BOOT: &str = "corrosion-boot-id";
const BOOT_ID: &str = "/proc/sys/kernel/random/boot_id";

/// How the daemon before this one stopped, as far as its store is concerned.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LastStop {
    /// It stopped gracefully with its writes on disk.
    Clean,
    /// The kernel kept running, so its page cache kept every commit.
    SameBoot,
    /// The kernel restarted without a graceful stop: a power cut or a crash.
    Unclean,
    /// No record, as on the first start of a daemon that keeps one.
    Unknown,
}

fn last_stop(data_dir: &Path) -> LastStop {
    if data_dir.join(STOPPED_CLEANLY).exists() {
        return LastStop::Clean;
    }
    match (
        fs::read_to_string(data_dir.join(STARTED_IN_BOOT)),
        fs::read_to_string(BOOT_ID),
    ) {
        (Ok(recorded), Ok(current)) if recorded.trim() == current.trim() => LastStop::SameBoot,
        (Ok(_), Ok(_)) => LastStop::Unclean,
        _ => LastStop::Unknown,
    }
}

fn record_start(data_dir: &Path) -> io::Result<()> {
    match fs::remove_file(data_dir.join(STOPPED_CLEANLY)) {
        Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
        _ => {}
    }
    atomic_write(
        &data_dir.join(STARTED_IN_BOOT),
        fs::read_to_string(BOOT_ID)?.trim().as_bytes(),
        0o600,
    )
}

/// Called once this daemon's writers have stopped. Corrosion outlives the daemon, so its
/// commits are synced first: a power cut after a clean stop must still lose nothing.
pub(crate) fn record_clean_stop(data_dir: &Path, store_dir: &Path) -> io::Result<()> {
    rustix::fs::syncfs(fs::File::open(store_dir)?)?;
    atomic_write(&data_dir.join(STOPPED_CLEANLY), b"", 0o600)
}

/// What to do about this store's actor.
#[derive(Debug, Eq, PartialEq)]
pub(crate) enum Action {
    Keep,
    /// Move to a new actor, and wait for the old one to reach `pull` when a peer
    /// showed what was lost.
    Rotate {
        pull: Option<(String, i64)>,
    },
}

/// After an unclean stop, any peer that did not answer may hold what was lost, and a
/// new actor costs only a row, so only every peer vouching for this head keeps it.
pub(crate) fn decide(last: LastStop, tail: OwnTail, every_peer_answered: bool) -> Action {
    match tail {
        OwnTail::Lost {
            actor, peer_head, ..
        } => Action::Rotate {
            pull: Some((actor, peer_head)),
        },
        OwnTail::Current { .. } if !every_peer_answered && last == LastStop::Unclean => {
            Action::Rotate { pull: None }
        }
        OwnTail::Current { .. } => Action::Keep,
    }
}

/// Runs the check, and the rotation it calls for, before any writer starts. A failure
/// to check or rotate leaves the daemon starting, on the old actor or the new one, with
/// Corrosion kept running. Only a store handoff left half done fails the start, so the
/// next one finishes it before Corrosion runs.
pub(crate) async fn recover(
    corrosion: &mut RunningCorrosion,
    network: Option<&mut NetworkPlane>,
    local: &RecordOwner,
    data_dir: &Path,
) -> Result<(), Error> {
    let last = last_stop(data_dir);
    let settled = if local.record().phase() != LocalMachinePhase::Participating {
        true
    } else if matches!(last, LastStop::Clean | LastStop::SameBoot) {
        tracing::info!("lost-tail check skipped, the last stop lost no commit ({last:?})");
        true
    } else {
        check(corrosion, network, local, last).await?
    };
    // Recorded only once settled, so a start that dies mid-check checks again.
    if settled && let Err(error) = record_start(data_dir) {
        tracing::warn!(error = %ployz_core::error_chain::inline(&error), "could not record this start, so the next one checks the store after any reboot");
    }
    Ok(())
}

/// Whether the store's actor is settled: kept with every reachable peer behind it, or
/// rotated. A failure leaves it unsettled.
async fn check(
    corrosion: &mut RunningCorrosion,
    network: Option<&mut NetworkPlane>,
    local: &RecordOwner,
    last: LastStop,
) -> Result<bool, Error> {
    let record = local.record();
    let store = corrosion.store().clone();
    let machines = match store.machines().await {
        Ok(machines) => machines,
        Err(error) => {
            tracing::warn!(error = %ployz_core::error_chain::inline(&error), "lost-tail check skipped, could not read the Machine list");
            return Ok(false);
        }
    };
    if alone(&machines, &record.id(), record.bootstrap()) {
        tracing::info!("lost-tail check skipped, no peer to compare the store with");
        return Ok(true);
    }
    // At boot WireGuard knows only the bootstrap peers; the rest come from the store.
    if let Some(network) = network
        && let Err(error) = network.rebuild(&store, &machines, local).await
    {
        tracing::warn!(error = %ployz_core::error_chain::inline(&error), "lost-tail check could not add peers to WireGuard");
    }
    let (actor, head) = match tokio::try_join!(store.local_actor(), store.version()) {
        Ok((actor, versions)) => {
            let head = versions
                .iter()
                .find(|(known, _)| actor_key(known) == actor_key(&actor))
                .map_or(0, |(_, version)| *version);
            (actor, head)
        }
        Err(error) => {
            tracing::warn!(error = %ployz_core::error_chain::inline(&error), "lost-tail check skipped, could not read the store's versions");
            return Ok(false);
        }
    };
    let peers = machines
        .observations
        .iter()
        .filter(|peer| peer.id != record.id())
        .map(|peer| SocketAddr::new(IpAddr::V6(peer.management_address().0), MACHINE_API_PORT))
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let answers = ask(&peers, &actor, head).await;
    let every_peer_answered = answers.len() == peers.len();
    let tail = own_tail(&actor, head, &answers);
    tracing::info!(
        "lost-tail check after a {last:?} stop: actor {actor} is at {head} here, {} of {} peers answered: {tail:?}",
        answers.len(),
        peers.len()
    );
    let Action::Rotate { pull } = decide(last, tail, every_peer_answered) else {
        return Ok(true);
    };
    match &pull {
        Some((lost, peer_head)) => tracing::warn!(
            "an unclean stop lost store versions {}..={peer_head} of actor {lost} here, which peers hold; moving this Machine's writes to a new Corrosion actor",
            head + 1
        ),
        None => tracing::warn!(
            "after an unclean stop not every peer could vouch for actor {actor}; moving this Machine's writes to a new Corrosion actor"
        ),
    }
    if let Err(error) = corrosion.rotate_actor().await {
        if corrosion.handoff_pending() {
            return Err(error);
        }
        tracing::warn!(error = %ployz_core::error_chain::inline(&error), "could not rotate the Corrosion actor");
        return Ok(false);
    }
    let rotated = store.local_actor().await.unwrap_or_default();
    let Some((lost, peer_head)) = pull else {
        tracing::info!("rotated Corrosion actor {actor} to {rotated}");
        return Ok(true);
    };
    let target = BTreeMap::from([(lost.clone(), peer_head)]);
    match tokio::time::timeout(CATCH_UP_FOR, wait_for_catch_up(&store, &target)).await {
        Ok(Ok(())) => {
            tracing::info!(
                "rotated Corrosion actor {lost} to {rotated}, and pulled back its versions through {peer_head}"
            );
        }
        Ok(Err(error)) => {
            tracing::warn!(error = %ployz_core::error_chain::inline(&error), "rotated Corrosion actor {lost} to {rotated}, but could not wait for its lost versions");
        }
        Err(_) => tracing::warn!(
            "rotated Corrosion actor {lost} to {rotated}; its lost versions did not come back within {}s",
            CATCH_UP_FOR.as_secs()
        ),
    }
    Ok(true)
}

/// Each peer's answer, asking again those that have not answered until all have, one
/// shows a lost tail, or [`ASK_FOR`] passes.
async fn ask(peers: &[SocketAddr], actor: &str, head: i64) -> Vec<BTreeMap<String, i64>> {
    let deadline = Instant::now() + ASK_FOR;
    let mut answers = BTreeMap::new();
    loop {
        let unanswered = peers
            .iter()
            .filter(|peer| !answers.contains_key(*peer))
            .copied()
            .collect::<Vec<_>>();
        let round = join_all(unanswered.into_iter().map(|peer| async move {
            let answer = tokio::time::timeout(PEER_TIMEOUT, target_versions(peer)).await;
            (peer, answer.ok().and_then(Result::ok))
        }))
        .await;
        answers.extend(
            round
                .into_iter()
                .filter_map(|(peer, answer)| Some((peer, answer?))),
        );
        let answers_now = answers.values().cloned().collect::<Vec<_>>();
        if answers.len() == peers.len()
            || matches!(own_tail(actor, head, &answers_now), OwnTail::Lost { .. })
            || Instant::now() + PEER_RETRY > deadline
        {
            return answers_now;
        }
        tokio::time::sleep(PEER_RETRY).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const ACTOR: &str = "938f6a63-6a8b-4e2f-abbd-b571548cad0e";

    fn answer(entries: &[(&str, i64)]) -> BTreeMap<String, i64> {
        entries
            .iter()
            .map(|(actor, version)| ((*actor).to_owned(), *version))
            .collect()
    }

    #[test]
    fn a_peer_holding_versions_past_this_head_means_a_lost_tail() {
        let answers = [
            answer(&[(ACTOR, 17), ("other", 30)]),
            answer(&[("938F6A636A8B4E2FABBDB571548CAD0E", 20)]),
        ];
        assert_eq!(
            own_tail(ACTOR, 17, &answers),
            OwnTail::Lost {
                actor: "938F6A636A8B4E2FABBDB571548CAD0E".into(),
                head: 17,
                peer_head: 20,
            }
        );
    }

    #[test]
    fn peers_at_or_behind_this_head_mean_nothing_was_lost() {
        let answers = [answer(&[(ACTOR, 17)]), answer(&[(ACTOR, 15)])];
        assert_eq!(
            own_tail(ACTOR, 17, &answers),
            OwnTail::Current {
                head: 17,
                peer_head: 17
            }
        );
    }

    #[test]
    fn after_an_unclean_stop_a_head_only_some_peers_vouch_for_is_rotated() {
        let current = || OwnTail::Current {
            head: 17,
            peer_head: 17,
        };
        assert_eq!(
            decide(LastStop::Unclean, current(), false),
            Action::Rotate { pull: None }
        );
        assert_eq!(decide(LastStop::Unclean, current(), true), Action::Keep);
        assert_eq!(decide(LastStop::Unknown, current(), false), Action::Keep);
    }

    #[test]
    fn a_shown_loss_rotates_and_pulls_back_to_the_peer_head() {
        let lost = OwnTail::Lost {
            actor: ACTOR.into(),
            head: 7,
            peer_head: 10,
        };
        assert_eq!(
            decide(LastStop::Unknown, lost, true),
            Action::Rotate {
                pull: Some((ACTOR.into(), 10))
            }
        );
    }

    #[test]
    fn a_clean_stop_and_a_daemon_crash_are_told_from_a_lost_kernel() {
        let root = tempfile::tempdir().unwrap();
        let dir = root.path();
        assert_eq!(last_stop(dir), LastStop::Unknown);
        record_start(dir).unwrap();
        assert_eq!(last_stop(dir), LastStop::SameBoot);
        fs::write(dir.join(STARTED_IN_BOOT), "an earlier boot").unwrap();
        assert_eq!(last_stop(dir), LastStop::Unclean);
        record_clean_stop(dir, dir).unwrap();
        assert_eq!(last_stop(dir), LastStop::Clean);
        record_start(dir).unwrap();
        assert_eq!(last_stop(dir), LastStop::SameBoot);
    }

    #[test]
    fn peers_that_never_saw_this_actor_mean_nothing_was_lost() {
        assert_eq!(
            own_tail(ACTOR, 0, &[answer(&[("other", 4)])]),
            OwnTail::Current {
                head: 0,
                peer_head: 0
            }
        );
    }
}
