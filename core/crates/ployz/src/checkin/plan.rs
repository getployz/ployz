//! One check-in pass: read how far preparation has got, probe the host, post the signed
//! check-in and decide what the loop does next.

use std::{fs, io, path::Path, time::Duration};

use chrono::{DateTime, Utc};
use ployz_core::cloud_contract::{CHECKIN_PROTOCOL_VERSION, ENROLL_OPTIONS};
use ployz_core::host_preparation::{PrepSteps, StateError, read_state};

use super::host::PROBE_TIMEOUT;
use super::paths::Paths;
use super::probe::{self, Probes};
use super::status::{LocalCheckin, Refusal, Seen, record};
use super::wire::{Answer, CheckinRequest, PrepRecord, host_report};
use super::{
    Attempt, CheckinClient, CheckinKey, Hold, Host, Issue, StopReason, client, enroll, unit,
};

pub(super) const PREPARE_RETRY: Duration = Duration::from_secs(10 * 60);
pub(super) const PREPARE_TRIES: u32 = 3;
/// The widest gap, on Cloud's clock, one refused pass to the next can explain. A wider one, or a
/// second refusal dated before the first, starts the confirmation over.
const REFUSAL_SPAN: Duration = Duration::from_secs(60 * 60);
pub(super) const REVOKED: &str = "Cloud no longer accepts this Fleet Token.";
pub(super) const REFUSED_ONCE: &str =
    "Cloud refused this Fleet Token. It is checked once more before the loop removes it";
const STALE_PLOYZD: &str = "ployzd_outdated";
const IN_ANOTHER_CLUSTER: &str = "in_another_cluster";
const WIREGUARD_PORT_IN_USE: &str = "wireguard_port_in_use";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Next {
    Wait {
        after: Duration,
    },
    /// Cloud has an enrollment ready. Only an install pass answers this, and leaves it to the loop.
    Enrolling,
    Retry {
        after: Duration,
        why: String,
    },
    RefusedOnce {
        after: Duration,
    },
    Held {
        after: Duration,
        hold: Hold,
        why: String,
    },
    Stop(StopReason, String),
    ClockOff(String),
    NotCloud(String),
    Enrolled,
}

#[cfg(test)]
pub(crate) async fn pass<H: Host>(
    host: &H,
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    now: DateTime<Utc>,
) -> Next {
    pass_from(
        host,
        client,
        key,
        paths,
        now,
        false,
        &mut Probes::load(paths.probe(), host.binder()),
    )
    .await
}

/// What an explicit `--install` pass got back, and what it saw for status.json. The install
/// records `seen` only once it keeps the token sent: a replacement Cloud didn't take leaves the
/// held token's status, and its enrollment, as they were.
pub(crate) struct InstallPass {
    pub(crate) next: Next,
    pub(crate) seen: Option<LocalCheckin>,
}

#[cfg(test)]
impl InstallPass {
    /// Records what the pass saw, as a pass the install keeps does.
    pub(crate) fn record(self, paths: &Paths) -> Next {
        if let Some(seen) = &self.seen {
            record(seen, paths);
        }
        self.next
    }
}

/// Whether this host joined through `attempt`: it didn't fail, and the daemon now participates.
pub(crate) async fn joined<H: Host>(host: &H, attempt: Option<&Attempt>) -> bool {
    attempt.is_some_and(|attempt| attempt.failure.is_none()) && host.participating().await.is_some()
}

/// The pass an explicit `--install` makes. It leaves preparation alone, since the token may yet be
/// refused; `restart_preparation` runs once Cloud has answered.
pub(crate) async fn install_pass<H: Host>(
    host: &H,
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    now: DateTime<Utc>,
) -> InstallPass {
    let (next, seen) = check_in(
        host,
        client,
        key,
        paths,
        now,
        true,
        &mut Probes::load(paths.probe(), host.binder()),
    )
    .await;
    InstallPass { next, seen }
}

pub(super) async fn pass_from<H: Host>(
    host: &H,
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    now: DateTime<Utc>,
    installing: bool,
    probes: &mut Probes,
) -> Next {
    let (next, seen) = check_in(host, client, key, paths, now, installing, probes).await;
    if let Some(seen) = seen {
        record(&seen, paths);
    }
    next
}

