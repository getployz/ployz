//! Log evidence commands.

use clap::Parser;
use ployz_core::ids::{ContainerId, NodeId};
use ployz_sdk_types::{LogsTailLines, LogsTailRequest, LogsTailResult};

use crate::commands::{PloyzctlCliError, clap_error, invalid_value};

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

pub fn parse_logs_tail_command(rest: &[String]) -> Result<LogsTailCommand, PloyzctlCliError> {
    let parsed =
        LogsTailCli::try_parse_from(std::iter::once("logs".to_owned()).chain(rest.iter().cloned()))
            .map_err(clap_error)?;

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

#[derive(Debug, Parser)]
#[command(name = "logs")]
struct LogsTailCli {
    container_id: String,
    #[arg(long)]
    node: Option<String>,
    #[arg(long)]
    tail: Option<String>,
}
