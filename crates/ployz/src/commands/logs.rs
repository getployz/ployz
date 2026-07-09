//! Log evidence commands.

use clap::{Args, Subcommand};
use ployz_core::ids::{ContainerId, MachineId, NamespaceId, ServiceId};
use ployz_sdk_types::{LogsTailLines, LogsTailRequest, LogsTailResult, LogsTailTarget};

use crate::commands::{PloyzctlCliError, cli_error, invalid_value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsTailCommand {
    target: LogsTailTarget,
    tail_lines: Option<LogsTailLines>,
    since_unix_seconds: Option<u64>,
    pub follow: bool,
}

impl LogsTailCommand {
    #[must_use]
    pub fn into_request(self) -> LogsTailRequest {
        LogsTailRequest {
            target: self.target,
            tail_lines: self.tail_lines,
            since_unix_seconds: self.since_unix_seconds,
        }
    }

    #[must_use]
    pub fn request_after(&self, since_unix_seconds: u64) -> LogsTailRequest {
        LogsTailRequest {
            target: self.target.clone(),
            tail_lines: self.tail_lines,
            since_unix_seconds: Some(since_unix_seconds),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsTailOutput {
    result: LogsTailResult,
}

impl LogsTailOutput {
    #[must_use]
    pub const fn new(result: LogsTailResult) -> Self {
        Self { result }
    }

    #[must_use]
    pub fn render(&self) -> String {
        self.result.text.clone()
    }
}

pub(crate) fn logs_tail_command(parsed: LogsCli) -> Result<LogsTailCommand, PloyzctlCliError> {
    match parsed.command {
        Some(LogsSubcommandCli::Tail(command)) => logs_container_tail_command(command),
        None => logs_service_tail_command(parsed.service),
    }
}

fn logs_service_tail_command(parsed: LogsServiceCli) -> Result<LogsTailCommand, PloyzctlCliError> {
    let Some(service_id) = parsed.service_id else {
        return Err(cli_error(
            "logs requires <service_id> or the tail subcommand",
        ));
    };
    let namespace_id = parsed
        .namespace
        .map(NamespaceId::try_new)
        .transpose()
        .map_err(|error| invalid_value("--namespace", error))?
        .unwrap_or_else(|| NamespaceId::try_new("default").expect("default namespace is valid"));

    Ok(LogsTailCommand {
        target: LogsTailTarget::Service {
            namespace_id,
            service_id: ServiceId::try_new(service_id)
                .map_err(|error| invalid_value("<service_id>", error))?,
        },
        tail_lines: parse_tail(parsed.tail)?,
        since_unix_seconds: parse_since(parsed.since)?,
        follow: parsed.follow,
    })
}

fn logs_container_tail_command(
    parsed: LogsContainerTailCli,
) -> Result<LogsTailCommand, PloyzctlCliError> {
    Ok(LogsTailCommand {
        target: LogsTailTarget::Container {
            container_id: ContainerId::try_new(parsed.container_id)
                .map_err(|error| invalid_value("<container_id>", error))?,
            machine_id: parsed
                .machine
                .map(MachineId::try_new)
                .transpose()
                .map_err(|error| invalid_value("--machine", error))?,
        },
        tail_lines: parse_tail(parsed.tail)?,
        since_unix_seconds: parse_since(parsed.since)?,
        follow: parsed.follow,
    })
}

#[derive(Debug, Args)]
#[command(args_conflicts_with_subcommands = true)]
pub(crate) struct LogsCli {
    #[command(flatten)]
    service: LogsServiceCli,
    #[command(subcommand)]
    command: Option<LogsSubcommandCli>,
}

#[derive(Debug, Args)]
pub(crate) struct LogsServiceCli {
    service_id: Option<String>,
    #[arg(short = 'n', long = "namespace")]
    namespace: Option<String>,
    #[arg(long)]
    tail: Option<String>,
    #[arg(long)]
    since: Option<String>,
    #[arg(long)]
    follow: bool,
}

#[derive(Debug, Subcommand)]
pub(crate) enum LogsSubcommandCli {
    Tail(LogsContainerTailCli),
}

#[derive(Debug, Args)]
pub(crate) struct LogsContainerTailCli {
    container_id: String,
    #[arg(long)]
    machine: Option<String>,
    #[arg(long)]
    tail: Option<String>,
    #[arg(long)]
    since: Option<String>,
    #[arg(long)]
    follow: bool,
}

fn parse_tail(value: Option<String>) -> Result<Option<LogsTailLines>, PloyzctlCliError> {
    value
        .map(|value| {
            let parsed = value
                .parse::<u16>()
                .map_err(|error| invalid_value("--tail", error))?;
            LogsTailLines::try_new(parsed).map_err(|error| invalid_value("--tail", error))
        })
        .transpose()
}

fn parse_since(value: Option<String>) -> Result<Option<u64>, PloyzctlCliError> {
    value
        .map(|value| {
            let seconds =
                parse_duration_seconds(&value).map_err(|error| PloyzctlCliError::InvalidValue {
                    flag: "--since",
                    message: error,
                })?;
            current_unix_seconds().checked_sub(seconds).ok_or_else(|| {
                PloyzctlCliError::InvalidValue {
                    flag: "--since",
                    message: "duration is before the Unix epoch".to_owned(),
                }
            })
        })
        .transpose()
}

fn parse_duration_seconds(value: &str) -> Result<u64, String> {
    if value.is_empty() {
        return Err("duration is empty".to_owned());
    }
    let digits = value
        .find(|ch: char| !ch.is_ascii_digit())
        .unwrap_or(value.len());
    if digits == 0 {
        return Err("duration unit has no value".to_owned());
    }
    let amount = value[..digits]
        .parse::<u64>()
        .map_err(|error| format!("invalid duration: {error}"))?;
    match &value[digits..] {
        "" | "s" => Ok(amount),
        "m" => Ok(amount.saturating_mul(60)),
        "h" => Ok(amount.saturating_mul(60 * 60)),
        "d" => Ok(amount.saturating_mul(24 * 60 * 60)),
        unit => Err(format!("unsupported duration unit {unit:?}")),
    }
}

fn current_unix_seconds() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}