/// One check-in, and the status it saw. No status means an enroll attempt ran and recorded its
/// own.
async fn check_in<H: Host>(
    host: &H,
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    now: DateTime<Utc>,
    installing: bool,
    probes: &mut Probes,
) -> (Next, Option<LocalCheckin>) {
    let previous = LocalCheckin::read(&paths.status()).ok().flatten();
    let enrollment = previous.as_ref().filter(|status| {
        status.token_fingerprint.as_deref() == Some(client.fingerprint().as_str())
    });
    if !installing && enrollment.is_some_and(|status| status.answer == Seen::Joined) {
        return (Next::Enrolled, None);
    }
    let refusal = previous
        .as_ref()
        .filter(|status| status.answer == Seen::Refused)
        .and_then(|status| status.refused.clone())
        .filter(|refusal| refusal.token == client.fingerprint());
    let mut prepare_retries = previous.as_ref().map_or(0, |status| status.prepare_retries);
    let attempt = enrollment.and_then(|status| status.attempt.clone());
    let mut prepare_started = previous.and_then(|status| status.prepare_started);
    let stale = host.stale_ployzd().map(|message| Issue {
        code: STALE_PLOYZD.to_owned(),
        message,
    });
    let (liveness, notes) = observe_liveness(host, paths).await;
    notes.into_iter().for_each(crate::ui::note);
    // Read after liveness, so a run that completed in between reads as finished: ployzd saves
    // each step's outcome before it lets the lock go. A run that stops before such a save lands
    // can leave a step running, which reads as died mid-step.
    let (state, written) = read_prep_state(&paths.prepare_state).unzip();
    let preparation = preparation(
        state.as_ref(),
        written.flatten(),
        liveness,
        prepare_retries,
        prepare_started,
        now,
    );
    let mut preparing = liveness == Liveness::Live;
    // An older ployzd may not know how to prepare, and its blocker already says what to do.
    let run = if stale.is_some() || installing {
        None
    } else {
        match preparation {
            Preparation::NotStarted => Some(PrepareRun::Start),
            Preparation::RetryDue => Some(PrepareRun::Retry),
            Preparation::Running
            | Preparation::Unknown
            | Preparation::DiedMidStep
            | Preparation::Failed
            | Preparation::Finished
            | Preparation::GaveUp => None,
        }
    };
    if let Some(run) = run {
        match host.start_prepare().await {
            Ok(()) => {
                // The run just started may already be installing Docker before the probe below.
                // This pass printed its notes above.
                preparing = observe_liveness(host, paths).await.0 == Liveness::Live;
                prepare_started = Some(now);
                if run == PrepareRun::Retry {
                    prepare_retries += 1;
                }
            }
            Err(why) => crate::ui::note(format!("Could not start {}: {why}", unit::PREPARE)),
        }
    }
    let gave_up = preparation == Preparation::GaveUp && !installing;
    let state = state.unwrap_or_default();
    let prep = state.prep(gave_up);
    let mut status = LocalCheckin {
        answer: Seen::ProbeFailed,
        token_fingerprint: Some(client.fingerprint()),
        reason: None,
        message: None,
        at: now,
        prep: Some(prep),
        blockers: Vec::new(),
        prepare_retries,
        prepare_started,
        refused: None,
        attempt,
        no_cluster: false,
    };
    status.blockers.extend(stale.clone());
    let member = if status.attempt.is_none() {
        host.participating().await.map(|name| Issue {
            code: IN_ANOTHER_CLUSTER.to_owned(),
            message: format!(
                "Already in another cluster. Remove it there first: ployz server rm {name}"
            ),
        })
    } else {
        None
    };
    status.blockers.extend(member.clone());
    // Someone else on WireGuard's port leaves ployzd crash-looping once it starts.
    let port_taken = host.wireguard_port_holder().map(|holder| Issue {
        code: WIREGUARD_PORT_IN_USE.to_owned(),
        message: format!(
            "UDP port {} is in use by {holder}. Ployz needs it for WireGuard. Free it, for \
             example by restarting that program.",
            ployz_core::WIREGUARD_PORT
        ),
    });
    status.blockers.extend(port_taken.clone());
    let probed = tokio::time::timeout(PROBE_TIMEOUT, host.probe())
        .await
        .unwrap_or_else(|_| {
            Err(format!(
                "ployzd install --probe did not finish in {}s",
                PROBE_TIMEOUT.as_secs()
            ))
        });
    let probe = match probed {
        Ok(probe) => probe,
        Err(why) => {
            status.message = Some(why.clone());
            let next = Next::Retry {
                after: probes.retry_after(client::BACKOFF),
                why: format!("Could not probe this server: {why}."),
            };
            return (next, Some(status));
        }
    };
    let mut report = host_report(
        probe,
        host.hostname(),
        &state,
        preparing,
        gave_up,
        prepare_started,
    );
    report.blockers.extend(stale);
    report.blockers.extend(member);
    report.blockers.extend(port_taken);
    status.blockers.clone_from(&report.blockers);
    let request = CheckinRequest {
        protocol_version: CHECKIN_PROTOCOL_VERSION,
        checkin_key: key.public_hex(),
        cli_version: env!("CARGO_PKG_VERSION"),
        enroll_options: ENROLL_OPTIONS,
        report: &report,
        prep: Some(prep),
        attempt: status.attempt.as_ref(),
        probe: probes.report().await,
    };
    let sent = tokio::time::Instant::now();
    let reply = client.post(key, &request).await;
    if request.probe.is_some() && matches!(&reply, client::Reply::Answer(_)) {
        probes.reported();
    }
    let throttled = matches!(reply, client::Reply::Throttled(..));
    let next = match reply {
        client::Reply::Answer(Answer::Wait(_))
            if !installing && joined(host, status.attempt.as_ref()).await =>
        {
            status.answer = Seen::Joined;
            Next::Enrolled
        }
        client::Reply::Answer(Answer::Wait(wait)) => {
            status.waited(wait);
            if status.enroll_failed() {
                status.message = status
                    .attempt
                    .as_ref()
                    .and_then(|attempt| attempt.failure.as_ref())
                    .map(enroll::failure_line);
            }
            Next::Wait { after: wait.after }
        }
        client::Reply::Answer(Answer::Probe(wait, handout)) => {
            probes.open(handout, key, sent).await;
            if installing {
                crate::ui::note(format!(
                    "Listening on the probe ports for up to {}s.",
                    probe::WINDOW_CAP.as_secs()
                ));
                probes.finish().await;
            }
            status.waited(wait);
            Next::Wait { after: wait.after }
        }
        client::Reply::Answer(Answer::Enroll(_)) if installing => {
            probes.close().await;
            status.answer = Seen::Wait;
            Next::Enrolling
        }
        client::Reply::Answer(Answer::Enroll(instruction)) => {
            probes.close().await;
            let next = enroll::attempt(
                host,
                client,
                key,
                paths,
                &enroll::Report {
                    report: &report,
                    prep,
                },
                &instruction,
                status,
            )
            .await;
            return (next, None);
        }
        client::Reply::Answer(Answer::Stop(reason, message)) => {
            status.answer = Seen::Stop;
            status.reason = Some(reason);
            status.message = Some(message.clone());
            Next::Stop(reason, message)
        }
        client::Reply::Refused(date) => {
            let since = |first: &Refusal| date.signed_duration_since(first.date).to_std();
            let first = refusal
                .filter(|first| since(first).is_ok_and(|gap| gap <= REFUSAL_SPAN))
                .unwrap_or_else(|| Refusal {
                    token: client.fingerprint(),
                    date,
                });
            let waited = since(&first).unwrap_or_default();
            if waited >= client::BACKOFF {
                status.answer = Seen::Stop;
                status.reason = Some(StopReason::TokenRevoked);
                status.message = Some(REVOKED.to_owned());
                Next::Stop(StopReason::TokenRevoked, REVOKED.to_owned())
            } else {
                status.answer = Seen::Refused;
                status.message = Some(REFUSED_ONCE.to_owned());
                status.refused = Some(first);
                Next::RefusedOnce {
                    after: client::BACKOFF
                        .saturating_sub(waited)
                        .max(Duration::from_secs(1)),
                }
            }
        }
        client::Reply::Held(after, hold, why) => {
            status.answer = Seen::Held;
            status.message = Some(why.clone());
            Next::Held {
                after,
                hold,
                why: format!("Cloud held this check-in back: {why}"),
            }
        }
        client::Reply::Unreachable(after, why) | client::Reply::Throttled(after, why) => {
            // A report still owed retries sooner after a failure, but never sooner than a Retry-After asked.
            let after = if throttled {
                after
            } else {
                probes.retry_after(after)
            };
            status.answer = Seen::Unreachable;
            status.message = Some(why.clone());
            Next::Retry {
                after,
                why: format!(
                    "Could not reach Ployz Cloud: {}.",
                    why.trim_end_matches('.')
                ),
            }
        }
        client::Reply::NotCloud(why) => {
            status.answer = Seen::NotCloud;
            status.message = Some(why.clone());
            Next::NotCloud(why)
        }
        client::Reply::ClockOff(why) => {
            status.answer = Seen::ClockOff;
            status.message = Some(why.clone());
            Next::ClockOff(why)
        }
        client::Reply::Outdated(why) => {
            status.answer = Seen::Outdated;
            status.message = Some(why.clone());
            Next::Retry {
                after: client::OUTDATED_BACKOFF,
                why: format!("{why}."),
            }
        }
    };
    if matches!(next, Next::Stop(..) | Next::Enrolled) {
        probes.close().await;
    }
    (next, Some(status))
}

