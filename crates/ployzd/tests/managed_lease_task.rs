use std::net::{Ipv4Addr, Ipv6Addr};
use std::sync::Arc;
use std::time::Duration;

use ployz_core::cert::{
    AutoLeaseState, LeaseBearerToken, ManagedLeaseAcquireRequest, ManagedLeaseAcquisitionId,
    ManagedLeaseAddressSet, ManagedLeaseIntent, ManagedLeaseName, ManagedLeaseRenewRequest,
    PublicUrlMode,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{
    ManagedLeaseFailureClass, ManagedLeaseOperationState, ManagedLeaseSubject, OperationStatus,
};
use ployz_lease_worker::{
    Clock, ClockError, LeaseWorkerRequest, LeaseWorkerResponse, StubLeaseWorker, serve,
};
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
async fn auto_acquires_lease_record_and_completes_operation() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let intent = LeaseIntentStore::new(core_store.clone());
    intent
        .set_mode(PublicUrlMode::Auto)
        .await
        .expect("set auto mode");
    let repository = OperationRepository::open(core_store, nats.controller.clone());
    let (client, server) = stub_client(StubLeaseWorker::new()).await;

    let outcome = run_once(&intent, &repository, &client, empty_request())
        .await
        .expect("acquisition tick");
    let ManagedLeaseTaskOutcome::Acquired { operation_id } = outcome else {
        panic!("expected acquired outcome");
    };
    let stored = intent.load().await.expect("stored lease intent");
    let ManagedLeaseIntent::Auto { state } = stored else {
        panic!("lease stored");
    };
    let AutoLeaseState::RecordOnly { .. } = *state else {
        panic!("lease record stored");
    };
    let status = repository
        .get(&operation_id)
        .await
        .expect("operation read")
        .expect("operation exists");

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
async fn failed_acquisition_retries_with_the_same_identity_after_a_lost_response() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let intent = LeaseIntentStore::new(core_store.clone());
    intent
        .set_mode(PublicUrlMode::Auto)
        .await
        .expect("set auto mode");
    let repository = OperationRepository::open(core_store, nats.controller.clone());
    let unavailable = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("reserve unavailable address");
    let unavailable_address = unavailable.local_addr().expect("unavailable address");
    drop(unavailable);
    let unavailable_client = LeaseClient::new(
        LeaseWorkerUrl::try_new(format!("http://{unavailable_address}"))
            .expect("unavailable worker URL"),
    );

    assert!(matches!(
        run_once(&intent, &repository, &unavailable_client, empty_request())
            .await
            .expect("failed acquisition operation"),
        ManagedLeaseTaskOutcome::Failed { .. }
    ));
    let ManagedLeaseIntent::Auto { state } = intent.load().await.expect("persisted intent") else {
        panic!("auto intent");
    };
    let AutoLeaseState::Unacquired {
        acquisition_id,
        token,
    } = *state
    else {
        panic!("unacquired intent");
    };
    let request = ManagedLeaseAcquireRequest {
        acquisition_id,
        token,
        ipv4: Vec::new(),
        ipv6: Vec::new(),
    };
    let mut worker = StubLeaseWorker::new();
    let LeaseWorkerResponse::LeaseAcquired(processed) = worker
        .handle(LeaseWorkerRequest::Acquire(request))
        .expect("worker processed request before response was lost")
    else {
        panic!("lease acquired");
    };
    let (client, server) = stub_client(worker).await;

    assert!(matches!(
        run_once(&intent, &repository, &client, empty_request())
            .await
            .expect("retry acquisition"),
        ManagedLeaseTaskOutcome::Acquired { .. }
    ));
    let ManagedLeaseIntent::Auto { state } = intent.load().await.expect("stored lease") else {
        panic!("auto intent");
    };
    let AutoLeaseState::RecordOnly { lease } = *state else {
        panic!("record-only intent");
    };
    assert_eq!(lease, processed.lease);
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
    let (client, server) =
        stub_client(StubLeaseWorker::with_clock(ManualClock::new(1_700_000_000))).await;
    run_once(&intent, &repository, &client, empty_request())
        .await
        .expect("acquire lease");
    let pending = run_once(&intent, &repository, &client, empty_request())
        .await
        .expect("pending bundle tick");
    assert_eq!(pending, ManagedLeaseTaskOutcome::AwaitingCertificate);
    run_once(&intent, &repository, &client, empty_request())
        .await
        .expect("download bundle");

    let outcome = run_once(
        &intent,
        &repository,
        &client,
        gateway_request(&["203.0.113.8"], &[]),
    )
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
    run_once(&intent, &repository, &client, empty_request())
        .await
        .expect("acquire lease");
    run_once(&intent, &repository, &client, empty_request())
        .await
        .expect("pending bundle tick");
    run_once(&intent, &repository, &client, empty_request())
        .await
        .expect("download bundle");
    let ManagedLeaseIntent::Auto { state } = intent.load().await.expect("ready intent") else {
        panic!("auto intent");
    };
    let AutoLeaseState::Ready { lease, bundle } = *state else {
        panic!("ready lease");
    };
    let short_bundle = ployz_core::cert::ManagedCertBundle::try_new(
        lease.name.clone(),
        lease.name.wildcard_and_apex(),
        bundle.certificate_chain_pem,
        bundle.private_key_pem,
        ployz_core::cert::LeaseIssuedAt::try_new(now - 90).expect("issued"),
        ployz_core::cert::LeaseExpiresAt::try_new(now + 10).expect("expires"),
    )
    .expect("short bundle");
    intent
        .store_lease(lease.clone(), Some(short_bundle))
        .await
        .expect("store local lease");

    let outcome = run_once(&intent, &repository, &client, empty_request())
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
        .acquire(acquire_request("a1"))
        .await
        .expect("seed lease in worker");
    intent
        .restore_lease_record(acquired.lease)
        .await
        .expect("store mirrored lease record");

    let pending = run_once(&intent, &repository, &client, empty_request())
        .await
        .expect("pending bundle recovery tick");

    assert_eq!(pending, ManagedLeaseTaskOutcome::AwaitingCertificate);
    assert!(matches!(
        intent.load().await.expect("record-only intent"),
        ManagedLeaseIntent::Auto { state }
            if matches!(*state, AutoLeaseState::RecordOnly { .. })
    ));

    let outcome = run_once(&intent, &repository, &client, empty_request())
        .await
        .expect("ready bundle recovery tick");

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
async fn ready_refresh_pending_preserves_ready_state() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let intent = LeaseIntentStore::new(core_store.clone());
    intent
        .set_mode(PublicUrlMode::Auto)
        .await
        .expect("set auto mode");
    let repository = OperationRepository::open(core_store, nats.controller.clone());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let (client, server) = stub_client(StubLeaseWorker::with_clock(ManualClock::new(now))).await;
    let acquired = client
        .acquire(acquire_request("a2"))
        .await
        .expect("seed lease in worker");
    let bundle = ployz_core::cert::ManagedCertBundle::try_new(
        acquired.lease.name.clone(),
        acquired.lease.name.wildcard_and_apex(),
        "certificate".to_owned(),
        "private-key".to_owned(),
        ployz_core::cert::LeaseIssuedAt::try_new(now - 90).expect("issued"),
        ployz_core::cert::LeaseExpiresAt::try_new(now + 10).expect("expires"),
    )
    .expect("bundle");
    intent
        .store_successful_lease_application(
            acquired.lease.clone(),
            Some(bundle.clone()),
            empty_address_set(),
        )
        .await
        .expect("store ready lease");

    let outcome = run_once(&intent, &repository, &client, empty_request())
        .await
        .expect("pending refresh tick");

    assert_eq!(outcome, ManagedLeaseTaskOutcome::AwaitingCertificate);
    assert_eq!(
        intent.load().await.expect("ready intent"),
        ManagedLeaseIntent::Auto {
            state: Box::new(AutoLeaseState::Ready {
                lease: acquired.lease,
                bundle,
            })
        }
    );
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
        run_once(&intent, &repository, &client, empty_request()),
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
async fn changed_gateway_addresses_renew_record_only_lease_with_canonical_payload() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let intent = LeaseIntentStore::new(core_store.clone());
    intent
        .set_mode(PublicUrlMode::Auto)
        .await
        .expect("set auto mode");
    let repository = OperationRepository::open(core_store, nats.controller.clone());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let (client, worker, server) =
        stub_client_with_worker(StubLeaseWorker::with_clock(ManualClock::new(now))).await;
    let initial = gateway_request(&["203.0.113.8"], &[]);
    let acquired = client
        .acquire(acquire_request("b1"))
        .await
        .expect("seed lease");
    intent
        .store_successful_lease_application(acquired.lease.clone(), None, address_set(&initial))
        .await
        .expect("store applied lease");
    let changed = gateway_request(
        &["203.0.113.9", "203.0.113.8", "203.0.113.9"],
        &["2001:db8::8", "2001:db8::8"],
    );
    let expected_request = gateway_request(&["203.0.113.8", "203.0.113.9"], &["2001:db8::8"]);
    let expected_addresses = address_set(&expected_request);

    let outcome = run_once(&intent, &repository, &client, changed)
        .await
        .expect("address-change tick");
    let ManagedLeaseTaskOutcome::Renewed { operation_id } = outcome else {
        panic!("expected address-change renewal");
    };
    let status = repository
        .get(&operation_id)
        .await
        .expect("operation read")
        .expect("operation exists");

    assert!(matches!(
        status,
        OperationStatus::ManagedLease {
            subject: ManagedLeaseSubject::Renew {
                lease,
                addresses,
            },
            state: ManagedLeaseOperationState::Completed,
            ..
        } if lease == acquired.lease.name && *addresses == expected_addresses
    ));
    assert_eq!(
        worker.lock().await.renewal_request(&acquired.lease.name),
        Some(&expected_request)
    );
    assert!(
        !intent
            .applied_address_set_differs(&expected_addresses)
            .await
            .expect("stored applied addresses")
    );
    assert_eq!(
        run_once(&intent, &repository, &client, expected_request)
            .await
            .expect("same-address tick"),
        ManagedLeaseTaskOutcome::AwaitingCertificate
    );
    server.abort();
}

