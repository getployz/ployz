//! Mesh-authenticated token and machine mutations.

use std::time::Duration;

use hyper::{Response, StatusCode};
use ployz_core::corrosion::Principal;
use ployz_core::corrosion::{
    CorrosionDocumentVersion, CorrosionTimestamp, OperatorWriteProvenance, Sha256Hex, TokenDocument,
};
use ployz_core::ids::{MachineRowId, PeerId, TokenId};
use ployz_core::join::{
    MachineEndpointSetRefusal, MachineEndpointSetRequest, PreparedTokenCreation,
    TokenCreateRequest, TokenListRequest, TokenRevokeRefusal, TokenRevokeReply, TokenRevokeRequest,
    advertise_join_door_endpoints, set_machine_endpoint, token_list_reply,
};
use ployz_core::machine::MachineName;
use ployz_core::{
    ApiRefusal, MachineRemoveRefusal, MachineRemoveReply, MachineRemoveRequest, PeerRemoveRefusal,
    PeerRemoveReply, PeerRemoveRequest, V2Route, select_machine_removal, select_peer_removal,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use time::format_description::well_known::Rfc3339;
use time::{Duration as TimeDuration, OffsetDateTime};

use super::roster::corrosion_unavailable_refusal;
use super::server::{
    ApiService, BoundedBodyError, HttpBody, corrosion_unavailable_response, json_response,
    read_bounded_body, refusal_response,
};
use super::store::{
    MutationStoreError, delete_token, insert_token, read_accepted_roster, read_machine, read_token,
    read_tokens, remove_machine_and_sweep, remove_peer_if_matches,
    update_wireguard_endpoint_if_matches,
};

const MAX_MUTATION_REQUEST_BYTES: usize = 64 * 1024;
const MUTATION_BODY_READ_TIMEOUT: Duration = Duration::from_secs(10);

pub(super) async fn handle_mutation(
    service: &ApiService,
    route: V2Route,
    principal: Principal,
    request: hyper::Request<hyper::body::Incoming>,
) -> Response<HttpBody> {
    match route {
        V2Route::TokenCreate => {
            let request = match decode_request::<TokenCreateRequest>(request.into_body()).await {
                Ok(request) => request,
                Err(response) => return response,
            };
            token_create(service, principal, request).await
        }
        V2Route::TokenList => {
            let request = match decode_request::<TokenListRequest>(request.into_body()).await {
                Ok(request) => request,
                Err(response) => return response,
            };
            token_list(service, request).await
        }
        V2Route::TokenRevoke(route_token_id) => {
            let request = match decode_request::<TokenRevokeRequest>(request.into_body()).await {
                Ok(request) => request,
                Err(response) => return response,
            };
            if request.token_id != route_token_id {
                return simple_error(StatusCode::BAD_REQUEST, "route_body_mismatch");
            }
            token_revoke(service, request).await
        }
        V2Route::MachineEndpointSet => {
            let request =
                match decode_request::<MachineEndpointSetRequest>(request.into_body()).await {
                    Ok(request) => request,
                    Err(response) => return response,
                };
            machine_endpoint_set(service, principal, request).await
        }
        V2Route::MachineRemove => {
            let request = match decode_request::<MachineRemoveRequest>(request.into_body()).await {
                Ok(request) => request,
                Err(response) => return response,
            };
            machine_remove(service, request).await
        }
        V2Route::PeerRemove => {
            let request = match decode_request::<PeerRemoveRequest>(request.into_body()).await {
                Ok(request) => request,
                Err(response) => return response,
            };
            peer_remove(service, principal, request).await
        }
        V2Route::Version
        | V2Route::Founding
        | V2Route::Join
        | V2Route::MachineUpgrade
        | V2Route::Lens(_)
        | V2Route::LensWatch(_) => refusal_response(ApiRefusal::UnsupportedRoute),
    }
}

async fn token_create(
    service: &ApiService,
    principal: Principal,
    request: TokenCreateRequest,
) -> Response<HttpBody> {
    let roster = match read_accepted_roster(&service.corrosion, &service.cluster_id).await {
        Ok(roster) => roster,
        Err(error) => return store_failure("read roster for token creation", error),
    };
    let endpoints = match advertise_join_door_endpoints(
        roster.machines.iter().map(|machine| &machine.document),
    ) {
        Ok(endpoints) => endpoints,
        Err(refusal) => return typed_response(StatusCode::CONFLICT, &refusal),
    };
    let Some(admission) = service.join_door.admission() else {
        return corrosion_unavailable_response();
    };
    let material = &admission.material;
    let (created_at, expires_at) = match token_times(request.ttl_seconds.get()) {
        Ok(times) => times,
        Err(()) => return corrosion_unavailable_response(),
    };
    let token_id = match TokenId::try_new(ulid::Ulid::new().to_string()) {
        Ok(token_id) => token_id,
        Err(error) => {
            tracing::error!(error = %error, "generated invalid token ULID");
            return corrosion_unavailable_response();
        }
    };
    let mut secret_bytes = [0_u8; 32];
    if let Err(error) = getrandom::fill(&mut secret_bytes) {
        tracing::error!(error = %error, "operating-system randomness failed for join token");
        return corrosion_unavailable_response();
    }
    let secret = ployz_core::join::JoinTokenSecret::try_from_bytes(secret_bytes);
    let zero_hash = Sha256Hex::try_new("0".repeat(Sha256Hex::TEXT_LENGTH))
        .expect("fixed placeholder digest is canonical");
    let document = TokenDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: service.cluster_id.clone(),
        provenance: OperatorWriteProvenance {
            written_by: principal,
            written_at: created_at,
        },
        secret_sha256: zero_hash,
        created_at,
        expires_at,
    };
    let prepared = match PreparedTokenCreation::try_new(
        token_id,
        secret,
        material.fingerprint.clone(),
        endpoints,
        document,
    ) {
        Ok(prepared) => prepared,
        Err(error) => {
            tracing::error!(error = %error, "could not prepare join token");
            return corrosion_unavailable_response();
        }
    };
    if let Err(error) =
        insert_token(&service.corrosion, &prepared.token_id, &prepared.document).await
    {
        return store_failure("write token", error);
    }
    typed_response(StatusCode::OK, &prepared.reply)
}

