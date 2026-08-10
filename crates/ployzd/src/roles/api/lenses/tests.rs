//! Behavioral tests for the lens state cell.

use std::collections::{BTreeMap, VecDeque};
use std::future::pending;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ployz_core::corrosion::{ChangeId, QueryIdentity, SqliteParameter, Statement, StoredRow};
use ployz_core::ids::ClusterName;
use ployz_core::{ApiRefusal, LensCollection, LensSnapshot};
use serde_json::json;
use tokio::sync::{Mutex, Notify, mpsc, watch};

use super::actor::start_lens_with_store;
use super::adapter::{
    LensInput, LensStore, LensStoreError, LensSubscription, LensSubscriptionEvent,
};
use super::{LensEngineConfig, LensLimits, LensRecoveryPolicy};
use crate::corrosion::CorrosionClientError;

const CLUSTER: &str = "main";
const WRONG_MACHINE: &str = "legacy-node";
const MACHINE: &str = "edge";
const PEER: &str = "operator";
const NAMESPACE: &str = "production";
const OPERATION: &str = "release-1";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FakeCall {
    Query { input: LensInput, max_rows: usize },
    Subscribe(LensInput),
    Resume { input: LensInput, cursor: ChangeId },
}

#[derive(Debug)]
enum QueryPlan {
    Rows(Vec<StoredRow>),
    Pending,
}

#[derive(Debug)]
struct FakeStore {
    queries: Mutex<BTreeMap<LensInput, VecDeque<QueryPlan>>>,
    subscriptions: Mutex<BTreeMap<LensInput, VecDeque<FakeSubscription>>>,
    resumes: Mutex<BTreeMap<LensInput, VecDeque<FakeSubscription>>>,
    calls: Mutex<Vec<FakeCall>>,
    activity: Notify,
}

impl FakeStore {
    fn new() -> Self {
        Self {
            queries: Mutex::new(BTreeMap::new()),
            subscriptions: Mutex::new(BTreeMap::new()),
            resumes: Mutex::new(BTreeMap::new()),
            calls: Mutex::new(Vec::new()),
            activity: Notify::new(),
        }
    }

    async fn queue_query_rows(&self, input: LensInput, rows: Vec<StoredRow>) {
        self.queries
            .lock()
            .await
            .entry(input)
            .or_default()
            .push_back(QueryPlan::Rows(rows));
    }

    async fn queue_query_pending(&self, input: LensInput) {
        self.queries
            .lock()
            .await
            .entry(input)
            .or_default()
            .push_back(QueryPlan::Pending);
    }

    async fn queue_subscription(&self, input: LensInput, feed: Arc<FakeFeed>) {
        self.subscriptions
            .lock()
            .await
            .entry(input)
            .or_default()
            .push_back(FakeSubscription::new(input, feed));
    }

    async fn queue_resume(&self, input: LensInput, feed: Arc<FakeFeed>) {
        self.resumes
            .lock()
            .await
            .entry(input)
            .or_default()
            .push_back(FakeSubscription::new(input, feed));
    }

    async fn calls(&self) -> Vec<FakeCall> {
        self.calls.lock().await.clone()
    }

    async fn query_count(&self, input: LensInput) -> usize {
        self.calls
            .lock()
            .await
            .iter()
            .filter(|call| matches!(call, FakeCall::Query { input: found, .. } if *found == input))
            .count()
    }

    async fn resume_count(&self, input: LensInput) -> usize {
        self.calls
            .lock()
            .await
            .iter()
            .filter(|call| matches!(call, FakeCall::Resume { input: found, .. } if *found == input))
            .count()
    }

    async fn subscription_count(&self) -> usize {
        self.calls
            .lock()
            .await
            .iter()
            .filter(|call| matches!(call, FakeCall::Subscribe(_)))
            .count()
    }

