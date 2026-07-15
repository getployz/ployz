use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use crate::certificate::{
    AcmeIssueContext, AcmeIssuer, AcmeIssuerError, CertificateManager, CertificateManagerConfig,
    GatewayCertificateTarget, IssuedCertificate,
};
use crate::control::operation_evidence::OperationRepository;
use crate::control::reconciler::certificate::{CertificateRenewalOutcome, run_once_at};
use crate::control::store::CoreStore;
use crate::roles::gateway::projection::{
    GatewayCertificateMaterialFailure, GatewayCertificateMaterialFailureKind,
    GatewayProjectionInput, GatewayProjectionUpdate,
};
use crate::roles::gateway::route_table::{GatewayProjector, GatewayServingState};
use async_trait::async_trait;
use futures_util::StreamExt;
use ployz_core::certificate::{
    CertificateArtifactPushOk, CertificateArtifactPushRequest, CertificateArtifactPushResponse,
    CertificateArtifactStatusOk, CertificateArtifactStatusRequest,
    CertificateArtifactStatusResponse, LeaseBearerToken, ManagedCertBundle,
    ManagedLeaseAcquireRequest, ManagedLeaseAcquisitionId,
};
use ployz_core::ids::{MachineId, RouteBindingId};
use ployz_core::ingress::CertificateOwner;
use ployz_core::machine::rpc::MachineRpcResponse;
use ployz_core::operation::{CertOperationState, OperationStatus, RouteHostname};
use ployz_nats::subjects::{MachineServiceEndpoint, machine_service};
use ployz_test_lease_worker::{LeaseWorkerRequest, LeaseWorkerResponse, StubLeaseWorker};

#[test]
fn cold_gateway_with_missing_required_material_is_unavailable_not_last_known_good() {
    let mut projector = GatewayProjector::new();

    let tick = projector.apply_source_update(GatewayProjectionUpdate::Available(Box::new(
        GatewayProjectionInput {
            certificate_bundles: Vec::new(),
            certificate_failures: vec![GatewayCertificateMaterialFailure {
                owner: CertificateOwner::RouteBinding {
                    route_binding_id: RouteBindingId::try_new("route_app").expect("route id"),
                },
                kind: GatewayCertificateMaterialFailureKind::MissingOrInvalid,
                message: "certificate artifact missing".to_owned(),
            }],
            challenges: Vec::new(),
            routes: Vec::new(),
            serving: Vec::new(),
            observed_machines: Vec::new(),
        },
    )));

    assert!(matches!(
        tick.serving,
        GatewayServingState::Unavailable { .. }
    ));
}

#[tokio::test]
async fn returning_gateway_is_repaired_even_when_another_gateway_is_silent() {
    let first = MachineId::try_new("gateway_first").expect("machine id");
    let returning = MachineId::try_new("gateway_returning").expect("machine id");
    let silent = MachineId::try_new("gateway_silent").expect("machine id");
    let nats = ployz_test_support::nats::TestNats::start_with_machines(&[
        first.clone(),
        returning.clone(),
        silent.clone(),
    ])
    .await;
    let first_pushes = start_gateway(&nats, first.clone()).await;
    let returning_pushes = start_gateway(&nats, returning.clone()).await;
    let core = CoreStore::open_in_memory().await.expect("core store");
    let state = tempfile::tempdir().expect("certificate state");
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system time")
        .as_secs();
    let manager = CertificateManager::with_issuer_and_time(
        core.clone(),
        nats.controller.clone(),
        CertificateManagerConfig {
            state_dir: state.path().to_path_buf(),
            ..CertificateManagerConfig::for_core_db(state.path())
        },
        Arc::new(PanicIssuer),
        Arc::new(move || now),
    );

    manager
        .install_ployz_wildcard(worker_bundle(), &[target(first.clone())])
        .await
        .expect("wildcard installed");
    let outcome = run_once_at(
        &manager,
        &[target(first), target(returning), target(silent.clone())],
        u64::MAX,
    )
    .await
    .expect("known missing gateway is repaired");

    assert!(matches!(
        outcome,
        CertificateRenewalOutcome::Degraded { attempted: 1, failed: 0, unknown }
            if matches!(unknown.as_slice(), [failure] if failure.machine_id == silent)
    ));
    assert_eq!(first_pushes.load(Ordering::Relaxed), 1);
    assert_eq!(returning_pushes.load(Ordering::Relaxed), 1);
    let statuses = OperationRepository::open(core, nats.controller)
        .operation_statuses()
        .await
        .expect("operation statuses");
    assert_eq!(
        statuses
            .iter()
            .filter(|status| matches!(
                status,
                OperationStatus::Cert {
                    state: CertOperationState::Completed,
                    ..
                }
            ))
            .count(),
        2
    );
}

