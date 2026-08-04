use serde::{Deserialize, Serialize};

use super::core_types::*;
use super::ops::OperationApiResponse;

pub const MAX_LOGS_TAIL_LINES: u16 = 1_000;

pub type LogsTailResponse = OperationApiResponse<LogsTailResult, LogsTailError>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct LogsTailRequest {
    pub target: LogsTailTarget,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tail_lines: Option<LogsTailLines>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since_unix_seconds: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
pub enum LogsTailTarget {
    Service {
        namespace_id: NamespaceId,
        service_id: ServiceId,
    },
    Container {
        container_id: ContainerId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine_id: Option<MachineId>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct LogsTailResult {
    pub target: LogsTailResultTarget,
    pub text: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "target", rename_all = "snake_case", deny_unknown_fields)]
pub enum LogsTailResultTarget {
    Service {
        namespace_id: NamespaceId,
        service_id: ServiceId,
    },
    Container {
        machine_id: MachineId,
        container_id: ContainerId,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "SafeInteger<\"LogsTailLines\">"))]
#[serde(transparent)]
pub struct LogsTailLines(u16);

impl LogsTailLines {
    pub fn try_new(value: u16) -> Result<Self, LogsTailLinesError> {
        if value == 0 {
            return Err(LogsTailLinesError::Zero);
        }
        if value > MAX_LOGS_TAIL_LINES {
            return Err(LogsTailLinesError::TooLarge { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum LogsTailLinesError {
    #[error("logs tail lines must be greater than zero")]
    Zero,
    #[error("logs tail lines must be at most {MAX_LOGS_TAIL_LINES}")]
    TooLarge { value: u16 },
}

impl<'de> Deserialize<'de> for LogsTailLines {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u16::deserialize(deserializer)?;
        Self::try_new(value).map_err(serde::de::Error::custom)
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum LogsTailError {
    #[error("no such service {}", .service_id.as_str())]
    NoSuchService { service_id: ServiceId },
    #[error("no such container {}", .container_id.as_str())]
    NoSuchContainer { container_id: ContainerId },
    #[error("container {} exists on {} machines", .container_id.as_str(), .machine_ids.len())]
    AmbiguousContainer {
        container_id: ContainerId,
        machine_ids: Vec<MachineId>,
    },
    #[error(
        "log read failed on {} for {}: {message}",
        .machine_id.as_str(),
        .container_id.as_str()
    )]
    ReadFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
    },
    #[error("logs tail unavailable: {message}")]
    Unavailable {
        message: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        machine_id: Option<MachineId>,
    },
}