async fn token_list(service: &ApiService, request: TokenListRequest) -> Response<HttpBody> {
    let now = match now_timestamp() {
        Ok(now) => now,
        Err(()) => return corrosion_unavailable_response(),
    };
    let tokens = match read_tokens(&service.corrosion, &service.cluster_id).await {
        Ok(tokens) => tokens,
        Err(error) => return store_failure("list tokens", error),
    };
    typed_response(StatusCode::OK, &token_list_reply(request, now, tokens))
}

async fn token_revoke(service: &ApiService, request: TokenRevokeRequest) -> Response<HttpBody> {
    match read_token(&service.corrosion, &service.cluster_id, &request.token_id).await {
        Ok(Some(_)) => {}
        Ok(None) => {
            return typed_response(
                StatusCode::NOT_FOUND,
                &TokenRevokeRefusal::NotFound {
                    token_id: request.token_id,
                },
            );
        }
        Err(error) => return store_failure("read token for revocation", error),
    }
    if let Err(error) = delete_token(&service.corrosion, &request.token_id).await {
        return store_failure("revoke token", error);
    }
    typed_response(
        StatusCode::OK,
        &TokenRevokeReply {
            token_id: request.token_id,
        },
    )
}

async fn machine_endpoint_set(
    service: &ApiService,
    principal: Principal,
    request: MachineEndpointSetRequest,
) -> Response<HttpBody> {
    let roster = match read_accepted_roster(&service.corrosion, &service.cluster_id).await {
        Ok(roster) => roster,
        Err(error) => return store_failure("read roster for endpoint update", error),
    };
    let Some(machine) = roster
        .machines
        .into_iter()
        .find(|machine| machine.document.name == request.machine_name)
    else {
        return typed_response(
            StatusCode::NOT_FOUND,
            &MachineEndpointSetRefusal::NotFound {
                machine_name: request.machine_name,
            },
        );
    };
    let observed = machine.stored_document;
    let reply = match set_machine_endpoint(machine.id, &request, machine.document) {
        Ok(reply) => reply,
        Err(refusal) => return endpoint_refusal_response(refusal),
    };
    let written_at = match now_timestamp() {
        Ok(now) => now,
        Err(()) => return corrosion_unavailable_response(),
    };
    let machine_id = reply.machine_id;
    let provenance = OperatorWriteProvenance {
        written_by: principal,
        written_at,
    };
    if let Err(error) = update_wireguard_endpoint_if_matches(
        &service.corrosion,
        &machine_id,
        &observed,
        request.endpoint,
        &provenance,
    )
    .await
    {
        return store_failure("set machine endpoint", error);
    }
    let machine = match read_machine(&service.corrosion, &roster.cluster, &machine_id).await {
        Ok(Some(machine)) => machine,
        Ok(None) => {
            return typed_response(
                StatusCode::NOT_FOUND,
                &MachineEndpointSetRefusal::NotFound {
                    machine_name: request.machine_name,
                },
            );
        }
        Err(error) => return store_failure("read machine after endpoint update", error),
    };
    match set_machine_endpoint(machine_id, &request, machine) {
        Ok(reply) => typed_response(StatusCode::OK, &reply),
        Err(refusal) => endpoint_refusal_response(refusal),
    }
}

