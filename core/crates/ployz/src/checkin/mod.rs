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
use wire::{Answer, CheckinRequest, PrepState, Probe};
pub(crate) use wire::{DockerPrep, Issue, Prep, StopReason, ZfsPrep};

pub(crate) const ENROLL_LINE: &str = "Enroll needs a newer ployz. Waiting.";
const ENROLL_WAIT: Duration = Duration::from_secs(10);
const PREPARE_RETRY: Duration = Duration::from_secs(10 * 60);
const PREPARE_TRIES: u32 = 3;
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const TOKEN_SETTLE: Duration = Duration::from_secs(2);

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
}

pub(crate) trait Host {
    async fn probe(&self) -> Result<Probe, String>;
    fn hostname(&self) -> String;
    /// Start preparation unless it is already running; true when it started.
    async fn start_prepare(&self) -> Result<bool, String>;
    async fn disable_loop(&self) -> Result<(), String>;
}

pub(crate) struct System {
    pub(crate) ployzd: PathBuf,
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
        let _group = child.id().map(ProcessGroup);
        let output = child.wait_with_output().await.map_err(run)?;
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

    async fn start_prepare(&self) -> Result<bool, String> {
        let output = tokio::process::Command::new("systemctl")
            .args(["show", "--property=ActiveState", "--value", unit::PREPARE])
            .output()
            .await
            .map_err(|error| format!("run systemctl: {error}"))?;
        let state = String::from_utf8_lossy(&output.stdout);
        if matches!(
            state.trim(),
            "active" | "activating" | "deactivating" | "reloading"
        ) {
            return Ok(false);
        }
        systemctl(&["start", "--no-block", unit::PREPARE]).await?;
        Ok(true)
    }

    async fn disable_loop(&self) -> Result<(), String> {
        let disabled = systemctl(&["disable", unit::CHECKIN]).await;
        let stopped = systemctl(&["stop", "--no-block", unit::PREPARE]).await;
        disabled.and(stopped)
    }
}

/// Kills the probe's whole process group, so a hung `docker` or `lsblk` under
/// `ployzd` goes with it.
struct ProcessGroup(u32);

impl Drop for ProcessGroup {
    fn drop(&mut self) {
        if let Some(group) = i32::try_from(self.0)
            .ok()
            .and_then(rustix::process::Pid::from_raw)
        {
            let _ = rustix::process::kill_process_group(group, rustix::process::Signal::KILL);
        }
    }
}

pub(crate) async fn systemctl(args: &[&str]) -> Result<(), String> {
    let output = tokio::process::Command::new("systemctl")
        .args(args)
        .output()
        .await
        .map_err(|error| format!("run systemctl: {error}"))?;
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
    Refused(String),
}

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Seen {
    Wait,
    Enroll,
    Stop,
    Unreachable,
    Refused,
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

    pub(crate) fn headline(&self) -> Headline {
        match self.answer {
            Seen::Stop => Headline::ended(self.reason.map_or("Stopped", StopReason::title)),
            Seen::Unreachable => Headline::open("Can't reach Cloud"),
            Seen::Refused => Headline::open("Cloud refused this server"),
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
                stop(host, paths).await;
                return CheckinEnd::Stopped(reason);
            }
            Next::Refused(why) => {
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

pub(crate) async fn stop<H: Host>(host: &H, paths: &Paths) {
    match fs::remove_file(&paths.token_file) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => crate::ui::note(format!(
            "Could not remove the Fleet Token at {}: {error}",
            paths.token_file.display()
        )),
    }
    if let Err(why) = host.disable_loop().await {
        crate::ui::note(format!("Could not disable {}: {why}", unit::CHECKIN));
    }
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
    if previous
        .as_ref()
        .is_some_and(|status| status.answer == Seen::Refused)
        && let Ok(token) = FleetToken::read(&paths.token_file)
    {
        client.use_token(token);
    }
    let mut prepare_retries = previous.map_or(0, |status| status.prepare_retries);
    let state = read_prep_state(&paths.prepare_state);
    let restart = installing
        && state
            .as_ref()
            .is_some_and(|state| state.failed_at().is_some());
    let due = if restart {
        prepare_retries = 0;
        Some(PrepareRun::Start)
    } else {
        prepare_due(state.as_ref(), prepare_retries, now)
    };
    if let Some(run) = due {
        match host.start_prepare().await {
            Ok(started) => {
                if started && run == PrepareRun::Retry {
                    prepare_retries += 1;
                }
            }
            Err(why) => crate::ui::note(format!("Could not start {}: {why}", unit::PREPARE)),
        }
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
    };
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
    let report = probe.report(host.hostname(), state.as_ref());
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
        client::Reply::Unreachable(after, why) => {
            status.answer = Seen::Unreachable;
            status.message = Some(why.clone());
            Next::Retry {
                after,
                why: format!("Could not reach Ployz Cloud: {why}."),
            }
        }
        client::Reply::Refused(why) => {
            status.answer = Seen::Refused;
            status.message = Some(why.clone());
            Next::Refused(why)
        }
    };
    record(&status, paths);
    next
}

fn record(status: &LocalCheckin, paths: &Paths) {
    if let Err(error) = status.write(&paths.status()) {
        crate::ui::note(format!(
            "Could not write {}: {error}",
            paths.status().display()
        ));
    }
}

fn read_prep_state(path: &Path) -> Option<PrepState> {
    let bytes = match fs::read(path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return None,
        Err(error) => {
            crate::ui::note(format!("Could not read {}: {error}", path.display()));
            return None;
        }
    };
    serde_json::from_slice(&bytes)
        .inspect_err(|error| crate::ui::note(format!("Could not read {}: {error}", path.display())))
        .ok()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum PrepareRun {
    Start,
    Retry,
}

fn prepare_due(state: Option<&PrepState>, retries: u32, now: DateTime<Utc>) -> Option<PrepareRun> {
    let Some(state) = state else {
        return Some(PrepareRun::Start);
    };
    if state.installing() {
        // Running, or cut short by a reboot; `start_prepare` leaves a running one alone.
        return Some(PrepareRun::Start);
    }
    let failed_at = DateTime::parse_from_rfc3339(state.failed_at()?).ok()?;
    let waited = now.signed_duration_since(failed_at).to_std().ok()?;
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
