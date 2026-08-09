//! Public join-door request handling and admission.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use hyper::{Method, Response, StatusCode};
use ployz_core::corrosion::{
    MachineDocument, MachineTransport, MeshProvider, OperatorWriteProvenance, PeerDocument,
    PeerTransport, Principal, TokenDocument, derive_builtin_wireguard_member,
};
use ployz_core::ids::{MachineName, PeerName, TokenName};
use ployz_core::join::{
    AcceptedMachineRow, AcceptedPeerRow, CorrosionBootstrapFacts, JoinAcceptanceValidationError,
    JoinAdmissionAccepted, JoinAdmissionReply, JoinAdmissionRequest, JoinAdmissionValidationError,
    JoinAdmissionWrite, JoinDoorMaterial, JoinDoorRefusal, JoinMachineSubstrate, JoinMemberRequest,
    MachineJoinAccepted, MachineJoinRequest, PeerJoinAccepted, PeerJoinRequest,
    ReachableSeedMachine, classify_machine_admission, classify_peer_admission,
    prepare_machine_admission, prepare_peer_admission, validate_join_token,
};
use ployz_core::network::{
    MachineEndpointSubnet, MachineEndpointSubnetAllocationError, WireGuardPublicKey,
};
use ployz_core::{ApiRefusal, JOIN_ROUTE, V2Method};
use serde::Serialize;
use time::OffsetDateTime;
use time::format_description::well_known::Rfc3339;

pub(super) use super::server::BoundedBodyError as JoinBodyError;
use super::server::{
    ApiService, HttpBody, RouteRefusal, corrosion_unavailable_response, json_response,
    read_bounded_body, refusal_response_with_allow,
};
use super::store::{
    AcceptedMachine, AcceptedPeer, AcceptedRoster, MutationStoreError, TokenAuthorizedInsert,
    delete_machine_if_matches, delete_peer_if_matches, insert_machine_if_token_matches,
    insert_peer_if_token_matches, read_accepted_roster, read_machine, read_peer, read_token,
};

const MAX_JOIN_REQUEST_BYTES: usize = 64 * 1024;
const JOIN_BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) async fn handle_join(
    service: &ApiService,
    _peer: SocketAddr,
    mut request: hyper::Request<hyper::body::Incoming>,
    controller_admitted: bool,
) -> Response<HttpBody> {
    if let Err(error) = validate_join_door_route(request.method(), request.uri().path()) {
        return refusal_response_with_allow(error.refusal, error.allow);
    }
    let _controller_guard = if controller_admitted {
        None
    } else {
        let roster = match read_accepted_roster(&service.corrosion, &service.cluster_id).await {
            Ok(roster) => roster,
            Err(error) => {
                tracing::warn!(%error, "join door could not read the accepted roster");
                return corrosion_unavailable_response();
            }
        };
        match service
            .controller_forwarder
            .route_join(&roster, request)
            .await
        {
            super::controller_forwarding::MutationRouting::Local(admitted) => {
                request = admitted.request;
            }
            super::controller_forwarding::MutationRouting::Forwarded(response) => {
                return response;
            }
        }
        Some(Arc::clone(&service.controller_lock).lock_owned().await)
    };
    let body = match read_bounded_join_body(request.into_body()).await {
        Ok(body) => body,
        Err(JoinBodyError::TooLarge) => {
            return join_http_error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
        }
        Err(JoinBodyError::Deadline) => {
            return join_http_error(StatusCode::REQUEST_TIMEOUT, "request_timeout");
        }
        Err(JoinBodyError::Read) => {
            return join_http_error(StatusCode::BAD_REQUEST, "invalid_request");
        }
    };
    let request = match serde_json::from_slice::<JoinAdmissionRequest>(&body) {
        Ok(request) => request,
        Err(_) => return join_http_error(StatusCode::BAD_REQUEST, "invalid_request"),
    };
    match admit(service, request).await {
        Ok(admission) => typed_join_response(&JoinAdmissionReply::Accepted {
            admission: Box::new(admission),
        }),
        Err(AdmissionError::Refused(refusal)) => {
            typed_join_response(&JoinAdmissionReply::Refused { refusal })
        }
        Err(AdmissionError::Store(error)) => {
            tracing::warn!(error = %error, "join admission could not access Corrosion");
            corrosion_unavailable_response()
        }
        Err(AdmissionError::Invariant(message)) => {
            tracing::error!(%message, "join admission local invariant failed");
            corrosion_unavailable_response()
        }
        Err(AdmissionError::Acceptance(error)) => {
            tracing::error!(error = %error, "join admission built an invalid acceptance");
            corrosion_unavailable_response()
        }
    }
}