    async fn wait_for_queries(&self, input: LensInput, count: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if self.query_count(input).await >= count {
                    return;
                }
                self.activity.notified().await;
            }
        })
        .await
        .expect("lens queried the expected number of times");
    }

    async fn wait_for_subscriptions(&self, count: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if self.subscription_count().await >= count {
                    return;
                }
                self.activity.notified().await;
            }
        })
        .await
        .expect("lens opened the expected number of subscriptions");
    }

    async fn wait_for_resumes(&self, input: LensInput, count: usize) {
        tokio::time::timeout(Duration::from_secs(1), async {
            loop {
                if self.resume_count(input).await >= count {
                    return;
                }
                self.activity.notified().await;
            }
        })
        .await
        .expect("lens attempted the expected number of resumptions");
    }

    async fn pop_subscription(
        queue: &Mutex<BTreeMap<LensInput, VecDeque<FakeSubscription>>>,
        input: LensInput,
        kind: &str,
    ) -> Result<FakeSubscription, LensStoreError> {
        queue
            .lock()
            .await
            .get_mut(&input)
            .and_then(VecDeque::pop_front)
            .ok_or_else(|| {
                LensStoreError::Corrosion(CorrosionClientError::Protocol {
                    detail: format!("missing fake {input:?} {kind} plan"),
                })
            })
    }
}

#[async_trait]
impl LensStore for FakeStore {
    async fn query_rows(
        &self,
        input: LensInput,
        _cluster_id: &ClusterName,
        max_rows: usize,
    ) -> Result<Vec<StoredRow>, LensStoreError> {
        self.calls
            .lock()
            .await
            .push(FakeCall::Query { input, max_rows });
        self.activity.notify_waiters();
        let plan = self
            .queries
            .lock()
            .await
            .get_mut(&input)
            .and_then(VecDeque::pop_front)
            .ok_or_else(|| {
                LensStoreError::Corrosion(CorrosionClientError::Protocol {
                    detail: format!("missing fake {input:?} query plan"),
                })
            })?;
        match plan {
            QueryPlan::Rows(rows) => Ok(rows),
            QueryPlan::Pending => pending::<Result<Vec<StoredRow>, LensStoreError>>().await,
        }
    }

    async fn subscribe(
        &self,
        input: LensInput,
        _cluster_id: &ClusterName,
    ) -> Result<Box<dyn LensSubscription>, LensStoreError> {
        self.calls.lock().await.push(FakeCall::Subscribe(input));
        self.activity.notify_waiters();
        Ok(Box::new(
            Self::pop_subscription(&self.subscriptions, input, "subscription").await?,
        ))
    }

    async fn resume(
        &self,
        input: LensInput,
        _cluster_id: &ClusterName,
        _identity: &QueryIdentity,
        cursor: ChangeId,
    ) -> Result<Box<dyn LensSubscription>, LensStoreError> {
        self.calls
            .lock()
            .await
            .push(FakeCall::Resume { input, cursor });
        self.activity.notify_waiters();
        let mut subscription = Self::pop_subscription(&self.resumes, input, "resume").await?;
        subscription.cursor = Some(cursor);
        Ok(Box::new(subscription))
    }
}

#[derive(Debug)]
struct FakeFeed {
    events: Mutex<VecDeque<Result<FakeStreamEvent, CorrosionClientError>>>,
    activity: Notify,
}

impl FakeFeed {
    fn snapshot(watermark: ChangeId) -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(VecDeque::from([Ok(FakeStreamEvent::SnapshotEnd(
                watermark,
            ))])),
            activity: Notify::new(),
        })
    }

    fn empty() -> Arc<Self> {
        Arc::new(Self {
            events: Mutex::new(VecDeque::new()),
            activity: Notify::new(),
        })
    }

    async fn push(&self, event: FakeStreamEvent) {
        self.events.lock().await.push_back(Ok(event));
        self.activity.notify_waiters();
    }

    async fn fail(&self, error: CorrosionClientError) {
        self.events.lock().await.push_back(Err(error));
        self.activity.notify_waiters();
    }
}

#[derive(Debug)]
enum FakeStreamEvent {
    SnapshotEnd(ChangeId),
    Invalidation(ChangeId),
}

#[derive(Debug)]
struct FakeSubscription {
    identity: QueryIdentity,
    cursor: Option<ChangeId>,
    feed: Arc<FakeFeed>,
}

