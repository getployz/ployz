//! A Server removed with `ployz server rm` or `forget` while it was unreachable never
//! hears of it, so its services keep running on its own copy of each Volume (#2927).
//! This plane lets such a Machine fence itself: once its mesh has been silent for a
//! minute it asks each peer over the management relay whether the Cluster forgot its
//! Management Identity, and when one peer says so and none lists it, it departs as a
//! leave would, keeping every Volume copy on its disk.
//!
//! The question is asked only while the mesh is silent and never while this Machine
//! is alone, so a healthy Cluster asks nothing and a Founder with no peers never
//! fences on its own. A Listed answer anywhere wins over Forgotten, and the fence
//! itself is a leave, so the worst a wrong answer costs is a reset that keeps every
//! copy. A peer that answers is trusted to be a member, which it proves only by its
//! Management Identity being listed in this Machine's own view: a malicious former
//! member that kept its key could tell this Machine it was forgotten, costing it a
//! reset and nothing else. That is accepted for 0.3.1.

use std::{future::Future, time::Duration};

use futures_util::future::join_all;
use iroh::{Endpoint, RelayUrl};
use ployz_core::{ForgottenAnswer, Left, MachineId, MachineName};
use tokio::time::Instant;
use tokio_util::sync::CancellationToken;

use crate::{
    boot_pass::{Peers, probe_peers},
    corrosion::MachineView,
    machine::LocalMachine,
    management::forgotten,
};

/// How often the mesh is probed while nothing is known.
pub(crate) const MESH_PROBE_EVERY: Duration = Duration::from_secs(30);
/// How long the mesh stays silent before the question is asked.
pub(crate) const SILENCE: Duration = Duration::from_secs(60);
/// How long one peer is given to answer.
pub(crate) const ASK_TIMEOUT: Duration = Duration::from_secs(10);
/// Backoff between rounds of asking while the mesh stays silent.
pub(crate) const ASK_BACKOFF_MIN: Duration = Duration::from_secs(60);
pub(crate) const ASK_BACKOFF_MAX: Duration = Duration::from_secs(15 * 60);

/// What the mesh probe found.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum Mesh {
    Alone,
    Silent,
    Answered,
}

impl From<Peers> for Mesh {
    fn from(peers: Peers) -> Self {
        match peers {
            Peers::Alone => Self::Alone,
            Peers::Silent => Self::Silent,
            Peers::Answered(_) => Self::Answered,
        }
    }
}

/// One peer's answer; `None` when it could not be asked or did not answer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct Asked {
    pub(crate) peer: MachineName,
    pub(crate) answer: Option<ForgottenAnswer>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) enum Verdict {
    Fence { told_by: MachineName },
    Stay,
}

pub(crate) trait FenceHost: Send + Sync {
    fn mesh(&self) -> impl Future<Output = Mesh> + Send;
    /// Ask every listed peer whose Management Identity is known.
    fn ask(&self) -> impl Future<Output = Vec<Asked>> + Send;
    /// Depart keeping every copy. Err names why it did not happen; it is retried.
    fn fence(&self) -> impl Future<Output = Result<(), String>> + Send;
}

/// Fence when a peer says Forgotten and no peer says Listed.
pub(crate) fn verdict(asked: &[Asked]) -> Verdict {
    if asked
        .iter()
        .any(|asked| asked.answer == Some(ForgottenAnswer::Listed))
    {
        return Verdict::Stay;
    }
    asked
        .iter()
        .find(|asked| asked.answer == Some(ForgottenAnswer::Forgotten))
        .map_or(Verdict::Stay, |asked| Verdict::Fence {
            told_by: asked.peer.clone(),
        })
}

/// Probe the mesh until `shutdown`, or until this Machine fenced itself.
pub(crate) async fn run(host: impl FenceHost, shutdown: CancellationToken) {
    let mut silent_since = None;
    let mut next_ask = None;
    let mut backoff = ASK_BACKOFF_MIN;
    loop {
        tokio::select! {
            () = shutdown.cancelled() => return,
            () = tokio::time::sleep(MESH_PROBE_EVERY) => {}
        }
        match host.mesh().await {
            Mesh::Alone | Mesh::Answered => {
                silent_since = None;
                next_ask = None;
                backoff = ASK_BACKOFF_MIN;
                continue;
            }
            Mesh::Silent => {}
        }
        let now = Instant::now();
        let since = *silent_since.get_or_insert(now);
        if now.duration_since(since) < SILENCE || next_ask.is_some_and(|at| now < at) {
            continue;
        }
        next_ask = Some(now + backoff);
        backoff = (backoff * 2).min(ASK_BACKOFF_MAX);
        let asked = host.ask().await;
        let Verdict::Fence { told_by } = verdict(&asked) else {
            tracing::debug!(
                ?asked,
                "the mesh is silent and no peer says this Machine was forgotten"
            );
            continue;
        };
        tracing::warn!(%told_by, "this Machine was forgotten by its Cluster; fencing and keeping every Volume copy");
        match host.fence().await {
            Ok(()) => return,
            Err(error) => tracing::warn!(error, "the fence did not happen; retrying"),
        }
    }
}