/// After Cloud answers an explicit `--install`: a failed or unfinished preparation starts over at
/// once, with its automatic retries restored. Its state file stays, since ployzd records there what
/// it added. Returns whether it started one.
pub(crate) async fn restart_preparation<H: Host>(
    host: &H,
    paths: &Paths,
    now: DateTime<Utc>,
) -> bool {
    let (liveness, notes) = observe_liveness(host, paths).await;
    notes.into_iter().for_each(crate::ui::note);
    let (state, written) = read_prep_state(&paths.prepare_state).unzip();
    let status = LocalCheckin::read(&paths.status()).ok().flatten();
    let preparation = preparation(
        state.as_ref(),
        written.flatten(),
        liveness,
        status.as_ref().map_or(0, |status| status.prepare_retries),
        status.as_ref().and_then(|status| status.prepare_started),
        now,
    );
    let settled = match preparation {
        Preparation::Running | Preparation::Unknown | Preparation::Finished => true,
        Preparation::NotStarted
        | Preparation::DiedMidStep
        | Preparation::Failed
        | Preparation::RetryDue
        | Preparation::GaveUp => false,
    };
    if settled || host.stale_ployzd().is_some() {
        return false;
    }
    if let Err(why) = host.start_prepare().await {
        crate::ui::note(format!("Could not start {}: {why}", unit::PREPARE));
        return false;
    }
    if let Some(mut status) = status {
        status.restarted_preparation(now);
        record(&status, paths);
    }
    true
}

