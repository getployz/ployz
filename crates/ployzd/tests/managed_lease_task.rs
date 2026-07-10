use std::sync::Arc;
use std::time::Duration;

use ployz_core::cert::{
    AutoLeaseState, ManagedLeaseAcquireRequest, ManagedLeaseIntent, ManagedLeaseName, PublicUrlMode,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{
    ManagedLeaseFailureClass, ManagedLeaseOperationState, ManagedLeaseSubject, OperationStatus,
};
use ployz_lease_worker::{Clock, ClockError, StubLeaseWorker, serve};
use ployzd::core_store::CoreStore;
use ployzd::fact_cache::FactCache;
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

    let outcome = run_once(&intent, &repository, &client, &FactCache::default())
        .await
        .expect("acquisition tick");
    let ManagedLeaseTaskOutcome::Acquired { operation_id } = outcome else {
        panic!("expected acquired outcome");
    };
    let stored = intent.load().await.expect("stored lease intent");
    let ManagedLeaseIntent::Auto { state } = stored else {
        panic!("ready lease stored");
    };
    let AutoLeaseState::Ready { lease, bundle } = *state else {
        panic!("ready lease stored");
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
            subject: ManagedLeaseSubject::Acquire,
            state: ManagedLeaseOperationState::Completed,
            ..
        }
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
        .acquire(ManagedLeaseAcquireRequest {
            ipv4: Vec::new(),
            ipv6: Vec::new(),
        })
        .await
        .expect("seed lease in worker");
    intent
        .store_lease(acquired.lease, acquired.bundle)
        .await
        .expect("seed local lease");

    let outcome = run_once(&intent, &repository, &client, &FactCache::default())
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
async fn due_certificate_refresh_downloads_bundle_without_renewing_lease() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let intent = LeaseIntentStore::new(core_store.clone());
    intent
        .set_mode(PublicUrlMode::Auto)
        .await
        .expect("auto mode");
    let repository = OperationRepository::open(core_store, nats.controller.clone());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let (client, server) = stub_client(StubLeaseWorker::with_clock(ManualClock::new(now))).await;
    let acquired = client
        .acquire(ManagedLeaseAcquireRequest {
            ipv4: Vec::new(),
            ipv6: Vec::new(),
        })
        .await
        .expect("seed lease");
    let lease = acquired.lease.clone();
    let short_bundle = ployz_core::cert::ManagedCertBundle::try_new(
        lease.name.clone(),
        lease.name.wildcard_and_apex(),
        acquired.bundle.certificate_chain_pem,
        acquired.bundle.private_key_pem,
        ployz_core::cert::LeaseIssuedAt::try_new(now - 90).expect("issued"),
        ployz_core::cert::LeaseExpiresAt::try_new(now + 10).expect("expires"),
    )
    .expect("short bundle");
    intent
        .store_lease(lease.clone(), short_bundle)
        .await
        .expect("store local lease");

    let outcome = run_once(&intent, &repository, &client, &FactCache::default())
        .await
        .expect("refresh tick");

    assert!(matches!(
        outcome,
        ManagedLeaseTaskOutcome::BundleDownloaded { .. }
    ));
    let ManagedLeaseIntent::Auto { state } = intent.load().await.expect("intent") else {
        panic!("auto intent");
    };
    let AutoLeaseState::Ready { lease: stored, .. } = *state else {
        panic!("ready lease");
    };
    assert_eq!(stored, lease);
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
        .acquire(ManagedLeaseAcquireRequest {
            ipv4: Vec::new(),
            ipv6: Vec::new(),
        })
        .await
        .expect("seed lease in worker");
    intent
        .restore_lease_record(acquired.lease)
        .await
        .expect("restore mirrored lease record");

    let outcome = run_once(&intent, &repository, &client, &FactCache::default())
        .await
        .expect("bundle recovery tick");

    assert!(matches!(
        outcome,
        ManagedLeaseTaskOutcome::BundleDownloaded { .. }
    ));
    let ManagedLeaseIntent::Auto { state } = intent.load().await.expect("lease intent") else {
        panic!("auto lease intent");
    };
    assert!(matches!(*state, AutoLeaseState::Ready { .. }));
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
        run_once(&intent, &repository, &client, &FactCache::default()),
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
            subject: ManagedLeaseSubject::Renew {
                lease: ManagedLeaseName::try_new("cluster-one").expect("lease name"),
            },
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
            state: ManagedLeaseOperationState::Failed {
                failure: ployz_core::ops::ManagedLeaseOperationFailure {
                    class: ManagedLeaseFailureClass::Interrupted,
                    ..
                }
            },
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
