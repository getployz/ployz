//! Machine-scoped gateway RPC service.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use crate::roles::gateway::pingora::PingoraRouteRegistry;
use crate::roles::gateway::projection::{
    GatewayCertificateFailureAvailability, GatewayProjectionError,
};
use crate::roles::gateway::protocol::{
    GatewayStatusGetOk, GatewayStatusGetRequest, GatewayStatusGetResponse,
};
use crate::roles::gateway::route_table::GatewayProjector;
use crate::roles::gateway::source::{GatewayCertificateStore, GatewayCertificateStoreError};
use crate::service_catalog::{gateway_role_service, machine_endpoint_spec};
use ployz_core::cert::{
    CertificateArtifactPushOk, CertificateArtifactPushRequest, CertificateArtifactPushResponse,
    CertificateArtifactRemoveOk, CertificateArtifactRemoveRequest,
    CertificateArtifactRemoveResponse, CertificateChallengeApplyRequest,
    CertificateChallengeApplyResponse, CertificateChallengeMutationOk,
    CertificateChallengeRemoveRequest, CertificateChallengeRemoveResponse,
    CertificateChallengeStatusOk, CertificateChallengeStatusRequest,
    CertificateChallengeStatusResponse, GatewayCertificateRpcError,
};
use ployz_core::ids::MachineId;
use ployz_core::ops::FailureMessage;
use ployz_core::state::{GatewayServingStatus, GatewayStatusObservation};
use ployz_nats::service_runtime::{
    NatsClient, NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError,
    RunningNatsService, decode_json_request, start_nats_service,
};
use ployz_nats::subjects::MachineServiceEndpoint;

pub async fn start_gateway_certificate_service(
    client: NatsClient,
    machine_id: MachineId,
    certificate_store: GatewayCertificateStore,
    registry: PingoraRouteRegistry,
) -> Result<RunningNatsService, NatsServiceRuntimeError> {
    start_gateway_role_service(
        client,
        machine_id,
        certificate_store,
        registry,
        Arc::new(Mutex::new(GatewayProjector::new())),
        "0.0.0.0:0".parse().expect("static socket address"),
    )
    .await
}

pub(super) async fn start_gateway_role_service(
    client: NatsClient,
    machine_id: MachineId,
    certificate_store: GatewayCertificateStore,
    registry: PingoraRouteRegistry,
    gateway_runtime: Arc<Mutex<GatewayProjector>>,
    listen_addr: SocketAddr,
) -> Result<RunningNatsService, NatsServiceRuntimeError> {
    let mut service = start_nats_service(client, &gateway_role_service(&machine_id)).await?;
    let artifact_spec =
        machine_endpoint_spec(&machine_id, MachineServiceEndpoint::CertificateArtifactPush);
    let artifact_machine_id = machine_id.clone();
    let artifact_store = certificate_store.clone();
    if let Err(error) = service
        .bind_endpoint(&artifact_spec, move |request| {
            let machine_id = artifact_machine_id.clone();
            let store = artifact_store.clone();
            async move { handle_certificate_artifact_push(machine_id, store, request) }
        })
        .await
    {
        let _ = service.shutdown().await;
        return Err(error);
    }
    let remove_spec = machine_endpoint_spec(
        &machine_id,
        MachineServiceEndpoint::CertificateArtifactRemove,
    );
    let remove_machine_id = machine_id.clone();
    if let Err(error) = service
        .bind_endpoint(&remove_spec, move |request| {
            let machine_id = remove_machine_id.clone();
            let store = certificate_store.clone();
            async move { handle_certificate_artifact_remove(machine_id, store, request) }
        })
        .await
    {
        let _ = service.shutdown().await;
        return Err(error);
    }
    let apply_spec = machine_endpoint_spec(
        &machine_id,
        MachineServiceEndpoint::CertificateChallengeApply,
    );
    let apply_machine_id = machine_id.clone();
    let apply_registry = registry.clone();
    if let Err(error) = service
        .bind_endpoint(&apply_spec, move |request| {
            let machine_id = apply_machine_id.clone();
            let registry = apply_registry.clone();
            async move { handle_certificate_challenge_apply(machine_id, registry, request) }
        })
        .await
    {
        let _ = service.shutdown().await;
        return Err(error);
    }
    let remove_challenge_spec = machine_endpoint_spec(
        &machine_id,
        MachineServiceEndpoint::CertificateChallengeRemove,
    );
    let remove_challenge_machine_id = machine_id.clone();
    let remove_challenge_registry = registry.clone();
    if let Err(error) = service
        .bind_endpoint(&remove_challenge_spec, move |request| {
            let machine_id = remove_challenge_machine_id.clone();
            let registry = remove_challenge_registry.clone();
            async move { handle_certificate_challenge_remove(machine_id, registry, request) }
        })
        .await
    {
        let _ = service.shutdown().await;
        return Err(error);
    }
    let challenge_spec = machine_endpoint_spec(
        &machine_id,
        MachineServiceEndpoint::CertificateChallengeStatus,
    );
    let challenge_machine_id = machine_id.clone();
    if let Err(error) = service
        .bind_endpoint(&challenge_spec, move |request| {
            let machine_id = challenge_machine_id.clone();
            let registry = registry.clone();
            async move { handle_certificate_challenge_status(machine_id, registry, request) }
        })
        .await
    {
        let _ = service.shutdown().await;
        return Err(error);
    }
    let status_spec = machine_endpoint_spec(&machine_id, MachineServiceEndpoint::GatewayStatusGet);
    if let Err(error) = service
        .bind_endpoint(&status_spec, move |request| {
            let machine_id = machine_id.clone();
            let runtime = Arc::clone(&gateway_runtime);
            async move { handle_gateway_status_get(machine_id, listen_addr, &runtime, request) }
        })
        .await
    {
        let _ = service.shutdown().await;
        return Err(error);
    }
    Ok(service)
}