impl FakeSubscription {
    fn new(input: LensInput, feed: Arc<FakeFeed>) -> Self {
        let id = match input {
            LensInput::Cluster => "a2f5d373-8de3-4900-89bd-b2db4af843ef",
            LensInput::Machines => "b2f5d373-8de3-4900-89bd-b2db4af843ef",
            LensInput::Namespaces => "c2f5d373-8de3-4900-89bd-b2db4af843ef",
            LensInput::Endpoints => "d2f5d373-8de3-4900-89bd-b2db4af843ef",
            LensInput::MachineStatus => "e2f5d373-8de3-4900-89bd-b2db4af843ef",
            LensInput::Operations => "f2f5d373-8de3-4900-89bd-b2db4af843ef",
        };
        let identity = QueryIdentity::from_parts(Some(id), Some("fake-query"))
            .expect("valid fake query identity");
        Self {
            identity,
            cursor: None,
            feed,
        }
    }
}

#[async_trait]
impl LensSubscription for FakeSubscription {
    fn identity(&self) -> &QueryIdentity {
        &self.identity
    }

    fn cursor(&self) -> Option<ChangeId> {
        self.cursor
    }

    async fn next(&mut self) -> Result<LensSubscriptionEvent, LensStoreError> {
        loop {
            let event = self.feed.events.lock().await.pop_front();
            let Some(event) = event else {
                self.feed.activity.notified().await;
                continue;
            };
            match event {
                Err(error) => return Err(LensStoreError::Corrosion(error)),
                Ok(FakeStreamEvent::SnapshotEnd(watermark)) => {
                    self.cursor = Some(watermark);
                    return Ok(LensSubscriptionEvent::SnapshotEnd { watermark });
                }
                Ok(FakeStreamEvent::Invalidation(cursor)) => {
                    self.cursor = Some(cursor);
                    return Ok(LensSubscriptionEvent::Invalidation);
                }
            }
        }
    }
}

fn cluster_id() -> ClusterName {
    ClusterName::try_new(CLUSTER).expect("fixture cluster id")
}

fn policy(max_attempts: u32) -> LensRecoveryPolicy {
    LensRecoveryPolicy::try_new(
        NonZeroU32::new(max_attempts).expect("nonzero test attempts"),
        Duration::from_millis(1),
        Duration::from_millis(4),
    )
    .expect("test recovery policy")
}

fn config(max_attempts: u32) -> LensEngineConfig {
    LensEngineConfig::new(cluster_id(), policy(max_attempts))
}

fn bounded_config(
    max_attempts: u32,
    query_timeout: Duration,
    initial_snapshot_timeout: Duration,
    max_rows: usize,
) -> LensEngineConfig {
    config(max_attempts).with_limits(LensLimits::for_test(
        query_timeout,
        initial_snapshot_timeout,
        max_rows,
    ))
}

fn discarded_lifecycle_sender() -> mpsc::UnboundedSender<LensCollection> {
    let (sender, _failures) = mpsc::unbounded_channel();
    sender
}

fn stored(key: &str, document: serde_json::Value) -> StoredRow {
    StoredRow::new(
        key,
        serde_json::to_string(&document).expect("fixture document JSON"),
    )
}

fn cluster_row() -> StoredRow {
    stored(
        CLUSTER,
        json!({
            "v": 1,
            "cluster_id": CLUSTER,
            "written_by": { "kind": "peer", "peer_id": PEER },
            "written_at": "2026-08-04T10:00:00Z",
            "name": "acme",
            "storage_default": "plain",
            "hostname_mode": { "mode": "disabled" },
            "prefix": "10.210.0.0/16",
            "provider": "builtin_wireguard",
            "acme_directory_url": "https://acme.example/directory",
            "acme_contact": null
        }),
    )
}

fn machine_row(id: &str, provider: &str) -> StoredRow {
    let transport = match provider {
        "wireguard" => json!({
            "kind": "wireguard",
            "pubkey": "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "addr_v6": "fd00::20",
            "endpoint": null,
            "subnet_v4": "10.210.20.0/24"
        }),
        "tailscale" => json!({
            "kind": "tailscale",
            "ip": "100.64.0.20",
            "subnet_v4": "10.210.20.0/24"
        }),
        _ => panic!("unknown test provider"),
    };
    stored(
        id,
        json!({
            "v": 1,
            "cluster_id": CLUSTER,
            "written_by": { "kind": "peer", "peer_id": PEER },
            "written_at": "2026-08-04T10:00:00Z",
            "name": id,
            "lifecycle": "active",
            "transport": transport,
            "storage": { "mode": "plain", "reason": { "kind": "default" } }
        }),
    )
}