async fn start_gateway(
    nats: &ployz_test_support::nats::TestNats,
    machine_id: MachineId,
) -> Arc<AtomicUsize> {
    let client = nats.machine_client(&machine_id).await;
    let mut requests = client
        .subscribe(machine_service(
            &machine_id,
            MachineServiceEndpoint::CertificateArtifactPush,
        ))
        .await
        .expect("subscribe");
    let mut statuses = client
        .subscribe(machine_service(
            &machine_id,
            MachineServiceEndpoint::CertificateArtifactStatus,
        ))
        .await
        .expect("subscribe status");
    client.flush().await.expect("flush subscription");
    let pushes = Arc::new(AtomicUsize::new(0));
    let stored_cert_ids = Arc::new(std::sync::Mutex::new(std::collections::BTreeSet::new()));
    let recorded = Arc::clone(&pushes);
    let pushed_cert_ids = Arc::clone(&stored_cert_ids);
    let push_client = client.clone();
    let push_machine_id = machine_id.clone();
    tokio::spawn(async move {
        while let Some(message) = requests.next().await {
            let Some(reply) = message.reply else {
                continue;
            };
            let request: CertificateArtifactPushRequest =
                serde_json::from_slice(&message.payload).expect("push request");
            recorded.fetch_add(1, Ordering::Relaxed);
            let response: CertificateArtifactPushResponse =
                MachineRpcResponse::Ok(CertificateArtifactPushOk {
                    machine_id: push_machine_id.clone(),
                    cert_id: request.bundle.active_cert().cert_id.clone(),
                    digest: request.expected_digest,
                });
            pushed_cert_ids
                .lock()
                .expect("stored cert ids")
                .insert(request.bundle.active_cert().cert_id.clone());
            push_client
                .publish(
                    reply,
                    serde_json::to_vec(&response).expect("response").into(),
                )
                .await
                .expect("publish response");
        }
    });
    let status_cert_ids = Arc::clone(&stored_cert_ids);
    tokio::spawn(async move {
        while let Some(message) = statuses.next().await {
            let Some(reply) = message.reply else {
                continue;
            };
            let request: CertificateArtifactStatusRequest =
                serde_json::from_slice(&message.payload).expect("status request");
            let missing_cert_ids = {
                let stored = status_cert_ids.lock().expect("stored cert ids");
                request
                    .desired
                    .into_iter()
                    .filter(|certificate| !stored.contains(&certificate.cert_id))
                    .map(|certificate| certificate.cert_id)
                    .collect()
            };
            let response: CertificateArtifactStatusResponse =
                MachineRpcResponse::Ok(CertificateArtifactStatusOk {
                    machine_id: machine_id.clone(),
                    missing_cert_ids,
                });
            client
                .publish(
                    reply,
                    serde_json::to_vec(&response).expect("response").into(),
                )
                .await
                .expect("publish status response");
        }
    });
    pushes
}

fn target(machine_id: MachineId) -> GatewayCertificateTarget {
    GatewayCertificateTarget {
        machine_id,
        public_ips: Vec::new(),
    }
}

fn worker_bundle() -> ManagedCertBundle {
    let mut worker = StubLeaseWorker::new();
    let LeaseWorkerResponse::LeaseAcquired(acquired) = worker
        .handle(LeaseWorkerRequest::Acquire(ManagedLeaseAcquireRequest {
            acquisition_id: ManagedLeaseAcquisitionId::try_new("a1").expect("acquisition id"),
            token: LeaseBearerToken::try_new("token").expect("token"),
            ipv4: Vec::new(),
            ipv6: Vec::new(),
        }))
        .expect("acquire")
    else {
        panic!("acquire response");
    };
    let _ = worker
        .handle(LeaseWorkerRequest::DownloadBundle {
            lease: acquired.lease.name.clone(),
            token: acquired.lease.token.clone(),
        })
        .expect("pending bundle");
    let LeaseWorkerResponse::Bundle(bundle) = worker
        .handle(LeaseWorkerRequest::DownloadBundle {
            lease: acquired.lease.name,
            token: acquired.lease.token,
        })
        .expect("ready bundle")
    else {
        panic!("ready bundle response");
    };
    bundle
}

struct PanicIssuer;

#[async_trait]
impl AcmeIssuer for PanicIssuer {
    async fn issue_http01(
        &self,
        _context: &AcmeIssueContext,
        _hostname: &RouteHostname,
    ) -> Result<IssuedCertificate, AcmeIssuerError> {
        panic!("Ployz wildcard must not use exact ACME issuance")
    }
}