#[tokio::test]
async fn changed_gateway_addresses_renew_ready_lease_before_normal_due_time() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let intent = LeaseIntentStore::new(core_store.clone());
    intent
        .set_mode(PublicUrlMode::Auto)
        .await
        .expect("set auto mode");
    let repository = OperationRepository::open(core_store, nats.controller.clone());
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock")
        .as_secs();
    let (client, server) = stub_client(StubLeaseWorker::with_clock(ManualClock::new(now))).await;
    let initial = gateway_request(&["203.0.113.8"], &[]);
    let acquired = client
        .acquire(acquire_request("b2"))
        .await
        .expect("seed lease");
    let _ = client
        .download_bundle(acquired.lease.name.clone(), acquired.lease.token.clone())
        .await
        .expect("pending bundle");
    let ployzd::lease::BundleDownloadOutcome::Ready(bundle) = client
        .download_bundle(acquired.lease.name.clone(), acquired.lease.token.clone())
        .await
        .expect("ready bundle")
    else {
        panic!("expected ready bundle");
    };
    intent
        .store_successful_lease_application(
            acquired.lease.clone(),
            Some(bundle),
            address_set(&initial),
        )
        .await
        .expect("store ready lease");
    let changed = gateway_request(&["203.0.113.9"], &[]);

    let outcome = run_once(&intent, &repository, &client, changed)
        .await
        .expect("address-change tick");

    assert!(matches!(outcome, ManagedLeaseTaskOutcome::Renewed { .. }));
    server.abort();
}

