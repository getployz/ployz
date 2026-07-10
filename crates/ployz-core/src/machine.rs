//! Machine state and machine-add operation policy.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

use crate::dataplane::MachineEndpointSubnet;
use crate::ids::{MachineId, OperationId, SubjectToken, SubjectTokenError};
use crate::ops::{
    FailureMessage, MachineAddOperationState, MachineAddOperationStateName, OperationIdempotencyKey,
};
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
pub struct FirstMachineActivationPlan {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub name: MachineName,
}

pub fn plan_first_machine_activation(
    machine_id: &MachineId,
) -> Result<FirstMachineActivationPlan, SubjectTokenError> {
    Ok(FirstMachineActivationPlan {
        operation_id: OperationId::try_new(format!("op_init_{}", machine_id.as_str()))?,
        idempotency_key: OperationIdempotencyKey::try_new(format!(
            "idem_init_{}",
            machine_id.as_str()
        ))?,
        name: MachineName::try_new(machine_id.as_str())?,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
#[derive(thiserror::Error)]
pub enum MachineAddFailure {
    #[error("join token is invalid")]
    InvalidJoinToken,
    #[error("join token expired at unix second {}", .expired_at.unix_seconds())]
    JoinTokenExpired { expired_at: JoinTokenExpiresAt },
    #[error("machine bootstrap failed: {message}")]
    BootstrapFailed { message: FailureMessage },
    #[error("machine readiness failed: {evidence}")]
    ReadinessFailed { evidence: MachineReadinessEvidence },
    #[error("overlay connectivity proof failed: {evidence}")]
    ConnectivityProofFailed { evidence: ConnectivityProofEvidence },
    #[error("authorization render failed: {message}")]
    AuthorizationRenderFailed { message: FailureMessage },
    #[error("NATS reload failed: {message}")]
    NatsReloadFailed { message: FailureMessage },
    #[error("minted credential is unusable: {message}")]
    MintedCredentialUnusable { message: FailureMessage },
    /// Credential provisioning progressed but its operation evidence could
    /// not be recorded; the mint fails terminally instead of stranding the
    /// operation non-terminal.
    #[error("credential evidence write failed: {message}")]
    CredentialEvidenceWriteFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(try_from = "ConnectivityProofEvidenceWire")]
pub struct ConnectivityProofEvidence {
    unreachable_peers: Vec<ConnectivityProofUnreachablePeer>,
}

impl ConnectivityProofEvidence {
    pub fn try_new(
        unreachable_peers: Vec<ConnectivityProofUnreachablePeer>,
    ) -> Result<Self, ConnectivityProofEvidenceError> {
        if unreachable_peers.is_empty() {
            return Err(ConnectivityProofEvidenceError::Empty);
        }
        Ok(Self { unreachable_peers })
    }

    #[must_use]
    pub fn unreachable_peers(&self) -> &[ConnectivityProofUnreachablePeer] {
        &self.unreachable_peers
    }
}

impl fmt::Display for ConnectivityProofEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (index, peer) in self.unreachable_peers.iter().enumerate() {
            if index > 0 {
                formatter.write_str(", ")?;
            }
            write!(
                formatter,
                "{} at {}",
                peer.machine_id.as_str(),
                peer.gateway
            )?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct ConnectivityProofUnreachablePeer {
    pub machine_id: MachineId,
    #[cfg_attr(feature = "typescript", ts(type = "string"))]
    pub gateway: std::net::Ipv4Addr,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ConnectivityProofEvidenceError {
    #[error("connectivity proof evidence has no unreachable peers")]
    Empty,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ConnectivityProofEvidenceWire {
    unreachable_peers: Vec<ConnectivityProofUnreachablePeer>,
}

impl TryFrom<ConnectivityProofEvidenceWire> for ConnectivityProofEvidence {
    type Error = ConnectivityProofEvidenceError;

    fn try_from(wire: ConnectivityProofEvidenceWire) -> Result<Self, Self::Error> {
        Self::try_new(wire.unreachable_peers)
    }
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

pub fn active_machine_from_completed_add(
    operation_id: OperationId,
    machine_id: MachineId,
    name: MachineName,
    endpoint_subnet: MachineEndpointSubnet,
    operation: MachineAddOperationState,
) -> Result<ActiveMachineState, MachineTransitionRejected> {
    let MachineAddOperationState::Completed = operation else {
        return Err(MachineTransitionRejected {
            current: operation.name(),
        });
    };

    Ok(ActiveMachineState {
        lifecycle: crate::state::MachineLifecycle::Active,
        machine_id,
        name,
        activated_by: operation_id,
        control_endpoints: Vec::new(),
        mesh_endpoints: Vec::new(),
        endpoint_subnet,
    })
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineReadinessEvidence {
    pub nats_connection: MachineReadinessCheck,
    pub heartbeat: MachineReadinessCheck,
    pub machine_inspect: MachineReadinessCheck,
}

impl fmt::Display for MachineReadinessEvidence {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let Self {
            nats_connection,
            heartbeat,
            machine_inspect,
        } = self;
        write!(
            formatter,
            "nats connection {nats_connection}; heartbeat {heartbeat}; machine inspect {machine_inspect}"
        )
    }
}

impl MachineReadinessEvidence {
    #[must_use]
    pub fn confirmed() -> Self {
        Self {
            nats_connection: MachineReadinessCheck::Confirmed,
            heartbeat: MachineReadinessCheck::Confirmed,
            machine_inspect: MachineReadinessCheck::Confirmed,
        }
    }

    #[must_use]
    pub const fn is_confirmed(&self) -> bool {
        matches!(self.nats_connection, MachineReadinessCheck::Confirmed)
            && matches!(self.heartbeat, MachineReadinessCheck::Confirmed)
            && matches!(self.machine_inspect, MachineReadinessCheck::Confirmed)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum MachineReadinessCheck {
    Confirmed,
    Missing { reason: FailureMessage },
}

impl fmt::Display for MachineReadinessCheck {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Confirmed => formatter.write_str("confirmed"),
            Self::Missing { reason } => write!(formatter, "missing ({reason})"),
        }
    }
}
