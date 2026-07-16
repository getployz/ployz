//! Bounded Host Runner subprocess execution.

use std::ffi::OsString;
use std::fs::File;
use std::io;
use std::io::{Read, Write};
use std::path::Path;
use std::process::{Child, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use ployz_core::operation::FailureMessage;
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
}

impl Default for SystemHostRunnerCommandRunner {
    fn default() -> Self {
        Self::new(Duration::from_secs(30))
    }
}

#[derive(Debug)]
enum DownloadAttemptError {
    Transient(String),
    Permanent(String),
}

#[derive(Debug, Clone, Copy)]
enum DownloadFailureEnd {
    PermanentFailure,
    RetryLimit,
    OverallDeadline,
}

fn download_with_retry(
    source: &str,
    destination: &Path,
    deadline: Duration,
    initial_backoff: Duration,
    mut attempt_download: impl FnMut(&mut File, Duration) -> Result<(), DownloadAttemptError>,
) -> Result<(), FailureMessage> {
    const MAX_ATTEMPTS: usize = 11;
    const MAX_BACKOFF: Duration = Duration::from_secs(1);

    let deadline = Instant::now() + deadline;
    let mut backoff = initial_backoff;
    let mut last_error = "overall deadline elapsed".to_owned();

    for attempts in 1..=MAX_ATTEMPTS {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(download_failure(
                source,
                attempts - 1,
                &last_error,
                DownloadFailureEnd::OverallDeadline,
            ));
        }

        let mut file = File::create(destination).map_err(|error| {
            download_failure(
                source,
                attempts - 1,
                &format!("failed to create downloaded artifact: {error}"),
                DownloadFailureEnd::PermanentFailure,
            )
        })?;
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(download_failure(
                source,
                attempts - 1,
                &last_error,
                DownloadFailureEnd::OverallDeadline,
            ));
        }
        match attempt_download(&mut file, remaining) {
            Ok(()) if deadline.saturating_duration_since(Instant::now()).is_zero() => {
                return Err(download_failure(
                    source,
                    attempts,
                    "attempt completed after the overall deadline",
                    DownloadFailureEnd::OverallDeadline,
                ));
            }
            Ok(()) => return Ok(()),
            Err(DownloadAttemptError::Permanent(error)) => {
                return Err(download_failure(
                    source,
                    attempts,
                    &error,
                    DownloadFailureEnd::PermanentFailure,
                ));
            }
            Err(DownloadAttemptError::Transient(error)) => last_error = error,
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(download_failure(
                source,
                attempts,
                &last_error,
                DownloadFailureEnd::OverallDeadline,
            ));
        }
        if attempts < MAX_ATTEMPTS {
            thread::sleep(backoff.min(remaining));
            backoff = backoff.saturating_mul(2).min(MAX_BACKOFF);
        }
    }

    Err(download_failure(
        source,
        MAX_ATTEMPTS,
        &last_error,
        DownloadFailureEnd::RetryLimit,
    ))
}

fn download_failure(
    source: &str,
    attempts: usize,
    error: &str,
    ending: DownloadFailureEnd,
) -> FailureMessage {
    let attempt = if attempts == 1 { "attempt" } else { "attempts" };
    let ending = match ending {
        DownloadFailureEnd::PermanentFailure => "permanent failure",
        DownloadFailureEnd::RetryLimit => "retry limit",
        DownloadFailureEnd::OverallDeadline => "overall deadline",
    };
    failure_message(format!(
        "artifact download from {source} failed after {attempts} {attempt}: {error}; ended by {ending}"
    ))
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
        download_with_retry(
            url,
            destination,
            Duration::from_secs(10 * 60),
            Duration::from_millis(10),
            |file, remaining| download_attempt(url, file, remaining),
        )
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

fn download_attempt(
    source: &str,
    destination: &mut File,
    remaining: Duration,
) -> Result<(), DownloadAttemptError> {
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(remaining))
        .timeout_resolve(Some(remaining.min(Duration::from_secs(5))))
        .timeout_connect(Some(remaining.min(Duration::from_secs(5))))
        .build()
        .into();
    let mut response = agent.get(source).call().map_err(classify_download_error)?;
    let mut reader = response.body_mut().as_reader();
    let mut buffer = [0; 16 * 1024];
    loop {
        let count = reader.read(&mut buffer).map_err(classify_body_read_error)?;
        if count == 0 {
            return Ok(());
        }
        let (bytes, _) = buffer.split_at(count);
        destination
            .write_all(bytes)
            .map_err(|error| DownloadAttemptError::Permanent(error.to_string()))?;
    }
}

