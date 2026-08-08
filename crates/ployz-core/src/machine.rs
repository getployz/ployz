//! Machine identity, state, lifecycle, and testimony.

mod dataplane_admission;
pub mod lifecycle;
pub mod roles;
pub mod rpc;
pub mod runtime;
pub mod storage;
pub mod testimony;

pub use dataplane_admission::{
    validate_declared_local_machine, validate_declared_machine, validate_placement_machine_peers,
    validate_target_machine,
};
pub use lifecycle::MachineLifecycle;
pub use roles::{GatewayRole, InstallRolePolicy};
pub use rpc::{MachineRpcResponder, MachineRpcResponse};
pub use runtime::*;
pub use storage::*;
pub use testimony::{
    GatewayHttpFailure, GatewayProcessAttempt, GatewayProcessHealth, GatewayServingStatus,
    GatewayStatusObservation, GatewayStatusPublishFailure, GatewayWatchFailure,
    MachineEndpointObservation,
};

use std::fmt;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::ids::{MachineId, SubjectToken, SubjectTokenError};
use crate::operation::FailureMessage;
use crate::wire::{positive_u64_wire_error, positive_u64_wire_newtype};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"MachineName\">"))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "Brand<string, \"JoinTokenFingerprint\">"))]
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
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DataplaneAdmissionPeer {
    pub public_key: crate::network::WireGuardPublicKey,
    pub endpoint_subnet: crate::network::WireGuardPeerEndpointSubnet,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireGuardReadinessFailure {
    InterfaceMissing,
    InterfaceMtuUnavailable {
        observed: crate::network::WireGuardInterfaceMtu,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
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
