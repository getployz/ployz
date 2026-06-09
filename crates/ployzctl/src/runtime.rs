//! Runtime execution for parsed CLI commands.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use crate::api_client::{OperationApiClient, OperationApiClientError};
use crate::commands::init::FirstNodeInitMode;
use crate::commands::{PloyzctlCommand, USAGE};
use ployz_nats::connect::{
    NatsClientUrl, NatsClientUrlError, NatsConnectError, connect_with_timeout,
};
use ployz_sdk_types::{BackupCreateError, DeploySubmitError, MachineAddError, OpsWatchError};

pub const PLOYZ_NATS_URL_ENV: &str = "PLOYZ_NATS_URL";
pub const DEFAULT_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
pub const DEFAULT_KEEPER_INSTALL_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_KEEPER_OUTPUT_BYTES: usize = 64 * 1024;

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PloyzctlRuntimeConfig {
    pub nats_url: Option<String>,
    pub nats_connect_timeout: Option<Duration>,
    pub keeper_install_timeout: Option<Duration>,
}

impl PloyzctlRuntimeConfig {
    #[must_use]
    pub fn from_env() -> Self {
        Self {
            nats_url: std::env::var(PLOYZ_NATS_URL_ENV)
                .ok()
                .filter(|value| !value.trim().is_empty()),
            nats_connect_timeout: None,
            keeper_install_timeout: None,
        }
    }

    #[must_use]
    pub fn with_nats_url(mut self, nats_url: Option<String>) -> Self {
        if nats_url.is_some() {
            self.nats_url = nats_url;
        }
        self
    }

    #[must_use]
    pub fn nats_connect_timeout(&self) -> Duration {
        self.nats_connect_timeout
            .unwrap_or(DEFAULT_NATS_CONNECT_TIMEOUT)
    }

    #[must_use]
    pub fn keeper_install_timeout(&self) -> Duration {
        self.keeper_install_timeout
            .unwrap_or(DEFAULT_KEEPER_INSTALL_TIMEOUT)
    }
}

