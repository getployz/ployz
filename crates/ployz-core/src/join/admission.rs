//! Join request validation, authority-row construction, and retry policy.

use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::corrosion::{
    ClusterDocument, CorrosionDocumentVersion, CorrosionTimestamp, MachineDocument,
    MachineTransport, MeshProvider, OperationInitiator, OperatorWriteProvenance, PeerDocument,
    PeerTransport, derive_builtin_wireguard_member,
};
use crate::ids::{MachineName, PeerName, TokenName};
use crate::machine::MachineLifecycle;
use crate::network::{MachineEndpointSubnet, WireGuardPublicKey};

use super::acceptance::{MachineJoinAccepted, PeerJoinAccepted};
use super::arrival::{JoinStorageChoice, JoinStorageFacts, select_join_storage};
use super::token::JoinTokenProof;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MachineJoinRequest {
    pub name: MachineName,
    pub public_key: WireGuardPublicKey,
    #[cfg_attr(feature = "ts", ts(type = "string | null"))]
    pub endpoint: Option<SocketAddr>,
    pub storage_choice: JoinStorageChoice,
    pub storage_facts: JoinStorageFacts,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct PeerJoinRequest {
    pub name: PeerName,
    pub public_key: WireGuardPublicKey,
    #[cfg_attr(feature = "ts", ts(type = "string | null"))]
    pub endpoint: Option<SocketAddr>,
}

/// Why a join candidate cannot become an authority row.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JoinAdmissionValidationError {
    #[error("join admission supports builtin_wireguard, not {found:?}")]
    UnsupportedProvider { found: MeshProvider },
    #[error("machine subnet {subnet:?} is outside cluster prefix")]
    EndpointSubnetOutsideClusterPrefix { subnet: MachineEndpointSubnet },
    #[error("join endpoint port cannot be zero")]
    EndpointPortZero,
    #[error("join admission provenance must name an API token")]
    ProvenanceIsNotApiToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AcceptedMachineRow {
    pub machine_id: MachineName,
    pub document: MachineDocument,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct AcceptedPeerRow {
    pub peer_id: PeerName,
    pub document: PeerDocument,
}

/// Builds the exact machine row authorized by one validated door request.
pub fn prepare_machine_admission(
    cluster: &ClusterDocument,
    request: MachineJoinRequest,
    subnet_v4: MachineEndpointSubnet,
    provenance: OperatorWriteProvenance,
) -> Result<AcceptedMachineRow, JoinAdmissionValidationError> {
    validate_admission_inputs(cluster, request.endpoint, &provenance)?;
    if !cluster.prefix.contains_subnet(&subnet_v4) {
        return Err(
            JoinAdmissionValidationError::EndpointSubnetOutsideClusterPrefix { subnet: subnet_v4 },
        );
    }
    let addr_v6 = derive_builtin_wireguard_member(&cluster.cluster_id, &request.public_key)
        .bind_address()
        .get();
    let storage = select_join_storage(
        cluster.storage_default,
        request.storage_choice,
        request.storage_facts,
    );
    Ok(AcceptedMachineRow {
        machine_id: request.name.clone(),
        document: MachineDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster.cluster_id.clone(),
            provenance,
            name: request.name,
            lifecycle: MachineLifecycle::Active,
            transport: MachineTransport::Wireguard {
                pubkey: request.public_key,
                addr_v6,
                endpoint: request.endpoint,
                subnet_v4,
            },
            storage,
        },
    })
}

/// Builds the exact peer row authorized by one validated door request.
pub fn prepare_peer_admission(
    cluster: &ClusterDocument,
    request: PeerJoinRequest,
    provenance: OperatorWriteProvenance,
) -> Result<AcceptedPeerRow, JoinAdmissionValidationError> {
    validate_admission_inputs(cluster, request.endpoint, &provenance)?;
    let addr_v6 = derive_builtin_wireguard_member(&cluster.cluster_id, &request.public_key)
        .bind_address()
        .get();
    Ok(AcceptedPeerRow {
        peer_id: request.name.clone(),
        document: PeerDocument {
            v: CorrosionDocumentVersion::V1,
            cluster_id: cluster.cluster_id.clone(),
            provenance,
            name: request.name,
            transport: PeerTransport::Wireguard {
                pubkey: request.public_key,
                addr_v6,
                endpoint: request.endpoint,
            },
        },
    })
}

fn validate_admission_inputs(
    cluster: &ClusterDocument,
    endpoint: Option<SocketAddr>,
    provenance: &OperatorWriteProvenance,
) -> Result<(), JoinAdmissionValidationError> {
    if cluster.provider != MeshProvider::BuiltinWireguard {
        return Err(JoinAdmissionValidationError::UnsupportedProvider {
            found: cluster.provider,
        });
    }
    if endpoint.is_some_and(|endpoint| endpoint.port() == 0) {
        return Err(JoinAdmissionValidationError::EndpointPortZero);
    }
    if !matches!(provenance.written_by, OperationInitiator::ApiToken { .. }) {
        return Err(JoinAdmissionValidationError::ProvenanceIsNotApiToken);
    }
    Ok(())
}

/// The request union keeps peer admission structurally free of storage and `/24` fields.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JoinMemberRequest {
    Machine { request: MachineJoinRequest },
    Peer { request: PeerJoinRequest },
}

