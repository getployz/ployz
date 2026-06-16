//! Reusable SSH layer for remote machine bootstrap.
//!
//! `machine init` and `machine add` both need the same primitives: parse a
//! `user@host` target, run a bounded remote command with phase-labeled
//! failure evidence, and read the remote hostname that becomes the default
//! machine identity. Remote commands stay POSIX-shaped; every failure names
//! the phase and carries captured stdout/stderr.

use std::fmt;
use std::io::Read;
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

pub const DEFAULT_SSH_COMMAND_TIMEOUT: Duration = Duration::from_secs(60);
pub const SSH_CONNECT_TIMEOUT_SECS: u32 = 10;
const MAX_SSH_OUTPUT_BYTES: usize = 64 * 1024;
const WAIT_POLL_INTERVAL: Duration = Duration::from_millis(50);

/// A validated `user@host` SSH destination.
///
/// Validation is deliberately conservative: it rejects anything `ssh` could
/// parse as an option (leading `-`) and anything outside plain user/host
/// characters, so the destination can be passed as a single argv entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshTarget {
    user: String,
    host: String,
}

impl SshTarget {
    pub fn parse(raw: &str) -> Result<Self, SshTargetParseError> {
        let Some((user, host)) = raw.split_once('@') else {
            return Err(SshTargetParseError::MissingUser {
                target: raw.to_owned(),
            });
        };
        if user.is_empty() {
            return Err(SshTargetParseError::EmptyUser {
                target: raw.to_owned(),
            });
        }
        if host.is_empty() {
            return Err(SshTargetParseError::EmptyHost {
                target: raw.to_owned(),
            });
        }
        if !is_valid_user(user) {
            return Err(SshTargetParseError::InvalidUser {
                target: raw.to_owned(),
            });
        }
        if !is_valid_host(host) {
            return Err(SshTargetParseError::InvalidHost {
                target: raw.to_owned(),
            });
        }
        Ok(Self {
            user: user.to_owned(),
            host: host.to_owned(),
        })
    }

    #[must_use]
    pub fn user(&self) -> &str {
        &self.user
    }

    #[must_use]
    pub fn host(&self) -> &str {
        &self.host
    }

    /// The `user@host` string handed to `ssh` as the destination argument.
    #[must_use]
    pub fn destination(&self) -> String {
        format!("{}@{}", self.user, self.host)
    }
}

fn is_valid_user(user: &str) -> bool {
    !user.starts_with('-')
        && user
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-'))
}

fn is_valid_host(host: &str) -> bool {
    !host.starts_with('-')
        && host
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b':'))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshTargetParseError {
    MissingUser { target: String },
    EmptyUser { target: String },
    EmptyHost { target: String },
    InvalidUser { target: String },
    InvalidHost { target: String },
}

impl fmt::Display for SshTargetParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingUser { target } => write!(
                formatter,
                "ssh target \"{target}\" must look like user@host (for example root@203.0.113.10)"
            ),
            Self::EmptyUser { target } => {
                write!(
                    formatter,
                    "ssh target \"{target}\" has an empty user before '@'"
                )
            }
            Self::EmptyHost { target } => {
                write!(
                    formatter,
                    "ssh target \"{target}\" has an empty host after '@'"
                )
            }
            Self::InvalidUser { target } => write!(
                formatter,
                "ssh target \"{target}\" has an unsupported user (letters, digits, '.', '_', and '-' only)"
            ),
            Self::InvalidHost { target } => write!(
                formatter,
                "ssh target \"{target}\" has an unsupported host (letters, digits, '.', ':', and '-' only, not starting with '-')"
            ),
        }
    }
}

impl std::error::Error for SshTargetParseError {}

/// Remote bootstrap phases, named in every SSH failure so the operator knows
/// where the flow stopped (R14).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshPhase {
    ReadHostname,
    RunInstaller,
}

impl SshPhase {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ReadHostname => "read-hostname",
            Self::RunInstaller => "run-installer",
        }
    }
}

/// Runs remote commands over the system `ssh` with a hard per-command
/// timeout and bounded output capture.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshClient {
    program: PathBuf,
    command_timeout: Duration,
}

