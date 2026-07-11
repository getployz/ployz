//! Machine-scoped gateway certificate RPC service.

use crate::roles::gateway::pingora::PingoraRouteRegistry;
use crate::roles::gateway::source::{GatewayCertificateStore, GatewayCertificateStoreError};
use crate::service_catalog::{gateway_role_service, machine_endpoint_spec};
use ployz_core::cert::{
    CertificateArtifactPushOk, CertificateArtifactPushRequest, CertificateArtifactPushResponse,
    CertificateChallengeStatusOk, CertificateChallengeStatusRequest,
    CertificateChallengeStatusResponse, GatewayCertificateRpcError,
};
use ployz_core::ids::MachineId;
use ployz_core::ops::FailureMessage;
use ployz_core::subjects::MachineServiceEndpoint;
use ployz_nats::service_runtime::{
    NatsClient, NatsServiceRequest, NatsServiceResponse, NatsServiceRuntimeError,
    RunningNatsService, decode_json_request, start_nats_service,
};

pub async fn start_gateway_certificate_service(
    client: NatsClient,
    machine_id: MachineId,
    certificate_store: GatewayCertificateStore,
    registry: PingoraRouteRegistry,
) -> Result<RunningNatsService, NatsServiceRuntimeError> {
    let mut runtime = start_nats_service(client, &gateway_role_service(&machine_id)).await?;
    let artifact_spec =
        machine_endpoint_spec(&machine_id, MachineServiceEndpoint::CertificateArtifactPush);
    let artifact_machine_id = machine_id.clone();
    if let Err(error) = runtime
        .bind_endpoint(&artifact_spec, move |request| {
            let machine_id = artifact_machine_id.clone();
            let store = certificate_store.clone();
            async move { handle_certificate_artifact_push(machine_id, store, request) }
        })
        .await
    {
        let _ = runtime.shutdown().await;
        return Err(error);
    }
    let challenge_spec = machine_endpoint_spec(
        &machine_id,
        MachineServiceEndpoint::CertificateChallengeStatus,
    );
    if let Err(error) = runtime
        .bind_endpoint(&challenge_spec, move |request| {
            let machine_id = machine_id.clone();
            let registry = registry.clone();
            async move { handle_certificate_challenge_status(machine_id, registry, request) }
        })
        .await
    {
        let _ = runtime.shutdown().await;
        return Err(error);
    }
    Ok(runtime)
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