fn classify_download_error(error: ureq::Error) -> DownloadAttemptError {
    let message = error.to_string();
    if transient_download_error(&error) {
        DownloadAttemptError::Transient(message)
    } else {
        DownloadAttemptError::Permanent(message)
    }
}

fn transient_download_error(error: &ureq::Error) -> bool {
    matches!(
        error,
        ureq::Error::StatusCode(408 | 429 | 500..=599)
            | ureq::Error::Io(_)
            | ureq::Error::Timeout(_)
            | ureq::Error::HostNotFound
            | ureq::Error::ConnectionFailed
    )
}

fn classify_body_read_error(error: io::Error) -> DownloadAttemptError {
    let message = error.to_string();
    let transient = error
        .get_ref()
        .and_then(|error| error.downcast_ref::<ureq::Error>())
        .map_or_else(
            || {
                matches!(
                    error.kind(),
                    io::ErrorKind::TimedOut
                        | io::ErrorKind::ConnectionReset
                        | io::ErrorKind::ConnectionAborted
                        | io::ErrorKind::BrokenPipe
                        | io::ErrorKind::UnexpectedEof
                        | io::ErrorKind::NotConnected
                        | io::ErrorKind::Interrupted
                )
            },
            transient_download_error,
        );
    if transient {
        DownloadAttemptError::Transient(message)
    } else {
        DownloadAttemptError::Permanent(message)
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
    pub stdout_truncated: bool,
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
        stdout_truncated: output.stdout_truncated,
        failure,
    })
}