async fn admit(
    service: &ApiService,
    request: JoinAdmissionRequest,
) -> Result<JoinAdmissionAccepted, AdmissionError> {
    let token_id = request.token.token_id.clone();
    let token = read_token(&service.corrosion, &service.cluster_id, &token_id).await?;
    let now = now_timestamp()?;
    let principal = validate_join_token(
        &request.token,
        token.as_ref().map(|document| (&token_id, document)),
        now,
    )?;
    let Some(token) = token else {
        return Err(AdmissionError::Invariant(
            "Core accepted a missing join-token row",
        ));
    };
    let authority = ValidatedTokenAuthority {
        token_id: token_id.clone(),
        document: token,
    };

    let Some(admission) = service.join_door.admission() else {
        return Err(AdmissionError::Invariant(
            "join door runtime is unavailable",
        ));
    };
    let door = admission.material.as_ref().clone();
    let substrate = admission.substrate.as_ref().clone();
    let roster = read_accepted_roster(&service.corrosion, &service.cluster_id).await?;
    if roster.cluster.provider != MeshProvider::BuiltinWireguard {
        return Err(JoinDoorRefusal::InvalidAdmission {
            reason: JoinAdmissionValidationError::UnsupportedProvider {
                found: roster.cluster.provider,
            },
        }
        .into());
    }
    match request.member {
        JoinMemberRequest::Machine { request } => {
            admit_machine(
                service, roster, request, door, substrate, principal, &authority,
            )
            .await
        }
        JoinMemberRequest::Peer { request } => {
            admit_peer(service, roster, request, principal, &authority).await
        }
    }
}

struct ValidatedTokenAuthority {
    token_id: TokenName,
    document: TokenDocument,
}

async fn admit_machine(
    service: &ApiService,
    roster: AcceptedRoster,
    request: MachineJoinRequest,
    door: JoinDoorMaterial,
    substrate: JoinMachineSubstrate,
    principal: Principal,
    authority: &ValidatedTokenAuthority,
) -> Result<JoinAdmissionAccepted, AdmissionError> {
    reachable_seed(service, &roster, Some(&request.name))?;
    if let Some(existing) = read_machine(&service.corrosion, &roster.cluster, &request.name).await?
    {
        return reuse_machine(
            service, roster, &request, existing, door, substrate, principal,
        )
        .await;
    }
    if roster
        .machines
        .iter()
        .any(|machine| machine.document.name == request.name)
    {
        return Err(machine_name_conflict(request.name.as_str()).into());
    }
    if roster_has_identity(&roster, &request.public_key, None, None) {
        return Err(JoinDoorRefusal::IdentityConflict.into());
    }

    let row = prepare_machine_admission(
        &roster.cluster,
        request.clone(),
        allocate_subnet(&roster)?,
        OperatorWriteProvenance {
            written_by: principal.clone(),
            written_at: now_timestamp()?,
        },
    )
    .map_err(invalid_admission)?;
    let document = row.document;

    match insert_machine_if_token_matches(
        &service.corrosion,
        &request.name,
        &document,
        &authority.token_id,
        &authority.document,
    )
    .await
    {
        Ok(TokenAuthorizedInsert::Inserted) => {}
        Ok(TokenAuthorizedInsert::TokenUnavailable) => {
            return Err(token_unavailable_refusal(authority)?.into());
        }
        Err(write_error) => {
            let refreshed = read_accepted_roster(&service.corrosion, &service.cluster_id).await?;
            if let Some(existing) =
                read_machine(&service.corrosion, &refreshed.cluster, &request.name).await?
            {
                return reuse_machine(
                    service, refreshed, &request, existing, door, substrate, principal,
                )
                .await;
            }
            return Err(write_error.into());
        }
    }

    settle_machine(
        service,
        &request,
        MachineSettlement {
            document,
            door,
            substrate,
        },
    )
    .await
}

struct MachineSettlement {
    document: MachineDocument,
    door: JoinDoorMaterial,
    substrate: JoinMachineSubstrate,
}

