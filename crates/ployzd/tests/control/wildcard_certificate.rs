use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use async_trait::async_trait;
use futures_util::StreamExt;
use ployz_core::certificate::{
    CertificateArtifactPushOk, CertificateArtifactPushRequest, CertificateArtifactPushResponse,
    LeaseBearerToken, ManagedCertBundle, ManagedLeaseAcquireRequest, ManagedLeaseAcquisitionId,
};
use ployz_core::ids::{MachineId, RouteBindingId};
use ployz_core::ingress::CertificateOwner;
use ployz_core::machine::rpc::MachineRpcResponse;
use ployz_core::operation::{CertOperationState, OperationStatus, RouteHostname};
use ployz_lease_worker::{LeaseWorkerRequest, LeaseWorkerResponse, StubLeaseWorker};
use ployz_nats::subjects::{MachineServiceEndpoint, machine_service};
use ployzd::certificate::{
    AcmeIssueContext, AcmeIssuer, AcmeIssuerError, CertificateManager, CertificateManagerConfig,
    GatewayCertificateTarget, IssuedCertificate,
};
use ployzd::control::operation_evidence::OperationRepository;
use ployzd::control::reconciler::certificate::{CertificateRenewalOutcome, run_once_at};
use ployzd::control::store::CoreStore;
use ployzd::roles::gateway::projection::{
    GatewayCertificateMaterialFailure, GatewayCertificateMaterialFailureKind,
    GatewayProjectionInput, GatewayProjectionUpdate,
};
use ployzd::roles::gateway::route_table::{GatewayProjector, GatewayServingState};

#[test]
fn cold_gateway_with_missing_required_material_is_unavailable_not_last_known_good() {
    let mut projector = GatewayProjector::new();

    let tick = projector.apply_source_update(GatewayProjectionUpdate::SourceAvailable(Box::new(
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
async fn wildcard_install_and_returning_gateway_sync_use_cert_operations_without_acme() {
    let first = MachineId::try_new("gateway_first").expect("machine id");
    let returning = MachineId::try_new("gateway_returning").expect("machine id");
    let nats = ployz_test_support::nats::TestNats::start_with_machines(&[
        first.clone(),
        returning.clone(),
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
    let outcome = run_once_at(&manager, &[target(first), target(returning)], u64::MAX)
        .await
        .expect("returning gateway synchronized");

    assert_eq!(
        outcome,
        CertificateRenewalOutcome::Attempted {
            attempted: 1,
            failed: 0,
        }
    );
    assert_eq!(first_pushes.load(Ordering::Relaxed), 2);
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
    client.flush().await.expect("flush subscription");
    let pushes = Arc::new(AtomicUsize::new(0));
    let recorded = Arc::clone(&pushes);
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
                    machine_id: machine_id.clone(),
                    cert_id: request.bundle.active_cert().cert_id.clone(),
                    digest: request.expected_digest,
                });
            client
                .publish(
                    reply,
                    serde_json::to_vec(&response).expect("response").into(),
                )
                .await
                .expect("publish response");
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
