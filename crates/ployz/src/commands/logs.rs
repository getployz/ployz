//! Log evidence commands.

use clap::Args;
use ployz_core::ids::{ContainerId, MachineId};
use ployz_sdk_types::{LogsTailLines, LogsTailRequest, LogsTailResult};

use crate::commands::{PloyzctlCliError, invalid_value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsTailCommand {
    container_id: ContainerId,
    machine_id: Option<MachineId>,
    tail_lines: Option<LogsTailLines>,
}

impl LogsTailCommand {
    #[must_use]
    pub fn into_request(self) -> LogsTailRequest {
        LogsTailRequest {
            container_id: self.container_id,
            machine_id: self.machine_id,
            tail_lines: self.tail_lines,
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

pub(crate) fn logs_tail_command(parsed: LogsTailCli) -> Result<LogsTailCommand, PloyzctlCliError> {
    Ok(LogsTailCommand {
        container_id: ContainerId::try_new(parsed.container_id)
            .map_err(|error| invalid_value("<container_id>", error))?,
        machine_id: parsed
            .machine
            .map(MachineId::try_new)
            .transpose()
            .map_err(|error| invalid_value("--machine", error))?,
        tail_lines: parsed
            .tail
            .map(|value| {
                let parsed = value
                    .parse::<u16>()
                    .map_err(|error| invalid_value("--tail", error))?;
                LogsTailLines::try_new(parsed).map_err(|error| invalid_value("--tail", error))
            })
            .transpose()?,
    })
}

#[derive(Debug, Args)]
pub(crate) struct LogsTailCli {
    container_id: String,
    #[arg(long)]
    machine: Option<String>,
    #[arg(long)]
    tail: Option<String>,
}
