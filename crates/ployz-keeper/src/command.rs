//! Keeper subprocess execution.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::io::Read;
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::Duration;

use ployz_core::ops::FailureMessage;
use wait_timeout::ChildExt;

const DOCKER_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
const DATAPLANE_HOST_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);

pub trait KeeperCommandRunner {
    fn is_linux(&mut self) -> bool;
    fn current_uid(&mut self) -> Result<u32, FailureMessage>;
    fn systemctl(&mut self, args: &[&str]) -> Result<(), FailureMessage>;
    fn download(&mut self, url: &str, destination: &Path) -> Result<(), FailureMessage>;
    fn docker_info(&mut self) -> Result<(), FailureMessage>;
    fn enable_docker_service(&mut self) -> Result<(), FailureMessage>;
    fn run_docker_install_script(&mut self, script: &Path) -> Result<(), FailureMessage>;
    fn prepare_dataplane_host(&mut self) -> Result<(), FailureMessage>;
}

#[derive(Debug, Clone, Copy)]
pub struct SystemKeeperCommandRunner {
    timeout: Duration,
}

impl SystemKeeperCommandRunner {
    #[must_use]
    pub const fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl Default for SystemKeeperCommandRunner {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

impl KeeperCommandRunner for SystemKeeperCommandRunner {
    fn is_linux(&mut self) -> bool {
        std::env::consts::OS == "linux"
    }

    fn current_uid(&mut self) -> Result<u32, FailureMessage> {
        let output = run_command("id", &["-u"], self.timeout)?;
        if !output.status.success() {
            return Err(failure_message(format!(
                "id -u failed: {}",
                output.failure_summary()
            )));
        }
        output
            .stdout
            .trim()
            .parse::<u32>()
            .map_err(|error| failure_message(format!("id -u returned invalid uid: {error}")))
    }

    fn systemctl(&mut self, args: &[&str]) -> Result<(), FailureMessage> {
        let output = run_command("systemctl", args, self.timeout)?;
        if output.status.success() {
            return Ok(());
        }
        Err(failure_message(format!(
            "systemctl failed: {}",
            output.failure_summary()
        )))
    }

    fn download(&mut self, url: &str, destination: &Path) -> Result<(), FailureMessage> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .timeout_global(Some(self.timeout))
            .timeout_connect(Some(self.timeout.min(Duration::from_secs(5))))
            .build()
            .into();
        let mut response = agent
            .get(url)
            .call()
            .map_err(|error| failure_message(format!("artifact download failed: {error}")))?;
        let mut destination = File::create(destination).map_err(|error| {
            failure_message(format!("failed to create downloaded artifact: {error}"))
        })?;
        io::copy(&mut response.body_mut().as_reader(), &mut destination).map_err(|error| {
            failure_message(format!("failed to write downloaded artifact: {error}"))
        })?;
        Ok(())
    }

    fn docker_info(&mut self) -> Result<(), FailureMessage> {
        let output = run_command("docker", &["info"], self.timeout)?;
        if output.status.success() {
            return Ok(());
        }
        Err(failure_message(format!(
            "docker info failed: {}",
            output.failure_summary()
        )))
    }

    fn enable_docker_service(&mut self) -> Result<(), FailureMessage> {
        self.systemctl(&["enable", "--now", "docker"])
    }

    fn run_docker_install_script(&mut self, script: &Path) -> Result<(), FailureMessage> {
        let output = run_os_command_with_display(
            "sh",
            &[script.as_os_str().to_os_string()],
            "sh <docker-install-script>".to_owned(),
            DOCKER_INSTALL_TIMEOUT,
        )?;
        if output.status.success() {
            return Ok(());
        }
        Err(failure_message(format!(
            "docker install script failed: {}",
            output.failure_summary()
        )))
    }

    fn prepare_dataplane_host(&mut self) -> Result<(), FailureMessage> {
        if dataplane_host_ready(self.timeout) {
            return Ok(());
        }

        let output = run_command("apt-get", &["update"], DATAPLANE_HOST_INSTALL_TIMEOUT)?;
        if !output.status.success() {
            return Err(failure_message(format!(
                "apt-get update failed: {}",
                output.failure_summary()
            )));
        }

        let output = run_os_command_with_display(
            "env",
            &[
                OsString::from("DEBIAN_FRONTEND=noninteractive"),
                OsString::from("apt-get"),
                OsString::from("install"),
                OsString::from("-y"),
                OsString::from("wireguard-tools"),
                OsString::from("iproute2"),
            ],
            "apt-get install -y wireguard-tools iproute2".to_owned(),
            DATAPLANE_HOST_INSTALL_TIMEOUT,
        )?;
        if !output.status.success() {
            return Err(failure_message(format!(
                "dataplane host package install failed: {}",
                output.failure_summary()
            )));
        }

        if dataplane_host_ready(self.timeout) {
            return Ok(());
        }
        Err(failure_message(
            "dataplane host packages installed but wg/ip/tc/tun are not ready",
        ))
    }
}

fn dataplane_host_ready(timeout: Duration) -> bool {
    Path::new("/dev/net/tun").exists()
        && command_success("wg", &["--version"], timeout)
        && command_success("ip", &["-V"], timeout)
        && command_success("tc", &["-V"], timeout)
}

fn command_success(program: &str, args: &[&str], timeout: Duration) -> bool {
    run_command(program, args, timeout).is_ok_and(|output| output.status.success())
}

struct CapturedCommandOutput {
    command: String,
    status: ExitStatus,
    stdout: String,
    stderr: String,
}

impl CapturedCommandOutput {
    fn failure_summary(&self) -> String {
        let mut summary = format!("{} exited with status {}", self.command, self.status);
        let stdout = self.stdout.trim();
        if !stdout.is_empty() {
            summary.push_str("; stdout: ");
            summary.push_str(stdout);
        }
        let stderr = self.stderr.trim();
        if !stderr.is_empty() {
            summary.push_str("; stderr: ");
            summary.push_str(stderr);
        }
        summary
    }
}

fn run_command(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<CapturedCommandOutput, FailureMessage> {
    let args = args.iter().map(OsString::from).collect::<Vec<_>>();
    run_os_command_with_display(program, &args, render_command(program, &args), timeout)
}

fn run_os_command_with_display(
    program: &str,
    args: &[OsString],
    display_command: String,
    timeout: Duration,
) -> Result<CapturedCommandOutput, FailureMessage> {
    let mut child = Command::new(program)
        .args(args)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| failure_message(format!("failed to run {display_command}: {error}")))?;
    let stdout = child
        .stdout
        .take()
        .map(|pipe| thread::spawn(move || read_limited_pipe(pipe)))
        .expect("stdout is piped");
    let stderr = child
        .stderr
        .take()
        .map(|pipe| thread::spawn(move || read_limited_pipe(pipe)))
        .expect("stderr is piped");
    let status = wait_for_child(&display_command, &mut child, timeout)?;
    Ok(CapturedCommandOutput {
        command: display_command,
        status,
        stdout: stdout
            .join()
            .unwrap_or_else(|_error| Err("stdout reader panicked".to_owned()))
            .map_err(failure_message)?,
        stderr: stderr
            .join()
            .unwrap_or_else(|_error| Err("stderr reader panicked".to_owned()))
            .map_err(failure_message)?,
    })
}

fn read_limited_pipe(pipe: impl Read) -> Result<String, String> {
    let mut output = String::new();
    pipe.take(4096)
        .read_to_string(&mut output)
        .map_err(|error| error.to_string())?;
    Ok(output)
}

fn render_command(program: &str, args: &[OsString]) -> String {
    if args.is_empty() {
        return program.to_owned();
    }
    let args = args
        .iter()
        .map(|arg| arg.to_string_lossy())
        .collect::<Vec<_>>();
    format!("{} {}", program, args.join(" "))
}

fn wait_for_child(
    command: &str,
    child: &mut Child,
    timeout: Duration,
) -> Result<ExitStatus, FailureMessage> {
    match child
        .wait_timeout(timeout)
        .map_err(|error| failure_message(format!("failed to wait for {command}: {error}")))?
    {
        Some(status) => Ok(status),
        None => {
            let _ = child.kill();
            let _ = child.wait();
            Err(failure_message(format!(
                "{command} timed out after {}s",
                timeout.as_secs()
            )))
        }
    }
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("keeper generated a non-empty failure message")
}

#[cfg(test)]
mod tests {
    use super::DOCKER_INSTALL_TIMEOUT;
    use std::ffi::OsString;
    use std::time::Duration;

    #[test]
    fn command_failure_summary_uses_redacted_display_command() {
        let secret_url = "https://example.invalid/artifact?token=secret";
        let output = super::run_os_command_with_display(
            "false",
            &[OsString::from(secret_url)],
            "download <redacted-url>".to_owned(),
            Duration::from_secs(1),
        )
        .expect("false command runs");

        let summary = output.failure_summary();

        assert!(summary.contains("download <redacted-url>"));
        assert!(!summary.contains(secret_url));
        assert!(!summary.contains("secret"));
    }

    #[test]
    fn docker_install_timeout_allows_first_bootstrap_package_install() {
        assert_eq!(DOCKER_INSTALL_TIMEOUT, Duration::from_secs(300));
    }
}