pub async fn execute_command(
    command: PloyzctlCommand,
    config: &PloyzctlRuntimeConfig,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    match command {
        PloyzctlCommand::Help => Ok(PloyzctlExecutionOutput::stdout(format!("{USAGE}\n"))),
        PloyzctlCommand::Deploy(command) => {
            let api = operation_api_client(config).await?;
            let request = command.into_request();
            let accepted = api
                .deploy_submit(&request)
                .await
                .map_err(|source| PloyzctlExecutionError::DeploySubmitApi { source })?;

            Ok(PloyzctlExecutionOutput::stdout(
                crate::commands::deploy::DetachedDeployOutput::from_accepted(accepted).render(),
            ))
        }
        PloyzctlCommand::BackupCreate(command) => {
            let api = operation_api_client(config).await?;
            let request = command.into_request();
            let accepted = api
                .backup_create(&request)
                .await
                .map_err(|source| PloyzctlExecutionError::BackupCreateApi { source })?;

            Ok(PloyzctlExecutionOutput::stdout(
                crate::commands::backup::BackupCreateOutput::from_accepted(accepted).render(),
            ))
        }
        PloyzctlCommand::BackupRestorePlan(_) => Ok(PloyzctlExecutionOutput::stdout(
            crate::commands::backup::BackupRestorePlanOutput::single_core().render(),
        )),
        PloyzctlCommand::Init(command) => match &command.mode {
            FirstNodeInitMode::RunKeeperInstall {
                keeper_install,
                keeper_binary,
            } => run_keeper_first_node_install(
                keeper_binary,
                keeper_install,
                config.keeper_install_timeout(),
            ),
            FirstNodeInitMode::Summary { .. } | FirstNodeInitMode::EmitKeeperInstall(_) => {
                Ok(PloyzctlExecutionOutput::stdout(command.render()))
            }
        },
        PloyzctlCommand::InitJoinTemplate(command) => {
            Ok(PloyzctlExecutionOutput::stdout(command.render_json()))
        }
        PloyzctlCommand::MachineAdd(command) => {
            let api = operation_api_client(config).await?;
            let request = command.into_request();
            let accepted = api
                .machine_add(&request)
                .await
                .map_err(|source| PloyzctlExecutionError::MachineAddApi { source })?;

            Ok(PloyzctlExecutionOutput::stdout(
                crate::commands::machine::MachineAddOutput::from_accepted(accepted)
                    .with_nats_url(config.nats_url.clone())
                    .render(),
            ))
        }
        PloyzctlCommand::OpsWatch(command) => {
            let api = operation_api_client(config).await?;
            let request = command.into_request();
            let page = api
                .ops_watch(&request)
                .await
                .map_err(|source| PloyzctlExecutionError::OpsWatchApi { source })?;

            Ok(PloyzctlExecutionOutput::stdout(
                crate::commands::ops::WatchOutput {
                    events: page.events,
                }
                .render(),
            ))
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PloyzctlExecutionOutput {
    pub stdout: String,
    pub stderr: String,
}

impl PloyzctlExecutionOutput {
    #[must_use]
    pub fn stdout(stdout: String) -> Self {
        Self {
            stdout,
            stderr: String::new(),
        }
    }
}

fn run_keeper_first_node_install(
    keeper_binary: &str,
    keeper_install: &ployz_core::install::KeeperFirstNodeInstall,
    timeout: Duration,
) -> Result<PloyzctlExecutionOutput, PloyzctlExecutionError> {
    let args = keeper_install.command_args();
    let mut capture =
        OutputCapture::new().map_err(|message| PloyzctlExecutionError::KeeperFirstNodeInstall {
            source: Box::new(LocalKeeperInstallError::CaptureSetup {
                command: render_command(keeper_binary, &args),
                message,
            }),
        })?;
    let mut child = Command::new(keeper_binary)
        .args(&args)
        .stdout(capture.stdout_stdio().map_err(|message| {
            PloyzctlExecutionError::KeeperFirstNodeInstall {
                source: Box::new(LocalKeeperInstallError::CaptureSetup {
                    command: render_command(keeper_binary, &args),
                    message,
                }),
            }
        })?)
        .stderr(capture.stderr_stdio().map_err(|message| {
            PloyzctlExecutionError::KeeperFirstNodeInstall {
                source: Box::new(LocalKeeperInstallError::CaptureSetup {
                    command: render_command(keeper_binary, &args),
                    message,
                }),
            }
        })?)
        .spawn()
        .map_err(|error| PloyzctlExecutionError::KeeperFirstNodeInstall {
            source: Box::new(LocalKeeperInstallError::Spawn {
                command: render_command(keeper_binary, &args),
                message: error.to_string(),
            }),
        })?;
    let started_at = Instant::now();

    let status = loop {
        match child
            .try_wait()
            .map_err(|error| PloyzctlExecutionError::KeeperFirstNodeInstall {
                source: Box::new(LocalKeeperInstallError::Wait {
                    command: render_command(keeper_binary, &args),
                    message: error.to_string(),
                }),
            })? {
            Some(status) => break status,
            None if started_at.elapsed() >= timeout => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(PloyzctlExecutionError::KeeperFirstNodeInstall {
                    source: Box::new(LocalKeeperInstallError::Timeout {
                        command: render_command(keeper_binary, &args),
                        timeout,
                        stdout: capture.stdout_output(),
                        stderr: capture.stderr_output(),
                    }),
                });
            }
            None => thread::sleep(Duration::from_millis(50)),
        }
    };

    let stdout = capture.stdout_output();
    let stderr = capture.stderr_output();
    if status.success() {
        if stdout.read_error.is_some() || stderr.read_error.is_some() {
            return Err(PloyzctlExecutionError::KeeperFirstNodeInstall {
                source: Box::new(LocalKeeperInstallError::CaptureIncomplete {
                    command: render_command(keeper_binary, &args),
                    stdout,
                    stderr,
                }),
            });
        }
        Ok(PloyzctlExecutionOutput {
            stdout: stdout.text,
            stderr: stderr.text,
        })
    } else {
        Err(PloyzctlExecutionError::KeeperFirstNodeInstall {
            source: Box::new(LocalKeeperInstallError::Failed {
                command: render_command(keeper_binary, &args),
                status,
                stdout,
                stderr,
            }),
        })
    }
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

async fn operation_api_client(
    config: &PloyzctlRuntimeConfig,
) -> Result<OperationApiClient, PloyzctlExecutionError> {
    let nats_url = config.nats_url.clone();
    let Some(nats_url) = nats_url else {
        return Err(PloyzctlExecutionError::MissingNatsUrl);
    };

    let nats_url =
        NatsClientUrl::try_new(nats_url).map_err(PloyzctlExecutionError::InvalidNatsUrl)?;

    connect_with_timeout(&nats_url, config.nats_connect_timeout())
        .await
        .map(OperationApiClient::new)
        .map_err(PloyzctlExecutionError::NatsConnect)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PloyzctlExecutionError {
    MissingNatsUrl,
    InvalidNatsUrl(NatsClientUrlError),
    NatsConnect(NatsConnectError),
    KeeperFirstNodeInstall {
        source: Box<LocalKeeperInstallError>,
    },
    DeploySubmitApi {
        source: OperationApiClientError<DeploySubmitError>,
    },
    BackupCreateApi {
        source: OperationApiClientError<BackupCreateError>,
    },
    MachineAddApi {
        source: OperationApiClientError<MachineAddError>,
    },
    OpsWatchApi {
        source: OperationApiClientError<OpsWatchError>,
    },
}

impl fmt::Display for PloyzctlExecutionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingNatsUrl => write!(formatter, "--nats or {PLOYZ_NATS_URL_ENV} is required"),
            Self::InvalidNatsUrl(error) => {
                write!(
                    formatter,
                    "--nats or {PLOYZ_NATS_URL_ENV} is invalid: {error:?}"
                )
            }
            Self::NatsConnect(error) => write!(formatter, "{error}"),
            Self::KeeperFirstNodeInstall { source } => write!(formatter, "{source}"),
            Self::DeploySubmitApi { source } => write!(formatter, "{source}"),
            Self::BackupCreateApi { source } => write!(formatter, "{source}"),
            Self::MachineAddApi { source } => write!(formatter, "{source}"),
            Self::OpsWatchApi { source } => write!(formatter, "{source}"),
        }
    }
}

impl std::error::Error for PloyzctlExecutionError {}

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