fn service_row(name: &str) -> StoredRow {
    stored(
        NAMESPACE,
        json!({
            "v": 1,
            "cluster_id": CLUSTER,
            "written_by": { "kind": "peer", "peer_id": PEER },
            "written_at": "2026-08-04T10:00:00Z",
            "name": NAMESPACE,
            "services": {
                (name): {
                    "image": "ghcr.io/acme/api:2026-08-04",
                    "env_fingerprints": {},
                    "mode": "replicated",
                    "replicas": 1,
                    "pinned_machines": [],
                    "active_deploy": OPERATION,
                    "previous_image": null,
                    "deployed_at": "2026-08-04T10:01:00Z"
                }
            }
        }),
    )
}

async fn next_cell(
    receiver: &mut watch::Receiver<Option<Result<LensSnapshot, ApiRefusal>>>,
) -> Result<LensSnapshot, ApiRefusal> {
    tokio::time::timeout(Duration::from_secs(1), async {
        loop {
            receiver
                .changed()
                .await
                .expect("lens sender remains available until the cell changes");
            let Some(cell) = receiver.borrow_and_update().clone() else {
                continue;
            };
            return cell;
        }
    })
    .await
    .expect("lens emitted a state cell value")
}

fn service_name(snapshot: &LensSnapshot) -> &str {
    let LensSnapshot::Services { rows } = snapshot else {
        panic!("expected services snapshot")
    };
    let [row] = rows.as_slice() else {
        panic!("expected one named service")
    };
    row.key
        .rsplit('/')
        .next()
        .expect("service lens row has a natural composite key")
}

async fn assert_no_cell_change(
    receiver: &mut watch::Receiver<Option<Result<LensSnapshot, ApiRefusal>>>,
) {
    tokio::time::sleep(Duration::from_millis(20)).await;
    assert!(
        !receiver
            .has_changed()
            .expect("lens sender remains available"),
        "unchanged snapshot must not publish a duplicate cell value"
    );
}

#[tokio::test]
async fn first_state_is_ok_and_unchanged_invalidations_are_silent() {
    let store = Arc::new(FakeStore::new());
    let feed = FakeFeed::snapshot(ChangeId::new(0));
    store
        .queue_subscription(LensInput::Namespaces, Arc::clone(&feed))
        .await;
    for name in ["before", "after", "after"] {
        store
            .queue_query_rows(LensInput::Namespaces, vec![service_row(name)])
            .await;
    }

    let watch = start_lens_with_store(
        Arc::clone(&store),
        LensCollection::Services,
        config(2),
        discarded_lifecycle_sender(),
    );
    let mut receiver = watch.subscribe();
    let initial = next_cell(&mut receiver)
        .await
        .expect("initial snapshot is successful");
    assert_eq!(service_name(&initial), "before");

    feed.push(FakeStreamEvent::Invalidation(ChangeId::new(1)))
        .await;
    let changed = next_cell(&mut receiver)
        .await
        .expect("changed snapshot is successful");
    assert_eq!(service_name(&changed), "after");

    feed.push(FakeStreamEvent::Invalidation(ChangeId::new(2)))
        .await;
    store.wait_for_queries(LensInput::Namespaces, 3).await;
    assert_no_cell_change(&mut receiver).await;
    watch.shutdown().await;
}

