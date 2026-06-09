//! Log evidence commands.

use ployz_core::ids::{ContainerId, NodeId};
use ployz_sdk_types::{LogsTailLines, LogsTailRequest, LogsTailResult};

use crate::commands::{ArgCursor, PloyzctlCliError, flag_value, invalid_value, set_once};

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
    let [container_id, tail @ ..] = rest else {
        return Err(PloyzctlCliError::MissingRequiredArgument {
            flag: "<container_id>",
        });
    };
    let container_id = ContainerId::try_new(flag_value("<container_id>", container_id)?)
        .map_err(|error| invalid_value("<container_id>", error))?;
    let mut cursor = ArgCursor::new(tail);
    let mut node_id = None;
    let mut tail_lines = None;

    while !cursor.is_empty() {
        if let Some(value) = cursor.take_value("--node")? {
            let node = NodeId::try_new(value).map_err(|error| invalid_value("--node", error))?;
            set_once(&mut node_id, node, "--node")?;
            continue;
        }
        if let Some(value) = cursor.take_value("--tail")? {
            let parsed = value
                .parse::<u16>()
                .map_err(|error| invalid_value("--tail", error))?;
            let lines =
                LogsTailLines::try_new(parsed).map_err(|error| invalid_value("--tail", error))?;
            set_once(&mut tail_lines, lines, "--tail")?;
            continue;
        }
        return Err(cursor.unexpected());
    }

    Ok(LogsTailCommand {
        container_id,
        node_id,
        tail_lines,
    })
}
