//! Mesh-authenticated token and machine mutations.

use std::time::Duration;

use hyper::{Response, StatusCode};
use ployz_core::corrosion::Principal;
use ployz_core::corrosion::{
    CorrosionDocumentVersion, CorrosionTimestamp, NamespaceDocument, OperatorWriteProvenance,
    Sha256Hex, TokenDocument,
};
use ployz_core::join::{
    MachineEndpointSetRefusal, MachineEndpointSetRequest, PreparedTokenCreation,
    TokenCreateRequest, TokenListRequest, TokenRevokeRefusal, TokenRevokeReply, TokenRevokeRequest,
    advertise_join_door_endpoints, set_machine_endpoint, token_list_reply,
};
use ployz_core::machine::MachineName;
use ployz_core::{
    ApiRefusal, CorrosionNamespaceCreateRefusal, CorrosionNamespaceCreateReply,
    CorrosionNamespaceCreateRequest, CorrosionNamespaceRemoveRefusal,
    CorrosionNamespaceRemoveReply, CorrosionNamespaceRemoveRequest, MachineRemoveRefusal,
    MachineRemoveReply, MachineRemoveRequest, V2Route, select_machine_removal,
};
use serde::Serialize;
use serde::de::DeserializeOwned;
use time::format_description::well_known::Rfc3339;
use time::{Duration as TimeDuration, OffsetDateTime};

use super::namespace_store::{
    NamespaceCreateOutcome, NamespaceDeleteOutcome, NamespaceStore, NamespaceStoreError,
};
use super::roster::corrosion_unavailable_refusal;
use super::server::{
    ApiService, BoundedBodyError, HttpBody, corrosion_unavailable_response, json_response,
    read_bounded_body, refusal_response,
};
use super::store::{
    ConditionalNamedDelete, MutationStoreError, delete_token_if_matches, insert_token,
    read_accepted_roster, read_machine, read_token, read_tokens, remove_machine_and_sweep,
    token_name_is_occupied, update_wireguard_endpoint_if_matches,
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
        V2Route::NamespaceCreate => {
            let request = match decode_request::<CorrosionNamespaceCreateRequest>(
                request.into_body(),
            )
            .await
            {
                Ok(request) => request,
                Err(response) => return response,
            };
            namespace_create(service, principal, request).await
        }
        V2Route::NamespaceRemove => {
            let request = match decode_request::<CorrosionNamespaceRemoveRequest>(
                request.into_body(),
            )
            .await
            {
                Ok(request) => request,
                Err(response) => return response,
            };
            namespace_remove(service, request).await
        }
        V2Route::Version
        | V2Route::Founding
        | V2Route::Join
        | V2Route::MachineUpgrade
        | V2Route::Deploy
        | V2Route::DeployInspect
        | V2Route::DeployPrepare
        | V2Route::DeployRetire
        | V2Route::ServiceLogsProbe
        | V2Route::ServiceLogsTail(_, _)
        | V2Route::ServiceLogsFollow(_, _)
        | V2Route::Status
        | V2Route::Doctor
        | V2Route::PeerRemove
        | V2Route::ServiceRemove
        | V2Route::RouteRemove
        | V2Route::RouteAttach
        | V2Route::Lens(_)
        | V2Route::LensWatch(_) => refusal_response(ApiRefusal::UnsupportedRoute),
    }
}

async fn namespace_create(
    service: &ApiService,
    principal: Principal,
    request: CorrosionNamespaceCreateRequest,
) -> Response<HttpBody> {
    let namespace_id = request.namespace_name.clone();
    let written_at = match now_timestamp() {
        Ok(written_at) => written_at,
        Err(()) => return corrosion_unavailable_response(),
    };
    let document = NamespaceDocument {
        v: CorrosionDocumentVersion::V1,
        cluster_id: service.cluster_id.clone(),
        provenance: OperatorWriteProvenance {
            written_by: principal,
            written_at,
        },
        name: request.namespace_name,
        services: std::collections::BTreeMap::new(),
    };
    let store = NamespaceStore::new(service.corrosion.clone(), service.cluster_id.clone());
    match store.create(&namespace_id, &document).await {
        Ok(NamespaceCreateOutcome::Created { namespace_name }) => typed_response(
            StatusCode::OK,
            &CorrosionNamespaceCreateReply {
                namespace_name,
                document,
            },
        ),
        Ok(NamespaceCreateOutcome::AlreadyExists) => typed_response(
            StatusCode::CONFLICT,
            &CorrosionNamespaceCreateRefusal::AlreadyExists {
                namespace_name: document.name,
            },
        ),
        Err(error) => operation_store_failure("create namespace", error),
    }
}

