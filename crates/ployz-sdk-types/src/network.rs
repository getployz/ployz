use serde::{Deserialize, Serialize};
use ts_rs::TS;

use std::net::Ipv4Addr;

use crate::core_types::{
    ActiveMachineState, EventSequence, FailureMessage, InternalDnsStatus, InternalServiceName,
    MachineDataplaneStatus, MachineId, NetworkStatusMode, OperationId,
};
use crate::ops::{AcceptedOperation, OperationApiResponse};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "page", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkStatusRequest {
    First {
        mode: NetworkStatusMode,
    },
    Continuation {
        mode: NetworkStatusMode,
        snapshot: NetworkStatusIntentFingerprint,
        after: MachineId,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NetworkStatusResult {
    pub snapshot: NetworkStatusIntentFingerprint,
    pub machines: Vec<NetworkStatusMachine>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub next_cursor: Option<MachineId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(type = "Brand<string, \"NetworkStatusIntentFingerprint\">")]
#[serde(try_from = "String", into = "String")]
pub struct NetworkStatusIntentFingerprint(String);

impl NetworkStatusIntentFingerprint {
    pub fn try_new(value: impl Into<String>) -> Result<Self, NetworkStatusIntentFingerprintError> {
        let value = value.into();
        if value.len() != 64
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(NetworkStatusIntentFingerprintError::Invalid { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for NetworkStatusIntentFingerprint {
    type Error = NetworkStatusIntentFingerprintError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<NetworkStatusIntentFingerprint> for String {
    fn from(value: NetworkStatusIntentFingerprint) -> Self {
        let NetworkStatusIntentFingerprint(value) = value;
        value
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NetworkStatusIntentFingerprintError {
    #[error("network status intent fingerprint {value:?} must be 64 lowercase hex characters")]
    Invalid { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NetworkStatusMachine {
    pub active: ActiveMachineState,
    pub dataplane: NetworkDataplaneTestimony,
    pub internal_dns: NetworkInternalDnsTestimony,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkDataplaneTestimony {
    Answered { value: Box<MachineDataplaneStatus> },
    NoAnswer,
    ReadFailed { message: FailureMessage },
    WrongResponder { actual_machine_id: MachineId },
    TimedOut,
    RequestFailed { message: String },
    ProtocolFailed { message: String },
    DecodeFailed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkInternalDnsTestimony {
    Answered { value: InternalDnsStatus },
    NoAnswer,
    WrongResponder { actual_machine_id: MachineId },
    TimedOut,
    RequestFailed { message: String },
    ProtocolFailed { message: String },
    DecodeFailed { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkStatusError {
    #[error("network status unavailable: {message}")]
    Unavailable { message: String },
    #[error(
        "network status intent changed between pages (requested {}, current {})",
        .requested.as_str(),
        .current.as_str()
    )]
    SnapshotChanged {
        requested: NetworkStatusIntentFingerprint,
        current: NetworkStatusIntentFingerprint,
    },
}

pub type NetworkStatusResponse = OperationApiResponse<NetworkStatusResult, NetworkStatusError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NetworkResolveRequest {
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NetworkResolveResult {
    pub name: InternalServiceName,
    pub machines: Vec<NetworkResolveMachineTestimony>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkResolveMachineTestimony {
    Answered {
        machine_id: MachineId,
        addresses: Vec<Ipv4Addr>,
    },
    NoAnswer {
        machine_id: MachineId,
    },
    WrongResponder {
        machine_id: MachineId,
        actual_machine_id: MachineId,
    },
    TimedOut {
        machine_id: MachineId,
    },
    RequestFailed {
        machine_id: MachineId,
        message: String,
    },
    ProtocolFailed {
        machine_id: MachineId,
        message: String,
    },
    DecodeFailed {
        machine_id: MachineId,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkResolveError {
    #[error("invalid internal service name {name:?}")]
    InvalidName { name: String },
    #[error("network resolve unavailable: {message}")]
    Unavailable { message: String },
}

pub type NetworkResolveResponse = OperationApiResponse<NetworkResolveResult, NetworkResolveError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct NetworkRepairRequest {
    pub operation_id: OperationId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub machine_id: Option<MachineId>,
}

pub type NetworkRepairResponse = OperationApiResponse<AcceptedOperation, NetworkRepairError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS, thiserror::Error)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
pub enum NetworkRepairError {
    #[error("network repair {} requires at least one active machine", .operation_id.as_str())]
    NoActiveMachines { operation_id: OperationId },
    #[error(
        "network repair {} target machine {} does not exist",
        .operation_id.as_str(),
        .machine_id.as_str()
    )]
    TargetMachineNotFound {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    #[error("network repair {} unavailable: {message}", .operation_id.as_str())]
    Unavailable {
        operation_id: OperationId,
        message: String,
    },
    #[error(
        "operation {} already recorded a different event at sequence {}",
        .operation_id.as_str(),
        .sequence.get()
    )]
    DuplicateSequenceMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
}
