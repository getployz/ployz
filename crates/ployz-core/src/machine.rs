//! Machine state and machine-add operation policy.

use serde::{Deserialize, Serialize};
use std::{fmt, num::NonZeroU64};

use crate::ids::{NodeId, OperationId, SubjectToken, SubjectTokenError};
use crate::ops::FailureMessage;
use crate::roles::{FirstNodeGateway, JoinedNodeProcessSet, joined_node_process_set};
use crate::state::ActiveMachineState;
use crate::wire::{PositiveU64StringError, format_u64_string, parse_positive_u64_string};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "Brand<string, \"MachineName\">"))]
#[serde(transparent)]
pub struct MachineName(SubjectToken);

impl MachineName {
    pub fn try_new(value: impl Into<String>) -> Result<Self, SubjectTokenError> {
        Ok(Self(SubjectToken::try_new(value)?))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineReservation {
    pub node_id: NodeId,
    pub name: MachineName,
    pub operation_id: OperationId,
}

impl MachineReservation {
    #[must_use]
    pub const fn is_schedulable(&self) -> bool {
        false
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"JoinTokenFingerprint\">")
)]
#[serde(transparent)]
pub struct JoinTokenFingerprint(SubjectToken);

impl JoinTokenFingerprint {
    pub fn try_new(value: impl Into<String>) -> Result<Self, SubjectTokenError> {
        Ok(Self(SubjectToken::try_new(value)?))
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"JoinTokenExpiresAt\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct JoinTokenExpiresAt(NonZeroU64);

impl JoinTokenExpiresAt {
    pub fn try_new(unix_seconds: u64) -> Result<Self, JoinTokenTimeError> {
        let Some(value) = NonZeroU64::new(unix_seconds) else {
            return Err(JoinTokenTimeError::Zero);
        };
        Ok(Self(value))
    }

    #[must_use]
    pub const fn unix_seconds(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<String> for JoinTokenExpiresAt {
    type Error = JoinTokenTimeError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        parse_join_token_time(value).and_then(Self::try_new)
    }
}

impl From<JoinTokenExpiresAt> for String {
    fn from(value: JoinTokenExpiresAt) -> Self {
        format_u64_string(value.unix_seconds())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"JoinTokenRedeemedAt\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct JoinTokenRedeemedAt(NonZeroU64);

impl JoinTokenRedeemedAt {
    pub fn try_new(unix_seconds: u64) -> Result<Self, JoinTokenTimeError> {
        let Some(value) = NonZeroU64::new(unix_seconds) else {
            return Err(JoinTokenTimeError::Zero);
        };
        Ok(Self(value))
    }

    #[must_use]
    pub const fn unix_seconds(self) -> u64 {
        self.0.get()
    }
}

impl TryFrom<String> for JoinTokenRedeemedAt {
    type Error = JoinTokenTimeError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        parse_join_token_time(value).and_then(Self::try_new)
    }
}

impl From<JoinTokenRedeemedAt> for String {
    fn from(value: JoinTokenRedeemedAt) -> Self {
        format_u64_string(value.unix_seconds())
    }
}

fn parse_join_token_time(value: String) -> Result<u64, JoinTokenTimeError> {
    match parse_positive_u64_string(value) {
        Ok(value) => Ok(value),
        Err(PositiveU64StringError::Zero) => Err(JoinTokenTimeError::Zero),
        Err(PositiveU64StringError::Invalid { value }) => {
            Err(JoinTokenTimeError::InvalidWireValue { value })
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum JoinTokenTimeError {
    Zero,
    InvalidWireValue { value: String },
}

impl fmt::Display for JoinTokenTimeError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Zero => formatter.write_str("join token timestamp must be greater than zero"),
            Self::InvalidWireValue { value } => write!(
                formatter,
                "join token timestamp {value:?} must be a positive integer string"
            ),
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RawJoinToken(SubjectToken);

impl RawJoinToken {
    pub fn try_new(value: impl Into<String>) -> Result<Self, SubjectTokenError> {
        Ok(Self(SubjectToken::try_new(value)?))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

impl fmt::Debug for RawJoinToken {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("RawJoinToken")
            .field(&"[redacted]")
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct IssuedJoinToken {
    pub fingerprint: JoinTokenFingerprint,
    pub expires_at: JoinTokenExpiresAt,
}

impl IssuedJoinToken {
    #[must_use]
    pub const fn new(fingerprint: JoinTokenFingerprint, expires_at: JoinTokenExpiresAt) -> Self {
        Self {
            fingerprint,
            expires_at,
        }
    }

    #[must_use]
    pub fn matches(&self, presented: &JoinTokenFingerprint) -> bool {
        self.fingerprint == *presented
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddCommand {
    pub operation_id: OperationId,
    pub node_id: NodeId,
    pub name: MachineName,
    pub join_token: IssuedJoinToken,
    pub gateway: FirstNodeGateway,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddPlan {
    pub reservation: MachineReservation,
    pub operation: MachineAddOperationState,
    pub process_set: JoinedNodeProcessSet,
}

#[must_use]
pub fn plan_machine_add(command: MachineAddCommand) -> MachineAddPlan {
    MachineAddPlan {
        reservation: MachineReservation {
            node_id: command.node_id.clone(),
            name: command.name,
            operation_id: command.operation_id.clone(),
        },
        operation: MachineAddOperationState::Pending {
            join_token: command.join_token,
        },
        process_set: joined_node_process_set(&command.node_id, command.gateway),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineAddOperationState {
    Pending {
        join_token: IssuedJoinToken,
    },
    Joining {
        joined_at: JoinTokenRedeemedAt,
    },
    Completed,
    Failed {
        failure: MachineAddFailure,
    },
    Cancelled {
        reason: crate::ops::CancellationReason,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MachineAddOperationStateName {
    Pending,
    Joining,
    Completed,
    Failed,
    Cancelled,
}

impl MachineAddOperationState {
    #[must_use]
    pub const fn name(&self) -> MachineAddOperationStateName {
        match self {
            Self::Pending { .. } => MachineAddOperationStateName::Pending,
            Self::Joining { .. } => MachineAddOperationStateName::Joining,
            Self::Completed => MachineAddOperationStateName::Completed,
            Self::Failed { .. } => MachineAddOperationStateName::Failed,
            Self::Cancelled { .. } => MachineAddOperationStateName::Cancelled,
        }
    }

    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed { .. } | Self::Cancelled { .. }
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineAddFailure {
    InvalidJoinToken,
    JoinTokenExpired { expired_at: JoinTokenExpiresAt },
    ReadinessFailed { evidence: MachineReadinessEvidence },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineTransitionRejected {
    pub current: MachineAddOperationStateName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineJoinOutcome {
    Redeemed(MachineAddOperationState),
    Failed(MachineAddOperationState),
    Rejected(MachineTransitionRejected),
}

#[must_use]
pub fn redeem_join_token(
    operation: MachineAddOperationState,
    presented: &JoinTokenFingerprint,
    now: JoinTokenRedeemedAt,
) -> MachineJoinOutcome {
    let MachineAddOperationState::Pending { join_token } = operation else {
        return MachineJoinOutcome::Rejected(MachineTransitionRejected {
            current: operation.name(),
        });
    };

    if !join_token.matches(presented) {
        return MachineJoinOutcome::Failed(MachineAddOperationState::Failed {
            failure: MachineAddFailure::InvalidJoinToken,
        });
    }

    if join_token.expires_at.unix_seconds() <= now.unix_seconds() {
        return MachineJoinOutcome::Failed(MachineAddOperationState::Failed {
            failure: MachineAddFailure::JoinTokenExpired {
                expired_at: join_token.expires_at,
            },
        });
    }

    MachineJoinOutcome::Redeemed(MachineAddOperationState::Joining { joined_at: now })
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineActivationOutcome {
    Completed {
        operation: MachineAddOperationState,
        active_machine: ActiveMachineState,
    },
    Failed(MachineAddOperationState),
    Rejected(MachineTransitionRejected),
}

#[must_use]
pub fn activate_joined_machine(
    reservation: MachineReservation,
    operation: MachineAddOperationState,
    evidence: MachineReadinessEvidence,
) -> MachineActivationOutcome {
    let MachineAddOperationState::Joining { .. } = operation else {
        return MachineActivationOutcome::Rejected(MachineTransitionRejected {
            current: operation.name(),
        });
    };

    if !evidence.is_confirmed() {
        return MachineActivationOutcome::Failed(MachineAddOperationState::Failed {
            failure: MachineAddFailure::ReadinessFailed { evidence },
        });
    }

    MachineActivationOutcome::Completed {
        operation: MachineAddOperationState::Completed,
        active_machine: ActiveMachineState {
            node_id: reservation.node_id,
            name: reservation.name,
            activated_by: reservation.operation_id,
        },
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineReadinessEvidence {
    pub nats_tunnel: MachineReadinessCheck,
    pub heartbeat: MachineReadinessCheck,
    pub node_inspect: MachineReadinessCheck,
}

impl MachineReadinessEvidence {
    #[must_use]
    pub fn confirmed() -> Self {
        Self {
            nats_tunnel: MachineReadinessCheck::Confirmed,
            heartbeat: MachineReadinessCheck::Confirmed,
            node_inspect: MachineReadinessCheck::Confirmed,
        }
    }

    #[must_use]
    pub const fn is_confirmed(&self) -> bool {
        matches!(self.nats_tunnel, MachineReadinessCheck::Confirmed)
            && matches!(self.heartbeat, MachineReadinessCheck::Confirmed)
            && matches!(self.node_inspect, MachineReadinessCheck::Confirmed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineReadinessCheck {
    Confirmed,
    Missing { reason: FailureMessage },
}
