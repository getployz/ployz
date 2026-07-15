//! Machine state and machine-add operation policy.

mod dataplane_admission;
pub mod lifecycle;
pub mod roles;
pub mod rpc;
pub mod runtime;
pub mod testimony;

pub use dataplane_admission::{
    validate_declared_local_machine, validate_declared_machine, validate_placement_machine_peers,
    validate_target_machine,
};
pub use lifecycle::{
    DataplaneUnavailableReason, MachineLifecycle, MachineUsabilityReason, placement_rejection,
};
pub use roles::{
    DaemonProcessRole, FirstMachineNatsServer, FirstMachineProcessSet, GatewayRole,
    InstallRolePolicy, JoinedMachineProcessSet, plan_first_machine_process_set,
    plan_joined_machine_process_set,
};
pub use rpc::{MachineRpcResponder, MachineRpcResponse};
pub use runtime::*;
pub use testimony::{
    GatewayHttpFailure, GatewayProcessAttempt, GatewayProcessHealth, GatewayServingStatus,
    GatewayStatusObservation, GatewayStatusPublishFailure, GatewayWatchFailure,
    MachineEndpointObservation,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fmt;

use crate::ids::{MachineId, OperationId, SubjectToken, SubjectTokenError};
use crate::intent::{ActiveMachineState, StagedMachineDataplaneState};
use crate::operation::{
    FailureMessage, MachineAddOperationState, MachineAddOperationStateName, OperationIdempotencyKey,
};
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
    #[error("dataplane projection admission failed for {}: {}", .evidence.machine_id.as_str(), .evidence.reason)]
    DataplaneProjectionAdmissionFailed {
        evidence: DataplaneProjectionAdmissionEvidence,
    },
    #[error("authorization render failed: {message}")]
    AuthorizationRenderFailed { message: FailureMessage },
    #[error("NATS reload failed: {message}")]
    NatsReloadFailed { message: FailureMessage },
    #[error("minted credential is unusable: {message}")]
    MintedCredentialUnusable { message: FailureMessage },
    #[error("credential mint task was interrupted: {message}")]
    ControlTaskInterrupted { message: FailureMessage },
    /// Credential provisioning progressed but its operation evidence could
    /// not be recorded; the mint fails terminally instead of stranding the
    /// operation non-terminal.
    #[error("credential evidence write failed: {message}")]
    CredentialEvidenceWriteFailed { message: FailureMessage },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DataplaneProjectionAdmissionEvidence {
    pub machine_id: MachineId,
    pub reason: DataplaneProjectionAdmissionFailure,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DataplaneAdmissionPeer {
    pub public_key: crate::network::WireGuardPublicKey,
    pub endpoint_subnet: crate::network::WireGuardPeerEndpointSubnet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireGuardReadinessFailure {
    InterfaceMissing,
    InterfaceMtuUnavailable {
        observed: crate::network::WireGuardInterfaceMtu,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DataplaneProjectionAdmissionFailure {
    NoAnswer {
        message: FailureMessage,
    },
    UnusableProjection {
        failure: crate::network::DataplaneProjectionFailure,
    },
    AwaitingTargetRevision {
        expected: crate::network::DataplaneProjectionRevision,
        observed: Option<crate::network::DataplaneProjectionRevision>,
    },
    EndpointBridgeNotReady {
        status: crate::network::EndpointBridgeStatus,
    },
    WireGuardNotReady {
        failure: WireGuardReadinessFailure,
    },
    EbpfNotReady {
        status: crate::network::EbpfAttachmentStatus,
    },
    PeerSetMismatch {
        expected: Vec<DataplaneAdmissionPeer>,
        observed: Vec<DataplaneAdmissionPeer>,
    },
    PeerHandshakeNever {
        peer_machine_id: MachineId,
    },
    PeerHandshakeStale {
        peer_machine_id: MachineId,
        observed_age_seconds: u64,
    },
}

impl fmt::Display for DataplaneProjectionAdmissionFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NoAnswer { message } => write!(formatter, "no answer: {message}"),
            Self::UnusableProjection { failure } => {
                write!(formatter, "unusable projection: {failure:?}")
            }
            Self::AwaitingTargetRevision { expected, observed } => match observed {
                Some(observed) => write!(
                    formatter,
                    "awaiting target revision {}: observed {}",
                    expected.as_str(),
                    observed.as_str()
                ),
                None => write!(
                    formatter,
                    "awaiting target revision {}: no attempted revision",
                    expected.as_str()
                ),
            },
            Self::EndpointBridgeNotReady { status } => {
                write!(formatter, "endpoint bridge not ready: {status:?}")
            }
            Self::WireGuardNotReady { failure } => {
                write!(formatter, "WireGuard not ready: {failure:?}")
            }
            Self::EbpfNotReady { status } => write!(formatter, "eBPF not ready: {status:?}"),
            Self::PeerSetMismatch { .. } => formatter.write_str("peer set mismatch"),
            Self::PeerHandshakeNever { peer_machine_id } => write!(
                formatter,
                "peer {} has never completed a handshake",
                peer_machine_id.as_str()
            ),
            Self::PeerHandshakeStale {
                peer_machine_id,
                observed_age_seconds,
            } => write!(
                formatter,
                "peer {} handshake is {observed_age_seconds}s old",
                peer_machine_id.as_str()
            ),
        }
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
    name: MachineName,
    roles: InstallRolePolicy,
    staged: StagedMachineDataplaneState,
    operation: MachineAddOperationState,
) -> Result<ActiveMachineState, MachineTransitionRejected> {
    let MachineAddOperationState::Completed = operation else {
        return Err(MachineTransitionRejected {
            current: operation.name(),
        });
    };

    let StagedMachineDataplaneState {
        operation_id,
        machine_id,
        endpoint_subnet,
        mesh_endpoints,
        wireguard_public_key,
    } = staged;
    Ok(ActiveMachineState {
        lifecycle: MachineLifecycle::Active,
        machine_id,
        name,
        activated_by: operation_id,
        roles,
        control_endpoints: Vec::new(),
        mesh_endpoints,
        endpoint_subnet,
        wireguard_public_key,
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