async fn machine_remove(service: &ApiService, request: MachineRemoveRequest) -> Response<HttpBody> {
    let roster = match read_accepted_roster(&service.corrosion, &service.cluster_id).await {
        Ok(roster) => roster,
        Err(error) => return store_failure("read roster for machine removal", error),
    };
    let requested_id_is_stored = request
        .machine_id
        .as_ref()
        .is_some_and(|machine_id| roster.contains_stored_machine_id(machine_id));
    let reply = match machine_removal_reply(
        &request,
        roster
            .machine_removal_candidates
            .into_iter()
            .map(|machine| (machine.id, machine.name)),
        requested_id_is_stored,
    ) {
        Ok(reply) => reply,
        Err(refusal) => return machine_remove_refusal_response(refusal),
    };
    if let MachineRemoveReply::Removed { machine_id } = &reply
        && let Err(error) = remove_machine_and_sweep(&service.corrosion, machine_id).await
    {
        return store_failure("fence machine and sweep testimony", error);
    }
    typed_response(StatusCode::OK, &reply)
}

async fn peer_remove(
    service: &ApiService,
    principal: Principal,
    request: PeerRemoveRequest,
) -> Response<HttpBody> {
    let Principal::Peer {
        peer_id: caller_peer_id,
    } = principal
    else {
        return refusal_response(ApiRefusal::UnsupportedRoute);
    };
    let roster = match read_accepted_roster(&service.corrosion, &service.cluster_id).await {
        Ok(roster) => roster,
        Err(error) => return store_failure("read roster for peer removal", error),
    };
    let requested_id_is_stored = request
        .peer_id
        .as_ref()
        .is_some_and(|peer_id| roster.contains_stored_peer_id(peer_id));
    let reply = match peer_removal_reply(
        &request,
        roster
            .peer_removal_candidates
            .iter()
            .map(|peer| (peer.id.clone(), peer.name.clone())),
        requested_id_is_stored,
        &caller_peer_id,
    ) {
        Ok(reply) => reply,
        Err(refusal) => return peer_remove_refusal_response(refusal),
    };
    if let PeerRemoveReply::Removed { peer_id } = &reply {
        let Some(candidate) = roster
            .peer_removal_candidates
            .iter()
            .find(|peer| peer.id == *peer_id)
        else {
            return corrosion_unavailable_response();
        };
        if let Err(error) =
            remove_peer_if_matches(&service.corrosion, peer_id, &candidate.stored_document).await
        {
            return store_failure("remove peer", error);
        }
    }
    typed_response(StatusCode::OK, &reply)
}

fn peer_removal_reply(
    request: &PeerRemoveRequest,
    accepted: impl IntoIterator<Item = (PeerId, String)>,
    requested_id_is_stored: bool,
    caller_peer_id: &PeerId,
) -> Result<PeerRemoveReply, PeerRemoveRefusal> {
    match select_peer_removal(request, accepted) {
        Ok(peer_id) if peer_id == *caller_peer_id => Err(PeerRemoveRefusal::SelfRemoval {
            peer_name: request.peer_name.clone(),
            peer_id,
        }),
        Ok(peer_id) => Ok(PeerRemoveReply::Removed { peer_id }),
        Err(PeerRemoveRefusal::NotFound { .. })
            if request.peer_id.is_some() && !requested_id_is_stored =>
        {
            let Some(peer_id) = request.peer_id.clone() else {
                unreachable!("the retry guard requires a peer id")
            };
            Ok(PeerRemoveReply::AlreadyAbsent { peer_id })
        }
        Err(refusal) => Err(refusal),
    }
}

fn machine_removal_reply(
    request: &MachineRemoveRequest,
    accepted: impl IntoIterator<Item = (MachineRowId, MachineName)>,
    requested_id_is_stored: bool,
) -> Result<MachineRemoveReply, MachineRemoveRefusal> {
    match select_machine_removal(request, accepted) {
        Ok(machine_id) => Ok(MachineRemoveReply::Removed { machine_id }),
        Err(MachineRemoveRefusal::NotFound { .. })
            if request.machine_id.is_some() && !requested_id_is_stored =>
        {
            let Some(machine_id) = request.machine_id.clone() else {
                unreachable!("the retry guard requires a machine id")
            };
            Ok(MachineRemoveReply::AlreadyAbsent { machine_id })
        }
        Err(refusal) => Err(refusal),
    }
}

