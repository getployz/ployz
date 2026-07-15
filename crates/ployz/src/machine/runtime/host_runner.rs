//! Runs the Host Runner first-machine install as a local subprocess with bounded
//! output capture.

use std::fmt;
use std::io::{Read, Write};
use std::process::{Command, ExitStatus, Stdio};
use std::thread::{self, JoinHandle};
use std::time::Duration;

use ployz_core::install::FirstMachineInstallSpec;
use wait_timeout::ChildExt;

const MAX_HOST_RUNNER_OUTPUT_BYTES: usize = 64 * 1024;

/// Captured stdout/stderr of a successful Host Runner install run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct HostRunnerInstallOutput {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

pub(crate) fn run_host_runner_first_machine_install(
    host_runner_binary: &str,
    host_runner_install: &FirstMachineInstallSpec,
    timeout: Duration,
) -> Result<HostRunnerInstallOutput, Box<LocalHostRunnerInstallError>> {
    let args = vec![
        "host".to_owned(),
        "install".to_owned(),
        "--spec".to_owned(),
        "-".to_owned(),
    ];
    let command = render_command(host_runner_binary, &args);
    let spec =
        serde_json::to_vec(host_runner_install).expect("first-machine install spec serializes");
    let mut child = Command::new(host_runner_binary)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|error| {
            Box::new(LocalHostRunnerInstallError::Spawn {
                command: command.clone(),
                message: error.to_string(),
            })
        })?;
    let Some(mut stdin) = child.stdin.take() else {
        return Err(Box::new(LocalHostRunnerInstallError::Stdin {
            command,
            message: "Host Runner stdin was not piped".to_owned(),
        }));
    };
    let Some(stdout) = child.stdout.take() else {
        return Err(Box::new(LocalHostRunnerInstallError::Stdout {
            command,
            message: "Host Runner stdout was not piped".to_owned(),
        }));
    };
    let Some(stderr) = child.stderr.take() else {
        return Err(Box::new(LocalHostRunnerInstallError::Stderr {
            command,
            message: "Host Runner stderr was not piped".to_owned(),
        }));
    };
    let stdout_reader = thread::spawn(move || read_limited_output(stdout));
    let stderr_reader = thread::spawn(move || read_limited_output(stderr));
    stdin.write_all(&spec).map_err(|error| {
        Box::new(LocalHostRunnerInstallError::Stdin {
            command: command.clone(),
            message: error.to_string(),
        })
    })?;
    drop(stdin);
    let status = match child.wait_timeout(timeout).map_err(|error| {
        Box::new(LocalHostRunnerInstallError::Wait {
            command: command.clone(),
            message: error.to_string(),
        })
    })? {
        Some(status) => status,
        None => {
            let _ = child.kill();
            let _ = child.wait();
            let stdout = join_output(stdout_reader);
            let stderr = join_output(stderr_reader);
            return Err(Box::new(LocalHostRunnerInstallError::Timeout {
                command,
                timeout,
                stdout,
                stderr,
            }));
        }
    };

    let stdout = join_output(stdout_reader);
    let stderr = join_output(stderr_reader);
    if status.success() {
        if stdout.read_error.is_some() || stderr.read_error.is_some() {
            return Err(Box::new(LocalHostRunnerInstallError::CaptureIncomplete {
                command,
                stdout,
                stderr,
            }));
        }
        Ok(HostRunnerInstallOutput {
            stdout: stdout.text,
            stderr: stderr.text,
        })
    } else {
        Err(Box::new(LocalHostRunnerInstallError::Failed {
            command,
            status,
            stdout,
            stderr,
        }))
    }
}

fn join_output(reader: JoinHandle<LimitedOutput>) -> LimitedOutput {
    reader.join().unwrap_or_else(|_error| LimitedOutput {
        text: String::new(),
        truncated: false,
        read_error: Some("output reader panicked".to_owned()),
    })
}

