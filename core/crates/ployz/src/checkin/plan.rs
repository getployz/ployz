//! One check-in pass: read how far preparation has got, probe the host, post the signed
//! check-in and decide what the loop does next.

use std::{fs, io, path::Path, time::Duration};

use chrono::{DateTime, Utc};
use ployz_core::cloud_contract::{CHECKIN_PROTOCOL_VERSION, ENROLL_OPTIONS};
use ployz_core::host_preparation::{PrepSteps, StateError, read_state};

use super::host::PROBE_TIMEOUT;
use super::paths::Paths;
use super::probe::{self, Handout, Probes};
use super::status::{LocalCheckin, Recalled, Refusal, Seen, record};
use super::wire::{Answer, CheckinRequest, HostReport, PrepRecord, Wait, host_report};
use super::{
    Attempt, CheckinClient, CheckinKey, EnrollInstruction, Hold, Host, Issue, Prep, StopReason,
    client, enroll, unit,
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

/// What the loop does after a pass that did not join this host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Next {
    Wait {
        after: Duration,
    },
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
}

/// How a pass of the loop ended.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Pass {
    Next(Next),
    /// This host joined: the pass saw it had, or ran the enrollment that did it.
    Joined,
}

/// What an explicit `--install` pass got back. Cloud's enrollment is the loop's to run, so the
/// install only learns that it is ready.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Posted {
    Said(Next),
    EnrollReady,
}

impl From<Next> for Posted {
    fn from(next: Next) -> Self {
        Self::Said(next)
    }
}

#[cfg(test)]
impl Posted {
    /// The `Next` of a pass that was not ready to enroll.
    pub(crate) fn next(self) -> Next {
        match self {
            Self::Said(next) => next,
            Self::EnrollReady => panic!("Cloud was ready to enroll"),
        }
    }
}

#[cfg(test)]
impl Pass {
    /// The `Next` of a pass that did not join.
    pub(crate) fn next(self) -> Next {
        match self {
            Self::Next(next) => next,
            Self::Joined => panic!("the pass joined this host"),
        }
    }
}

#[cfg(test)]
pub(crate) async fn pass<H: Host>(
    host: &H,
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    now: DateTime<Utc>,
) -> Next {
    pass_with(
        host,
        client,
        key,
        paths,
        now,
        &mut Probes::load(paths.probe(), host.binder()),
    )
    .await
}

#[cfg(test)]
pub(crate) async fn passed<H: Host>(
    host: &H,
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    now: DateTime<Utc>,
) -> Pass {
    loop_pass(
        host,
        client,
        key,
        paths,
        now,
        &mut Probes::load(paths.probe(), host.binder()),
    )
    .await
}

#[cfg(test)]
pub(crate) async fn pass_with<H: Host>(
    host: &H,
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    now: DateTime<Utc>,
    probes: &mut Probes,
) -> Next {
    loop_pass(host, client, key, paths, now, probes)
        .await
        .next()
}

/// What an explicit `--install` pass got back, and what it saw for status.json. The install
/// records `seen` only once it keeps the token sent: a replacement Cloud didn't take leaves the
/// held token's status, and its enrollment, as they were.
pub(crate) struct InstallPass {
    pub(crate) posted: Posted,
    pub(crate) seen: Option<LocalCheckin>,
}

