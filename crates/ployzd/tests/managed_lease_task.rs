use std::sync::Arc;
use std::time::Duration;

use ployz_core::cert::ManagedLeaseName;
use ployz_core::cert::PublicUrlMode;
use ployz_core::ids::OperationId;
use ployz_core::ops::{ManagedLeaseOperationState, OperationStatus};
use ployz_lease_worker::{Clock, ClockError, StubLeaseWorker, serve};
use ployz_test_support::ids::machine_id;
use ployzd::core_store::CoreStore;
use ployzd::intent::lease_intent::LeaseIntentStore;
use ployzd::lease::task::{ManagedLeaseTaskOutcome, run_once};
use ployzd::lease::{LeaseClient, LeaseWorkerUrl};
use ployzd::operations::log::{ManagedLeaseOperationSubmission, OperationRepository};
use tokio::net::TcpListener;
use tokio::sync::Mutex;

// ponytail: The real stub worker runs in-process here; move it into DinD when the
// harness owns auxiliary-process lifecycle and request-count assertions.

#[tokio::test]
async fn auto_acquires_wildcard_bundle_and_completes_operation() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let intent = LeaseIntentStore::new(core_store.clone());
    intent
        .set_mode(PublicUrlMode::Auto)
        .await
        .expect("set auto mode");
    let repository = OperationRepository::open(core_store, nats.controller.clone());
    let (client, server) = stub_client(StubLeaseWorker::new()).await;

    let outcome = run_once(&intent, &repository, &client, &machine_id("core_1"))
        .await
        .expect("acquisition tick");
    let ManagedLeaseTaskOutcome::Acquired { operation_id } = outcome else {
        panic!("expected acquired outcome");
    };
    let stored = intent.load().await.expect("stored lease intent");
    let Some(lease) = stored.lease else {
        panic!("lease stored");
    };
    let Some(bundle) = stored.bundle else {
        panic!("bundle stored");
    };
    let status = repository
        .get(&operation_id)
        .await
        .expect("operation read")
        .expect("operation exists");

    assert_eq!(bundle.dns_names, lease.name.wildcard_and_apex());
    assert!(
        bundle
            .certificate_chain_pem
            .starts_with("-----BEGIN CERTIFICATE-----")
    );
    assert!(matches!(
        status,
        OperationStatus::ManagedLease {
            lease_name,
            state: ManagedLeaseOperationState::Completed,
            ..
        } if lease_name.as_str() == ployz_core::ops::MANAGED_LEASE_ACQUISITION_SUBJECT
    ));
    server.abort();
}

#[tokio::test]
async fn due_lease_renews_and_completes_operation() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let intent = LeaseIntentStore::new(core_store.clone());
    intent
        .set_mode(PublicUrlMode::Auto)
        .await
        .expect("set auto mode");
    let repository = OperationRepository::open(core_store, nats.controller.clone());
    let clock = ManualClock::new(1_700_000_000);
    let (client, server) = stub_client(StubLeaseWorker::with_clock(clock)).await;
    let acquired = client
        .acquire("core_1".to_owned())
        .await
        .expect("seed lease in worker");
    intent
        .store_lease(acquired.lease, acquired.bundle)
        .await
        .expect("seed local lease");

    let outcome = run_once(&intent, &repository, &client, &machine_id("core_1"))
        .await
        .expect("renewal tick");
    let ManagedLeaseTaskOutcome::Renewed { operation_id } = outcome else {
        panic!("expected renewed outcome");
    };
    let status = repository
        .get(&operation_id)
        .await
        .expect("operation read")
        .expect("operation exists");

    assert!(matches!(
        status,
        OperationStatus::ManagedLease {
            state: ManagedLeaseOperationState::Completed,
            ..
        }
    ));
    server.abort();
}

#[tokio::test]
async fn mirrored_lease_downloads_missing_local_bundle() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let intent = LeaseIntentStore::new(core_store.clone());
    let repository = OperationRepository::open(core_store, nats.controller.clone());
    let (client, server) = stub_client(StubLeaseWorker::new()).await;
    let acquired = client
        .acquire("core_1".to_owned())
        .await
        .expect("seed lease in worker");
    intent
        .restore_lease_record(acquired.lease)
        .await
        .expect("restore mirrored lease record");

    let outcome = run_once(&intent, &repository, &client, &machine_id("core_1"))
        .await
        .expect("bundle recovery tick");

    assert!(matches!(
        outcome,
        ManagedLeaseTaskOutcome::BundleDownloaded { .. }
    ));
    assert!(intent.load().await.expect("lease intent").bundle.is_some());
    server.abort();
}

#[tokio::test]
async fn none_mode_does_not_contact_worker() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let intent = LeaseIntentStore::new(core_store.clone());
    intent
        .set_mode(PublicUrlMode::None)
        .await
        .expect("set none mode");
    let repository = OperationRepository::open(core_store, nats.controller.clone());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind contact detector");
    let address = listener.local_addr().expect("detector address");
    let client =
        LeaseClient::new(LeaseWorkerUrl::try_new(format!("http://{address}")).expect("worker URL"));

    let outcome = tokio::time::timeout(
        Duration::from_millis(250),
        run_once(&intent, &repository, &client, &machine_id("core_1")),
    )
    .await
    .expect("none mode returns without an HTTP wait")
    .expect("none mode tick");

    assert_eq!(outcome, ManagedLeaseTaskOutcome::NoAction);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn accepted_operation_from_interrupted_tick_is_recovered_terminal() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let repository = OperationRepository::open(core_store, nats.controller.clone());
    let operation_id = OperationId::try_new("op_interrupted_lease").expect("operation id");
    repository
        .submit_managed_lease(ManagedLeaseOperationSubmission {
            operation_id: operation_id.clone(),
            lease_name: ManagedLeaseName::try_new("cluster-one").expect("lease name"),
        })
        .await
        .expect("accepted operation");

    ployzd::lease::task::recover_accepted_operations(&repository)
        .await
        .expect("recover accepted operation");
    let status = repository
        .get(&operation_id)
        .await
        .expect("status read")
        .expect("status exists");

    assert!(matches!(
        status,
        OperationStatus::ManagedLease {
            state: ManagedLeaseOperationState::Failed { .. },
            ..
        }
    ));
}

async fn stub_client<C: Clock + Send + 'static>(
    worker: StubLeaseWorker<C>,
) -> (LeaseClient, tokio::task::JoinHandle<()>) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub worker");
    let address = listener.local_addr().expect("stub worker address");
    let server = tokio::spawn(async move {
        let _ = serve(listener, Arc::new(Mutex::new(worker))).await;
    });
    let client =
        LeaseClient::new(LeaseWorkerUrl::try_new(format!("http://{address}")).expect("worker URL"));
    (client, server)
}

#[derive(Clone)]
struct ManualClock(u64);

impl ManualClock {
    fn new(now: u64) -> Self {
        Self(now)
    }
}

impl Clock for ManualClock {
    fn now_seconds(&self) -> Result<u64, ClockError> {
        Ok(self.0)
    }
}
