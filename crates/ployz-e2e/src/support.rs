use crate::error::{Error, Result};
use serde::Deserialize;
use std::net::TcpListener;
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

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
    #[serde(default)]
    pub(crate) workload_subnet_present: bool,
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
    VolumeZfsTransfer(VolumeZfsTransferPayload),
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
    pub(crate) machine_role: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct DoctorPeer {
    pub(crate) machine_id: String,
    pub(crate) machine_role: String,
    pub(crate) blocking: bool,
    pub(crate) store_lifecycle: String,
    pub(crate) wg_state: String,
    pub(crate) probe_state: String,
    pub(crate) corrosion_state: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VolumeZfsTransferPayload {
    pub(crate) transfer: VolumeZfsTransferInfo,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VolumeZfsTransferInfo {
    pub(crate) id: String,
    pub(crate) status: String,
    pub(crate) stage: String,
    #[serde(default)]
    pub(crate) bytes_transferred: Option<u64>,
    #[serde(default)]
    pub(crate) last_error: Option<String>,
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
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| Error::Io(format!("spawn {program}: {error}")))?;
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
    let deadline = Instant::now() + timeout;
    loop {
        if predicate()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Message(format!(
                "timed out after {}s",
                timeout.as_secs()
            )));
        }
        thread::sleep(POLL_INTERVAL);
    }
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