fn endpoint_refusal_response(refusal: MachineEndpointSetRefusal) -> Response<HttpBody> {
    let status = match &refusal {
        MachineEndpointSetRefusal::NotFound { .. } => StatusCode::NOT_FOUND,
        MachineEndpointSetRefusal::EndpointPortZero { .. } => StatusCode::BAD_REQUEST,
        MachineEndpointSetRefusal::ProviderDoesNotUseWireguard { .. } => StatusCode::CONFLICT,
    };
    typed_response(status, &refusal)
}

fn machine_remove_refusal_response(refusal: MachineRemoveRefusal) -> Response<HttpBody> {
    let status = match &refusal {
        MachineRemoveRefusal::NotFound { .. } => StatusCode::NOT_FOUND,
        MachineRemoveRefusal::Ambiguous { .. } | MachineRemoveRefusal::IdMismatch { .. } => {
            StatusCode::CONFLICT
        }
    };
    typed_response(status, &refusal)
}

fn peer_remove_refusal_response(refusal: PeerRemoveRefusal) -> Response<HttpBody> {
    let status = match &refusal {
        PeerRemoveRefusal::NotFound { .. } => StatusCode::NOT_FOUND,
        PeerRemoveRefusal::Ambiguous { .. }
        | PeerRemoveRefusal::IdMismatch { .. }
        | PeerRemoveRefusal::SelfRemoval { .. } => StatusCode::CONFLICT,
    };
    typed_response(status, &refusal)
}

fn token_times(ttl_seconds: u32) -> Result<(CorrosionTimestamp, CorrosionTimestamp), ()> {
    let now = OffsetDateTime::now_utc();
    let expires = now
        .checked_add(TimeDuration::seconds(i64::from(ttl_seconds)))
        .ok_or(())?;
    Ok((timestamp(now)?, timestamp(expires)?))
}

fn now_timestamp() -> Result<CorrosionTimestamp, ()> {
    timestamp(OffsetDateTime::now_utc())
}

fn timestamp(value: OffsetDateTime) -> Result<CorrosionTimestamp, ()> {
    let value = value.format(&Rfc3339).map_err(|error| {
        tracing::error!(error = %error, "could not format Corrosion timestamp");
    })?;
    CorrosionTimestamp::try_new(value).map_err(|error| {
        tracing::error!(error = %error, "could not construct Corrosion timestamp");
    })
}

pub(super) async fn decode_request<Request>(
    body: hyper::body::Incoming,
) -> Result<Request, Response<HttpBody>>
where
    Request: DeserializeOwned,
{
    let body = read_bounded_body(body, MAX_MUTATION_REQUEST_BYTES, MUTATION_BODY_READ_TIMEOUT)
        .await
        .map_err(|error| match error {
            BoundedBodyError::TooLarge => {
                simple_error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large")
            }
            BoundedBodyError::Deadline => {
                simple_error(StatusCode::REQUEST_TIMEOUT, "request_timeout")
            }
            BoundedBodyError::Read => simple_error(StatusCode::BAD_REQUEST, "invalid_request"),
        })?;
    serde_json::from_slice(&body)
        .map_err(|_| simple_error(StatusCode::BAD_REQUEST, "invalid_request"))
}

pub(super) fn typed_response<Value>(status: StatusCode, value: &Value) -> Response<HttpBody>
where
    Value: Serialize + ?Sized,
{
    match serde_json::to_vec(value) {
        Ok(body) => json_response(status, body),
        Err(error) => {
            tracing::error!(error = %error, "could not encode API mutation response");
            corrosion_unavailable_response()
        }
    }
}

fn simple_error(status: StatusCode, kind: &'static str) -> Response<HttpBody> {
    json_response(status, format!("{{\"kind\":\"{kind}\"}}").into_bytes())
}

