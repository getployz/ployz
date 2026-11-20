//! Log tail request DTOs.

use serde::{Deserialize, Serialize};

use crate::{ContainerId, MachineLogService};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct LogsOptions {
    pub follow: bool,
    pub tail: i32,
    /// Unix nanoseconds; Docker sees whole seconds and ployzd drops the
    /// earlier output of that second.
    #[serde(default)]
    pub since_nanos: Option<i64>,
    #[serde(default)]
    pub until_unix_seconds: Option<i64>,
}

/// What a live log read follows: one Container's output, or one service on the Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveLogTarget {
    Container(ContainerId),
    Machine(MachineLogService),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TailLogsRequest {
    pub target: LiveLogTarget,
    pub options: LogsOptions,
}