#[cfg(test)]
impl InstallPass {
    /// Records what the pass saw, as a pass the install keeps does.
    pub(crate) fn record(self, paths: &Paths) -> Posted {
        if let Some(seen) = &self.seen {
            record(seen, paths);
        }
        self.posted
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
    install_pass_with(
        host,
        client,
        key,
        paths,
        now,
        &mut Probes::load(paths.probe(), host.binder()),
    )
    .await
}

pub(super) async fn install_pass_with<H: Host>(
    host: &H,
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    now: DateTime<Utc>,
    probes: &mut Probes,
) -> InstallPass {
    let start = Start {
        mode: Mode::Install,
        recalled: Recalled::of(LocalCheckin::recall(&paths.status()), &client.fingerprint()),
    };
    let exchange = match exchange(host, client, key, paths, now, start, probes).await {
        Ok(exchange) => exchange,
        Err(unprobed) => {
            return InstallPass {
                posted: unprobed.next.into(),
                seen: Some(unprobed.status),
            };
        }
    };
    let Exchange {
        mut status,
        heard,
        sent,
        ..
    } = exchange;
    let posted = match heard {
        Heard::Wait(wait) => waiting(&mut status, wait).into(),
        Heard::Probe(wait, handout) => {
            probes.open(handout, key, sent).await;
            crate::ui::note(format!(
                "Listening on the probe ports for up to {}s.",
                probe::WINDOW_CAP.as_secs()
            ));
            probes.finish().await;
            status.waited(wait);
            Next::Wait { after: wait.after }.into()
        }
        Heard::Enroll(_) => {
            probes.close().await;
            status.view.answer = Seen::Wait;
            Posted::EnrollReady
        }
        Heard::Verdict(verdict) => {
            let next = verdict.write(&mut status);
            if matches!(next, Next::Stop(..)) {
                probes.close().await;
            }
            next.into()
        }
    };
    InstallPass {
        posted,
        seen: Some(status),
    }
}

/// One pass of the check-in loop, which records what it saw. A host Cloud's enrollment already
/// joined ends the pass before it asks anything of the host or Cloud.
pub(super) async fn loop_pass<H: Host>(
    host: &H,
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    now: DateTime<Utc>,
    probes: &mut Probes,
) -> Pass {
    let recalled = Recalled::of(LocalCheckin::recall(&paths.status()), &client.fingerprint());
    if recalled.joined {
        return Pass::Joined;
    }
    let start = Start {
        mode: Mode::Loop,
        recalled,
    };
    let exchange = match exchange(host, client, key, paths, now, start, probes).await {
        Ok(exchange) => exchange,
        Err(unprobed) => {
            record(&unprobed.status, paths);
            return Pass::Next(unprobed.next);
        }
    };
    let Exchange {
        mut status,
        report,
        prep,
        heard,
        sent,
    } = exchange;
    let pass = match heard {
        Heard::Wait(_) if joined(host, status.memory.attempt.as_ref()).await => {
            status.view.answer = Seen::Joined;
            Pass::Joined
        }
        Heard::Wait(wait) => Pass::Next(waiting(&mut status, wait)),
        Heard::Probe(wait, handout) => {
            probes.open(handout, key, sent).await;
            status.waited(wait);
            Pass::Next(Next::Wait { after: wait.after })
        }
        Heard::Enroll(instruction) => {
            probes.close().await;
            return enroll::attempt(
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
        }
        Heard::Verdict(verdict) => Pass::Next(verdict.write(&mut status)),
    };
    if matches!(pass, Pass::Joined | Pass::Next(Next::Stop(..))) {
        probes.close().await;
    }
    record(&status, paths);
    pass
}

/// Whether a pass is the loop's own or the one an explicit `--install` makes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Loop,
    Install,
}

/// How a pass starts: its mode, and what it recalls of the record the last pass left.
struct Start {
    mode: Mode,
    recalled: Recalled,
}

/// A pass that got as far as posting, and what Cloud answered.
struct Exchange {
    status: LocalCheckin,
    report: HostReport,
    prep: Prep,
    heard: Heard,
    sent: tokio::time::Instant,
}

/// A pass that never reached Cloud, since the host's probe failed.
struct Unprobed {
    next: Next,
    status: LocalCheckin,
}

/// What Cloud's reply asks of the pass. A wait, a probe window and an enrollment mean something
/// different to the loop and to an install; every other reply is a [`Verdict`] either would reach.
enum Heard {
    Wait(Wait),
    Probe(Wait, Handout),
    Enroll(Box<EnrollInstruction>),
    Verdict(Verdict),
}

/// What a reply that ends the pass the same way in either mode says: what `ployz status` shows,
/// and what the loop does next.
struct Verdict {
    seen: Seen,
    reason: Option<StopReason>,
    message: String,
    refused: Option<Refusal>,
    next: Next,
}

impl Verdict {
    fn said(seen: Seen, message: String, next: Next) -> Self {
        Self {
            seen,
            reason: None,
            message,
            refused: None,
            next,
        }
    }

    fn stop(reason: StopReason, message: String) -> Self {
        Self {
            reason: Some(reason),
            ..Self::said(Seen::Stop, message.clone(), Next::Stop(reason, message))
        }
    }

