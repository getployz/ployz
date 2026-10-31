//! `ployz server checkin`: the loop a waiting host runs until Cloud enrolls,
//! rejects or forgets it.
//!
//! Each pass probes the host, reads how far preparation has got, posts one signed
//! check-in and acts on the answer. The host keeps no Cluster state: Cloud steers,
//! and `status.json` only remembers the last answer for `ployz status`.

mod client;
mod enroll;
mod probe;
pub(crate) mod unit;
mod wire;

use std::{
    fs, io,
    os::unix::fs::{OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    time::Duration,
};

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

pub(crate) use client::{CheckinClient, CheckinKey, FleetToken, Hold, KeyError};
pub(crate) use enroll::Committed;
use probe::Probes;
use wire::{Answer, CheckinRequest, PrepState, Wait};
pub(crate) use wire::{
    Attempt, DOCKER_INSTALL_FAILED, DockerPrep, EnrollInstruction, Issue, Prep, Probe, StopReason,
    ZfsPrep,
};

const PREPARE_RETRY: Duration = Duration::from_secs(10 * 60);
const PREPARE_TRIES: u32 = 3;
const PROBE_TIMEOUT: Duration = Duration::from_secs(60);
const SYSTEMCTL_TIMEOUT: Duration = Duration::from_secs(30);
const TOKEN_SETTLE: Duration = Duration::from_secs(2);
/// The widest gap, on Cloud's clock, one refused pass to the next can explain. A wider one, or a
/// second refusal dated before the first, starts the confirmation over.
const REFUSAL_SPAN: Duration = Duration::from_secs(60 * 60);
const REVOKED: &str = "Cloud no longer accepts this Fleet Token.";
const REFUSED_ONCE: &str =
    "Cloud refused this Fleet Token. It is checked once more before the loop removes it";
const GAVE_UP: &str = "Preparation stopped mid-step on every try. Run sudo ployz server checkin --install to try again.";
const STALE_PLOYZD: &str = "ployzd_outdated";
const IN_ANOTHER_CLUSTER: &str = "in_another_cluster";
const WIREGUARD_PORT_IN_USE: &str = "wireguard_port_in_use";
pub(crate) const NO_CLUSTER: &str = "No cluster yet";
pub(crate) const NO_CLUSTER_WHY: &str = "Enroll one server in Ployz Cloud to start it.";

pub(crate) struct Paths {
    pub(crate) checkin_dir: PathBuf,
    pub(crate) prepare_state: PathBuf,
    pub(crate) token_file: PathBuf,
}

impl Paths {
    pub(crate) const CHECKIN_DIR: &str = "/var/lib/ployz-checkin";
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

    fn probe(&self) -> PathBuf {
        self.checkin_dir.join("probe.json")
    }

    pub(crate) fn loop_lock(&self) -> PathBuf {
        self.checkin_dir.join(LOOP_LOCK)
    }
}

const LOOP_LOCK: &str = "loop.lock";
/// Only root may lock it: a lock any user could take would let them hold the loop back from starting.
const LOOP_LOCK_MODE: u32 = 0o600;

/// Held for as long as the check-in loop runs, so `status` can tell a record no loop updates any more.
pub(crate) struct LoopRunning {
    _lock: fs::File,
    checkin_dir: PathBuf,
}

/// The check-in directories whose lock this process holds, so the records it writes there say so.
static HELD: std::sync::Mutex<Vec<PathBuf>> = std::sync::Mutex::new(Vec::new());

fn holds(checkin_dir: &Path) -> bool {
    HELD.lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .iter()
        .any(|held| held == checkin_dir)
}

impl Drop for LoopRunning {
    fn drop(&mut self) {
        HELD.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .retain(|held| held != &self.checkin_dir);
    }
}

/// The loop's lock, or none when another loop already holds it.
pub(crate) fn hold_loop(paths: &Paths) -> io::Result<Option<LoopRunning>> {
    fs::create_dir_all(&paths.checkin_dir)?;
    let lock = fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(LOOP_LOCK_MODE)
        .open(paths.loop_lock())?;
    // A lock an earlier release made readable by all is narrowed too.
    lock.set_permissions(fs::Permissions::from_mode(LOOP_LOCK_MODE))?;
    match lock.try_lock() {
        Ok(()) => {
            HELD.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(paths.checkin_dir.clone());
            Ok(Some(LoopRunning {
                _lock: lock,
                checkin_dir: paths.checkin_dir.clone(),
            }))
        }
        Err(fs::TryLockError::WouldBlock) => Ok(None),
        Err(fs::TryLockError::Error(error)) => Err(error),
    }
}

/// Whether the loop that wrote this host's record held the lock, and none holds it now. A record
/// from a loop older than the lock, or one that couldn't take it, claims nothing; so does a lock
/// a user can't open (anyone but root).
pub(crate) fn loop_stopped(checkin_dir: &Path) -> bool {
    #[derive(Deserialize)]
    struct Writer {
        #[serde(default)]
        locked: bool,
    }
    let locked = fs::read(checkin_dir.join("status.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<Writer>(&bytes).ok())
        .is_some_and(|writer| writer.locked);
    locked
        && fs::File::open(checkin_dir.join(LOOP_LOCK))
            .is_ok_and(|lock| lock.try_lock_shared().is_ok())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CheckinEnd {
    Stopped(StopReason),
    /// The token file changed as the loop stopped. The loop stays enabled to restart with it.
    TokenChanged,
    Enrolled,
}

pub(crate) trait Host {
    async fn probe(&self) -> Result<Probe, String>;
    fn hostname(&self) -> String;
    async fn prepare_running(&self) -> Result<bool, String>;
    async fn start_prepare(&self) -> Result<(), String>;
    async fn disable_loop(&self) -> Result<(), String>;
    /// Why the ployzd this loop runs with should be replaced, when it is older than this CLI.
    fn stale_ployzd(&self) -> Option<String>;
    /// Run `server add` here as `instruction` says, telling `progress` each stage it starts.
    async fn enroll(
        &self,
        instruction: &EnrollInstruction,
        progress: &dyn Fn(crate::cloud_enroll::EnrollStage),
    ) -> Result<Committed, String>;
    async fn discard_zfs_build(&self) -> Result<(), String>;
    /// The name this Server holds in the cluster it already participates in.
    async fn participating(&self) -> Option<String>;
    /// What ployzd says of this host's Machine, asked once per pass.
    async fn local_machine(&self) -> LocalMachine {
        LocalMachine {
            phase: None,
            participating: self.participating().await,
        }
    }
    /// The program holding UDP [`ployz_core::WIREGUARD_PORT`], which ployzd needs for WireGuard.
    fn wireguard_port_holder(&self) -> Option<String> {
        None
    }
}

pub(crate) struct System {
    pub(crate) ployzd: PathBuf,
    pub(crate) stale: Option<String>,
    pub(crate) cloud: String,
}

impl System {
    pub(crate) fn with(ployzd: PathBuf) -> Self {
        Self {
            ployzd,
            stale: None,
            cloud: String::new(),
        }
    }
}

impl System {
    /// Whether this ployzd answers the host probe in time, so the loop can run with it.
    pub(crate) async fn probes(ployzd: &Path) -> bool {
        tokio::time::timeout(PROBE_TIMEOUT, Self::with(ployzd.to_owned()).probe())
            .await
            .is_ok_and(|probed| probed.is_ok())
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
        systemctl(&["disable", unit::CHECKIN]).await
    }

    fn stale_ployzd(&self) -> Option<String> {
        self.stale.clone()
    }

    async fn enroll(
        &self,
        instruction: &EnrollInstruction,
        progress: &dyn Fn(crate::cloud_enroll::EnrollStage),
    ) -> Result<Committed, String> {
        enroll::server_add(self, instruction, progress).await
    }

    async fn discard_zfs_build(&self) -> Result<(), String> {
        enroll::discard_zfs_build(&self.ployzd).await
    }

    fn wireguard_port_holder(&self) -> Option<String> {
        ployz_core::port_holder::udp_holder(Path::new("/proc"), ployz_core::WIREGUARD_PORT)
    }

    async fn participating(&self) -> Option<String> {
        self.participating_as(local_machine(PROBE_TIMEOUT).await?)
    }

    async fn local_machine(&self) -> LocalMachine {
        let details = local_machine(PROBE_TIMEOUT).await;
        LocalMachine {
            phase: details.as_ref().map(|details| details.phase.clone()),
            participating: details.and_then(|details| self.participating_as(details)),
        }
    }
}

impl System {
    fn participating_as(&self, details: ployz_core::MachineDetails) -> Option<String> {
        (details.phase == ployz_core::LocalMachinePhase::Participating).then(|| {
            details
                .machine
                .map_or_else(|| self.hostname(), |machine| machine.name.to_string())
        })
    }
}

/// What ployzd said of this host's Machine.
pub(crate) struct LocalMachine {
    /// Its Local Machine Phase, or none when ployzd didn't answer.
    pub(crate) phase: Option<ployz_core::LocalMachinePhase>,
    /// The name this Server holds in the cluster it participates in.
    pub(crate) participating: Option<String>,
}

/// What this host's daemon says of its Machine over the local socket, or none when it doesn't
/// answer within `timeout`.
pub(crate) async fn local_machine(timeout: Duration) -> Option<ployz_core::MachineDetails> {
    use crate::context::{Connection, ConnectionSource, SelectedConnections};
    use ployz_core::{InspectRequest, op};

    let inspect = async {
        let connection = Connection::unix(Path::new(crate::connect::DEFAULT_LOCAL_SOCKET)).ok()?;
        let mut client = crate::connect::connect_selected_with(
            SelectedConnections {
                source: ConnectionSource::LocalSocket,
                connections: vec![connection],
            },
            std::sync::Arc::new(crate::connect::SystemConnector::default()),
        )
        .await
        .ok()?;
        client
            .call_repeatable::<op::Inspect>(InspectRequest::default(), None)
            .await
            .ok()
    };
    tokio::time::timeout(timeout, inspect).await.ok().flatten()
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

#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum Seen {
    Wait,
    Enroll,
    Joined,
    Stop,
    Refused,
    Held,
    Unreachable,
    NotCloud,
    ClockOff,
    Outdated,
    ProbeFailed,
    NoPloyzd,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct LocalCheckin {
    pub(crate) answer: Seen,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) token_fingerprint: Option<String>,
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
    /// The enroll attempt Cloud last instructed, sent on every check-in until Cloud replaces it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub(crate) attempt: Option<Attempt>,
    /// Cloud's last wait said the Organization has no cluster for this host to join yet.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub(crate) no_cluster: bool,
}

/// Cloud's first refusal of a token, dated by Cloud's clock. A second refusal of the same token
/// confirms it once Cloud dates that one BACKOFF later, across restarts.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub(crate) struct Refusal {
    pub(crate) token: String,
    pub(crate) date: DateTime<Utc>,
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
            Seen::Stop
                | Seen::Refused
                | Seen::Held
                | Seen::NotCloud
                | Seen::ClockOff
                | Seen::Outdated
                | Seen::ProbeFailed
                | Seen::NoPloyzd
        ) || self.enroll_failed()
            || (self.answer == Seen::Wait && self.no_cluster)
    }

    fn waited(&mut self, wait: Wait) {
        self.answer = Seen::Wait;
        self.no_cluster = wait.no_cluster;
        if wait.no_cluster {
            self.message = Some(NO_CLUSTER_WHY.to_owned());
        }
    }

    /// A preparation started at `now` tries Docker's install again, so its last failure no
    /// longer stands, and its automatic retries are restored.
    pub(crate) fn restarted_preparation(&mut self, now: DateTime<Utc>) {
        self.prepare_started = Some(now);
        self.prepare_retries = 0;
        self.blockers
            .retain(|blocker| blocker.code != DOCKER_INSTALL_FAILED);
    }

    fn enroll_failed(&self) -> bool {
        self.answer == Seen::Wait
            && self
                .attempt
                .as_ref()
                .is_some_and(|attempt| attempt.failure.is_some())
    }

    pub(crate) fn headline(&self) -> Headline {
        match self.answer {
            Seen::Stop => Headline::ended(self.reason.map_or("Stopped", StopReason::title)),
            Seen::Refused => Headline::open("Token refused"),
            Seen::Held => Headline::open("Held by Cloud"),
            Seen::Unreachable => Headline::open("Can't reach Cloud"),
            Seen::NotCloud => Headline::open("Not Ployz Cloud"),
            Seen::ClockOff => Headline::open("Clock is off"),
            Seen::Outdated => Headline::open("Needs a newer ployz"),
            Seen::ProbeFailed => Headline::open("Can't inspect this host"),
            Seen::NoPloyzd => Headline::open("No ployzd"),
            Seen::Enroll => Headline::open(
                self.attempt
                    .as_ref()
                    .map_or("Enrolling", |attempt| enroll::title(attempt.stage)),
            ),
            Seen::Joined => Headline::open("Joined"),
            Seen::Wait if !self.blockers.is_empty() => Headline::open("Blocked"),
            Seen::Wait if self.enroll_failed() => Headline::open("Enroll failed"),
            Seen::Wait => match self.prep {
                Some(prep) if prep.docker == DockerPrep::Installing => {
                    Headline::open("Installing Docker")
                }
                Some(prep) if prep.zfs == ZfsPrep::Installing => Headline::open("Building ZFS"),
                Some(_) | None if self.no_cluster => Headline::open(NO_CLUSTER),
                Some(_) | None => Headline::open("Waiting for approval"),
            },
        }
    }

    fn write(&self, path: &Path) -> io::Result<()> {
        let directory = path.parent().unwrap_or(Path::new("."));
        fs::create_dir_all(directory)?;
        let mut file = tempfile::NamedTempFile::new_in(directory)?;
        // Whether the loop writing this held its lock, for `loop_stopped`.
        #[derive(Serialize)]
        struct Written<'a> {
            #[serde(flatten)]
            record: &'a LocalCheckin,
            #[serde(skip_serializing_if = "std::ops::Not::not")]
            locked: bool,
        }
        let written = Written {
            record: self,
            locked: holds(directory),
        };
        serde_json::to_writer_pretty(&mut file, &written)?;
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
    let mut said = Said::default();
    let mut probes = Probes::load(paths.probe());
    loop {
        match pass_from(host, client, key, paths, Utc::now(), false, &mut probes).await {
            Next::Wait { after } => {
                said.fresh(None);
                tokio::time::sleep(after).await;
            }
            // Cloud holds a full token back pass after pass, so the hold is logged once.
            Next::Held { after, why, .. } => {
                let line = format!("{why} Trying again in {}s.", after.as_secs());
                if let Some(line) = said.fresh(Some(line)) {
                    crate::ui::note(line);
                }
                tokio::time::sleep(after).await;
            }
            Next::Retry { after, why } => {
                said.fresh(None);
                crate::ui::note(format!("{why} Trying again in {}s.", after.as_secs()));
                tokio::time::sleep(after).await;
            }
            Next::RefusedOnce { after } => {
                said.fresh(None);
                crate::ui::note(format!(
                    "{REFUSED_ONCE}. Trying again in {}s.",
                    after.as_secs()
                ));
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
            Next::ClockOff(why) | Next::NotCloud(why) => {
                said.fresh(None);
                crate::ui::note(format!(
                    "{why}. Trying again in {}s.",
                    client::BACKOFF.as_secs()
                ));
                tokio::time::sleep(client::BACKOFF).await;
            }
            Next::Enrolled => return enroll::finish(host, paths).await,
            Next::Enrolling => unreachable!("only an install pass leaves enrolling to the loop"),
        }
    }
}

/// The last line the loop logged, so a line repeated pass after pass is logged once.
#[derive(Default)]
struct Said(Option<String>);

impl Said {
    /// The line, unless the loop just logged it. None says nothing, so any next line is fresh.
    fn fresh(&mut self, line: Option<String>) -> Option<String> {
        if line == self.0 {
            return None;
        }
        self.0.clone_from(&line);
        line
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
        &mut Probes::load(paths.probe()),
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
        &mut Probes::load(paths.probe()),
    )
    .await;
    InstallPass { next, seen }
}

async fn pass_from<H: Host>(
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
    let seen = host.prepare_running().await;
    // Only a unit seen running may vouch for a stopped Docker; an unreadable one still counts as
    // running so as not to start a second preparation.
    let mut confirmed = seen == Ok(true);
    let running = seen.unwrap_or_else(|why| {
        crate::ui::note(format!("Could not read {}: {why}", unit::PREPARE));
        true
    });
    // Read after the unit is seen stopped, so a run that finished in between reads as finished.
    let (mut state, written) = read_prep_state(&paths.prepare_state).unzip();
    // An older ployzd may not know how to prepare, and its blocker already says what to do.
    let due = if running || stale.is_some() || installing {
        None
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
                // The run just started may already be installing Docker before the probe below.
                confirmed = host.prepare_running().await == Ok(true);
                prepare_started = Some(now);
                if run == PrepareRun::Retry {
                    prepare_retries += 1;
                }
            }
            Err(why) => crate::ui::note(format!("Could not start {}: {why}", unit::PREPARE)),
        }
    } else if !running && !installing && prepare_retries >= PREPARE_TRIES {
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
    let local = host.local_machine().await;
    let member = if status.attempt.is_none() {
        local.participating.clone().map(|name| Issue {
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
    let mut report = probe.report(host.hostname(), state.as_ref(), confirmed, prepare_started);
    report.blockers.extend(stale);
    report.blockers.extend(member);
    report.blockers.extend(port_taken);
    status.blockers.clone_from(&report.blockers);
    let request = CheckinRequest {
        protocol_version: wire::PROTOCOL_VERSION,
        checkin_key: key.public_hex(),
        cli_version: env!("CARGO_PKG_VERSION"),
        report: &report,
        prep: Some(prep),
        attempt: status.attempt.as_ref(),
        probe: probes.report().await,
        machine_phase: local.phase.clone(),
    };
    let sent = tokio::time::Instant::now();
    let reply = client.post(key, &request).await;
    if request.probe.is_some() && matches!(&reply, client::Reply::Answer(_)) {
        probes.reported();
    }
    let throttled = matches!(reply, client::Reply::Throttled(..));
    let next = match reply {
        client::Reply::Answer(Answer::Wait(_))
            if !installing
                && local.participating.is_some()
                // Asked again only once it looks joined, so a reset since then doesn't stop the loop.
                && joined(host, status.attempt.as_ref()).await =>
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
    let running = host.prepare_running().await.unwrap_or_else(|why| {
        crate::ui::note(format!("Could not read {}: {why}", unit::PREPARE));
        true
    });
    let state = read_prep_state(&paths.prepare_state).map(|(state, _)| state);
    let finished = state
        .as_ref()
        .is_some_and(|state| !state.installing() && state.failed_at().is_none());
    if running || finished || host.stale_ployzd().is_some() {
        return false;
    }
    if let Err(why) = host.start_prepare().await {
        crate::ui::note(format!("Could not start {}: {why}", unit::PREPARE));
        return false;
    }
    if let Some(mut status) = LocalCheckin::read(&paths.status()).ok().flatten() {
        status.restarted_preparation(now);
        record(&status, paths);
    }
    true
}

/// Says in status.json why the loop can't start, keeping what the last pass saw of the host.
pub(crate) fn no_ployzd(paths: &Paths, why: &str, now: DateTime<Utc>) {
    let previous = LocalCheckin::read(&paths.status()).ok().flatten();
    let status = LocalCheckin {
        answer: Seen::NoPloyzd,
        token_fingerprint: previous
            .as_ref()
            .and_then(|status| status.token_fingerprint.clone()),
        reason: None,
        message: Some(why.to_owned()),
        at: now,
        prep: previous.as_ref().and_then(|status| status.prep),
        blockers: Vec::new(),
        prepare_retries: previous.as_ref().map_or(0, |status| status.prepare_retries),
        prepare_started: previous.as_ref().and_then(|status| status.prepare_started),
        refused: None,
        attempt: previous.and_then(|status| status.attempt),
        no_cluster: false,
    };
    record(&status, paths);
}

pub(crate) fn record(status: &LocalCheckin, paths: &Paths) {
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
