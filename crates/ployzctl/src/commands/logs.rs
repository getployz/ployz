//! Log evidence commands.

use clap::Args;
use ployz_core::ids::{ContainerId, NodeId};
use ployz_sdk_types::{LogsTailLines, LogsTailRequest, LogsTailResult};

use crate::commands::{PloyzctlCliError, invalid_value};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LogsTailCommand {
    container_id: ContainerId,
    node_id: Option<NodeId>,
    tail_lines: Option<LogsTailLines>,
}

impl LogsTailCommand {
    #[must_use]
    pub fn into_request(self) -> LogsTailRequest {
        LogsTailRequest {
            container_id: self.container_id,
            node_id: self.node_id,
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
        node_id: parsed
            .node
            .map(NodeId::try_new)
            .transpose()
            .map_err(|error| invalid_value("--node", error))?,
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
    node: Option<String>,
    #[arg(long)]
    tail: Option<String>,
}