#[tokio::test]
async fn unchanged_resume_is_silent_and_validated_recovery_resets_its_budget() {
    let store = Arc::new(FakeStore::new());
    let initial_feed = FakeFeed::snapshot(ChangeId::new(4));
    let resumed_feed = FakeFeed::empty();
    let reset_feed = FakeFeed::snapshot(ChangeId::new(9));
    store
        .queue_subscription(LensInput::Namespaces, Arc::clone(&initial_feed))
        .await;
    store
        .queue_subscription(LensInput::Namespaces, Arc::clone(&reset_feed))
        .await;
    store
        .queue_resume(LensInput::Namespaces, Arc::clone(&resumed_feed))
        .await;
    for name in ["initial", "initial", "reset"] {
        store
            .queue_query_rows(LensInput::Namespaces, vec![service_row(name)])
            .await;
    }

    let watch = start_lens_with_store(
        Arc::clone(&store),
        LensCollection::Services,
        config(1),
        discarded_lifecycle_sender(),
    );
    let mut receiver = watch.subscribe();
    let _ = next_cell(&mut receiver).await.expect("initial state");

    initial_feed
        .fail(CorrosionClientError::SubscriptionEnded)
        .await;
    store.wait_for_queries(LensInput::Namespaces, 2).await;
    assert_no_cell_change(&mut receiver).await;
    assert!(matches!(
        store.calls().await.as_slice(),
        calls if calls.iter().any(|call| matches!(
            call,
            FakeCall::Resume { input: LensInput::Namespaces, cursor }
                if *cursor == ChangeId::new(4)
        ))
    ));

    resumed_feed
        .fail(CorrosionClientError::NonContiguousChange {
            expected: ChangeId::new(5),
            actual: ChangeId::new(7),
        })
        .await;
    let reset = next_cell(&mut receiver)
        .await
        .expect("fresh recovery state");
    assert_eq!(service_name(&reset), "reset");
    let calls = store.calls().await;
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(call, FakeCall::Subscribe(LensInput::Namespaces)))
            .count(),
        2
    );
    watch.shutdown().await;
}

#[tokio::test]
async fn exhausted_recovery_publishes_a_terminal_state_then_reports_lifecycle_failure() {
    let store = Arc::new(FakeStore::new());
    let feed = FakeFeed::snapshot(ChangeId::new(0));
    store
        .queue_subscription(LensInput::Namespaces, Arc::clone(&feed))
        .await;
    store
        .queue_resume(LensInput::Namespaces, FakeFeed::empty())
        .await;
    store
        .queue_resume(LensInput::Namespaces, FakeFeed::empty())
        .await;
    store
        .queue_query_rows(LensInput::Namespaces, vec![service_row("initial")])
        .await;
    for _ in 0..3 {
        store.queue_query_pending(LensInput::Namespaces).await;
    }

    let (lifecycle_sender, mut lifecycle_failures) = mpsc::unbounded_channel();
    let watch = start_lens_with_store(
        Arc::clone(&store),
        LensCollection::Services,
        bounded_config(2, Duration::from_millis(1), Duration::from_millis(20), 17),
        lifecycle_sender,
    );
    let mut receiver = watch.subscribe();
    let _ = next_cell(&mut receiver).await.expect("initial state");
    feed.push(FakeStreamEvent::Invalidation(ChangeId::new(1)))
        .await;

    let terminal = next_cell(&mut receiver).await;
    assert!(matches!(
        terminal,
        Err(ApiRefusal::CorrosionUnavailable { .. })
    ));
    let lifecycle_failure = tokio::time::timeout(Duration::from_secs(1), lifecycle_failures.recv())
        .await
        .expect("exhausted lens reports its lifecycle failure")
        .expect("API server still receives lens lifecycle failures");
    assert_eq!(lifecycle_failure, LensCollection::Services);
    let calls = store.calls().await;
    assert_eq!(
        calls
            .iter()
            .filter(|call| matches!(
                call,
                FakeCall::Resume {
                    input: LensInput::Namespaces,
                    ..
                }
            ))
            .count(),
        2
    );
    assert_eq!(store.query_count(LensInput::Namespaces).await, 4);
    watch.shutdown().await;
}

