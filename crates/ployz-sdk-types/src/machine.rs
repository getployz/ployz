use std::fmt;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use crate::core_types::*;
use crate::ops::{AcceptedOperation, OperationApiResponse};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineAddRequest {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub machine_id: MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
}

pub type MachineAddResponse = OperationApiResponse<MachineAddAccepted, MachineAddError>;

pub type InitFirstMachineActivateResponse =
    OperationApiResponse<InitFirstMachineActivated, InitFirstMachineActivateError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct InitFirstMachineActivateRequest {
    pub machine_id: MachineId,
    pub roles: InstallRolePolicy,
    #[serde(default = "PublicUrlMode::default_mode")]
    pub public_url_mode: PublicUrlMode,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct InitFirstMachineActivated {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
}

pub type MachineListResponse = OperationApiResponse<MachineListResult, MachineListError>;

pub type MachineInspectResponse = OperationApiResponse<MachineSnapshot, MachineInspectError>;

pub type MachineUpdateResponse = OperationApiResponse<AcceptedOperation, MachineUpdateError>;
pub type MachineJoinRedeemResponse =
    OperationApiResponse<MachineJoinRedeemed, MachineJoinRedeemError>;

pub type MachineJoinReportResponse =
    OperationApiResponse<MachineJoinReported, MachineJoinReportError>;
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineAddAccepted {
    pub accepted: AcceptedOperation,
    pub machine_id: MachineId,
    pub bootstrap_url: MachineBootstrapUrl,
    pub join_bundle: MachineJoinBundle,
    pub join_token: MachineJoinToken,
    pub join_secret_delivery: MachineJoinSecretDelivery,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineListRequest {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineInspectRequest {
    pub machine_id: MachineId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineUpdateRequest {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub target_version: InstallArtifactVersion,
}

/// One request shape for both lifecycle endpoints: the endpoint carries the
/// verb (drain or resume), the body only names the operation and machine.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineLifecycleRequest {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
}
pub type MachineDrainResponse = OperationApiResponse<AcceptedOperation, MachineLifecycleError>;
pub type MachineResumeResponse = OperationApiResponse<AcceptedOperation, MachineLifecycleError>;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineLifecycleError {
    #[error("no such machine {} for operation {}", .machine_id.as_str(), .operation_id.as_str())]
    NoSuchMachine {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    #[error("machine lifecycle {} unavailable: {message}", .operation_id.as_str())]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineListResult {
    pub machines: Vec<MachineSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineSnapshot {
    pub active: ActiveMachineState,
    pub testimony: MachineTestimony,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineTestimony {
    Answered {
        endpoints: Option<MachineEndpointObservation>,
        gateway: Option<GatewayStatusObservation>,
        observed_container_count: usize,
        disk_space: MachineDiskSpace,
        /// When this machine last self-reported, as display evidence for the
        /// operator. Never an input to behavior: liveness surfaces at the point
        /// of use (ADR 0027).
        last_observed_at_unix_seconds: u64,
    },
    NoAnswer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineListError {
    #[error("machine list unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineInspectError {
    #[error("no such machine {}", .machine_id.as_str())]
    NoSuchMachine { machine_id: MachineId },
    #[error("machine inspect unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineJoinRedeemRequest {
    pub join_token: MachineJoinToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineJoinReportRequest {
    pub join_token: MachineJoinToken,
    pub outcome: MachineJoinReportOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineJoinReportOutcome {
    Completed,
    Failed { failure: MachineJoinReportFailure },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineJoinReportFailure {
    BootstrapFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineJoinReportedOutcome {
    Completed,
    Failed { failure: MachineJoinReportedFailure },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineJoinReportedFailure {
    BootstrapFailed {
        message: FailureMessage,
    },
    ConnectivityProofFailed {
        evidence: crate::core_types::ConnectivityProofEvidence,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineJoinReported {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub last_event_sequence: EventSequence,
    pub outcome: MachineJoinReportedOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct MachineJoinRedeemed {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    pub join_bundle: MachineJoinBundle,
    pub secret_delivery: MachineJoinSecretDelivery,
    pub joined_at: JoinTokenRedeemedAt,
    pub last_event_sequence: EventSequence,
    pub result: MachineJoinRedeemResult,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum MachineJoinRedeemResult {
    Joined,
    AlreadyJoined,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineJoinRedeemError {
    #[error("join token is invalid")]
    InvalidJoinToken,
    #[error("join token is unknown")]
    UnknownJoinToken,
    /// The operation is accepted but its per-machine credential has not
    /// reached `material-ready` yet. The Host Runner retries redeem boundedly
    /// until the material lands or the join token TTL expires.
    #[error("material for operation {} is not ready yet", .operation_id.as_str())]
    MaterialNotReady { operation_id: OperationId },
    #[error("machine add {} rejected: {failure}", .operation_id.as_str())]
    Rejected {
        operation_id: OperationId,
        failure: MachineAddFailure,
    },
    #[error(
        "operation {} is not pending (currently {})",
        .operation_id.as_str(),
        .current.as_str()
    )]
    OperationNotPending {
        operation_id: OperationId,
        current: MachineAddOperationStateName,
    },
    #[error("machine join redeem unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum InitFirstMachineActivateError {
    #[error("first machine activation plan is invalid")]
    InvalidPlan,
    #[error("first machine activation unavailable: {message}")]
    Unavailable { message: String },
    #[error("machine add failed: {failure}")]
    MachineAdd { failure: MachineAddError },
    #[error("join redeem failed: {failure}")]
    JoinRedeem { failure: MachineJoinRedeemError },
    #[error("join report failed: {failure}")]
    JoinReport { failure: MachineJoinReportError },
    /// Control could not write the first machine's `machine.seed` after the
    /// minted material was redeemed.
    #[error("machine seed write failed: {message}")]
    MachineSeedWrite { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineJoinReportError {
    #[error("join token is invalid")]
    InvalidJoinToken,
    #[error("join token is unknown")]
    UnknownJoinToken,
    #[error(
        "operation {} is not joining (currently {})",
        .operation_id.as_str(),
        .current.as_str()
    )]
    OperationNotJoining {
        operation_id: OperationId,
        current: MachineAddOperationStateName,
    },
    #[error("machine join report unavailable: {message}")]
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineAddError {
    #[error("machine add {} unavailable: {message}", .operation_id.as_str())]
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
    #[error("operation {} already exists for this idempotency key", .operation_id.as_str())]
    DuplicateIdempotencyKey { operation_id: OperationId },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[serde(tag = "error", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineUpdateError {
    #[error("no such machine {} for operation {}", .machine_id.as_str(), .operation_id.as_str())]
    NoSuchMachine {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    #[error(
        "operation {}: updating the current machine {} is unsupported",
        .operation_id.as_str(),
        .machine_id.as_str()
    )]
    CurrentMachineUnsupported {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    #[error("machine update {} unavailable: {message}", .operation_id.as_str())]
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

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize, TS)]
#[ts(type = "Brand<string, \"MachineJoinToken\">")]
#[serde(transparent)]
pub struct MachineJoinToken(String);

impl MachineJoinToken {
    pub fn try_new(value: impl Into<String>) -> Result<Self, BootstrapCommandError> {
        let value = value.into();
        if value.is_empty() {
            return Err(BootstrapCommandError::EmptyJoinToken);
        }
        if value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(BootstrapCommandError::InvalidJoinToken);
        }

        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MachineJoinToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("MachineJoinToken")
            .field(&"[redacted]")
            .finish()
    }
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapCommandError {
    EmptyJoinToken,
    InvalidJoinToken,
}
