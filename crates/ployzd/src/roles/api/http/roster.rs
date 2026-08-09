//! Reader-fenced roster loading and socket-peer resolution.

use std::net::{IpAddr, SocketAddr};

use ployz_core::corrosion::{MachineDocument, Principal, resolve_source_principal};
use ployz_core::ids::{ClusterName, MachineName, PeerName};
use ployz_core::{ApiRefusal, CorrosionRetryAfterSeconds};

use super::store::{AcceptedRoster, MutationStoreError, read_accepted_roster};
use crate::corrosion::CorrosionClient;

pub(super) async fn resolve_peer_principal(
    corrosion: &CorrosionClient,
    cluster_id: &ClusterName,
    source: IpAddr,
) -> Result<(Principal, AcceptedRoster), PeerPrincipalError> {
    let roster = read_accepted_roster(corrosion, cluster_id)
        .await
        .map_err(accepted_roster_refusal)
        .map_err(PeerPrincipalError::Refusal)?;
    let principals = roster.principals();
    if principals.is_empty() {
        return Err(PeerPrincipalError::EmptyAcceptedRoster { source });
    }
    let principal = resolve_source_principal(source, &principals)
        .map_err(ApiRefusal::from)
        .map_err(PeerPrincipalError::Refusal)?;
    Ok((principal, roster))
}

pub(super) async fn validate_listener_identity(
    corrosion: &CorrosionClient,
    cluster_id: &ClusterName,
    local_machine_id: &MachineName,
    listen_addr: SocketAddr,
) -> Result<MachineDocument, ApiListenerValidationError> {
    let roster = read_accepted_roster(corrosion, cluster_id)
        .await
        .map_err(accepted_roster_refusal)
        .map_err(|refusal| ApiListenerValidationError::Refusal { refusal })?;
    let principal = resolve_source_principal(listen_addr.ip(), &roster.principals())
        .map_err(ApiRefusal::from)
        .map_err(|refusal| ApiListenerValidationError::Refusal { refusal })?;
    validate_listener_principal(local_machine_id, listen_addr, principal)?;
    roster
        .machines
        .into_iter()
        .find(|machine| machine.id == *local_machine_id)
        .map(|machine| machine.document)
        .ok_or(ApiListenerValidationError::Refusal {
            refusal: ApiRefusal::InvalidCluster,
        })
}

#[derive(Debug)]
pub(super) enum PeerPrincipalError {
    EmptyAcceptedRoster { source: IpAddr },
    Refusal(ApiRefusal),
}

impl PeerPrincipalError {
    pub(super) fn into_refusal(self) -> ApiRefusal {
        match self {
            Self::EmptyAcceptedRoster { source } => ApiRefusal::UnknownSource { source },
            Self::Refusal(refusal) => refusal,
        }
    }
}

pub(super) fn validate_listener_principal(
    local_machine_id: &MachineName,
    listen_addr: SocketAddr,
    principal: Principal,
) -> Result<(), ApiListenerValidationError> {
    match principal {
        Principal::Machine { machine_id } if machine_id == *local_machine_id => Ok(()),
        Principal::Machine { machine_id } => Err(ApiListenerValidationError::OtherMachine {
            listen_addr,
            expected_machine_id: local_machine_id.clone(),
            found_machine_id: machine_id,
        }),
        Principal::Peer { peer_id } => Err(ApiListenerValidationError::Peer {
            listen_addr,
            expected_machine_id: local_machine_id.clone(),
            peer_id,
        }),
        Principal::ApiToken { token_id } => Err(ApiListenerValidationError::ApiToken {
            listen_addr,
            expected_machine_id: local_machine_id.clone(),
            token_id,
        }),
    }
}

pub(super) fn corrosion_unavailable_refusal() -> ApiRefusal {
    ApiRefusal::CorrosionUnavailable {
        retry_after_seconds: CorrosionRetryAfterSeconds::DEFAULT,
    }
}

fn accepted_roster_refusal(error: MutationStoreError) -> ApiRefusal {
    tracing::warn!(%error, "could not read the accepted API roster");
    match error {
        MutationStoreError::MissingCluster => ApiRefusal::MissingCluster,
        MutationStoreError::InvalidCluster | MutationStoreError::InvalidAcceptedId { .. } => {
            ApiRefusal::InvalidCluster
        }
        MutationStoreError::Client(_)
        | MutationStoreError::StoredRows(_)
        | MutationStoreError::DuplicatePrimaryKey { .. }
        | MutationStoreError::Encode { .. }
        | MutationStoreError::UnexpectedWriteResult { .. }
        | MutationStoreError::ConcurrentMachineMutation { .. } => corrosion_unavailable_refusal(),
    }
}

/// A roster-backed listener identity validation failure.
#[derive(Debug, thiserror::Error)]
pub enum ApiListenerValidationError {
    #[error("API listener source was refused by the accepted roster: {refusal:?}")]
    Refusal { refusal: ApiRefusal },
    #[error(
        "API listener {listen_addr} resolves to machine {found_machine_id}, not local machine {expected_machine_id}"
    )]
    OtherMachine {
        listen_addr: SocketAddr,
        expected_machine_id: MachineName,
        found_machine_id: MachineName,
    },
    #[error(
        "API listener {listen_addr} resolves to peer {peer_id}, not local machine {expected_machine_id}"
    )]
    Peer {
        listen_addr: SocketAddr,
        expected_machine_id: MachineName,
        peer_id: PeerName,
    },
    #[error(
        "API listener {listen_addr} resolves to API token {token_id}, not local machine {expected_machine_id}"
    )]
    ApiToken {
        listen_addr: SocketAddr,
        expected_machine_id: MachineName,
        token_id: ployz_core::ids::TokenName,
    },
}