#[tokio::test]
async fn machine_inputs_open_together_and_accept_either_snapshot_order() {
    let store = Arc::new(FakeStore::new());
    let cluster_feed = FakeFeed::empty();
    let machines_feed = FakeFeed::snapshot(ChangeId::new(0));
    store
        .queue_subscription(LensInput::Cluster, Arc::clone(&cluster_feed))
        .await;
    store
        .queue_subscription(LensInput::Machines, Arc::clone(&machines_feed))
        .await;
    store
        .queue_query_rows(LensInput::Cluster, vec![cluster_row()])
        .await;
    store
        .queue_query_rows(
            LensInput::Machines,
            vec![
                machine_row(WRONG_MACHINE, "tailscale"),
                machine_row(MACHINE, "wireguard"),
            ],
        )
        .await;
    store
        .queue_query_rows(LensInput::Cluster, vec![StoredRow::new(CLUSTER, "{}")])
        .await;
    store
        .queue_query_rows(LensInput::Machines, vec![machine_row(MACHINE, "wireguard")])
        .await;

    let (lifecycle_sender, mut lifecycle_failures) = mpsc::unbounded_channel();
    let watch = start_lens_with_store(
        Arc::clone(&store),
        LensCollection::Machines,
        config(2),
        lifecycle_sender.clone(),
    );
    let mut receiver = watch.subscribe();
    store.wait_for_subscriptions(2).await;
    let calls = store.calls().await;
    assert!(matches!(
        calls.as_slice(),
        [
            FakeCall::Subscribe(LensInput::Cluster),
            FakeCall::Subscribe(LensInput::Machines),
            ..
        ]
    ));
    assert_eq!(store.query_count(LensInput::Cluster).await, 0);

    cluster_feed
        .push(FakeStreamEvent::SnapshotEnd(ChangeId::new(0)))
        .await;
    let initial = next_cell(&mut receiver).await.expect("machines snapshot");
    let LensSnapshot::Machines { rows, .. } = initial else {
        panic!("expected machines snapshot")
    };
    let [row] = rows.as_slice() else {
        panic!("expected only the provider-fenced machine")
    };
    assert_eq!(row.id.as_str(), MACHINE);

    cluster_feed
        .push(FakeStreamEvent::Invalidation(ChangeId::new(1)))
        .await;
    let terminal = next_cell(&mut receiver).await;
    assert!(matches!(terminal, Err(ApiRefusal::InvalidCluster)));
    assert!(
        tokio::time::timeout(Duration::from_millis(20), lifecycle_failures.recv())
            .await
            .is_err(),
        "a typed invalid-cluster refusal must not restart the API role"
    );
    watch.shutdown().await;
}

#[tokio::test]
async fn initial_snapshot_exhaustion_publishes_terminal_then_reports_lifecycle_failure() {
    let store = Arc::new(FakeStore::new());
    store
        .queue_subscription(LensInput::Namespaces, FakeFeed::empty())
        .await;

    let (lifecycle_sender, mut lifecycle_failures) = mpsc::unbounded_channel();
    let watch = start_lens_with_store(
        Arc::clone(&store),
        LensCollection::Services,
        bounded_config(1, Duration::from_millis(20), Duration::from_millis(1), 10),
        lifecycle_sender,
    );
    let mut receiver = watch.subscribe();
    let first = next_cell(&mut receiver).await;
    assert!(matches!(
        first,
        Err(ApiRefusal::CorrosionUnavailable { .. })
    ));
    let lifecycle_failure = tokio::time::timeout(Duration::from_secs(1), lifecycle_failures.recv())
        .await
        .expect("initial exhaustion reports its lifecycle failure")
        .expect("API server still receives lens lifecycle failures");
    assert_eq!(lifecycle_failure, LensCollection::Services);
    watch.shutdown().await;
}

#[tokio::test]
async fn controlled_shutdown_during_recovery_backoff_does_not_report_lifecycle_failure() {
    let store = Arc::new(FakeStore::new());
    let feed = FakeFeed::snapshot(ChangeId::new(0));
    store
        .queue_subscription(LensInput::Namespaces, Arc::clone(&feed))
        .await;
    store
        .queue_query_rows(LensInput::Namespaces, vec![service_row("initial")])
        .await;
    let recovery = LensRecoveryPolicy::try_new(
        NonZeroU32::new(1).expect("one recovery attempt"),
        Duration::from_secs(1),
        Duration::from_secs(1),
    )
    .expect("one-second recovery backoff is valid");
    let (lifecycle_sender, mut lifecycle_failures) = mpsc::unbounded_channel();
    let watch = start_lens_with_store(
        Arc::clone(&store),
        LensCollection::Services,
        LensEngineConfig::new(cluster_id(), recovery),
        lifecycle_sender.clone(),
    );
    let mut receiver = watch.subscribe();
    let _ = next_cell(&mut receiver).await.expect("initial state");

    feed.fail(CorrosionClientError::SubscriptionEnded).await;
    store.wait_for_resumes(LensInput::Namespaces, 1).await;
    tokio::time::timeout(Duration::from_millis(100), watch.shutdown())
        .await
        .expect("controlled shutdown interrupts recovery backoff");
    assert!(
        tokio::time::timeout(Duration::from_millis(20), lifecycle_failures.recv())
            .await
            .is_err(),
        "controlled shutdown must not restart the API role"
    );
}