async fn settle_machine(
    service: &ApiService,
    request: &MachineJoinRequest,
    settlement: MachineSettlement,
) -> Result<JoinAdmissionAccepted, AdmissionError> {
    let MachineSettlement {
        document,
        door,
        substrate,
    } = settlement;
    let roster = read_accepted_roster(&service.corrosion, &service.cluster_id).await?;
    let Some(accepted) = roster
        .machines
        .iter()
        .find(|machine| machine.id == request.name)
        .cloned()
    else {
        cleanup_machine(service, &request.name, &document).await?;
        return Err(machine_name_conflict(request.name.as_str()).into());
    };
    if let Err(error) = ensure_machine_matches(&roster, request, &accepted) {
        cleanup_machine(service, &request.name, &document).await?;
        return Err(error);
    }
    if roster_has_identity(&roster, &request.public_key, Some(&request.name), None)
        || !machine_owns_subnet(&roster, &accepted)
    {
        cleanup_machine(service, &request.name, &document).await?;
        return Err(JoinDoorRefusal::IdentityConflict.into());
    }
    let acceptance = machine_acceptance(service, &roster, request, accepted, door, substrate);
    if acceptance.is_err() {
        cleanup_machine(service, &request.name, &document).await?;
    }
    acceptance
}

async fn reuse_machine(
    service: &ApiService,
    roster: AcceptedRoster,
    request: &MachineJoinRequest,
    document: MachineDocument,
    door: JoinDoorMaterial,
    substrate: JoinMachineSubstrate,
    _principal: Principal,
) -> Result<JoinAdmissionAccepted, AdmissionError> {
    let row = AcceptedMachineRow {
        machine_id: request.name.clone(),
        document: document.clone(),
    };
    match classify_machine_admission(&roster.cluster, request, Some(&row)) {
        Ok(JoinAdmissionWrite::Reuse) => {}
        Ok(JoinAdmissionWrite::Create) => {
            return Err(AdmissionError::Invariant(
                "an existing machine row was classified as a create",
            ));
        }
        Err(_) => return Err(machine_name_conflict(request.name.as_str()).into()),
    }
    let Some(accepted) = roster
        .machines
        .iter()
        .find(|machine| machine.id == request.name)
        .cloned()
    else {
        return Err(machine_name_conflict(request.name.as_str()).into());
    };
    if roster_has_identity(&roster, &request.public_key, Some(&request.name), None)
        || !machine_owns_subnet(&roster, &accepted)
    {
        return Err(JoinDoorRefusal::IdentityConflict.into());
    }
    machine_acceptance(service, &roster, request, accepted, door, substrate)
}

fn ensure_machine_matches(
    roster: &AcceptedRoster,
    request: &MachineJoinRequest,
    machine: &AcceptedMachine,
) -> Result<(), AdmissionError> {
    let row = AcceptedMachineRow {
        machine_id: machine.id.clone(),
        document: machine.document.clone(),
    };
    match classify_machine_admission(&roster.cluster, request, Some(&row)) {
        Ok(JoinAdmissionWrite::Reuse) => Ok(()),
        Ok(JoinAdmissionWrite::Create) => Err(AdmissionError::Invariant(
            "an accepted machine row was classified as a create",
        )),
        Err(_) => Err(machine_name_conflict(request.name.as_str()).into()),
    }
}

async fn cleanup_machine(
    service: &ApiService,
    machine_id: &MachineName,
    expected: &MachineDocument,
) -> Result<(), AdmissionError> {
    delete_machine_if_matches(&service.corrosion, machine_id, expected).await?;
    Ok(())
}

