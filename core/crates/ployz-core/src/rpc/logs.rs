//! Log tail and history request DTOs.

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

/// The most rows one history page carries.
pub const LOG_HISTORY_PAGE_LIMIT: u16 = 5_000;

/// One page of a Machine's Log Store, rows ordered by timestamp.
///
/// Every selector field that is set must match a container's saved metadata.
/// `since_nanos` is inclusive and `until_nanos` exclusive. `cursor` is the
/// `next` of the previous page's [`HistoryRow::End`].
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct LogHistoryRequest {
    #[serde(default)]
    pub namespace: Option<String>,
    #[serde(default)]
    pub service: Option<String>,
    #[serde(default)]
    pub deployment: Option<String>,
    #[serde(default)]
    pub container_id: Option<ContainerId>,
    #[serde(default)]
    pub since_nanos: Option<i64>,
    #[serde(default)]
    pub until_nanos: Option<i64>,
    pub direction: LogDirection,
    pub limit: u16,
    #[serde(default)]
    pub cursor: Option<String>,
}

impl LogHistoryRequest {
    /// # Errors
    ///
    /// Names the first field outside the contract.
    pub fn validate(&self) -> Result<(), String> {
        if self.namespace.is_none()
            && self.service.is_none()
            && self.deployment.is_none()
            && self.container_id.is_none()
        {
            return Err("select a namespace, service, deployment, or container".into());
        }
        if !(1..=LOG_HISTORY_PAGE_LIMIT).contains(&self.limit) {
            return Err(format!("limit must be 1..={LOG_HISTORY_PAGE_LIMIT}"));
        }
        if let (Some(since), Some(until)) = (self.since_nanos, self.until_nanos)
            && since > until
        {
            return Err("since_nanos must not be after until_nanos".into());
        }
        Ok(())
    }
}

/// Which end of the time range a history page starts from.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LogDirection {
    #[default]
    Forward,
    /// Newest first: the last rows of the range.
    Backward,
}