#[tokio::test]
async fn failed_address_change_renewal_preserves_lease_and_applied_addresses() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let intent = LeaseIntentStore::new(core_store.clone());
    intent
        .set_mode(PublicUrlMode::Auto)
        .await
        .expect("set auto mode");
    let repository = OperationRepository::open(core_store, nats.controller.clone());
    let (client, server) = stub_client(StubLeaseWorker::new()).await;
    let initial = gateway_request(&["203.0.113.8"], &[]);
    let acquired = client
        .acquire(acquire_request("b3"))
        .await
        .expect("seed lease");
    let mut prior_lease = acquired.lease;
    prior_lease.token = LeaseBearerToken::try_new("wrong-token").expect("token");
    intent
        .store_successful_lease_application(prior_lease.clone(), None, address_set(&initial))
        .await
        .expect("store prior lease");
    let changed = gateway_request(&["203.0.113.9"], &[]);

    let outcome = run_once(&intent, &repository, &client, changed.clone())
        .await
        .expect("failed renewal tick");
    let ManagedLeaseTaskOutcome::Failed { operation_id } = outcome else {
        panic!("expected failed renewal");
    };
    let status = repository
        .get(&operation_id)
        .await
        .expect("operation read")
        .expect("operation exists");

    assert_eq!(
        intent.load().await.expect("prior lease remains"),
        ManagedLeaseIntent::Auto {
            state: Box::new(AutoLeaseState::RecordOnly { lease: prior_lease })
        }
    );
    assert!(
        !intent
            .applied_address_set_differs(&address_set(&initial))
            .await
            .expect("prior addresses remain")
    );
    assert!(
        intent
            .applied_address_set_differs(&address_set(&changed))
            .await
            .expect("changed addresses not recorded")
    );
    assert!(matches!(
        status,
        OperationStatus::ManagedLease {
            state: ManagedLeaseOperationState::Failed {
                failure: ployz_core::ops::ManagedLeaseOperationFailure {
                    class: ManagedLeaseFailureClass::WorkerUnauthorized,
                    ..
                }
            },
            ..
        }
    ));
    server.abort();
}

