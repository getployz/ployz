//! Host Runner subprocess execution.

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

pub trait HostRunnerCommandRunner {
    fn command(
        &mut self,
        program: &str,
        args: &[&str],
    ) -> Result<HostRunnerCommandOutput, FailureMessage>;
    fn command_with_timeout(
        &mut self,
        program: &str,
        args: &[&str],
        _timeout: Duration,
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        self.command(program, args)
    }
    fn read_os_release(&mut self) -> Result<String, FailureMessage> {
        let output = self.command("cat", &["/etc/os-release"])?;
        if output.success {
            return Ok(output.stdout);
        }
        Err(failure_message(format!(
            "failed to read /etc/os-release: {}",
            output.failure
        )))
    }
    fn is_linux(&mut self) -> bool;
    fn current_uid(&mut self) -> Result<u32, FailureMessage>;
    fn download(&mut self, url: &str, destination: &Path) -> Result<(), FailureMessage>;
    fn docker_info(&mut self) -> Result<(), FailureMessage>;
    fn docker_is_installed(&mut self) -> bool;
    fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage>;
    fn docker_has_insecure_registry(&mut self, cidr: &str) -> Result<bool, FailureMessage>;
    fn dataplane_host_ready(&mut self) -> bool {
        false
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SystemHostRunnerCommandRunner {
    timeout: Duration,
}

impl SystemHostRunnerCommandRunner {
    #[must_use]
    pub const fn new(timeout: Duration) -> Self {
        Self { timeout }
    }

    #[must_use]
    pub const fn timeout(&self) -> Duration {
        self.timeout
    }
}

impl Default for SystemHostRunnerCommandRunner {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

impl HostRunnerCommandRunner for SystemHostRunnerCommandRunner {
    fn command(
        &mut self,
        program: &str,
        args: &[&str],
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        host_runner_command(program, args, self.timeout)
    }

    fn command_with_timeout(
        &mut self,
        program: &str,
        args: &[&str],
        timeout: Duration,
    ) -> Result<HostRunnerCommandOutput, FailureMessage> {
        host_runner_command(program, args, timeout)
    }
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

    fn docker_is_installed(&mut self) -> bool {
        command_success("docker", &["--version"], self.timeout)
    }

    fn docker_uses_containerd_snapshotter(&mut self) -> Result<bool, FailureMessage> {
        let output = run_command(
            "docker",
            &["info", "--format", "{{json .DriverStatus}}"],
            self.timeout,
        )?;
        if !output.status.success() {
            return Err(failure_message(format!(
                "docker info failed: {}",
                output.failure_summary()
            )));
        }
        Ok(output.stdout.contains("io.containerd.snapshotter.v1"))
    }

    fn docker_has_insecure_registry(&mut self, cidr: &str) -> Result<bool, FailureMessage> {
        let output = run_command(
            "docker",
            &[
                "info",
                "--format",
                "{{json .RegistryConfig.InsecureRegistryCIDRs}}",
            ],
            self.timeout,
        )?;
        if !output.status.success() {
            return Err(failure_message(format!(
                "docker info failed: {}",
                output.failure_summary()
            )));
        }
        let registries =
            serde_json::from_str::<Vec<String>>(output.stdout.trim()).map_err(|error| {
                failure_message(format!(
                    "docker info returned invalid insecure registries: {error}"
                ))
            })?;
        Ok(registries.iter().any(|registry| registry == cidr))
    }

    fn dataplane_host_ready(&mut self) -> bool {
        dataplane_host_ready(self.timeout)
    }
}

fn dataplane_host_ready(timeout: Duration) -> bool {
    Path::new("/dev/net/tun").exists()
        && command_success("wg", &["--version"], timeout)
        && command_success("ip", &["-V"], timeout)
        && command_success("tc", &["-V"], timeout)
        && command_success("ping", &["-V"], timeout)
}

fn command_success(program: &str, args: &[&str], timeout: Duration) -> bool {
    run_command(program, args, timeout).is_ok_and(|output| output.status.success())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HostRunnerCommandOutput {
    pub success: bool,
    pub exit_code: Option<i32>,
    pub stdout: String,
    pub failure: String,
}

fn host_runner_command(
    program: &str,
    args: &[&str],
    timeout: Duration,
) -> Result<HostRunnerCommandOutput, FailureMessage> {
    let output = run_command(program, args, timeout)?;
    let failure = if output.status.success() {
        String::new()
    } else {
        output.failure_summary()
    };
    Ok(HostRunnerCommandOutput {
        success: output.status.success(),
        exit_code: output.status.code(),
        stdout: output.stdout,
        failure,
    })
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

fn read_limited_pipe(mut pipe: impl Read) -> Result<String, String> {
    let mut buffer = Vec::new();
    (&mut pipe)
        .take(4096)
        .read_to_end(&mut buffer)
        .map_err(|error| error.to_string())?;
    // Drain any output past the cap into a sink. Otherwise a child that writes
    // more than 4 KB keeps writing to a pipe we have already stopped reading,
    // takes SIGPIPE, and is misreported as a failed command (e.g. `systemctl
    // list-units` on a host with more than 4 KB of active services).
    io::copy(&mut pipe, &mut io::sink()).map_err(|error| error.to_string())?;
    Ok(String::from_utf8_lossy(&buffer).into_owned())
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
    FailureMessage::try_new(message).expect("Host Runner generated a non-empty failure message")
}

#[cfg(test)]
mod tests {
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
    fn large_command_output_does_not_sigpipe_the_child() {
        // A child that writes well past the 4 KB read cap must still succeed:
        // the reader drains the remainder instead of closing the pipe and
        // killing the child with SIGPIPE. Regression for the machine-join
        // firewall preflight, which reads `systemctl list-units` output.
        let output = super::run_os_command_with_display(
            "sh",
            &[OsString::from("-c"), OsString::from("seq 1 20000")],
            "sh -c seq".to_owned(),
            Duration::from_secs(10),
        )
        .expect("sh command runs");

        assert!(
            output.status.success(),
            "a command that writes past the read cap must not be reported as failed: {}",
            output.failure_summary()
        );
        assert!(!output.stdout.is_empty());
    }
}