fn read_limited_output(mut reader: impl Read) -> LimitedOutput {
    let mut output = Vec::new();
    let mut truncated = false;
    let mut buffer = [0; 8192];

    loop {
        match reader.read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => {
                let remaining = MAX_HOST_RUNNER_OUTPUT_BYTES.saturating_sub(output.len());
                if remaining > 0 {
                    let keep = read.min(remaining);
                    let Some(bytes) = buffer.get(..keep) else {
                        return LimitedOutput {
                            text: String::from_utf8_lossy(&output).into_owned(),
                            truncated,
                            read_error: Some("output slice exceeded read buffer".to_owned()),
                        };
                    };
                    output.extend_from_slice(bytes);
                }
                if read > remaining {
                    truncated = true;
                }
            }
            Err(error) => {
                return LimitedOutput {
                    text: String::from_utf8_lossy(&output).into_owned(),
                    truncated,
                    read_error: Some(error.to_string()),
                };
            }
        }
    }

    LimitedOutput {
        text: String::from_utf8_lossy(&output).into_owned(),
        truncated,
        read_error: None,
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LimitedOutput {
    text: String,
    truncated: bool,
    read_error: Option<String>,
}

impl LimitedOutput {
    fn summary(&self, label: &str) -> Option<String> {
        let mut summary = String::new();
        let trimmed = self.text.trim();
        if !trimmed.is_empty() {
            summary.push_str(label);
            summary.push_str(": ");
            summary.push_str(trimmed);
        }
        if self.truncated {
            if !summary.is_empty() {
                summary.push_str("; ");
            }
            summary.push_str(label);
            summary.push_str(" truncated");
        }
        if let Some(read_error) = &self.read_error {
            if !summary.is_empty() {
                summary.push_str("; ");
            }
            summary.push_str(label);
            summary.push_str(" read failed: ");
            summary.push_str(read_error);
        }
        if summary.is_empty() {
            None
        } else {
            Some(summary)
        }
    }
}

fn render_command(program: &str, args: &[String]) -> String {
    std::iter::once(program.to_owned())
        .chain(args.iter().cloned())
        .collect::<Vec<_>>()
        .join(" ")
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LocalHostRunnerInstallError {
    Stdout {
        command: String,
        message: String,
    },
    Stderr {
        command: String,
        message: String,
    },
    Spawn {
        command: String,
        message: String,
    },
    Stdin {
        command: String,
        message: String,
    },
    Wait {
        command: String,
        message: String,
    },
    Timeout {
        command: String,
        timeout: Duration,
        stdout: LimitedOutput,
        stderr: LimitedOutput,
    },
    CaptureIncomplete {
        command: String,
        stdout: LimitedOutput,
        stderr: LimitedOutput,
    },
    Failed {
        command: String,
        status: ExitStatus,
        stdout: LimitedOutput,
        stderr: LimitedOutput,
    },
}

impl fmt::Display for LocalHostRunnerInstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Stdout { command, message } => {
                write!(formatter, "{command} stdout capture failed: {message}")
            }
            Self::Stderr { command, message } => {
                write!(formatter, "{command} stderr capture failed: {message}")
            }
            Self::Spawn { command, message } => {
                write!(formatter, "{command} failed to start: {message}")
            }
            Self::Stdin { command, message } => {
                write!(formatter, "{command} failed while writing stdin: {message}")
            }
            Self::Wait { command, message } => {
                write!(formatter, "{command} failed while waiting: {message}")
            }
            Self::Timeout {
                command,
                timeout,
                stdout,
                stderr,
            } => {
                write!(
                    formatter,
                    "{command} timed out after {}s",
                    timeout.as_secs()
                )?;
                write_output_summary(formatter, stdout, stderr)
            }
            Self::CaptureIncomplete {
                command,
                stdout,
                stderr,
            } => {
                write!(formatter, "{command} output capture was incomplete")?;
                write_output_summary(formatter, stdout, stderr)
            }
            Self::Failed {
                command,
                status,
                stdout,
                stderr,
            } => {
                write!(formatter, "{command} exited with status {status}")?;
                write_output_summary(formatter, stdout, stderr)
            }
        }
    }
}

impl std::error::Error for LocalHostRunnerInstallError {}

fn write_output_summary(
    formatter: &mut fmt::Formatter<'_>,
    stdout: &LimitedOutput,
    stderr: &LimitedOutput,
) -> fmt::Result {
    for summary in [stdout.summary("stdout"), stderr.summary("stderr")]
        .into_iter()
        .flatten()
    {
        write!(formatter, "; {summary}")?;
    }
    Ok(())
}