impl SshClient {
    /// The production client: the `ssh` found on `PATH`.
    #[must_use]
    pub fn system(command_timeout: Duration) -> Self {
        Self {
            program: PathBuf::from("ssh"),
            command_timeout,
        }
    }

    /// Test seam: run a stand-in program instead of the system `ssh`. The
    /// program receives the same argv shape (`-o` options, destination,
    /// remote command).
    #[must_use]
    pub fn with_program(program: PathBuf, command_timeout: Duration) -> Self {
        Self {
            program,
            command_timeout,
        }
    }

    /// Runs one bounded remote command. Non-zero exit, timeout, and spawn
    /// failures all surface as phase-labeled errors with captured output.
    pub fn run(
        &self,
        target: &SshTarget,
        phase: SshPhase,
        remote_command: &str,
    ) -> Result<SshCommandOutput, Box<SshCommandError>> {
        let destination = target.destination();
        let rendered = format!("{} {destination} {remote_command}", self.program.display());

        let mut child = Command::new(&self.program)
            .arg("-o")
            .arg("BatchMode=yes")
            .arg("-o")
            .arg(format!("ConnectTimeout={SSH_CONNECT_TIMEOUT_SECS}"))
            .arg(&destination)
            .arg(remote_command)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|error| {
                Box::new(SshCommandError::Spawn {
                    phase,
                    command: rendered.clone(),
                    message: error.to_string(),
                })
            })?;

        let Some(stdout_pipe) = child.stdout.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Box::new(SshCommandError::CaptureSetup {
                phase,
                command: rendered,
                message: "child stdout was not piped".to_owned(),
            }));
        };
        let Some(stderr_pipe) = child.stderr.take() else {
            let _ = child.kill();
            let _ = child.wait();
            return Err(Box::new(SshCommandError::CaptureSetup {
                phase,
                command: rendered,
                message: "child stderr was not piped".to_owned(),
            }));
        };
        let stdout_reader = spawn_bounded_reader(stdout_pipe);
        let stderr_reader = spawn_bounded_reader(stderr_pipe);

        let started_at = Instant::now();
        let status = loop {
            match child.try_wait() {
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Box::new(SshCommandError::Wait {
                        phase,
                        command: rendered,
                        message: error.to_string(),
                    }));
                }
                Ok(Some(status)) => break status,
                Ok(None) if started_at.elapsed() >= self.command_timeout => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(Box::new(SshCommandError::Timeout {
                        phase,
                        command: rendered,
                        timeout: self.command_timeout,
                        stdout: join_bounded_reader(stdout_reader),
                        stderr: join_bounded_reader(stderr_reader),
                    }));
                }
                Ok(None) => thread::sleep(WAIT_POLL_INTERVAL),
            }
        };

        let stdout = join_bounded_reader(stdout_reader);
        let stderr = join_bounded_reader(stderr_reader);
        if status.success() {
            Ok(SshCommandOutput { stdout, stderr })
        } else {
            Err(Box::new(SshCommandError::Failed {
                phase,
                command: rendered,
                status,
                stdout,
                stderr,
            }))
        }
    }

    /// Reads the remote system hostname (the default machine identity).
    /// Returns the first line of `hostname`, trimmed.
    pub fn read_remote_hostname(&self, target: &SshTarget) -> Result<String, Box<SshCommandError>> {
        let output = self.run(target, SshPhase::ReadHostname, "hostname")?;
        let hostname = output
            .stdout
            .text
            .lines()
            .next()
            .map(str::trim)
            .unwrap_or_default();
        if hostname.is_empty() {
            return Err(Box::new(SshCommandError::EmptyRemoteHostname {
                destination: target.destination(),
            }));
        }
        Ok(hostname.to_owned())
    }
}

/// Captured stream output, capped at [`MAX_SSH_OUTPUT_BYTES`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoundedOutput {
    pub text: String,
    pub truncated: bool,
}