#[tokio::test]
async fn row_cap_is_passed_through_the_lens_query_seam() {
    let store = Arc::new(FakeStore::new());
    store
        .queue_subscription(LensInput::Namespaces, FakeFeed::snapshot(ChangeId::new(0)))
        .await;
    store
        .queue_query_rows(LensInput::Namespaces, vec![service_row("initial")])
        .await;

    let watch = start_lens_with_store(
        Arc::clone(&store),
        LensCollection::Services,
        bounded_config(1, Duration::from_millis(20), Duration::from_millis(20), 1),
        discarded_lifecycle_sender(),
    );
    let mut receiver = watch.subscribe();
    let _ = next_cell(&mut receiver).await.expect("initial state");
    assert!(store.calls().await.iter().any(|call| matches!(
        call,
        FakeCall::Query {
            input: LensInput::Namespaces,
            max_rows: 1
        }
    )));
    watch.shutdown().await;
}

#[tokio::test]
async fn latest_complete_state_coalesces_for_a_slow_downstream_receiver() {
    let store = Arc::new(FakeStore::new());
    let feed = FakeFeed::snapshot(ChangeId::new(0));
    store
        .queue_subscription(LensInput::Namespaces, Arc::clone(&feed))
        .await;
    for name in ["initial", "first", "latest"] {
        store
            .queue_query_rows(LensInput::Namespaces, vec![service_row(name)])
            .await;
    }

    let watch = start_lens_with_store(
        Arc::clone(&store),
        LensCollection::Services,
        config(2),
        discarded_lifecycle_sender(),
    );
    let mut receiver = watch.subscribe();
    let _ = next_cell(&mut receiver).await.expect("initial state");
    feed.push(FakeStreamEvent::Invalidation(ChangeId::new(1)))
        .await;
    feed.push(FakeStreamEvent::Invalidation(ChangeId::new(2)))
        .await;
    store.wait_for_queries(LensInput::Namespaces, 3).await;

    let Some(Ok(snapshot)) = receiver.borrow_and_update().clone() else {
        panic!("latest successful lens state is present")
    };
    assert_eq!(service_name(&snapshot), "latest");
    watch.shutdown().await;
}

#[tokio::test]
async fn shutdown_cancels_an_idle_subscription_wait() {
    let store = Arc::new(FakeStore::new());
    store
        .queue_subscription(LensInput::Namespaces, FakeFeed::snapshot(ChangeId::new(0)))
        .await;
    store
        .queue_query_rows(LensInput::Namespaces, vec![service_row("initial")])
        .await;

    let watch = start_lens_with_store(
        Arc::clone(&store),
        LensCollection::Services,
        config(2),
        discarded_lifecycle_sender(),
    );
    let mut receiver = watch.subscribe();
    let _ = next_cell(&mut receiver).await.expect("initial state");

    tokio::time::timeout(Duration::from_secs(1), watch.shutdown())
        .await
        .expect("shutdown cancels the idle Corrosion wait");
}

#[test]
fn lens_queries_are_scoped_to_the_configured_cluster() {
    let cluster = cluster_id();
    assert_eq!(
        LensInput::Cluster.statement(&cluster),
        Statement::with_params(
            "SELECT id, document FROM cluster WHERE id = ?",
            vec![SqliteParameter::Text(CLUSTER.to_owned())],
        )
    );
    assert_eq!(
        LensInput::MachineStatus.statement(&cluster),
        Statement::with_params(
            "SELECT machine_id AS id, document FROM machine_status WHERE json_extract(document, '$.cluster_id') = ?",
            vec![SqliteParameter::Text(CLUSTER.to_owned())],
        )
    );
}