/// ployzd's preparation state and when it was last written, or none when there is none or it
/// can't be read.
pub(super) fn read_prep_state(path: &Path) -> Option<(PrepSteps, Option<DateTime<Utc>>)> {
    let note = |cause: &dyn std::fmt::Display| {
        crate::ui::note(format!("Could not read {}: {cause}", path.display()));
    };
    let state = match read_state(path) {
        Ok(state) => state?,
        Err(StateError::Unreadable(error)) => {
            note(&error);
            return None;
        }
        Err(StateError::Malformed(error)) => {
            note(&error);
            return None;
        }
    };
    let written = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .map(DateTime::<Utc>::from);
    Some((state, written))
}

/// Whether host preparation runs, from the two things that can say so: systemd's
/// `ployz-prepare` unit, and the mark a preparation run puts on ployzd's lock, which the build
/// children it starts keep. Either one seen running makes it live, as a run started by hand is,
/// including a build that outlived its hand-run ployzd. A mark that can't be read leaves the unit
/// to decide alone, as before there was a mark; a unit that can't be read leaves it unknown,
/// which never counts as stopped.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Liveness {
    Live,
    Stopped,
    Unknown,
}

/// Whether this process has printed the note that the lock's filesystem keeps no mark it can
/// read. It prints once per process on purpose, so the long-running check-in loop doesn't repeat
/// it every pass. The reader still checks the filesystem of the file it opens on every pass
/// ([`preparation_marked`]).
static NOTED_UNREADABLE_FILESYSTEM: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