fn handle_gateway_status_get(
    machine_id: MachineId,
    listen_addr: SocketAddr,
    runtime: &Mutex<GatewayProjector>,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    if let Err(response) = decode_json_request::<GatewayStatusGetRequest>(&request) {
        return response;
    }
    let status = gateway_status_observation(&machine_id, listen_addr, runtime);
    NatsServiceResponse::json_ok(&GatewayStatusGetResponse::Ok(GatewayStatusGetOk {
        observation: status,
    }))
}

pub(super) fn gateway_status_observation(
    machine_id: &MachineId,
    listen_addr: SocketAddr,
    runtime: &Mutex<GatewayProjector>,
) -> GatewayStatusObservation {
    let runtime = runtime
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let state = runtime.state();
    let (serving, route_count) = match (&state.last_good, &state.last_error) {
        (Some(projection), None) => (GatewayServingStatus::Current, projection.routes.len()),
        (
            Some(_),
            Some(GatewayProjectionError::CertificateMaterial {
                availability: GatewayCertificateFailureAvailability::Unavailable,
                ..
            }),
        ) => (GatewayServingStatus::Unavailable, 0),
        (Some(projection), Some(_)) => {
            (GatewayServingStatus::LastKnownGood, projection.routes.len())
        }
        (None, Some(_)) | (None, None) => (GatewayServingStatus::Unavailable, 0),
    };
    GatewayStatusObservation {
        machine_id: machine_id.clone(),
        listen_addr,
        serving,
        route_count,
    }
}

fn handle_certificate_artifact_push(
    machine_id: MachineId,
    store: GatewayCertificateStore,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<CertificateArtifactPushRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    let cert_id = request.bundle.active_cert().cert_id.clone();
    let digest = request.expected_digest.clone();
    match store.push_at(&request, current_unix_seconds()) {
        Ok(()) => NatsServiceResponse::json_ok(&CertificateArtifactPushResponse::Ok(
            CertificateArtifactPushOk {
                machine_id,
                cert_id,
                digest,
            },
        )),
        Err(error) => {
            let rpc_error = match &error {
                GatewayCertificateStoreError::ArtifactFile { .. } => {
                    GatewayCertificateRpcError::ArtifactStoreFailed {
                        message: failure_message(error.to_string()),
                    }
                }
                GatewayCertificateStoreError::SizeMismatch { .. }
                | GatewayCertificateStoreError::DigestMismatch { .. }
                | GatewayCertificateStoreError::InvalidMaterial { .. }
                | GatewayCertificateStoreError::NotUsable { .. } => {
                    GatewayCertificateRpcError::InvalidRequest {
                        message: failure_message(error.to_string()),
                    }
                }
            };
            NatsServiceResponse::json_domain_error(&CertificateArtifactPushResponse::DomainError {
                machine_id,
                error: rpc_error,
            })
        }
    }
}

fn handle_certificate_artifact_remove(
    machine_id: MachineId,
    store: GatewayCertificateStore,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<CertificateArtifactRemoveRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    match store.remove(&request) {
        Ok(()) => NatsServiceResponse::json_ok(&CertificateArtifactRemoveResponse::Ok(
            CertificateArtifactRemoveOk {
                machine_id,
                cert_id: request.cert_id,
                digest: request.digest,
            },
        )),
        Err(error) => NatsServiceResponse::json_domain_error(
            &CertificateArtifactRemoveResponse::DomainError {
                machine_id,
                error: GatewayCertificateRpcError::ArtifactStoreFailed {
                    message: failure_message(error.to_string()),
                },
            },
        ),
    }
}

fn handle_certificate_challenge_apply(
    machine_id: MachineId,
    registry: PingoraRouteRegistry,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<CertificateChallengeApplyRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    registry.apply_challenge(&request.challenge);
    NatsServiceResponse::json_ok(&CertificateChallengeApplyResponse::Ok(
        CertificateChallengeMutationOk { machine_id },
    ))
}

fn handle_certificate_challenge_remove(
    machine_id: MachineId,
    registry: PingoraRouteRegistry,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<CertificateChallengeRemoveRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    registry.remove_challenge(&request.challenge);
    NatsServiceResponse::json_ok(&CertificateChallengeRemoveResponse::Ok(
        CertificateChallengeMutationOk { machine_id },
    ))
}

fn handle_certificate_challenge_status(
    machine_id: MachineId,
    registry: PingoraRouteRegistry,
    request: NatsServiceRequest,
) -> NatsServiceResponse {
    let request = match decode_json_request::<CertificateChallengeStatusRequest>(&request) {
        Ok(request) => request,
        Err(response) => return response,
    };
    NatsServiceResponse::json_ok(&CertificateChallengeStatusResponse::Ok(
        CertificateChallengeStatusOk {
            machine_id,
            application: registry.challenge_application_status(&request.challenge),
        },
    ))
}

pub(super) fn current_unix_seconds() -> u64 {
    let Ok(elapsed) = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH) else {
        return 0;
    };
    elapsed.as_secs()
}

fn failure_message(message: String) -> FailureMessage {
    let Ok(message) = FailureMessage::try_new(message) else {
        unreachable!("rendered gateway certificate failures are non-empty")
    };
    message
}