#[tokio::test]
async fn acquisition_does_not_mark_gateway_addresses_applied() {
    let nats = ployz_test_support::nats::TestNats::start().await;
    let core_store = CoreStore::open_in_memory().await.expect("core store");
    let intent = LeaseIntentStore::new(core_store.clone());
    intent
        .set_mode(PublicUrlMode::Auto)
        .await
        .expect("set auto mode");
    let repository = OperationRepository::open(core_store.clone(), nats.controller.clone());
    let (client, server) = stub_client(StubLeaseWorker::new()).await;
    let request = gateway_request(&["203.0.113.8"], &["2001:db8::8"]);

    let outcome = run_once(&intent, &repository, &client, request.clone())
        .await
        .expect("acquisition tick");

    assert!(matches!(outcome, ManagedLeaseTaskOutcome::Acquired { .. }));
    assert_eq!(
        intent
            .applied_address_set()
            .await
            .expect("applied-address evidence"),
        None
    );

    let follow_up = run_once(&intent, &repository, &client, request.clone())
        .await
        .expect("post-acquisition address publication tick");
    assert!(matches!(follow_up, ManagedLeaseTaskOutcome::Renewed { .. }));
    assert_eq!(
        intent
            .applied_address_set()
            .await
            .expect("applied-address evidence"),
        Some(address_set(&request))
    );
    server.abort();
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
                addresses: Box::new(empty_address_set()),
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
    let (client, _worker, server) = stub_client_with_worker(worker).await;
    (client, server)
}

async fn stub_client_with_worker<C: Clock + Send + 'static>(
    worker: StubLeaseWorker<C>,
) -> (
    LeaseClient,
    Arc<Mutex<StubLeaseWorker<C>>>,
    tokio::task::JoinHandle<()>,
) {
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind stub worker");
    let address = listener.local_addr().expect("stub worker address");
    let worker = Arc::new(Mutex::new(worker));
    let service_worker = Arc::clone(&worker);
    let server = tokio::spawn(async move {
        let _ = serve(listener, service_worker).await;
    });
    let client =
        LeaseClient::new(LeaseWorkerUrl::try_new(format!("http://{address}")).expect("worker URL"));
    (client, worker, server)
}

fn acquire_request(id: &str) -> ManagedLeaseAcquireRequest {
    ManagedLeaseAcquireRequest {
        acquisition_id: ManagedLeaseAcquisitionId::try_new(id).expect("acquisition id"),
        token: LeaseBearerToken::try_new("client-token").expect("token"),
        ipv4: Vec::new(),
        ipv6: Vec::new(),
    }
}

fn empty_request() -> ManagedLeaseRenewRequest {
    ManagedLeaseRenewRequest {
        ipv4: Vec::new(),
        ipv6: Vec::new(),
    }
}

fn empty_address_set() -> ManagedLeaseAddressSet {
    address_set(&empty_request())
}

fn gateway_request(ipv4: &[&str], ipv6: &[&str]) -> ManagedLeaseRenewRequest {
    ManagedLeaseRenewRequest {
        ipv4: ipv4
            .iter()
            .map(|address| address.parse::<Ipv4Addr>().expect("IPv4"))
            .collect(),
        ipv6: ipv6
            .iter()
            .map(|address| address.parse::<Ipv6Addr>().expect("IPv6"))
            .collect(),
    }
}

fn address_set(request: &ManagedLeaseRenewRequest) -> ManagedLeaseAddressSet {
    ManagedLeaseAddressSet::new(request.ipv4.clone(), request.ipv6.clone())
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
