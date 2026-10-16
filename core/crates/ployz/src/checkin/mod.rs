//! `ployz server checkin`: the loop a waiting host runs until Cloud enrolls,
//! rejects or forgets it.
//!
//! Each pass probes the host, reads how far preparation has got, posts one signed
//! check-in and acts on the answer. The host keeps no Cluster state: Cloud steers,
//! and `status.json` only remembers the last answer for `ployz status`.

mod client;
pub(crate) mod unit;
mod wire;

use std::{
    fs, io,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub(crate) use client::{CheckinClient, CheckinKey, FleetToken, KeyError};
use wire::{Answer, CheckinRequest, PrepState};
pub(crate) use wire::{DockerPrep, Issue, Prep, Probe, StopReason, ZfsPrep};

pub(crate) const ENROLL_LINE: &str = "Enroll needs a newer ployz. Waiting.";
const ENROLL_WAIT: Duration = Duration::from_secs(10);
const PREPARE_RETRY: Duration = Duration::from_secs(10 * 60);
const PREPARE_TRIES: u32 = 3;
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(30);
const TOKEN_SETTLE: Duration = Duration::from_secs(2);
const REVOKED: &str = "Cloud no longer accepts this Fleet Token.";
const REFUSED_ONCE: &str =
    "Cloud refused this Fleet Token. It is checked once more before the loop removes it";
const GAVE_UP: &str = "Preparation stopped mid-step on every try. Run sudo ployz server checkin --install to try again.";
const STALE_PLOYZD: &str = "ployzd_outdated";

pub(crate) struct Paths {
    pub(crate) checkin_dir: PathBuf,
    pub(crate) prepare_state: PathBuf,
    pub(crate) token_file: PathBuf,
}

impl Paths {
    pub(crate) const CHECKIN_DIR: &str = "/var/lib/ployz/checkin";
    pub(crate) const TOKEN_FILE: &str = "/etc/ployz/fleet-token";

    pub(crate) fn system(token_file: PathBuf) -> Self {
        Self {
            checkin_dir: PathBuf::from(Self::CHECKIN_DIR),
            prepare_state: PathBuf::from("/var/lib/ployz/prepare/state.json"),
            token_file,
        }
    }

    pub(crate) fn key(&self) -> PathBuf {
        self.checkin_dir.join("key")
    }

    pub(crate) fn status(&self) -> PathBuf {
        self.checkin_dir.join("status.json")
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckinEnd {
    Stopped(StopReason),
    /// The token file changed as the loop stopped. The loop stays enabled to restart with it.
    TokenChanged,
}

pub(crate) trait Host {
    async fn probe(&self) -> Result<Probe, String>;
    fn hostname(&self) -> String;
    async fn prepare_running(&self) -> Result<bool, String>;
    async fn start_prepare(&self) -> Result<(), String>;
    async fn disable_loop(&self) -> Result<(), String>;
    /// Why the ployzd this loop runs with should be replaced, when it is older than this CLI.
    fn stale_ployzd(&self) -> Option<String>;
}

pub(crate) struct System {
    pub(crate) ployzd: PathBuf,
    pub(crate) stale: Option<String>,
}

impl System {
    pub(crate) fn with(ployzd: PathBuf) -> Self {
        Self {
            ployzd,
            stale: None,
        }
    }
}

impl Host for System {
    async fn probe(&self) -> Result<Probe, String> {
        let run = |error: io::Error| format!("run {}: {error}", self.ployzd.display());
        let child = tokio::process::Command::new(&self.ployzd)
            .args(["install", "--probe", "--json"])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .process_group(0)
            .kill_on_drop(true)
            .spawn()
            .map_err(run)?;
        let group = ProcessGroup(child.id());
        let output = child.wait_with_output().await.map_err(run)?;
        // Reaped, so the id may already belong to someone else's group.
        group.reaped();
        if !output.status.success() {
            return Err(format!(
                "ployzd install --probe exited with {}: {}",
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            ));
        }
        serde_json::from_slice(&output.stdout)
            .map_err(|error| format!("read the host probe: {error}"))
    }

    fn hostname(&self) -> String {
        fs::read_to_string("/proc/sys/kernel/hostname")
            .map(|name| name.trim().to_owned())
            .unwrap_or_default()
    }

    async fn prepare_running(&self) -> Result<bool, String> {
        let output =
            systemctl_output(&["show", "--property=ActiveState", "--value", unit::PREPARE]).await?;
        Ok(matches!(
            String::from_utf8_lossy(&output.stdout).trim(),
            "active" | "activating" | "deactivating" | "reloading"
        ))
    }

    async fn start_prepare(&self) -> Result<(), String> {
        systemctl(&["start", "--no-block", unit::PREPARE]).await
    }

    async fn disable_loop(&self) -> Result<(), String> {
        let disabled = systemctl(&["disable", unit::CHECKIN]).await;
        let stopped = systemctl(&["stop", "--no-block", unit::PREPARE]).await;
        disabled.and(stopped)
    }

    fn stale_ployzd(&self) -> Option<String> {
        self.stale.clone()
    }
}

/// Kills the probe's whole process group, so a hung `docker` or `lsblk` under
/// `ployzd` goes with it.
struct ProcessGroup(Option<u32>);

impl ProcessGroup {
    fn reaped(mut self) {
        self.0 = None;
    }
}

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Some(group) = self
            .0
            .and_then(|id| i32::try_from(id).ok())
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
    }
}

async fn systemctl_output(args: &[&str]) -> Result<std::process::Output, String> {
    let output = tokio::process::Command::new("systemctl")
        .args(args)
        .kill_on_drop(true)
        .output();
    tokio::time::timeout(SYSTEMCTL_TIMEOUT, output)
        .await
        .map_err(|_| {
            format!(
                "systemctl {} did not finish in {}s. The change may still apply",
                args.join(" "),
                SYSTEMCTL_TIMEOUT.as_secs()
            )
        })?
        .map_err(|error| format!("run systemctl: {error}"))
}

pub(crate) async fn systemctl(args: &[&str]) -> Result<(), String> {
    let output = systemctl_output(args).await?;
    if output.status.success() {
        Ok(())
    } else {
        Err(format!(
            "systemctl {} exited with {}: {}",
            args.join(" "),
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Next {
    Wait {
        after: Duration,
        log: Option<&'static str>,
    },
    Retry {
        after: Duration,
        why: String,
    },
    Stop(StopReason, String),
    ClockOff(String),
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Seen {
    Wait,
    Enroll,
    Stop,
    Refused,
    Unreachable,
    ClockOff,
    Outdated,
    ProbeFailed,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct LocalCheckin {
    pub(crate) answer: Seen,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) reason: Option<StopReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) message: Option<String>,
    pub(crate) at: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prep: Option<Prep>,
    #[serde(default)]
    pub(crate) blockers: Vec<Issue>,
    #[serde(default)]
    pub(crate) prepare_retries: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) prepare_started: Option<DateTime<Utc>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) refused: Option<Refusal>,
}

/// Cloud's first refusal of a token, which a second refusal of the same token confirms once
/// BACKOFF has passed, across restarts.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Refusal {
    pub(crate) token: String,
    pub(crate) at: DateTime<Utc>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Headline {
    pub(crate) title: &'static str,
    pub(crate) ended: bool,
}

impl Headline {
    const fn open(title: &'static str) -> Self {
        Self {
            title,
            ended: false,
        }
    }

    const fn ended(title: &'static str) -> Self {
        Self { title, ended: true }
    }
}

const STATUS_READABLE_BY_ALL: u32 = 0o644;

impl LocalCheckin {
    pub(crate) fn read(path: &Path) -> io::Result<Option<Self>> {
        match fs::read(path) {
            Ok(bytes) => serde_json::from_slice(&bytes)
                .map(Some)
                .map_err(io::Error::other),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(error) => Err(error),
        }
    }

    /// Whether `ployz status` should show the message: it says what ended the loop or what to fix.
    pub(crate) fn says_why(&self) -> bool {
        matches!(
            self.answer,
            Seen::Stop | Seen::Refused | Seen::ClockOff | Seen::Outdated | Seen::ProbeFailed
        )
    }

    pub(crate) fn headline(&self) -> Headline {
        match self.answer {
            Seen::Stop => Headline::ended(self.reason.map_or("Stopped", StopReason::title)),
            Seen::Refused => Headline::open("Token refused"),
            Seen::Unreachable => Headline::open("Can't reach Cloud"),
            Seen::ClockOff => Headline::open("Clock is off"),
            Seen::Outdated => Headline::open("Needs a newer ployz"),
            Seen::ProbeFailed => Headline::open("Can't inspect this host"),
            Seen::Enroll => Headline::open("Enroll needs a newer ployz"),
            Seen::Wait if !self.blockers.is_empty() => Headline::open("Blocked"),
            Seen::Wait => match self.prep {
                Some(prep) if prep.docker == DockerPrep::Installing => {
                    Headline::open("Installing Docker")
                }
                Some(prep) if prep.zfs == ZfsPrep::Installing => Headline::open("Building ZFS"),
                Some(_) | None => Headline::open("Waiting for approval"),
            },
        }
    }

    fn write(&self, path: &Path) -> io::Result<()> {
        let directory = path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        serde_json::to_writer_pretty(&mut file, self)?;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(STATUS_READABLE_BY_ALL))?;
        file.as_file().sync_all()?;
        file.persist(path).map_err(|error| error.error)?;
        Ok(())
    }
}

pub(crate) async fn run<H: Host>(
    host: &H,
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
) -> CheckinEnd {
    let mut last_log = None;
    loop {
        match pass(host, client, key, paths, Utc::now()).await {
            Next::Wait { after, log } => {
                if let Some(line) = log.filter(|line| last_log != Some(*line)) {
                    crate::ui::note(line);
                }
                last_log = log;
                tokio::time::sleep(after).await;
            }
            Next::Retry { after, why } => {
                last_log = None;
                crate::ui::note(format!("{why} Trying again in {}s.", after.as_secs()));
                tokio::time::sleep(after).await;
            }
            Next::Stop(reason, message) => {
                // A token answer is about the token that was sent, not one written since. A
                // rejection is about the host, whatever token it holds.
                if reason != StopReason::Rejected
                    && let Some(token) = replaced_token(client, &paths.token_file).await
                {
                    crate::ui::note(format!(
                        "{}. Checking in with the new Fleet Token.",
                        reason.title()
                    ));
                    client.use_token(token);
                    continue;
                }
                crate::ui::note(format!("{}. {message}", reason.title()));
                return stop(host, client, paths, reason).await;
            }
            Next::ClockOff(why) => {
                last_log = None;
                crate::ui::note(format!(
                    "{why}. Trying again in {}s.",
                    client::BACKOFF.as_secs()
                ));
                tokio::time::sleep(client::BACKOFF).await;
            }
        }
    }
}

/// The token in the file when it differs from the one sent. A file caught mid-write is read
/// again once it settles, so a replacement is not deleted as the old token.
async fn replaced_token(client: &CheckinClient, path: &Path) -> Option<FleetToken> {
    let token = match FleetToken::read(path) {
        Err(KeyError::Token { source, .. }) if source.kind() == io::ErrorKind::NotFound => None,
        Err(_) => {
            tokio::time::sleep(TOKEN_SETTLE).await;
            FleetToken::read(path).ok()
        }
        Ok(token) => Some(token),
    };
    token.filter(|token| !client.sends(token))
}

/// The Fleet Token the loop checks in with. With the file gone there is nothing to check in
/// with, so the loop disables itself and ends cleanly instead of failing at every restart.
pub(crate) async fn loop_token<H: Host>(
    host: &H,
    paths: &Paths,
) -> Result<Option<FleetToken>, KeyError> {
    match FleetToken::read(&paths.token_file) {
        Err(KeyError::Token { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            crate::ui::note(format!(
                "No Fleet Token at {}. The check-in loop stops.",
                paths.token_file.display()
            ));
            if let Err(why) = host.disable_loop().await {
                crate::ui::note(format!("Could not disable {}: {why}", unit::CHECKIN));
            }
            Ok(None)
        }
        read => read.map(Some),
    }
}

/// Removes the token file only while it still holds the token Cloud answered, so a token written
/// since survives, and the loop stays enabled to restart with it. A rejection is about the host,
/// so it ends the loop whatever the file holds.
pub(crate) async fn stop<H: Host>(
    host: &H,
    client: &CheckinClient,
    paths: &Paths,
    reason: StopReason,
) -> CheckinEnd {
    let held = FleetToken::read(&paths.token_file);
    let still_sent = held.as_ref().is_ok_and(|token| client.sends(token));
    let gone = matches!(&held, Err(KeyError::Token { source, .. }) if source.kind() == io::ErrorKind::NotFound);
    if still_sent && let Err(error) = fs::remove_file(&paths.token_file) {
        crate::ui::note(format!(
            "Could not remove the Fleet Token at {}: {error}",
            paths.token_file.display()
        ));
    }
    if !still_sent && !gone && reason != StopReason::Rejected {
        crate::ui::note(format!(
            "{} changed. The check-in loop restarts with it.",
            paths.token_file.display()
        ));
        return CheckinEnd::TokenChanged;
    }
    if let Err(why) = host.disable_loop().await {
        crate::ui::note(format!("Could not disable {}: {why}", unit::CHECKIN));
    }
    CheckinEnd::Stopped(reason)
}

pub(crate) async fn pass<H: Host>(
    host: &H,
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    now: DateTime<Utc>,
) -> Next {
    pass_from(host, client, key, paths, now, false).await
}

/// The pass an explicit `--install` makes: a failed preparation starts over at once, with its
/// automatic retries restored. Its state file stays, since ployzd records there what it added.
pub(crate) async fn install_pass<H: Host>(
    host: &H,
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    now: DateTime<Utc>,
) -> Next {
    pass_from(host, client, key, paths, now, true).await
}

async fn pass_from<H: Host>(
    host: &H,
    client: &mut CheckinClient,
    key: &CheckinKey,
    paths: &Paths,
    now: DateTime<Utc>,
    installing: bool,
) -> Next {
    let previous = LocalCheckin::read(&paths.status()).ok().flatten();
    let refusal = previous
        .as_ref()
        .filter(|status| status.answer == Seen::Refused)
        .and_then(|status| status.refused.clone())
        .filter(|refusal| refusal.token == client.fingerprint());
    let mut prepare_retries = previous.as_ref().map_or(0, |status| status.prepare_retries);
    let mut prepare_started = previous.and_then(|status| status.prepare_started);
    let running = host.prepare_running().await.unwrap_or_else(|why| {
        crate::ui::note(format!("Could not read {}: {why}", unit::PREPARE));
        true
    });
    // Read after the unit is seen stopped, so a run that finished in between reads as finished.
    let (mut state, written) = read_prep_state(&paths.prepare_state).unzip();
    let due = if running {
        None
    } else if installing
        && state
            .as_ref()
            .is_none_or(|state| state.installing() || state.failed_at().is_some())
    {
        prepare_retries = 0;
        Some(PrepareRun::Start)
    } else {
        // Only a step left installing is dated by its state file. A failure carries its own time.
        let written = written
            .flatten()
            .filter(|_| state.as_ref().is_some_and(PrepState::installing));
        let last_try = written.max(prepare_started);
        prepare_due(state.as_ref(), last_try, prepare_retries, now)
    };
    if let Some(run) = due {
        match host.start_prepare().await {
            Ok(()) => {
                prepare_started = Some(now);
                if run == PrepareRun::Retry {
                    prepare_retries += 1;
                }
            }
            Err(why) => crate::ui::note(format!("Could not start {}: {why}", unit::PREPARE)),
        }
    } else if !running && prepare_retries >= PREPARE_TRIES {
        state.get_or_insert_with(PrepState::fresh).give_up(GAVE_UP);
    }
    let prep = state.as_ref().map_or(
        Prep {
            docker: DockerPrep::Installing,
            zfs: ZfsPrep::Installing,
        },
        PrepState::prep,
    );
    let mut status = LocalCheckin {
        answer: Seen::ProbeFailed,
        reason: None,
        message: None,
        at: now,
        prep: Some(prep),
        blockers: Vec::new(),
        prepare_retries,
        prepare_started,
        refused: None,
    };
    let stale = host.stale_ployzd().map(|message| Issue {
        code: STALE_PLOYZD.to_owned(),
        message,
    });
    status.blockers.extend(stale.clone());
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
            record(&status, paths);
            return Next::Retry {
                after: client::BACKOFF,
                why: format!("Could not probe this server: {why}."),
            };
        }
    };
    let mut report = probe.report(host.hostname(), state.as_ref());
    report.blockers.extend(stale);
    status.blockers.clone_from(&report.blockers);
    let request = CheckinRequest {
        protocol_version: wire::PROTOCOL_VERSION,
        checkin_key: key.public_hex(),
        cli_version: env!("CARGO_PKG_VERSION"),
        report: &report,
        prep: Some(prep),
    };
    let next = match client.post(key, &request).await {
        client::Reply::Answer(Answer::Wait(after)) => {
            status.answer = Seen::Wait;
            Next::Wait { after, log: None }
        }
        client::Reply::Answer(Answer::Enroll) => {
            status.answer = Seen::Enroll;
            Next::Wait {
                after: ENROLL_WAIT,
                log: Some(ENROLL_LINE),
            }
        }
        client::Reply::Answer(Answer::Stop(reason, message)) => {
            status.answer = Seen::Stop;
            status.reason = Some(reason);
            status.message = Some(message.clone());
            Next::Stop(reason, message)
        }
        client::Reply::Refused => {
            let waited = refusal
                .as_ref()
                .and_then(|refusal| now.signed_duration_since(refusal.at).to_std().ok());
            if waited.is_some_and(|waited| waited >= client::BACKOFF) {
                status.answer = Seen::Stop;
                status.reason = Some(StopReason::TokenRevoked);
                status.message = Some(REVOKED.to_owned());
                Next::Stop(StopReason::TokenRevoked, REVOKED.to_owned())
            } else {
                status.answer = Seen::Refused;
                status.message = Some(REFUSED_ONCE.to_owned());
                status.refused = Some(refusal.unwrap_or_else(|| Refusal {
                    token: client.fingerprint(),
                    at: now,
                }));
                Next::Retry {
                    after: client::BACKOFF
                        .saturating_sub(waited.unwrap_or_default())
                        .max(Duration::from_secs(1)),
                    why: format!("{REFUSED_ONCE}."),
                }
            }
        }
        client::Reply::Unreachable(after, why) => {
            status.answer = Seen::Unreachable;
            status.message = Some(why.clone());
            Next::Retry {
                after,
                why: format!("Could not reach Ployz Cloud: {why}."),
            }
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
    record(&status, paths);
    next
}

/// Says in status.json why the loop can't start, keeping what the last pass saw of the host.
pub(crate) fn no_ployzd(paths: &Paths, why: &str, now: DateTime<Utc>) {
    let previous = LocalCheckin::read(&paths.status()).ok().flatten();
    let status = LocalCheckin {
        answer: Seen::ProbeFailed,
        reason: None,
        message: Some(why.to_owned()),
        at: now,
        prep: previous.as_ref().and_then(|status| status.prep),
        blockers: Vec::new(),
        prepare_retries: previous.as_ref().map_or(0, |status| status.prepare_retries),
        prepare_started: previous.and_then(|status| status.prepare_started),
        refused: None,
    };
    record(&status, paths);
}

fn record(status: &LocalCheckin, paths: &Paths) {
    if let Err(error) = status.write(&paths.status()) {
        crate::ui::note(format!(
            "Could not write {}: {error}",
            paths.status().display()
        ));
    }
}

/// The preparation state and when ployzd last wrote it.
fn read_prep_state(path: &Path) -> Option<(PrepState, Option<DateTime<Utc>>)> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            crate::ui::note(format!("Could not read {}: {error}", path.display()));
            return None;
        }
    };
    let state = serde_json::from_slice(&bytes)
        .inspect_err(|error| crate::ui::note(format!("Could not read {}: {error}", path.display())))
        .ok()?;
    let written = fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .map(DateTime::<Utc>::from);
    Some((state, written))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrepareRun {
    Start,
    Retry,
}

/// What to do about a preparation that is not running, `last_try` being its last start or, for a
/// step left installing, its last state write if later.
fn prepare_due(
    state: Option<&PrepState>,
    last_try: Option<DateTime<Utc>>,
    retries: u32,
    now: DateTime<Utc>,
) -> Option<PrepareRun> {
    // A start that left no state, or a step still installing with no run behind it, died
    // mid-step, by a crash or a reboot.
    let ended_at = if state.is_none_or(PrepState::installing) {
        let Some(last_try) = last_try else {
            return state.is_none().then_some(PrepareRun::Start);
        };
        last_try
    } else {
        let failed = DateTime::parse_from_rfc3339(state?.failed_at()?)
            .ok()?
            .to_utc();
        last_try.map_or(failed, |last_try| last_try.max(failed))
    };
    let waited = now.signed_duration_since(ended_at).to_std().ok()?;
    (waited >= PREPARE_RETRY && retries < PREPARE_TRIES).then_some(PrepareRun::Retry)
}

pub(crate) fn ago(at: DateTime<Utc>, now: DateTime<Utc>) -> String {
    let seconds = now.signed_duration_since(at).num_seconds().max(0);
    match seconds {
        0..60 => format!("{seconds}s ago"),
        60..3600 => format!("{}m ago", seconds / 60),
        3600..86400 => format!("{}h ago", seconds / 3600),
        _ => format!("{}d ago", seconds / 86400),
    }
}

#[cfg(test)]
#[path = "checkin_tests.rs"]
mod tests;
