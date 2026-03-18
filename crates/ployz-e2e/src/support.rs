use crate::error::{Error, Result};
use ployz_api::{MachineListPayload, MeshReadyPayload};
use ployz_sdk::{DaemonClient, StdioTransport};
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

pub(crate) fn daemon_machine_list_in_container(container_name: &str) -> Result<MachineListPayload> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Io(format!("build machine list runtime: {error}")))?;
    let transport = StdioTransport::new("docker")
        .arg("exec")
        .arg("-i")
        .arg(container_name)
        .arg("ployzd")
        .arg("rpc-stdio");
    let client = DaemonClient::new(transport);
    runtime
        .block_on(async { client.machine_list().await })
        .map_err(|error| Error::Io(format!("load machine list in '{container_name}': {error}")))
}

pub(crate) fn daemon_mesh_ready_in_container(container_name: &str) -> Result<MeshReadyPayload> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Io(format!("build mesh ready runtime: {error}")))?;
    let transport = StdioTransport::new("docker")
        .arg("exec")
        .arg("-i")
        .arg(container_name)
        .arg("ployzd")
        .arg("rpc-stdio");
    let client = DaemonClient::new(transport);
    runtime
        .block_on(async { client.mesh_ready().await })
        .map_err(|error| Error::Io(format!("probe mesh readiness in '{container_name}': {error}")))
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