pub(crate) struct DaemonHost {
    pub(crate) machine_id: MachineId,
    pub(crate) machine: LocalMachine,
    pub(crate) machines: MachineView,
    /// The daemon's own management endpoint, so the question travels under this
    /// Machine's Management Identity.
    pub(crate) endpoint: Endpoint,
    pub(crate) relay: RelayUrl,
}

impl FenceHost for DaemonHost {
    async fn mesh(&self) -> Mesh {
        probe_peers(
            &self.machines,
            &self.machine_id,
            self.machine.record().bootstrap(),
        )
        .await
        .into()
    }

    async fn ask(&self) -> Vec<Asked> {
        let Some(machines) = self.machines.current() else {
            return Vec::new();
        };
        let peers = machines
            .observations
            .iter()
            .filter(|peer| peer.id != self.machine_id)
            .filter_map(|peer| {
                peer.runtime
                    .management_identity
                    .map(|identity| (peer.name.clone(), identity))
            });
        join_all(peers.map(|(peer, identity)| async move {
            let answer = tokio::time::timeout(
                ASK_TIMEOUT,
                forgotten::ask(&self.endpoint, identity, &self.relay),
            )
            .await
            .ok()
            .flatten();
            Asked { peer, answer }
        }))
        .await
    }

    async fn fence(&self) -> Result<(), String> {
        match self.machine.fence_forgotten().await {
            Ok(Left::Left { .. }) => Ok(()),
            Ok(Left::Refused { error, .. }) => Err(error.message),
            Err(error) => Err(ployz_core::error_chain::inline(&error)),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::VecDeque, sync::Mutex};

    use super::*;

    fn name(name: &str) -> MachineName {
        MachineName::parse(name).unwrap()
    }

    fn asked(peer: &str, answer: Option<ForgottenAnswer>) -> Asked {
        Asked {
            peer: name(peer),
            answer,
        }
    }

    #[test]
    fn a_forgotten_answer_fences_unless_a_peer_still_lists_this_machine() {
        use ForgottenAnswer::{Forgotten, Listed, NoAnswer};
        let fence = |by: &str| Verdict::Fence { told_by: name(by) };
        let cases = [
            (vec![], Verdict::Stay),
            (vec![asked("a", None)], Verdict::Stay),
            (vec![asked("a", Some(NoAnswer))], Verdict::Stay),
            (vec![asked("a", Some(Forgotten))], fence("a")),
            (
                vec![asked("a", None), asked("b", Some(Forgotten))],
                fence("b"),
            ),
            (
                vec![asked("a", Some(Forgotten)), asked("b", Some(Listed))],
                Verdict::Stay,
            ),
            (
                vec![asked("a", Some(Listed)), asked("b", Some(Forgotten))],
                Verdict::Stay,
            ),
        ];
        for (answers, expected) in cases {
            assert_eq!(verdict(&answers), expected, "{answers:?}");
        }
    }

    /// Scripted probes, answers and fence results; each queue repeats its last entry.
    struct FakeHost {
        mesh: Mutex<VecDeque<Mesh>>,
        answers: Mutex<VecDeque<Vec<Asked>>>,
        fences: Mutex<VecDeque<Result<(), String>>>,
        asks: Mutex<Vec<Duration>>,
        fenced: Mutex<Vec<Duration>>,
        started: Instant,
    }

    impl FakeHost {
        fn new(mesh: impl IntoIterator<Item = Mesh>) -> Self {
            Self {
                mesh: Mutex::new(mesh.into_iter().collect()),
                answers: Mutex::default(),
                fences: Mutex::default(),
                asks: Mutex::default(),
                fenced: Mutex::default(),
                started: Instant::now(),
            }
        }

        fn answering(self, answers: impl IntoIterator<Item = Vec<Asked>>) -> Self {
            *self.answers.lock().unwrap() = answers.into_iter().collect();
            self
        }

        fn fencing(self, fences: impl IntoIterator<Item = Result<(), String>>) -> Self {
            *self.fences.lock().unwrap() = fences.into_iter().collect();
            self
        }

        fn elapsed(&self) -> Duration {
            self.started.elapsed()
        }
    }

    fn next<T: Clone>(queue: &Mutex<VecDeque<T>>) -> T {
        let mut queue = queue.lock().unwrap();
        if queue.len() > 1 {
            queue.pop_front().unwrap()
        } else {
            queue.front().cloned().expect("a scripted value")
        }
    }

    impl FenceHost for FakeHost {
        async fn mesh(&self) -> Mesh {
            next(&self.mesh)
        }

        async fn ask(&self) -> Vec<Asked> {
            self.asks.lock().unwrap().push(self.elapsed());
            next(&self.answers)
        }

        async fn fence(&self) -> Result<(), String> {
            self.fenced.lock().unwrap().push(self.elapsed());
            next(&self.fences)
        }
    }

    fn secs(values: &[u64]) -> Vec<Duration> {
        values
            .iter()
            .map(|value| Duration::from_secs(*value))
            .collect()
    }

    /// Runs the loop for `budget` of paused time; true when it returned by itself.
    async fn run_for(host: &FakeHost, budget: Duration) -> bool {
        tokio::time::timeout(budget, run(host, CancellationToken::new()))
            .await
            .is_ok()
    }

    impl<H: FenceHost> FenceHost for &H {
        fn mesh(&self) -> impl Future<Output = Mesh> + Send {
            (**self).mesh()
        }

        fn ask(&self) -> impl Future<Output = Vec<Asked>> + Send {
            (**self).ask()
        }

        fn fence(&self) -> impl Future<Output = Result<(), String>> + Send {
            (**self).fence()
        }
    }

    #[tokio::test(start_paused = true)]
    async fn a_silence_shorter_than_a_minute_asks_nothing() {
        let host = FakeHost::new([Mesh::Silent, Mesh::Answered]).answering([vec![]]);
        assert!(!run_for(&host, Duration::from_secs(3600)).await);
        assert_eq!(*host.asks.lock().unwrap(), Vec::<Duration>::new());
    }

    #[tokio::test(start_paused = true)]
    async fn a_machine_alone_never_asks() {
        let host = FakeHost::new([Mesh::Alone]).answering([vec![]]);
        assert!(!run_for(&host, Duration::from_secs(3600)).await);
        assert!(host.asks.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_silent_mesh_asks_with_backoff_and_an_answer_resets_it() {
        let silent = std::iter::repeat_n(Mesh::Silent, 9);
        let host = FakeHost::new(silent.chain([Mesh::Answered, Mesh::Silent]))
            .answering([vec![asked("a", Some(ForgottenAnswer::NoAnswer))]]);
        assert!(!run_for(&host, Duration::from_secs(480)).await);
        // Probes at 30 s steps: silent from 30 s, asked from 90 s, backoff 60 s then
        // 120 s, answered at 300 s, silent again from 330 s, asked again at 390 s
        // with the backoff back at 60 s.
        assert_eq!(*host.asks.lock().unwrap(), secs(&[90, 150, 270, 390, 450]));
        assert!(host.fenced.lock().unwrap().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn a_forgotten_answer_fences_and_the_loop_ends() {
        let host = FakeHost::new([Mesh::Silent])
            .answering([vec![
                asked("a", None),
                asked("b", Some(ForgottenAnswer::Forgotten)),
            ]])
            .fencing([Ok(())]);
        assert!(run_for(&host, Duration::from_secs(3600)).await);
        assert_eq!(*host.asks.lock().unwrap(), secs(&[90]));
        assert_eq!(*host.fenced.lock().unwrap(), secs(&[90]));
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_fence_is_asked_and_tried_again() {
        let host = FakeHost::new([Mesh::Silent])
            .answering([vec![asked("a", Some(ForgottenAnswer::Forgotten))]])
            .fencing([Err("an upgrade is running".into()), Ok(())]);
        assert!(run_for(&host, Duration::from_secs(3600)).await);
        assert_eq!(*host.asks.lock().unwrap(), secs(&[90, 150]));
        assert_eq!(*host.fenced.lock().unwrap(), secs(&[90, 150]));
    }
}