impl BoundedOutput {
    fn summary(&self, label: &str) -> Option<String> {
        let trimmed = self.text.trim();
        match (trimmed.is_empty(), self.truncated) {
            (true, false) => None,
            (true, true) => Some(format!("{label} truncated")),
            (false, false) => Some(format!("{label}: {trimmed}")),
            (false, true) => Some(format!("{label} (truncated): {trimmed}")),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshCommandOutput {
    pub stdout: BoundedOutput,
    pub stderr: BoundedOutput,
}

fn spawn_bounded_reader(pipe: impl Read + Send + 'static) -> JoinHandle<BoundedOutput> {
    thread::spawn(move || read_bounded(pipe))
}

fn join_bounded_reader(handle: JoinHandle<BoundedOutput>) -> BoundedOutput {
    // `read_bounded` never panics; an interrupted reader degrades to empty
    // capture rather than poisoning the error path.
    handle.join().unwrap_or_default()
}

fn read_bounded(mut pipe: impl Read) -> BoundedOutput {
    let mut collected = Vec::new();
    let mut truncated = false;
    let mut buffer = [0_u8; 8192];
    loop {
        match pipe.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let remaining = MAX_SSH_OUTPUT_BYTES.saturating_sub(collected.len());
                let keep = read.min(remaining);
                if let Some(bytes) = buffer.get(..keep) {
                    collected.extend_from_slice(bytes);
                }
                if read > remaining {
                    truncated = true;
                }
            }
            // The exit status decides success; a broken pipe just ends
            // capture.
            Err(_) => break,
        }
    }
    BoundedOutput {
        text: String::from_utf8_lossy(&collected).into_owned(),
        truncated,
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SshCommandError {
    Spawn {
        phase: SshPhase,
        command: String,
        message: String,
    },
    CaptureSetup {
        phase: SshPhase,
        command: String,
        message: String,
    },
    Wait {
        phase: SshPhase,
        command: String,
        message: String,
    },
    Timeout {
        phase: SshPhase,
        command: String,
        timeout: Duration,
        stdout: BoundedOutput,
        stderr: BoundedOutput,
    },
    Failed {
        phase: SshPhase,
        command: String,
        status: ExitStatus,
        stdout: BoundedOutput,
        stderr: BoundedOutput,
    },
    EmptyRemoteHostname {
        destination: String,
    },
}

impl fmt::Display for SshCommandError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Spawn {
                phase,
                command,
                message,
            } => write!(
                formatter,
                "ssh {} phase: {command} failed to start: {message}",
                phase.label()
            ),
            Self::CaptureSetup {
                phase,
                command,
                message,
            } => write!(
                formatter,
                "ssh {} phase: {command} output capture failed: {message}",
                phase.label()
            ),
            Self::Wait {
                phase,
                command,
                message,
            } => write!(
                formatter,
                "ssh {} phase: {command} failed while waiting: {message}",
                phase.label()
            ),
            Self::Timeout {
                phase,
                command,
                timeout,
                stdout,
                stderr,
            } => {
                write!(
                    formatter,
                    "ssh {} phase: {command} timed out after {}s",
                    phase.label(),
                    timeout.as_secs()
                )?;
                write_output_summary(formatter, stdout, stderr)
            }
            Self::Failed {
                phase,
                command,
                status,
                stdout,
                stderr,
            } => {
                write!(
                    formatter,
                    "ssh {} phase: {command} exited with status {status}",
                    phase.label()
                )?;
                write_output_summary(formatter, stdout, stderr)
            }
            Self::EmptyRemoteHostname { destination } => write!(
                formatter,
                "ssh {} phase: {destination} reported an empty hostname",
                SshPhase::ReadHostname.label()
            ),
        }
    }
}

impl std::error::Error for SshCommandError {}

fn write_output_summary(
    formatter: &mut fmt::Formatter<'_>,
    stdout: &BoundedOutput,
    stderr: &BoundedOutput,
) -> fmt::Result {
    for summary in [stdout.summary("stdout"), stderr.summary("stderr")]
        .into_iter()
        .flatten()
    {
        write!(formatter, "; {summary}")?;
    }
    Ok(())
}