/// The complete request accepted at the public join-only route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct JoinAdmissionRequest {
    pub token: JoinTokenProof,
    pub member: JoinMemberRequest,
}

/// Whether a matching accepted row must be created or may be reused on retry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JoinAdmissionWrite {
    Create,
    Reuse,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("an accepted row with this id differs from the requested join identity")]
pub struct JoinAdmissionIdempotencyError;

pub fn classify_machine_admission(
    cluster: &ClusterDocument,
    request: &MachineJoinRequest,
    existing: Option<&AcceptedMachineRow>,
) -> Result<JoinAdmissionWrite, JoinAdmissionIdempotencyError> {
    let Some(existing) = existing else {
        return Ok(JoinAdmissionWrite::Create);
    };
    let MachineTransport::Wireguard {
        pubkey,
        addr_v6,
        endpoint,
        ..
    } = &existing.document.transport
    else {
        return Err(JoinAdmissionIdempotencyError);
    };
    let expected_address =
        derive_builtin_wireguard_member(&cluster.cluster_id, &request.public_key)
            .bind_address()
            .get();
    let expected_storage = select_join_storage(
        cluster.storage_default,
        request.storage_choice,
        request.storage_facts,
    );
    if existing.machine_id == request.name
        && existing.document.cluster_id == cluster.cluster_id
        && existing.document.name == request.name
        && pubkey == &request.public_key
        && *addr_v6 == expected_address
        && endpoint == &request.endpoint
        && existing.document.storage == expected_storage
    {
        Ok(JoinAdmissionWrite::Reuse)
    } else {
        Err(JoinAdmissionIdempotencyError)
    }
}

pub fn classify_peer_admission(
    cluster: &ClusterDocument,
    request: &PeerJoinRequest,
    existing: Option<&AcceptedPeerRow>,
) -> Result<JoinAdmissionWrite, JoinAdmissionIdempotencyError> {
    let Some(existing) = existing else {
        return Ok(JoinAdmissionWrite::Create);
    };
    let PeerTransport::Wireguard {
        pubkey,
        addr_v6,
        endpoint,
    } = &existing.document.transport
    else {
        return Err(JoinAdmissionIdempotencyError);
    };
    let expected_address =
        derive_builtin_wireguard_member(&cluster.cluster_id, &request.public_key)
            .bind_address()
            .get();
    if existing.peer_id == request.name
        && existing.document.cluster_id == cluster.cluster_id
        && existing.document.name == request.name
        && pubkey == &request.public_key
        && *addr_v6 == expected_address
        && endpoint == &request.endpoint
    {
        Ok(JoinAdmissionWrite::Reuse)
    } else {
        Err(JoinAdmissionIdempotencyError)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JoinAdmissionAccepted {
    Machine { accepted: MachineJoinAccepted },
    Peer { accepted: PeerJoinAccepted },
}

/// Every refusal returned by the public join door.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, thiserror::Error)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum JoinDoorRefusal {
    #[error("join token {token_id} does not exist or was revoked; run `ployz token create <name>`")]
    TokenNotFound { token_id: TokenName },
    #[error("join token {token_id} expired at {expires_at}; run `ployz token create <name>`")]
    TokenExpired {
        token_id: TokenName,
        expires_at: CorrosionTimestamp,
    },
    #[error("join token {token_id} has the wrong secret; run `ployz token create <name>`")]
    TokenSecretMismatch { token_id: TokenName },
    #[error(
        "join admission is invalid ({reason}); correct the inputs and run `ployz machine join` again"
    )]
    InvalidAdmission {
        reason: JoinAdmissionValidationError,
    },
    #[error(
        "machine name {name:?} is already claimed; run `ployz machine rm {name}` before joining again"
    )]
    #[serde(rename = "name_conflict")]
    MachineNameConflict { name: String },
    #[error(
        "peer name {name:?} is already claimed; run `ployz peer rm {name}` before joining again"
    )]
    PeerNameConflict { name: String },
    #[error(
        "machine or peer identity is already claimed; run `ployz machine reset` before joining again"
    )]
    IdentityConflict,
    #[error(
        "the accepting machine has no reachable seed endpoint; run `ployz machine endpoint set` on a current cluster peer"
    )]
    NoReachableSeed,
    #[error(
        "the cluster endpoint subnet is exhausted; run `ployz machine rm` for an unused machine before joining again"
    )]
    EndpointSubnetExhausted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "outcome", rename_all = "snake_case")]
pub enum JoinAdmissionReply {
    Accepted {
        admission: Box<JoinAdmissionAccepted>,
    },
    Refused {
        refusal: JoinDoorRefusal,
    },
}