async fn admit_peer(
    service: &ApiService,
    mut roster: AcceptedRoster,
    request: PeerJoinRequest,
    principal: Principal,
    authority: &ValidatedTokenAuthority,
) -> Result<JoinAdmissionAccepted, AdmissionError> {
    reachable_seed(service, &roster, None)?;
    if let Some(existing) = read_peer(&service.corrosion, &roster.cluster, &request.name).await? {
        return reuse_peer(service, &roster, &request, existing);
    }
    if roster
        .peers
        .iter()
        .any(|peer| peer.document.name == request.name)
    {
        return Err(peer_name_conflict(request.name.as_str()).into());
    }
    if roster_has_identity(&roster, &request.public_key, None, None) {
        return Err(JoinDoorRefusal::IdentityConflict.into());
    }

    let row = prepare_peer_admission(
        &roster.cluster,
        request.clone(),
        OperatorWriteProvenance {
            written_by: principal,
            written_at: now_timestamp()?,
        },
    )
    .map_err(invalid_admission)?;
    let document = row.document;
    match insert_peer_if_token_matches(
        &service.corrosion,
        &request.name,
        &document,
        &authority.token_id,
        &authority.document,
    )
    .await
    {
        Ok(TokenAuthorizedInsert::Inserted) => {}
        Ok(TokenAuthorizedInsert::TokenUnavailable) => {
            return Err(token_unavailable_refusal(authority)?.into());
        }
        Err(write_error) => {
            let refreshed = read_accepted_roster(&service.corrosion, &service.cluster_id).await?;
            if let Some(existing) =
                read_peer(&service.corrosion, &refreshed.cluster, &request.name).await?
            {
                return reuse_peer(service, &refreshed, &request, existing);
            }
            return Err(write_error.into());
        }
    }

    roster = read_accepted_roster(&service.corrosion, &service.cluster_id).await?;
    let Some(accepted) = roster
        .peers
        .iter()
        .find(|peer| peer.id == request.name)
        .cloned()
    else {
        delete_created_peer(service, &request.name, &document).await?;
        return Err(peer_name_conflict(request.name.as_str()).into());
    };
    if let Err(error) = ensure_peer_matches(&roster, &request, &accepted) {
        delete_created_peer(service, &request.name, &document).await?;
        return Err(error);
    }
    if roster_has_identity(&roster, &request.public_key, None, Some(&request.name)) {
        delete_created_peer(service, &request.name, &document).await?;
        return Err(JoinDoorRefusal::IdentityConflict.into());
    }
    let acceptance = peer_acceptance(service, &roster, &request, accepted);
    if acceptance.is_err() {
        delete_created_peer(service, &request.name, &document).await?;
    }
    acceptance
}

fn reuse_peer(
    service: &ApiService,
    roster: &AcceptedRoster,
    request: &PeerJoinRequest,
    document: PeerDocument,
) -> Result<JoinAdmissionAccepted, AdmissionError> {
    let row = AcceptedPeerRow {
        peer_id: request.name.clone(),
        document,
    };
    match classify_peer_admission(&roster.cluster, request, Some(&row)) {
        Ok(JoinAdmissionWrite::Reuse) => {}
        Ok(JoinAdmissionWrite::Create) => {
            return Err(AdmissionError::Invariant(
                "an existing peer row was classified as a create",
            ));
        }
        Err(_) => return Err(peer_name_conflict(request.name.as_str()).into()),
    }
    let Some(accepted) = roster
        .peers
        .iter()
        .find(|peer| peer.id == request.name)
        .cloned()
    else {
        return Err(peer_name_conflict(request.name.as_str()).into());
    };
    if roster_has_identity(roster, &request.public_key, None, Some(&request.name)) {
        return Err(JoinDoorRefusal::IdentityConflict.into());
    }
    peer_acceptance(service, roster, request, accepted)
}

fn ensure_peer_matches(
    roster: &AcceptedRoster,
    request: &PeerJoinRequest,
    peer: &AcceptedPeer,
) -> Result<(), AdmissionError> {
    let row = AcceptedPeerRow {
        peer_id: peer.id.clone(),
        document: peer.document.clone(),
    };
    match classify_peer_admission(&roster.cluster, request, Some(&row)) {
        Ok(JoinAdmissionWrite::Reuse) => Ok(()),
        Ok(JoinAdmissionWrite::Create) => Err(AdmissionError::Invariant(
            "an accepted peer row was classified as a create",
        )),
        Err(_) => Err(peer_name_conflict(request.name.as_str()).into()),
    }
}

async fn delete_created_peer(
    service: &ApiService,
    peer_id: &PeerName,
    expected: &PeerDocument,
) -> Result<(), AdmissionError> {
    delete_peer_if_matches(&service.corrosion, peer_id, expected).await?;
    Ok(())
}

fn machine_acceptance(
    service: &ApiService,
    roster: &AcceptedRoster,
    request: &MachineJoinRequest,
    machine: AcceptedMachine,
    door: JoinDoorMaterial,
    substrate: JoinMachineSubstrate,
) -> Result<JoinAdmissionAccepted, AdmissionError> {
    let (seed, corrosion) = reachable_seed(service, roster, Some(&request.name))?;
    let fingerprint = door.fingerprint.clone();
    let accepted = MachineJoinAccepted {
        cluster: roster.cluster.clone(),
        machine: AcceptedMachineRow {
            machine_id: machine.id,
            document: machine.document,
        },
        seed,
        door,
        corrosion,
        substrate,
    }
    .try_validate(request, &fingerprint)?
    .into_accepted();
    Ok(JoinAdmissionAccepted::Machine { accepted })
}