struct CapturedCommandOutput {
    command: String,
    status: ExitStatus,
    stdout: String,
    stdout_truncated: bool,
    stderr: String,
    stderr_truncated: bool,
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
        if self.stderr_truncated {
            summary.push_str("; stderr evidence was truncated");
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
    let stdout = stdout
        .join()
        .unwrap_or_else(|_error| Err("stdout reader panicked".to_owned()))
        .map_err(failure_message)?;
    let stderr = stderr
        .join()
        .unwrap_or_else(|_error| Err("stderr reader panicked".to_owned()))
        .map_err(failure_message)?;
    Ok(CapturedCommandOutput {
        command: display_command,
        status,
        stdout: stdout.text,
        stdout_truncated: stdout.truncated,
        stderr: stderr.text,
        stderr_truncated: stderr.truncated,
    })
}

const COMMAND_OUTPUT_CAPTURE_LIMIT_BYTES: usize = 1024 * 1024;

struct LimitedPipeOutput {
    text: String,
    truncated: bool,
}

fn read_limited_pipe(mut pipe: impl Read) -> Result<LimitedPipeOutput, String> {
    let mut buffer = Vec::with_capacity(COMMAND_OUTPUT_CAPTURE_LIMIT_BYTES + 1);
    (&mut pipe)
        .take((COMMAND_OUTPUT_CAPTURE_LIMIT_BYTES + 1) as u64)
        .read_to_end(&mut buffer)
        .map_err(|error| error.to_string())?;
    let truncated = buffer.len() > COMMAND_OUTPUT_CAPTURE_LIMIT_BYTES;
    buffer.truncate(COMMAND_OUTPUT_CAPTURE_LIMIT_BYTES);
    // Drain any output past the cap into a sink. Otherwise a child that writes
    // more than the cap keeps writing to a pipe we have already stopped reading,
    // takes SIGPIPE, and is misreported as a failed command (e.g. `systemctl
    // list-units` on a host with many active services).
    io::copy(&mut pipe, &mut io::sink()).map_err(|error| error.to_string())?;
    Ok(LimitedPipeOutput {
        text: String::from_utf8_lossy(&buffer).into_owned(),
        truncated,
    })
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
    use std::process::Command;
    use std::time::Duration;

    use super::{
        DownloadAttemptError, classify_body_read_error, classify_download_error,
        download_with_retry,
    };

    #[test]
    fn http_statuses_classify_retryability() {
        for status in [408, 429, 500] {
            assert!(matches!(
                classify_download_error(ureq::Error::StatusCode(status)),
                DownloadAttemptError::Transient(_)
            ));
        }
        assert!(matches!(
            classify_download_error(ureq::Error::StatusCode(404)),
            DownloadAttemptError::Permanent(_)
        ));
    }

    #[test]
    fn download_retry_stops_when_destination_cannot_be_created() {
        let destination = tempfile::tempdir().expect("temporary directory");
        let mut attempts = 0;

        let error = download_with_retry(
            "https://example.invalid/artifact",
            destination.path(),
            Duration::from_secs(1),
            Duration::ZERO,
            |_file, _remaining| {
                attempts += 1;
                Ok(())
            },
        )
        .expect_err("a directory cannot be replaced by an artifact file")
        .to_string();

        assert_eq!(attempts, 0);
        assert!(error.contains("failed after 0 attempts"), "{error}");
        assert!(error.contains("ended by permanent failure"), "{error}");
    }

    #[test]
    fn body_read_errors_retry_transport_failures_only() {
        assert!(matches!(
            classify_body_read_error(std::io::ErrorKind::TimedOut.into()),
            DownloadAttemptError::Transient(_)
        ));
        assert!(matches!(
            classify_body_read_error(std::io::ErrorKind::ConnectionReset.into()),
            DownloadAttemptError::Transient(_)
        ));
        assert!(matches!(
            classify_body_read_error(std::io::ErrorKind::InvalidData.into()),
            DownloadAttemptError::Permanent(_)
        ));
    }

    #[test]
    fn download_retry_recovers_from_transient_failure() {
        let destination = tempfile::NamedTempFile::new().expect("temporary destination");
        let mut attempts = 0;

        download_with_retry(
            "https://example.invalid/artifact",
            destination.path(),
            Duration::from_secs(1),
            Duration::ZERO,
            |file, _remaining| {
                attempts += 1;
                if attempts == 1 {
                    return Err(DownloadAttemptError::Transient(
                        "connection reset".to_owned(),
                    ));
                }
                std::io::Write::write_all(file, b"artifact")
                    .map_err(|error| DownloadAttemptError::Permanent(error.to_string()))
            },
        )
        .expect("second attempt succeeds");

        assert_eq!(
            (
                attempts,
                std::fs::read(destination.path()).expect("read artifact")
            ),
            (2, b"artifact".to_vec())
        );
    }

    #[test]
    fn download_retry_reports_retry_limit_after_eleven_attempts() {
        let destination = tempfile::NamedTempFile::new().expect("temporary destination");
        let source = "https://example.invalid/artifact?token=raw";
        let mut attempts = 0;

        let error = download_with_retry(
            source,
            destination.path(),
            Duration::from_secs(5),
            Duration::ZERO,
            |_file, _remaining| {
                attempts += 1;
                Err(DownloadAttemptError::Transient(
                    "connection reset".to_owned(),
                ))
            },
        )
        .expect_err("retry limit is terminal")
        .to_string();

        assert_eq!(attempts, 11);
        assert!(error.contains("11 attempts"), "{error}");
        assert!(error.contains(source), "{error}");
        assert!(error.contains("ended by retry limit"), "{error}");
    }

    #[test]
    fn download_retry_reports_deadline_before_retry_limit() {
        let destination = tempfile::NamedTempFile::new().expect("temporary destination");
        let source = "https://example.invalid/slow-artifact";
        let mut attempts = 0;

        let error = download_with_retry(
            source,
            destination.path(),
            Duration::from_millis(50),
            Duration::ZERO,
            |_file, _remaining| {
                attempts += 1;
                std::thread::sleep(Duration::from_millis(100));
                Err(DownloadAttemptError::Transient("timed out".to_owned()))
            },
        )
        .expect_err("deadline is terminal")
        .to_string();

        assert_eq!(attempts, 1);
        assert!(error.contains("1 attempt"), "{error}");
        assert!(error.contains(source), "{error}");
        assert!(error.contains("ended by overall deadline"), "{error}");
    }

    #[test]
    fn download_retry_rejects_success_after_deadline() {
        let destination = tempfile::NamedTempFile::new().expect("temporary destination");

        let error = download_with_retry(
            "https://example.invalid/slow-artifact",
            destination.path(),
            Duration::from_millis(50),
            Duration::ZERO,
            |_file, _remaining| {
                std::thread::sleep(Duration::from_millis(100));
                Ok(())
            },
        )
        .expect_err("success after the deadline is terminal")
        .to_string();

        assert!(error.contains("1 attempt"), "{error}");
        assert!(error.contains("ended by overall deadline"), "{error}");
    }

    #[test]
    fn download_retry_replaces_partial_bytes_before_retry() {
        let destination = tempfile::NamedTempFile::new().expect("temporary destination");
        let mut attempts = 0;

        download_with_retry(
            "https://example.invalid/artifact",
            destination.path(),
            Duration::from_secs(1),
            Duration::ZERO,
            |file, _remaining| {
                attempts += 1;
                if attempts == 1 {
                    std::io::Write::write_all(file, b"partial garbage").expect("write partial");
                    return Err(DownloadAttemptError::Transient(
                        "body read failed".to_owned(),
                    ));
                }
                std::io::Write::write_all(file, b"ok")
                    .map_err(|error| DownloadAttemptError::Permanent(error.to_string()))
            },
        )
        .expect("retry succeeds");

        assert_eq!(
            std::fs::read(destination.path()).expect("read artifact"),
            b"ok"
        );
    }

    #[test]
    fn download_retry_stops_after_permanent_failure() {
        let destination = tempfile::NamedTempFile::new().expect("temporary destination");
        let source = "https://example.invalid/missing";
        let mut attempts = 0;

        let error = download_with_retry(
            source,
            destination.path(),
            Duration::from_secs(1),
            Duration::ZERO,
            |_file, _remaining| {
                attempts += 1;
                Err(DownloadAttemptError::Permanent(
                    "http status: 404".to_owned(),
                ))
            },
        )
        .expect_err("permanent failure is terminal")
        .to_string();

        assert_eq!(attempts, 1);
        assert!(error.contains("http status: 404"), "{error}");
        assert!(error.contains("ended by permanent failure"), "{error}");
    }

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
        // A child that writes well past the read cap must still succeed:
        // the reader drains the remainder instead of closing the pipe and
        // killing the child with SIGPIPE. Regression for the machine-join
        // firewall preflight, which reads `systemctl list-units` output.
        let output = super::run_os_command_with_display(
            "sh",
            &[OsString::from("-c"), OsString::from("seq 1 300000")],
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
        assert!(output.stdout_truncated);
    }

    #[test]
    fn exact_capture_limit_is_not_truncated() {
        let input = vec![b'x'; super::COMMAND_OUTPUT_CAPTURE_LIMIT_BYTES];

        let output = super::read_limited_pipe(input.as_slice()).expect("capture succeeds");

        assert_eq!(output.text.len(), super::COMMAND_OUTPUT_CAPTURE_LIMIT_BYTES);
        assert!(!output.truncated);
    }

    #[test]
    fn output_past_capture_limit_is_truncated() {
        let input = vec![b'x'; super::COMMAND_OUTPUT_CAPTURE_LIMIT_BYTES + 1];

        let output = super::read_limited_pipe(input.as_slice()).expect("capture succeeds");

        assert_eq!(output.text.len(), super::COMMAND_OUTPUT_CAPTURE_LIMIT_BYTES);
        assert!(output.truncated);
    }

    #[test]
    fn exact_capture_limit_stderr_is_not_reported_as_truncated() {
        let input = vec![b'e'; super::COMMAND_OUTPUT_CAPTURE_LIMIT_BYTES];
        let stderr = super::read_limited_pipe(input.as_slice()).expect("capture succeeds");
        let output = super::CapturedCommandOutput {
            command: "test-command".to_owned(),
            status: Command::new("false").status().expect("false command runs"),
            stdout: String::new(),
            stdout_truncated: false,
            stderr: stderr.text,
            stderr_truncated: stderr.truncated,
        };

        assert!(!output.stderr_truncated);
        assert!(
            !output
                .failure_summary()
                .contains("stderr evidence was truncated")
        );
    }

    #[test]
    fn stderr_past_capture_limit_is_reported_as_truncated() {
        let input = vec![b'e'; super::COMMAND_OUTPUT_CAPTURE_LIMIT_BYTES + 1];
        let stderr = super::read_limited_pipe(input.as_slice()).expect("capture succeeds");
        let output = super::CapturedCommandOutput {
            command: "test-command".to_owned(),
            status: Command::new("false").status().expect("false command runs"),
            stdout: String::new(),
            stdout_truncated: false,
            stderr: stderr.text,
            stderr_truncated: stderr.truncated,
        };

        assert!(output.stderr_truncated);
        assert!(
            output
                .failure_summary()
                .contains("stderr evidence was truncated")
        );
    }
}