async fn namespace_remove(
    service: &ApiService,
    request: CorrosionNamespaceRemoveRequest,
) -> Response<HttpBody> {
    let store = NamespaceStore::new(service.corrosion.clone(), service.cluster_id.clone());
    let selected = match store.by_id(&request.namespace_name).await {
        Ok(Some(namespace)) => namespace,
        Ok(None) => {
            return namespace_remove_refusal_response(CorrosionNamespaceRemoveRefusal::NotFound {
                namespace_name: request.namespace_name,
            });
        }
        Err(error) => return operation_store_failure("resolve namespace removal", error),
    };
    let namespace_name = selected.id.clone();
    match store.delete_if_empty(&selected).await {
        Ok(NamespaceDeleteOutcome::Removed) => typed_response(
            StatusCode::OK,
            &CorrosionNamespaceRemoveReply::Removed { namespace_name },
        ),
        Ok(NamespaceDeleteOutcome::AlreadyAbsent) => typed_response(
            StatusCode::OK,
            &CorrosionNamespaceRemoveReply::AlreadyAbsent { namespace_name },
        ),
        Ok(NamespaceDeleteOutcome::Changed) => {
            namespace_remove_refusal_response(CorrosionNamespaceRemoveRefusal::Changed {
                namespace_name,
            })
        }
        Ok(NamespaceDeleteOutcome::NotEmpty {
            service_names,
            route_binding_count,
        }) => namespace_remove_refusal_response(CorrosionNamespaceRemoveRefusal::NotEmpty {
            namespace_name,
            service_names,
            route_binding_count,
        }),
        Err(error) => operation_store_failure("remove namespace", error),
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
    let token_id = request.name;
    match token_name_is_occupied(&service.corrosion, &token_id).await {
        Ok(true) => {
            return typed_response(
                StatusCode::CONFLICT,
                &ployz_core::join::TokenCreateRefusal::NameConflict { name: token_id },
            );
        }
        Ok(false) => {}
        Err(error) => return store_failure("check token name", error),
    }
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
    let token = match read_token(&service.corrosion, &service.cluster_id, &request.token_id).await {
        Ok(Some(token)) => token,
        Ok(None) => {
            return token_revoke_refusal_response(TokenRevokeRefusal::NotFound {
                token_id: request.token_id,
            });
        }
        Err(error) => return store_failure("read token for revocation", error),
    };
    match delete_token_if_matches(&service.corrosion, &request.token_id, token.stored_document)
        .await
    {
        Ok(ConditionalNamedDelete::Deleted) => typed_response(
            StatusCode::OK,
            &TokenRevokeReply {
                token_id: request.token_id,
            },
        ),
        Ok(ConditionalNamedDelete::ConcurrentMutation) => {
            token_revoke_refusal_response(TokenRevokeRefusal::ConcurrentMutation {
                token_id: request.token_id,
            })
        }
        Err(error) => store_failure("revoke token", error),
    }
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
    let reply =
        match set_machine_endpoint(machine.document.name.clone(), &request, machine.document) {
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
    let candidates = roster.machines;
    let reply = match machine_removal_reply(
        &request,
        candidates
            .iter()
            .map(|machine| machine.document.name.clone()),
    ) {
        Ok(reply) => reply,
        Err(refusal) => return machine_remove_refusal_response(refusal),
    };
    let MachineRemoveReply::Removed { machine_name } = &reply;
    let observed = candidates
        .into_iter()
        .find(|machine| &machine.document.name == machine_name)
        .expect("selected machine removal retains its observed row");
    match remove_machine_and_sweep(
        &service.corrosion,
        &service.cluster_id,
        machine_name,
        &observed.stored_document,
    )
    .await
    {
        Ok(ConditionalNamedDelete::Deleted) => typed_response(StatusCode::OK, &reply),
        Ok(ConditionalNamedDelete::ConcurrentMutation) => {
            machine_remove_refusal_response(MachineRemoveRefusal::ConcurrentMutation {
                machine_name: machine_name.clone(),
            })
        }
        Err(error) => store_failure("fence machine and sweep testimony", error),
    }
}

fn machine_removal_reply(
    request: &MachineRemoveRequest,
    accepted: impl IntoIterator<Item = MachineName>,
) -> Result<MachineRemoveReply, MachineRemoveRefusal> {
    match select_machine_removal(request, accepted) {
        Ok(machine_name) => Ok(MachineRemoveReply::Removed { machine_name }),
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
        MachineRemoveRefusal::ConcurrentMutation { .. } => StatusCode::CONFLICT,
    };
    typed_response(status, &refusal)
}

fn token_revoke_refusal_response(refusal: TokenRevokeRefusal) -> Response<HttpBody> {
    let status = match &refusal {
        TokenRevokeRefusal::NotFound { .. } => StatusCode::NOT_FOUND,
        TokenRevokeRefusal::ConcurrentMutation { .. } => StatusCode::CONFLICT,
    };
    typed_response(status, &refusal)
}

fn namespace_remove_refusal_response(
    refusal: CorrosionNamespaceRemoveRefusal,
) -> Response<HttpBody> {
    let status = match &refusal {
        CorrosionNamespaceRemoveRefusal::NotFound { .. } => StatusCode::NOT_FOUND,
        CorrosionNamespaceRemoveRefusal::NotEmpty { .. }
        | CorrosionNamespaceRemoveRefusal::Changed { .. } => StatusCode::CONFLICT,
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

pub(super) fn now_timestamp() -> Result<CorrosionTimestamp, ()> {
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

pub(super) fn simple_error(status: StatusCode, kind: &'static str) -> Response<HttpBody> {
    json_response(status, format!("{{\"kind\":\"{kind}\"}}").into_bytes())
}

fn store_failure(action: &'static str, error: MutationStoreError) -> Response<HttpBody> {
    tracing::warn!(%action, error = %error, "API mutation could not reach durable Corrosion state");
    refusal_response(corrosion_unavailable_refusal())
}

fn operation_store_failure(action: &'static str, error: NamespaceStoreError) -> Response<HttpBody> {
    tracing::warn!(%action, error = %error, "API mutation could not reach durable Corrosion state");
    refusal_response(corrosion_unavailable_refusal())
}

#[cfg(test)]
mod tests {
    use ployz_core::ids::TokenName;

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
        assert_eq!(
            machine_remove_refusal_response(MachineRemoveRefusal::NotFound {
                machine_name: machine_name.clone(),
            })
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            machine_remove_refusal_response(MachineRemoveRefusal::ConcurrentMutation {
                machine_name,
            })
            .status(),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn token_revoke_refusals_have_stable_http_statuses() {
        let token_id = TokenName::try_new("bootstrap").expect("token id");
        assert_eq!(
            token_revoke_refusal_response(TokenRevokeRefusal::NotFound {
                token_id: token_id.clone(),
            })
            .status(),
            StatusCode::NOT_FOUND
        );
        assert_eq!(
            token_revoke_refusal_response(TokenRevokeRefusal::ConcurrentMutation { token_id })
                .status(),
            StatusCode::CONFLICT
        );
    }

    #[test]
    fn machine_removal_selects_the_canonical_name() {
        let machine_name = MachineName::try_new("machine-one").expect("machine name");
        let request = MachineRemoveRequest {
            machine_name: machine_name.clone(),
        };
        assert_eq!(
            machine_removal_reply(&request, [machine_name.clone()]),
            Ok(MachineRemoveReply::Removed { machine_name })
        );
    }
}
