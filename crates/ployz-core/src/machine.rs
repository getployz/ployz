//! Machine state and machine-add operation policy.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

use crate::ids::{NodeId, OperationId, SubjectToken, SubjectTokenError};
use crate::ops::{FailureMessage, OperationIdempotencyKey};
use crate::roles::{InstallRolePolicy, JoinedNodeProcessSet, plan_joined_node_process_set};
use crate::state::ActiveMachineState;
use crate::wire::{positive_u64_wire_error, positive_u64_wire_newtype};

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

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

positive_u64_wire_newtype! {
    pub struct JoinTokenExpiresAt;
    ts_brand: "Brand<string, \"JoinTokenExpiresAt\">";
    accessor: unix_seconds;
    error: JoinTokenTimeError;
}

positive_u64_wire_newtype! {
    pub struct JoinTokenRedeemedAt;
    ts_brand: "Brand<string, \"JoinTokenRedeemedAt\">";
    accessor: unix_seconds;
    error: JoinTokenTimeError;
}

positive_u64_wire_error! {
    pub enum JoinTokenTimeError;
    noun: "join token timestamp";
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

    pub fn fingerprint(&self) -> Result<JoinTokenFingerprint, SubjectTokenError> {
        let digest = Sha256::digest(self.as_str().as_bytes());
        JoinTokenFingerprint::try_new(format!("{digest:x}"))
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
    pub roles: InstallRolePolicy,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddPlan {
    pub reservation: MachineReservation,
    pub operation: MachineAddOperationState,
    pub process_set: JoinedNodeProcessSet,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstNodeActivationPlan {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub name: MachineName,
}

pub fn plan_first_node_activation(
    node_id: &NodeId,
) -> Result<FirstNodeActivationPlan, SubjectTokenError> {
    Ok(FirstNodeActivationPlan {
        operation_id: OperationId::try_new(format!("op_init_{}", node_id.as_str()))?,
        idempotency_key: OperationIdempotencyKey::try_new(format!(
            "idem_init_{}",
            node_id.as_str()
        ))?,
        name: MachineName::try_new(node_id.as_str())?,
    })
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
        process_set: plan_joined_node_process_set(&command.node_id, command.roles),
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
    JoinTokenExpired {
        expired_at: JoinTokenExpiresAt,
    },
    BootstrapFailed {
        message: FailureMessage,
    },
    ReadinessFailed {
        evidence: MachineReadinessEvidence,
    },
    AuthorizationRenderFailed {
        message: FailureMessage,
    },
    NatsReloadFailed {
        message: FailureMessage,
    },
    MintedCredentialUnusable {
        message: FailureMessage,
    },
    /// Credential provisioning progressed but its operation evidence could
    /// not be recorded; the mint fails terminally instead of stranding the
    /// operation non-terminal.
    CredentialEvidenceWriteFailed {
        message: FailureMessage,
    },
}

/// One step of the per-machine credential minting work that runs after a
/// machine-add submission is accepted. Each step is recorded as an
/// operation event so the audience can follow mint → render → reload →
/// verify → material-ready without reading logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum MachineCredentialProvisioningStep {
    Minted,
    Rendered,
    Reloaded,
    Verified,
    MaterialReady,
}

impl MachineCredentialProvisioningStep {
    /// The wire token used in event subjects and message ids.
    #[must_use]
    pub const fn as_subject_token(self) -> &'static str {
        match self {
            Self::Minted => "minted",
            Self::Rendered => "rendered",
            Self::Reloaded => "reloaded",
            Self::Verified => "verified",
            Self::MaterialReady => "material_ready",
        }
    }
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

pub fn redeem_pending_join_token(
    join_token: &IssuedJoinToken,
    presented: &JoinTokenFingerprint,
    now: JoinTokenRedeemedAt,
) -> Result<JoinTokenRedeemedAt, MachineAddFailure> {
    if !join_token.matches(presented) {
        return Err(MachineAddFailure::InvalidJoinToken);
    }

    if join_token.expires_at.unix_seconds() <= now.unix_seconds() {
        return Err(MachineAddFailure::JoinTokenExpired {
            expired_at: join_token.expires_at,
        });
    }

    Ok(now)
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

    match redeem_pending_join_token(&join_token, presented, now) {
        Ok(joined_at) => {
            MachineJoinOutcome::Redeemed(MachineAddOperationState::Joining { joined_at })
        }
        Err(failure) => MachineJoinOutcome::Failed(MachineAddOperationState::Failed { failure }),
    }
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

pub fn active_machine_from_completed_add(
    operation_id: OperationId,
    node_id: NodeId,
    name: MachineName,
    operation: MachineAddOperationState,
) -> Result<ActiveMachineState, MachineTransitionRejected> {
    let MachineAddOperationState::Completed = operation else {
        return Err(MachineTransitionRejected {
            current: operation.name(),
        });
    };

    Ok(ActiveMachineState {
        node_id,
        name,
        activated_by: operation_id,
    })
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
    pub nats_connection: MachineReadinessCheck,
    pub heartbeat: MachineReadinessCheck,
    pub node_inspect: MachineReadinessCheck,
}

impl MachineReadinessEvidence {
    #[must_use]
    pub fn confirmed() -> Self {
        Self {
            nats_connection: MachineReadinessCheck::Confirmed,
            heartbeat: MachineReadinessCheck::Confirmed,
            node_inspect: MachineReadinessCheck::Confirmed,
        }
    }

    #[must_use]
    pub const fn is_confirmed(&self) -> bool {
        matches!(self.nats_connection, MachineReadinessCheck::Confirmed)
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
