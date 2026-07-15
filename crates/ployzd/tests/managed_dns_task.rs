use std::sync::Arc;

use ployz_core::certificate::ManagedLeaseRenewRequest;
use ployz_core::ingress::{
    AutomaticHostnameConfiguration, IngressConfiguration, IngressEndpointProjection,
    IngressEndpointProjectionState, IngressEndpointSet, PloyzDnsTargetIntent,
};
use ployz_core::intent::recovery::ControlPlaneEpoch;
use ployz_core::operation::{
    ManagedDnsReconcileOperationState, ManagedDnsReconcileSubject, OperationStatus,
};
use ployz_lease_worker::{StubLeaseWorker, serve};
use ployzd::core_store::CoreStore;
use ployzd::intent::ingress_intent::{
    IngressIntentStore, ManagedDnsCheckpoint, PloyzDnsTargetAllocation, PloyzDnsTargetStore,
};
use ployzd::lease::task::{ManagedDnsTaskOutcome, reconcile_once};
use ployzd::lease::{LeaseClient, LeaseWorkerUrl};
use ployzd::operations::log::OperationRepository;
use tokio::net::TcpListener;
use tokio::sync::Mutex;

#[tokio::test]
async fn enabled_target_acquires_once_with_visible_secret_free_operation() {
    let fixture = Fixture::new().await;

    let outcome = fixture.reconcile(None, 1_700_000_000).await;
    let ManagedDnsTaskOutcome::Reconciled { operation_id } = outcome else {
        panic!("expected acquisition");
    };
    assert!(matches!(
        fixture.target.load_allocation().await.expect("allocation"),
        Some(PloyzDnsTargetAllocation::Allocated { .. })
    ));
    let status = fixture
        .repository
        .get(&operation_id)
        .await
        .expect("status")
        .expect("operation");
    assert!(matches!(
        status,
        OperationStatus::ManagedDnsReconcile {
            subject: ManagedDnsReconcileSubject::Acquire,
            state: ManagedDnsReconcileOperationState::Completed,
            ..
        }
    ));
    assert!(
        !serde_json::to_string(&status)
            .expect("status JSON")
            .contains("token")
    );

    assert_eq!(
        fixture.reconcile(None, 1_700_000_001).await,
        ManagedDnsTaskOutcome::AwaitingProjection
    );
}

#[tokio::test]
async fn current_projection_replaces_the_complete_set_and_checkpoint_gates_reapply() {
    let fixture = Fixture::new().await;
    fixture.reconcile(None, 1_700_000_000).await;
    let projection = projection(IngressEndpointProjectionState::Current {
        endpoints: endpoints(),
    });

    assert!(matches!(
        fixture.reconcile(Some(&projection), 1_700_000_001).await,
        ManagedDnsTaskOutcome::Reconciled { .. }
    ));
    let Some(PloyzDnsTargetAllocation::Allocated { lease }) =
        fixture.target.load_allocation().await.expect("allocation")
    else {
        panic!("allocated lease");
    };
    assert_eq!(
        fixture.worker.lock().await.renewal_request(&lease.name),
        Some(&ManagedLeaseRenewRequest {
            ipv4: vec!["8.8.8.8".parse().expect("IPv4")],
            ipv6: vec!["2001:4860:4860::8888".parse().expect("IPv6")],
        })
    );
    assert!(matches!(
        fixture.target.load_checkpoint().await.expect("checkpoint"),
        Some(ManagedDnsCheckpoint::Applied {
            last_applied_identity: Some(identity),
            ..
        }) if identity == projection.identity()
    ));
    assert_eq!(
        fixture.reconcile(Some(&projection), 1_700_000_002).await,
        ManagedDnsTaskOutcome::NoAction
    );
}

#[tokio::test]
async fn pending_and_get_error_do_not_publish_before_renewal_is_due() {
    let fixture = Fixture::new().await;
    fixture.reconcile(None, 1_700_000_000).await;
    let pending = projection(IngressEndpointProjectionState::Pending);

    assert_eq!(
        fixture.reconcile(Some(&pending), 1_700_000_001).await,
        ManagedDnsTaskOutcome::NoAction
    );
    assert_eq!(
        fixture.reconcile(None, 1_700_000_001).await,
        ManagedDnsTaskOutcome::AwaitingProjection
    );
}

struct Fixture {
    _nats: ployz_test_support::nats::TestNats,
    ingress: IngressIntentStore,
    target: PloyzDnsTargetStore,
    repository: OperationRepository,
    client: LeaseClient,
    worker: Arc<Mutex<StubLeaseWorker>>,
    _server: tokio::task::JoinHandle<()>,
}

impl Fixture {
    async fn new() -> Self {
        let nats = ployz_test_support::nats::TestNats::start().await;
        let store = CoreStore::open_in_memory().await.expect("store");
        let ingress = IngressIntentStore::new(store.clone());
        ingress
            .replace(
                IngressConfiguration::try_new(
                    AutomaticHostnameConfiguration::Disabled,
                    PloyzDnsTargetIntent::Enabled,
                )
                .expect("valid ingress configuration"),
            )
            .await
            .expect("configure ingress");
        let target = PloyzDnsTargetStore::new(store.clone());
        let repository = OperationRepository::open(store, nats.controller.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("listener");
        let address = listener.local_addr().expect("worker address");
        let worker = Arc::new(Mutex::new(StubLeaseWorker::new()));
        let service_worker = Arc::clone(&worker);
        let server = tokio::spawn(async move {
            let _ = serve(listener, service_worker).await;
        });
        let client = LeaseClient::new(
            LeaseWorkerUrl::try_new(format!("http://{address}")).expect("worker URL"),
        );
        Self {
            _nats: nats,
            ingress,
            target,
            repository,
            client,
            worker,
            _server: server,
        }
    }

    async fn reconcile(
        &self,
        projection: Option<&IngressEndpointProjection>,
        now_seconds: u64,
    ) -> ManagedDnsTaskOutcome {
        reconcile_once(
            &self.ingress,
            &self.target,
            &self.repository,
            &self.client,
            projection,
            now_seconds,
        )
        .await
        .expect("reconcile")
    }
}

fn projection(state: IngressEndpointProjectionState) -> IngressEndpointProjection {
    IngressEndpointProjection {
        control_plane_epoch: ControlPlaneEpoch::initial(),
        revision: 1,
        state,
    }
}

fn endpoints() -> IngressEndpointSet {
    IngressEndpointSet::try_new(
        ["8.8.8.8".parse().expect("IPv4")],
        ["2001:4860:4860::8888".parse().expect("IPv6")],
    )
    .expect("endpoints")
}