    /// Cloud's refusal of this token, dated `date`. A second one, dated BACKOFF after the first
    /// within [`REFUSAL_SPAN`], confirms it.
    fn refused(date: DateTime<Utc>, refusal: Option<Refusal>, fingerprint: &str) -> Self {
        let since = |first: &Refusal| date.signed_duration_since(first.date).to_std();
        let first = refusal
            .filter(|first| since(first).is_ok_and(|gap| gap <= REFUSAL_SPAN))
            .unwrap_or_else(|| Refusal {
                token: fingerprint.to_owned(),
                date,
            });
        let waited = since(&first).unwrap_or_default();
        if waited >= client::BACKOFF {
            Self::stop(StopReason::TokenRevoked, REVOKED.to_owned())
        } else {
            Self {
                refused: Some(first),
                ..Self::said(
                    Seen::Refused,
                    REFUSED_ONCE.to_owned(),
                    Next::RefusedOnce {
                        after: client::BACKOFF
                            .saturating_sub(waited)
                            .max(Duration::from_secs(1)),
                    },
                )
            }
        }
    }

    /// A failed post. A report still owed retries sooner after a failure, but never sooner than a
    /// Retry-After asked: the caller passes the wait it settled on.
    fn unreachable(after: Duration, why: String) -> Self {
        Self::said(
            Seen::Unreachable,
            why.clone(),
            Next::Retry {
                after,
                why: format!(
                    "Could not reach Ployz Cloud: {}.",
                    why.trim_end_matches('.')
                ),
            },
        )
    }

