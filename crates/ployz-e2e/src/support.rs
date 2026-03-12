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
struct ReadyPayload {
    ready: bool,
}

#[derive(Debug, Deserialize)]
struct ReadyEnvelope {
    message: String,
}

pub(crate) fn parse_ready(output: &str) -> Result<bool> {
    if let Ok(payload) = serde_json::from_str::<ReadyPayload>(output) {
        return Ok(payload.ready);
    }

    let envelope = serde_json::from_str::<ReadyEnvelope>(output).map_err(|error| {
        Error::Message(format!("failed to parse readiness response envelope: {error}"))
    })?;
    let payload = serde_json::from_str::<ReadyPayload>(&envelope.message).map_err(|error| {
        Error::Message(format!("failed to parse readiness response message: {error}"))
    })?;
    Ok(payload.ready)
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
