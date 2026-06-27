//! Runs the keeper first-machine install as a local subprocess with bounded
//! output capture.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom, Write};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use ployz_core::install::FirstMachineInstallSpec;

const MAX_KEEPER_OUTPUT_BYTES: usize = 64 * 1024;

/// Captured stdout/stderr of a successful keeper install run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct KeeperInstallOutput {
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

pub(crate) fn run_keeper_first_machine_install(
    keeper_binary: &str,
    keeper_install: &FirstMachineInstallSpec,
    timeout: Duration,
) -> Result<KeeperInstallOutput, Box<LocalKeeperInstallError>> {
    let args = vec![
        "first-machine-install".to_owned(),
        "--spec".to_owned(),
        "-".to_owned(),
    ];
    let command = render_command(keeper_binary, &args);
    let spec = serde_json::to_vec(keeper_install).expect("first-machine install spec serializes");
    let mut capture = OutputCapture::new().map_err(|message| capture_setup(&command, message))?;
    let stdout_stdio = capture
        .stdout_stdio()
        .map_err(|message| capture_setup(&command, message))?;
    let stderr_stdio = capture
        .stderr_stdio()
        .map_err(|message| capture_setup(&command, message))?;
    let mut child = Command::new(keeper_binary)
        .args(&args)
        .stdin(Stdio::piped())
        .stdout(stdout_stdio)
        .stderr(stderr_stdio)
        .spawn()
        .map_err(|error| {
            Box::new(LocalKeeperInstallError::Spawn {
                command: command.clone(),
                message: error.to_string(),
            })
        })?;
    let Some(mut stdin) = child.stdin.take() else {
        return Err(Box::new(LocalKeeperInstallError::Stdin {
            command,
            message: "keeper stdin was not piped".to_owned(),
        }));
    };
    stdin.write_all(&spec).map_err(|error| {
        Box::new(LocalKeeperInstallError::Stdin {
            command: command.clone(),
            message: error.to_string(),
        })
    })?;
    drop(stdin);
    let started_at = Instant::now();

    let status = loop {
        match child.try_wait().map_err(|error| {
            Box::new(LocalKeeperInstallError::Wait {
                command: command.clone(),
                message: error.to_string(),
            })
        })? {
            Some(status) => break status,
            None if started_at.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(Box::new(LocalKeeperInstallError::Timeout {
                    command,
                    timeout,
                    stdout: capture.stdout_output(),
                    stderr: capture.stderr_output(),
                }));
            }
            None => thread::sleep(Duration::from_millis(50)),
        }
    };

    let stdout = capture.stdout_output();
    let stderr = capture.stderr_output();
    if status.success() {
        if stdout.read_error.is_some() || stderr.read_error.is_some() {
            return Err(Box::new(LocalKeeperInstallError::CaptureIncomplete {
                command,
                stdout,
                stderr,
            }));
        }
        Ok(KeeperInstallOutput {
            stdout: stdout.text,
            stderr: stderr.text,
        })
    } else {
        Err(Box::new(LocalKeeperInstallError::Failed {
            command,
            status,
            stdout,
            stderr,
        }))
    }
}

fn capture_setup(command: &str, message: String) -> Box<LocalKeeperInstallError> {
    Box::new(LocalKeeperInstallError::CaptureSetup {
        command: command.to_owned(),
        message,
    })
}

struct OutputCapture {
    stdout: CapturedFile,
    stderr: CapturedFile,
}

impl OutputCapture {
    fn new() -> Result<Self, String> {
        Ok(Self {
            stdout: CapturedFile::new("stdout")?,
            stderr: CapturedFile::new("stderr")?,
        })
    }

    fn stdout_stdio(&self) -> Result<Stdio, String> {
        self.stdout.stdio()
    }

    fn stderr_stdio(&self) -> Result<Stdio, String> {
        self.stderr.stdio()
    }

    fn stdout_output(&mut self) -> LimitedOutput {
        self.stdout.output()
    }

    fn stderr_output(&mut self) -> LimitedOutput {
        self.stderr.output()
    }
}

struct CapturedFile {
    path: PathBuf,
    file: File,
}

impl CapturedFile {
    fn new(label: &str) -> Result<Self, String> {
        for attempt in 0..16 {
            let path = capture_path(label, attempt)?;
            match OpenOptions::new()
                .read(true)
                .write(true)
                .create_new(true)
                .open(&path)
            {
                Ok(file) => return Ok(Self { path, file }),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(format!(
                        "failed to create keeper {label} capture file {}: {error}",
                        path.display()
                    ));
                }
            }
        }

        Err(format!(
            "failed to create unique keeper {label} capture file"
        ))
    }

    fn stdio(&self) -> Result<Stdio, String> {
        self.file.try_clone().map(Stdio::from).map_err(|error| {
            format!(
                "failed to clone capture file {}: {error}",
                self.path.display()
            )
        })
    }

    fn output(&mut self) -> LimitedOutput {
        let mut output = Vec::new();
        let mut truncated = false;
        let mut buffer = [0; 8192];

        if let Err(error) = self.file.seek(SeekFrom::Start(0)) {
            return LimitedOutput {
                text: String::new(),
                truncated: false,
                read_error: Some(format!(
                    "failed to seek capture file {}: {error}",
                    self.path.display()
                )),
            };
        }

        loop {
            match self.file.read(&mut buffer) {
                Ok(0) => break,
                Ok(read) => {
                    let remaining = MAX_KEEPER_OUTPUT_BYTES.saturating_sub(output.len());
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
                        read_error: Some(format!(
                            "failed to read capture file {}: {error}",
                            self.path.display()
                        )),
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
}

impl Drop for CapturedFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

fn capture_path(label: &str, attempt: u8) -> Result<PathBuf, String> {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| format!("system clock is before unix epoch: {error}"))?
        .as_nanos();
    Ok(std::env::temp_dir().join(format!(
        "ployzctl-keeper-{}-{nanos}-{attempt}-{label}.log",
        std::process::id()
    )))
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
pub enum LocalKeeperInstallError {
    CaptureSetup {
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

impl fmt::Display for LocalKeeperInstallError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::CaptureSetup { command, message } => {
                write!(formatter, "{command} output capture failed: {message}")
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

impl std::error::Error for LocalKeeperInstallError {}

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