    /// Writes what the pass saw into `status` and returns what to do next.
    fn write(self, status: &mut LocalCheckin) -> Next {
        status.view.answer = self.seen;
        status.view.reason = self.reason;
        status.view.message = Some(self.message);
        status.memory.refused = self.refused;
        self.next
    }
}

/// Sorts what Cloud answered by what it asks of the pass.
fn hear(
    reply: client::Reply,
    refusal: Option<Refusal>,
    fingerprint: &str,
    probes: &Probes,
) -> Heard {
    let verdict = match reply {
        client::Reply::Answer(Answer::Wait(wait)) => return Heard::Wait(wait),
        client::Reply::Answer(Answer::Probe(wait, handout)) => return Heard::Probe(wait, handout),
        client::Reply::Answer(Answer::Enroll(instruction)) => return Heard::Enroll(instruction),
        client::Reply::Answer(Answer::Stop(reason, message)) => Verdict::stop(reason, message),
        client::Reply::Refused(date) => Verdict::refused(date, refusal, fingerprint),
        client::Reply::Held(after, hold, why) => Verdict::said(
            Seen::Held,
            why.clone(),
            Next::Held {
                after,
                hold,
                why: format!("Cloud held this check-in back: {why}"),
            },
        ),
        client::Reply::Unreachable(after, why) => {
            Verdict::unreachable(probes.retry_after(after), why)
        }
        client::Reply::Throttled(after, why) => Verdict::unreachable(after, why),
        client::Reply::NotCloud(why) => {
            Verdict::said(Seen::NotCloud, why.clone(), Next::NotCloud(why))
        }
        client::Reply::ClockOff(why) => {
            Verdict::said(Seen::ClockOff, why.clone(), Next::ClockOff(why))
        }
        client::Reply::Outdated(why) => Verdict::said(
            Seen::Outdated,
            why.clone(),
            Next::Retry {
                after: client::OUTDATED_BACKOFF,
                why: format!("{why}."),
            },
        ),
    };
    Heard::Verdict(verdict)
}

/// A wait Cloud answered, which says why when the enroll attempt it follows failed.
fn waiting(status: &mut LocalCheckin, wait: Wait) -> Next {
    status.waited(wait);
    if status.enroll_failed() {
        status.view.message = status
            .memory
            .attempt
            .as_ref()
            .and_then(|attempt| attempt.failure.as_ref())
            .map(enroll::failure_line);
    }
    Next::Wait { after: wait.after }
}

/// The steps both modes share: read the host and its preparation, probe it, post the check-in and
/// sort Cloud's reply.
async fn exchange<H: Host>(
    host: &H,
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    now: DateTime<Utc>,
    start: Start,
    probes: &mut Probes,
) -> Result<Exchange, Unprobed> {
    let Start {
        mode,
        recalled: Recalled {
            mut memory,
            refusal,
            ..
        },
    } = start;
    let installing = mode == Mode::Install;
    let fingerprint = client.fingerprint();
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
        memory.prepare_retries,
        memory.prepare_started,
        now,
    );
    let mut preparing = liveness == Liveness::Live;
    // An older ployzd may not know how to prepare, and its blocker already says what to do.
    let run = if stale.is_some() || installing {
        None
    } else {
        preparation.run()
    };
    if let Some(run) = run {
        match host.start_prepare().await {
            Ok(()) => {
                // The run just started may already be installing Docker before the probe below.
                // This pass printed its notes above.
                preparing = observe_liveness(host, paths).await.0 == Liveness::Live;
                memory.prepare_started = Some(now);
                if run == PrepareRun::Retry {
                    memory.prepare_retries += 1;
                }
            }
            Err(why) => crate::ui::note(format!("Could not start {}: {why}", unit::PREPARE)),
        }
    }
    let gave_up = preparation == Preparation::GaveUp && !installing;
    let prepare_started = memory.prepare_started;
    let state = state.unwrap_or_default();
    let prep = state.prep(gave_up);
    let mut status = LocalCheckin::new(Seen::ProbeFailed, now, memory);
    status.view.prep = Some(prep);
    status.view.blockers.extend(stale.clone());
    let member = if status.memory.attempt.is_none() {
        host.participating().await.map(|name| Issue {
            code: IN_ANOTHER_CLUSTER.to_owned(),
            message: format!(
                "Already in another cluster. Remove it there first: ployz server rm {name}"
            ),
        })
    } else {
        None
    };
    status.view.blockers.extend(member.clone());
    // Someone else on WireGuard's port leaves ployzd crash-looping once it starts.
    let port_taken = host.wireguard_port_holder().map(|holder| Issue {
        code: WIREGUARD_PORT_IN_USE.to_owned(),
        message: format!(
            "UDP port {} is in use by {holder}. Ployz needs it for WireGuard. Free it, for \
             example by restarting that program.",
            ployz_core::WIREGUARD_PORT
        ),
    });
    status.view.blockers.extend(port_taken.clone());
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
            status.view.message = Some(why.clone());
            let next = Next::Retry {
                after: probes.retry_after(client::BACKOFF),
                why: format!("Could not probe this server: {why}."),
            };
            return Err(Unprobed { next, status });
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
    status.view.blockers.clone_from(&report.blockers);
    let request = CheckinRequest {
        protocol_version: CHECKIN_PROTOCOL_VERSION,
        checkin_key: key.public_hex(),
        cli_version: env!("CARGO_PKG_VERSION"),
        enroll_options: ENROLL_OPTIONS,
        report: &report,
        prep: Some(prep),
        attempt: status.memory.attempt.as_ref(),
        probe: probes.report().await,
    };
    let sent = tokio::time::Instant::now();
    let reply = client.post(key, &request).await;
    if request.probe.is_some() && matches!(&reply, client::Reply::Answer(_)) {
        probes.reported();
    }
    let heard = hear(reply, refusal, &fingerprint, probes);
    Ok(Exchange {
        status,
        report,
        prep,
        heard,
        sent,
    })
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
    let status = LocalCheckin::recall(&paths.status());
    let memory = status.as_ref().map(|status| &status.memory);
    let preparation = preparation(
        state.as_ref(),
        written.flatten(),
        liveness,
        memory.map_or(0, |memory| memory.prepare_retries),
        memory.and_then(|memory| memory.prepare_started),
        now,
    );
    let settled = preparation.settled();
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

impl Preparation {
    /// The run this loop starts for it, if any.
    fn run(self) -> Option<PrepareRun> {
        match self {
            Self::NotStarted => Some(PrepareRun::Start),
            Self::RetryDue => Some(PrepareRun::Retry),
            Self::Running
            | Self::Unknown
            | Self::DiedMidStep
            | Self::Failed
            | Self::Finished
            | Self::GaveUp => None,
        }
    }

    /// Whether an install leaves it alone: a run is live, may be, or finished.
    fn settled(self) -> bool {
        match self {
            Self::Running | Self::Unknown | Self::Finished => true,
            Self::NotStarted | Self::DiedMidStep | Self::Failed | Self::RetryDue | Self::GaveUp => {
                false
            }
        }
    }
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