fn peer_acceptance(
    service: &ApiService,
    roster: &AcceptedRoster,
    request: &PeerJoinRequest,
    peer: AcceptedPeer,
) -> Result<JoinAdmissionAccepted, AdmissionError> {
    let (seed, corrosion) = reachable_seed(service, roster, None)?;
    let accepted = PeerJoinAccepted {
        cluster: roster.cluster.clone(),
        peer: AcceptedPeerRow {
            peer_id: peer.id,
            document: peer.document,
        },
        seed,
        corrosion,
    }
    .try_validate(request)?
    .into_accepted();
    Ok(JoinAdmissionAccepted::Peer { accepted })
}

fn reachable_seed(
    service: &ApiService,
    roster: &AcceptedRoster,
    joining_machine_id: Option<&MachineName>,
) -> Result<(ReachableSeedMachine, CorrosionBootstrapFacts), AdmissionError> {
    if joining_machine_id == Some(&service.local_machine_id) {
        return Err(JoinDoorRefusal::NoReachableSeed.into());
    }
    let candidate = roster
        .machines
        .iter()
        .find(|machine| machine.id == service.local_machine_id)
        .ok_or(JoinDoorRefusal::NoReachableSeed)?;
    let MachineTransport::Wireguard {
        pubkey,
        addr_v6,
        endpoint: Some(_),
        subnet_v4,
    } = &candidate.document.transport
    else {
        return Err(JoinDoorRefusal::NoReachableSeed.into());
    };
    if derive_builtin_wireguard_member(&roster.cluster.cluster_id, pubkey)
        .bind_address()
        .get()
        != *addr_v6
        || !roster.cluster.prefix.contains_subnet(subnet_v4)
    {
        return Err(JoinDoorRefusal::NoReachableSeed.into());
    }
    let seed =
        ReachableSeedMachine::try_new(candidate.id.clone(), candidate.document.transport.clone())?;
    let corrosion = CorrosionBootstrapFacts::try_new(SocketAddr::new(
        IpAddr::V6(*addr_v6),
        service.corrosion_gossip_port,
    ))?;
    Ok((seed, corrosion))
}

fn allocate_subnet(roster: &AcceptedRoster) -> Result<MachineEndpointSubnet, AdmissionError> {
    roster
        .cluster
        .prefix
        .allocate_next(
            roster
                .machines
                .iter()
                .map(|machine| match &machine.document.transport {
                    MachineTransport::Wireguard { subnet_v4, .. }
                    | MachineTransport::Tailscale { subnet_v4, .. } => subnet_v4.clone(),
                }),
        )
        .map_err(|MachineEndpointSubnetAllocationError::Exhausted { .. }| {
            JoinDoorRefusal::EndpointSubnetExhausted.into()
        })
}

fn machine_owns_subnet(roster: &AcceptedRoster, accepted: &AcceptedMachine) -> bool {
    let MachineTransport::Wireguard { subnet_v4, .. } = &accepted.document.transport else {
        return false;
    };
    roster
        .machines
        .iter()
        .filter(|machine| match &machine.document.transport {
            MachineTransport::Wireguard {
                subnet_v4: candidate,
                ..
            }
            | MachineTransport::Tailscale {
                subnet_v4: candidate,
                ..
            } => candidate == subnet_v4,
        })
        .count()
        == 1
}

fn roster_has_identity(
    roster: &AcceptedRoster,
    public_key: &WireGuardPublicKey,
    excluded_machine: Option<&MachineName>,
    excluded_peer: Option<&PeerName>,
) -> bool {
    let target = derive_builtin_wireguard_member(&roster.cluster.cluster_id, public_key).subnet();
    roster.machines.iter().any(|machine| {
        if excluded_machine == Some(&machine.id) {
            return false;
        }
        let MachineTransport::Wireguard { pubkey, .. } = &machine.document.transport else {
            return false;
        };
        pubkey == public_key
            || derive_builtin_wireguard_member(&roster.cluster.cluster_id, pubkey).subnet()
                == target
    }) || roster.peers.iter().any(|peer| {
        if excluded_peer == Some(&peer.id) {
            return false;
        }
        let PeerTransport::Wireguard { pubkey, .. } = &peer.document.transport else {
            return false;
        };
        pubkey == public_key
            || derive_builtin_wireguard_member(&roster.cluster.cluster_id, pubkey).subnet()
                == target
    })
}