/// Liveness as seen now, and the notes worth printing about how it was read.
pub(super) async fn observe_liveness<H: Host>(host: &H, paths: &Paths) -> (Liveness, Vec<String>) {
    let unit = host.prepare_running().await;
    if unit == Ok(true) {
        return (Liveness::Live, Vec::new());
    }
    let mut notes = Vec::new();
    if let Err(why) = &unit {
        notes.push(format!("Could not read {}: {why}", unit::PREPARE));
    }
    let lock = &paths.prepare_lock;
    let liveness = match (unit, host.preparation_marked(lock)) {
        (Ok(_), Ok(true)) => {
            notes.push(format!(
                "{} is stopped, but a preparation run holds {}; waiting for it.",
                unit::PREPARE,
                lock.display()
            ));
            Liveness::Live
        }
        (Err(_), Ok(true)) => Liveness::Live,
        (Ok(_), Ok(false)) => Liveness::Stopped,
        (Ok(_), Err(error)) => {
            // A kernel without open file description locks fails the query with EINVAL on every
            // pass, so that is never noted; a filesystem off the allowlist is noted once.
            let say = if error.kind() == io::ErrorKind::Unsupported {
                !NOTED_UNREADABLE_FILESYSTEM.swap(true, std::sync::atomic::Ordering::Relaxed)
            } else {
                error.kind() != io::ErrorKind::InvalidInput
            };
            if say {
                notes.push(format!(
                    "Could not tell whether a preparation run holds {}: {error}. Going by {} alone, which reads stopped.",
                    lock.display(),
                    unit::PREPARE
                ));
            }
            Liveness::Stopped
        }
        (Err(_), Ok(false) | Err(_)) => Liveness::Unknown,
    };
    (liveness, notes)
}

/// Host preparation as this check-in sees it now. Liveness says whether a run is live; the rest
/// comes from ployzd's state file and this loop's own starts. None of it is persisted.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Preparation {
    /// No run left a dated record, and this loop has started none.
    NotStarted,
    Running,
    /// Nothing says whether a run is live, so nothing is started or given up.
    Unknown,
    /// A run stopped with a step unfinished, too recently to retry.
    DiedMidStep,
    /// A step failed, and is not retried yet or any more.
    Failed,
    Finished,
    RetryDue,
    /// The retries are spent, and a run stopped with a step unfinished.
    GaveUp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrepareRun {
    Start,
    Retry,
}

/// How preparation stands, `written` being when the state file was last written, and `retries`
/// and `last_start` this loop's retries and its last start of `ployz-prepare`. A run that stopped
/// is retried once it has waited [`PREPARE_RETRY`] since the later of its last record and its
/// last start, up to [`PREPARE_TRIES`] times. A run's last record mid-step is its latest save:
/// the ZFS step saves what it adds as it goes, without a new time of its own.
pub(super) fn preparation(
    state: Option<&PrepSteps>,
    written: Option<DateTime<Utc>>,
    liveness: Liveness,
    retries: u32,
    last_start: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> Preparation {
    match liveness {
        Liveness::Live => return Preparation::Running,
        Liveness::Unknown => return Preparation::Unknown,
        Liveness::Stopped => {}
    }
    let stopped = match state {
        Some(state) if !state.unfinished() && !state.failed() => return Preparation::Finished,
        Some(state) if !state.unfinished() => Stopped::Failed(state.failed_at()),
        Some(state) => Stopped::MidStep(state.last_dated().max(written)),
        None => Stopped::MidStep(None),
    };
    let since = match stopped {
        Stopped::MidStep(dated) => dated.max(last_start),
        Stopped::Failed(at) => at.max(last_start),
    };
    let waited = since.map(|since| now.signed_duration_since(since).to_std());
    // A time ahead of this host's clock has not been waited out.
    let due = waited.is_none_or(|waited| waited.is_ok_and(|waited| waited >= PREPARE_RETRY));
    let spent = retries >= PREPARE_TRIES;
    match (stopped, since) {
        (Stopped::MidStep(_), None) => Preparation::NotStarted,
        (Stopped::MidStep(_), Some(_)) if spent => Preparation::GaveUp,
        (Stopped::MidStep(_), Some(_)) if !due => Preparation::DiedMidStep,
        (Stopped::Failed(_), _) if !due || spent => Preparation::Failed,
        (Stopped::MidStep(_) | Stopped::Failed(_), _) => Preparation::RetryDue,
    }
}

/// How a preparation that isn't running stopped: mid-step, last dated at the time given, or with
/// a step failed at the time given.
#[derive(Clone, Copy)]
enum Stopped {
    MidStep(Option<DateTime<Utc>>),
    Failed(Option<DateTime<Utc>>),
}
