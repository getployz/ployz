use crate::error::{Error, Result};
use backon::{BlockingRetryable, ConstantBuilder};
use serde::Deserialize;
use std::net::TcpListener;
use std::process::ExitStatus;
use std::time::Duration;
use xshell::{Shell, cmd};

const POLL_INTERVAL: Duration = Duration::from_secs(2);

#[derive(Debug)]
pub(crate) struct CommandOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl CommandOutput {
    #[must_use]
    pub(crate) fn combined(&self) -> String {
        if self.stderr.trim().is_empty() {
            self.stdout.trim().to_string()
        } else if self.stdout.trim().is_empty() {
            self.stderr.trim().to_string()
        } else {
            format!("{}\n{}", self.stdout.trim(), self.stderr.trim())
        }
    }
}

impl Default for CommandOutput {
    fn default() -> Self {
        Self {
            status: success_status(),
            stdout: String::new(),
            stderr: String::new(),
        }
    }
}

#[derive(Debug, Deserialize)]
pub(crate) struct ReadyPayload {
    pub(crate) ready: bool,
}

#[derive(Debug, Deserialize)]
struct ReadyEnvelope {
    message: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DaemonJsonResponse {
    pub(crate) ok: bool,
    pub(crate) code: String,
    pub(crate) message: String,
    pub(crate) payload: Option<DaemonJsonPayload>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub(crate) enum DaemonJsonPayload {
    Doctor(DoctorPayload),
    MachineList(MachineListPayload),
}

#[derive(Debug, Deserialize)]
pub(crate) struct MachineListPayload {
    pub(crate) rows: Vec<MachineListRow>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct MachineListRow {
    pub(crate) id: String,
    pub(crate) lifecycle: String,
    #[serde(default)]
    pub(crate) subnet: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DoctorPayload {
    pub(crate) overall: DoctorOverall,
    pub(crate) local: DoctorLocal,
    pub(crate) peers: Vec<DoctorPeer>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DoctorOverall {
    pub(crate) lifecycle: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DoctorLocal {
    pub(crate) storage: bool,
    pub(crate) storage_participation: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DoctorPeer {
    pub(crate) machine_id: String,
    pub(crate) storage: bool,
    pub(crate) storage_participation: String,
    pub(crate) blocking: bool,
    pub(crate) store_lifecycle: String,
    pub(crate) wg_state: String,
    pub(crate) probe_state: String,
}

pub(crate) fn parse_ready(output: &str) -> Result<bool> {
    Ok(parse_ready_payload(output)?.ready)
}

pub(crate) fn parse_ready_payload(output: &str) -> Result<ReadyPayload> {
    if let Ok(payload) = serde_json::from_str::<ReadyPayload>(output) {
        return Ok(payload);
    }

    let envelope = serde_json::from_str::<ReadyEnvelope>(output).map_err(|error| {
        Error::Message(format!(
            "failed to parse readiness response envelope: {error}"
        ))
    })?;
    serde_json::from_str::<ReadyPayload>(&envelope.message).map_err(|error| {
        Error::Message(format!(
            "failed to parse readiness response message: {error}"
        ))
    })
}

pub(crate) fn parse_daemon_json_response(output: &str) -> Result<DaemonJsonResponse> {
    serde_json::from_str::<DaemonJsonResponse>(output)
        .map_err(|error| Error::Message(format!("failed to parse daemon JSON response: {error}")))
}

pub(crate) fn docker_outer<const N: usize>(args: [&str; N]) -> Result<CommandOutput> {
    run_command_expect_ok("docker", &args)
}

pub(crate) fn docker_outer_raw<const N: usize>(args: [&str; N]) -> Result<CommandOutput> {
    run_command("docker", &args)
}

pub(crate) fn run_command(program: &str, args: &[&str]) -> Result<CommandOutput> {
    let sh = Shell::new().map_err(|error| Error::Io(format!("create shell: {error}")))?;
    let output = cmd!(sh, "{program} {args...}")
        .ignore_status()
        .output()
        .map_err(|error| Error::Io(format!("run {program}: {error}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok(CommandOutput {
        status: output.status,
        stdout,
        stderr,
    })
}

pub(crate) fn run_command_expect_ok(program: &str, args: &[&str]) -> Result<CommandOutput> {
    let output = run_command(program, args)?;
    if output.status.success() {
        return Ok(output);
    }
    Err(Error::CommandFailed {
        command: format!("{program} {}", args.join(" ")),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

pub(crate) fn wait_until<F>(timeout: Duration, mut predicate: F) -> Result<()>
where
    F: FnMut() -> Result<bool>,
{
    wait_until_with_interval(timeout, POLL_INTERVAL, &mut predicate)
}

pub(crate) fn wait_until_with_interval<F>(
    timeout: Duration,
    poll_interval: Duration,
    mut predicate: F,
) -> Result<()>
where
    F: FnMut() -> Result<bool>,
{
    let attempts = attempts_for_duration(timeout, poll_interval);
    let result = (|| match predicate() {
        Ok(true) => Ok(()),
        Ok(false) => Err(WaitAttemptError::Pending),
        Err(error) => Err(WaitAttemptError::Predicate(error)),
    })
    .retry(
        ConstantBuilder::default()
            .with_delay(poll_interval)
            .with_max_times(attempts),
    )
    .when(|error| matches!(error, WaitAttemptError::Pending))
    .call();

    match result {
        Ok(()) => Ok(()),
        Err(WaitAttemptError::Pending) => Err(Error::Message(format!(
            "timed out after {}s",
            timeout.as_secs()
        ))),
        Err(WaitAttemptError::Predicate(error)) => Err(error),
    }
}

enum WaitAttemptError {
    Pending,
    Predicate(Error),
}

fn attempts_for_duration(timeout: Duration, interval: Duration) -> usize {
    if interval.is_zero() {
        return 1;
    }
    let attempts = timeout.as_nanos() / interval.as_nanos() + 1;
    attempts.min(usize::MAX as u128) as usize
}

pub(crate) fn pick_free_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| Error::Io(format!("bind free port probe: {error}")))?;
    let port = listener
        .local_addr()
        .map_err(|error| Error::Io(format!("read free port probe address: {error}")))?
        .port();
    drop(listener);
    Ok(port)
}

#[cfg(unix)]
fn success_status() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatusExt::from_raw(0)
}

#[cfg(windows)]
fn success_status() -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatusExt::from_raw(0)
}