fn machine_name_conflict(name: &str) -> JoinDoorRefusal {
    JoinDoorRefusal::MachineNameConflict {
        name: name.to_owned(),
    }
}

fn peer_name_conflict(name: &str) -> JoinDoorRefusal {
    JoinDoorRefusal::PeerNameConflict {
        name: name.to_owned(),
    }
}

fn invalid_admission(reason: JoinAdmissionValidationError) -> AdmissionError {
    JoinDoorRefusal::InvalidAdmission { reason }.into()
}

fn token_unavailable_refusal(
    authority: &ValidatedTokenAuthority,
) -> Result<JoinDoorRefusal, AdmissionError> {
    if now_timestamp()? >= authority.document.expires_at {
        Ok(JoinDoorRefusal::TokenExpired {
            token_id: authority.token_id.clone(),
            expires_at: authority.document.expires_at,
        })
    } else {
        Ok(JoinDoorRefusal::TokenNotFound {
            token_id: authority.token_id.clone(),
        })
    }
}

fn now_timestamp() -> Result<ployz_core::corrosion::CorrosionTimestamp, AdmissionError> {
    let value = OffsetDateTime::now_utc()
        .format(&Rfc3339)
        .map_err(|_| AdmissionError::Invariant("current timestamp could not be formatted"))?;
    ployz_core::corrosion::CorrosionTimestamp::try_new(value)
        .map_err(|_| AdmissionError::Invariant("current timestamp was not canonical"))
}

fn typed_join_response(value: &impl Serialize) -> Response<HttpBody> {
    match serde_json::to_vec(value) {
        Ok(body) => json_response(StatusCode::OK, body),
        Err(error) => {
            tracing::error!(error = %error, "could not encode join-door response");
            corrosion_unavailable_response()
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum AdmissionError {
    #[error("join refused: {0:?}")]
    Refused(JoinDoorRefusal),
    #[error(transparent)]
    Store(#[from] MutationStoreError),
    #[error("join admission invariant failed: {0}")]
    Invariant(&'static str),
    #[error(transparent)]
    Acceptance(#[from] JoinAcceptanceValidationError),
}

impl From<JoinDoorRefusal> for AdmissionError {
    fn from(value: JoinDoorRefusal) -> Self {
        Self::Refused(value)
    }
}

pub(super) fn validate_join_door_route(method: &Method, path: &str) -> Result<(), RouteRefusal> {
    if path != JOIN_ROUTE {
        return Err(RouteRefusal {
            refusal: ApiRefusal::UnsupportedRoute,
            allow: None,
        });
    }
    if method != Method::POST {
        return Err(RouteRefusal {
            refusal: ApiRefusal::UnsupportedMethod {
                method: method.as_str().to_owned(),
            },
            allow: Some(V2Method::Post),
        });
    }
    Ok(())
}

async fn read_bounded_join_body(body: hyper::body::Incoming) -> Result<Vec<u8>, JoinBodyError> {
    read_bounded_body(body, MAX_JOIN_REQUEST_BYTES, JOIN_BODY_READ_TIMEOUT).await
}

#[cfg(test)]
pub(super) fn append_join_body_chunk(
    body: &mut Vec<u8>,
    chunk: &[u8],
) -> Result<(), JoinBodyError> {
    super::server::append_bounded_body_chunk(body, chunk, MAX_JOIN_REQUEST_BYTES)
}

fn join_http_error(status: StatusCode, kind: &'static str) -> Response<HttpBody> {
    json_response(status, format!("{{\"kind\":\"{kind}\"}}").into_bytes())
}

#[cfg(test)]
mod tests {
    use http_body_util::BodyExt as _;
    use hyper::StatusCode;
    use ployz_core::ids::TokenName;
    use ployz_core::join::{JoinAdmissionReply, JoinDoorRefusal};

    use super::typed_join_response;

    #[tokio::test]
    async fn typed_join_refusals_are_successful_http_replies() {
        let refusal = JoinDoorRefusal::TokenNotFound {
            token_id: TokenName::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("token id"),
        };
        let response = typed_join_response(&JoinAdmissionReply::Refused {
            refusal: refusal.clone(),
        });
        assert_eq!(response.status(), StatusCode::OK);
        let body = response
            .into_body()
            .collect()
            .await
            .expect("infallible body")
            .to_bytes();
        assert_eq!(
            serde_json::from_slice::<JoinAdmissionReply>(&body).expect("typed reply"),
            JoinAdmissionReply::Refused { refusal }
        );
    }
}