fn store_failure(action: &'static str, error: MutationStoreError) -> Response<HttpBody> {
    tracing::warn!(%action, error = %error, "API mutation could not reach durable Corrosion state");
    refusal_response(corrosion_unavailable_refusal())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_expiry_is_strictly_after_creation() {
        let (created, expires) = token_times(60).expect("valid bounded token times");
        assert!(expires > created);
    }

    #[test]
    fn zero_endpoint_port_is_a_bad_request_refusal() {
        let response = endpoint_refusal_response(MachineEndpointSetRefusal::EndpointPortZero {
            machine_name: MachineName::try_new("machine-one").expect("machine name"),
        });
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    }

    #[test]
    fn machine_remove_refusals_have_stable_http_statuses() {
        let machine_name = MachineName::try_new("machine-one").expect("machine name");
        let machine_id = ployz_core::ids::MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV")
            .expect("machine id");
        assert_eq!(
            machine_remove_refusal_response(MachineRemoveRefusal::NotFound {
                machine_name: machine_name.clone(),
            })
            .status(),
            StatusCode::NOT_FOUND
        );
        for refusal in [
            MachineRemoveRefusal::Ambiguous {
                machine_name: machine_name.clone(),
                machine_ids: vec![machine_id.clone()],
            },
            MachineRemoveRefusal::IdMismatch {
                machine_name,
                machine_id,
            },
        ] {
            assert_eq!(
                machine_remove_refusal_response(refusal).status(),
                StatusCode::CONFLICT
            );
        }
    }

    #[test]
    fn identity_qualified_machine_removal_retry_is_explicitly_idempotent() {
        let machine_name = MachineName::try_new("machine-one").expect("machine name");
        let machine_id = ployz_core::ids::MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV")
            .expect("machine id");
        let request = MachineRemoveRequest {
            machine_name,
            machine_id: Some(machine_id.clone()),
        };
        assert_eq!(
            machine_removal_reply(&request, std::iter::empty(), false),
            Ok(MachineRemoveReply::AlreadyAbsent { machine_id })
        );
    }

    #[test]
    fn identity_qualified_retry_refuses_an_id_owned_by_a_differently_named_machine() {
        let machine_id = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("machine id");
        let request = MachineRemoveRequest {
            machine_name: MachineName::try_new("edge-b").expect("requested machine name"),
            machine_id: Some(machine_id.clone()),
        };
        assert_eq!(
            machine_removal_reply(
                &request,
                [(
                    machine_id.clone(),
                    MachineName::try_new("edge-a").expect("accepted machine name"),
                )],
                true,
            ),
            Err(MachineRemoveRefusal::IdMismatch {
                machine_name: MachineName::try_new("edge-b").expect("requested machine name"),
                machine_id,
            })
        );
    }

    #[test]
    fn identity_qualified_removal_never_treats_a_stored_skipped_row_as_absent() {
        let machine_name = MachineName::try_new("machine-one").expect("machine name");
        let machine_id = MachineRowId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("machine id");
        let request = MachineRemoveRequest {
            machine_name: machine_name.clone(),
            machine_id: Some(machine_id),
        };

        assert_eq!(
            machine_removal_reply(&request, std::iter::empty(), true),
            Err(MachineRemoveRefusal::NotFound { machine_name })
        );
    }

    #[test]
    fn identity_qualified_peer_removal_retry_is_explicitly_idempotent() {
        let peer_id = PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("peer id");
        let caller = PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAW").expect("caller peer id");
        let request = PeerRemoveRequest {
            peer_name: "operator-laptop".to_owned(),
            peer_id: Some(peer_id.clone()),
        };
        assert_eq!(
            peer_removal_reply(&request, std::iter::empty(), false, &caller),
            Ok(PeerRemoveReply::AlreadyAbsent { peer_id })
        );
    }

    #[test]
    fn peer_removal_refuses_the_authenticated_caller_before_mutation() {
        let caller = PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("caller peer id");
        let request = PeerRemoveRequest {
            peer_name: "operator-laptop".to_owned(),
            peer_id: Some(caller.clone()),
        };
        assert_eq!(
            peer_removal_reply(
                &request,
                [(caller.clone(), "operator-laptop".to_owned())],
                true,
                &caller,
            ),
            Err(PeerRemoveRefusal::SelfRemoval {
                peer_name: "operator-laptop".to_owned(),
                peer_id: caller,
            })
        );
    }

    #[test]
    fn peer_remove_refusals_have_stable_http_statuses() {
        let peer_id = PeerId::try_new("01ARZ3NDEKTSV4RRFFQ69G5FAV").expect("peer id");
        assert_eq!(
            peer_remove_refusal_response(PeerRemoveRefusal::NotFound {
                peer_name: "operator-laptop".to_owned(),
            })
            .status(),
            StatusCode::NOT_FOUND
        );
        for refusal in [
            PeerRemoveRefusal::Ambiguous {
                peer_name: "operator-laptop".to_owned(),
                peer_ids: vec![peer_id.clone()],
            },
            PeerRemoveRefusal::IdMismatch {
                peer_name: "operator-laptop".to_owned(),
                peer_id: peer_id.clone(),
            },
            PeerRemoveRefusal::SelfRemoval {
                peer_name: "operator-laptop".to_owned(),
                peer_id,
            },
        ] {
            assert_eq!(
                peer_remove_refusal_response(refusal).status(),
                StatusCode::CONFLICT
            );
        }
    }
}
