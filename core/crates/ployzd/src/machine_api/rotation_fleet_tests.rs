//! Three real Machine API handlers over one fake Corrosion cluster, each on its own
//! fake Docker. The tests write membership by hand, and the ack rows too unless
//! they run the real Ingress watchers; every Container row reaches the cluster
//! through the real observation publisher.

#![expect(clippy::indexing_slicing, reason = "JSON fixture reads")]

use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU16,
    path::{Path, PathBuf},
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use ployz_core::{
    AdvertisedEndpoint, AnswerWhen, ConfiguredHealthcheck, ContainerId, ContainerLocator,
    ContainerNow, ContainerRefusalReason, ContainerWant, CopyRotation, DeploymentLogId,
    EnsureContainerRequest, EnsuredContainer, HealthVerdict, HealthcheckCommand, HealthcheckSpec,
    HttpHealthcheck, Machine, MachineId, MachineName, MachineRpc, MachineRuntime, NewContainer,
    ProxyRoutes, QualifiedService, ReadRoutesRequest, ResolvedServiceSpec, RoutesRead, RunRotation,
    RunTarget, WireGuardPublicKey, op, proxy_routes, service_containers,
};
use serde_json::{Value, json};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::UnixListener,
    sync::Notify,
};
use tokio_util::{codec::LengthDelimitedCodec, sync::CancellationToken};
use tonic::Request;

use super::MachineService;
use crate::corrosion::{
    AdminClient, IngressRouteAck, IngressRouteChange, IngressRouteRow, MachineView,
    ReplicatedStore, fake_cluster::FakeCluster,
};
use crate::docker::{
    ContainerRuntime,
    test_support::{FakeDocker, fake_runtime_with, spec_with_sources},
};
use crate::machine::{LocalMachineStore, RecordOwner};
use crate::route_acks::{DownMachines, load_down};

const SETTLE: Duration = Duration::from_secs(10);
const CLUSTER_CONTAINERS: &str = "SELECT id, machine_id, container FROM containers ORDER BY id";
const SERVICE_ROUTES: &str =
    "SELECT machine_id, service_id, deploy, containers FROM ingress_routes WHERE service_id = ?";

struct Node {
    service: MachineService,
    runtime: ContainerRuntime,
    fake: FakeDocker,
    row: Machine,
}

struct Fleet {
    nodes: Vec<Node>,
    store: ReplicatedStore,
    /// What every Machine's membership sample reads, as Corrosion admin states.
    states: Arc<Mutex<Vec<Value>>>,
    shutdown: CancellationToken,
    dir: PathBuf,
    cluster: FakeCluster,
}

impl Fleet {
    async fn start() -> Self {
        Self::rescanning(Duration::from_millis(100)).await
    }

    /// A fleet whose publishers also rescan Docker every `rescan`, since the fake
    /// Docker sends no events.
    async fn rescanning(rescan: Duration) -> Self {
        let cluster = FakeCluster::start();
        let store = cluster.store();
        let dir = std::env::temp_dir().join(format!("ployzd-rotation-{}", MachineId::random()));
        std::fs::create_dir_all(&dir).unwrap();
        let states = Arc::new(Mutex::new(Vec::new()));
        let admin = admin_socket(&dir, Arc::clone(&states));
        let shutdown = CancellationToken::new();
        let mut nodes = Vec::new();
        for octet in 1..=3_u8 {
            let data_dir = dir.join(format!("machine-{octet}"));
            let owner = RecordOwner::spawn(enrolled(&data_dir)).unwrap();
            let row = Machine {
                id: owner.record().id(),
                ..machine_row(octet)
            };
            store
                .machine_publication()
                .await
                .publish(&row)
                .await
                .unwrap();
            let (runtime, fake) = fake_runtime_with(FakeDocker {
                named_containers: Some(Arc::new(Mutex::new(BTreeMap::new()))),
                list_named: true,
                stamp_starts: true,
                ..Default::default()
            })
            .await;
            let runtime = runtime
                .replicating(
                    cluster.store(),
                    owner.clone(),
                    crate::storage::Plugin::default(),
                )
                .with_rescan_interval(rescan);
            tokio::spawn({
                let (runtime, shutdown) = (runtime.clone(), shutdown.clone());
                async move { runtime.publish_observations(shutdown).await }
            });
            let mut service = MachineService::with_cluster(
                owner,
                Some((cluster.store(), AdminClient::new(admin.clone()))),
            )
            .with_optional_containers(Some(runtime.clone()));
            service.local = service.local.with_zpool("true");
            nodes.push(Node {
                service,
                runtime,
                fake,
                row,
            });
        }
        Self {
            nodes,
            store,
            states,
            shutdown,
            dir,
            cluster,
        }
    }

    fn node(&self, index: usize) -> &Node {
        self.nodes.get(index).unwrap()
    }

    fn id(&self, index: usize) -> MachineId {
        self.node(index).row.id
    }

    fn down(&self, index: usize) {
        self.membership(index, "Down");
    }

    fn membership(&self, index: usize, state: &str) {
        let row = &self.node(index).row;
        let address = std::net::SocketAddr::new(
            row.management_address().0.into(),
            ployz_core::CORROSION_GOSSIP_PORT,
        );
        self.states
            .lock()
            .unwrap()
            .push(json!({ "id": { "addr": address }, "state": state }));
    }

    async fn addressed(&self, index: usize, container: ContainerId, address: &str) {
        let id = container.to_string();
        for named in self
            .node(index)
            .fake
            .named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .values_mut()
            .filter(|named| named["Id"] == id.as_str())
        {
            named["NetworkSettings"] = json!({ "Networks": { "ployz": { "IPAddress": address } } });
        }
        tokio::time::timeout(SETTLE, async {
            while self
                .store
                .container(&container)
                .await
                .unwrap()
                .is_none_or(|row| row.address.is_none())
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{container} never published an address"));
    }

    async fn run_ingress(&self) -> MachineView {
        let machines = MachineView::start(self.store.clone(), self.shutdown.clone());
        tokio::time::timeout(SETTLE, async {
            while machines
                .current()
                .is_none_or(|current| current.observations.len() < self.nodes.len())
            {
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("the Machine view never read every Machine");
        for node in &self.nodes {
            let dir = self.dir.join(format!("ingress-{}", node.row.name));
            std::fs::create_dir_all(&dir).unwrap();
            let (machines, admin, local) = (machines.clone(), self.admin(), node.row.id);
            tokio::spawn(crate::ingress::caddy::run(
                node.row.clone(),
                self.cluster.store(),
                move || {
                    let (machines, admin) = (machines.clone(), admin.clone());
                    async move { load_down(&machines, &admin, &local).await }
                },
                dir.join("Caddyfile"),
                dir.join("admin.sock"),
                self.shutdown.clone(),
            ));
        }
        machines
    }

    fn admin(&self) -> AdminClient {
        AdminClient::new(self.dir.join("admin.sock"))
    }

    async fn acked_by_all(&self, service: &QualifiedService, containers: &[ContainerId]) {
        let want: BTreeSet<_> = containers.iter().copied().collect();
        tokio::time::timeout(SETTLE, async {
            loop {
                let rows = self.store.service_ingress_routes(service).await.unwrap();
                let acked = self.nodes.iter().all(|node| {
                    matches!(
                        rows.get(&(node.row.id, service.clone())),
                        Some(IngressRouteRow::Acked(ack))
                            if ack.deploy.as_str() == "rotation" && ack.containers == want
                    )
                });
                if acked {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("every Ingress never acked {service} with {want:?}"));
    }

    async fn ack(&self, index: usize, service: &QualifiedService, containers: &[ContainerId]) {
        self.store
            .publish_ingress_routes(
                &self.id(index),
                &BTreeMap::from([(
                    service.clone(),
                    IngressRouteChange::Upsert(IngressRouteAck {
                        deploy: run("d1"),
                        containers: containers.iter().copied().collect(),
                    }),
                )]),
            )
            .await
            .unwrap();
    }

    async fn ack_all(&self, service: &QualifiedService, containers: &[ContainerId]) {
        for index in 0..self.nodes.len() {
            self.ack(index, service, containers).await;
        }
    }

    /// Wait until the cluster reads `container` with `rotation`, or reads no row for
    /// it when `rotation` is `None`.
    async fn published(&self, container: ContainerId, rotation: Option<CopyRotation>) {
        tokio::time::timeout(SETTLE, async {
            loop {
                let row = self.store.container(&container).await.unwrap();
                if row.map(|row| row.rotation().clone()) == rotation {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{container} never published as {rotation:?}"));
    }

    /// A new Pending copy of `name` on `index`, published to the cluster.
    async fn pending(&self, index: usize, key: &str, name: &str, r#gen: u64) -> ContainerId {
        let reply = ensure(
            self.node(index),
            ContainerWant::Running {
                container: RunTarget::New(new_copy(key, spec(name), r#gen)),
                rotation: RunRotation::Pending,
            },
        )
        .await;
        let id = present(&reply);
        self.published(id, Some(CopyRotation::Pending)).await;
        id
    }

    /// A new copy of `name` on `index` brought In, published to the cluster.
    async fn routed(&self, index: usize, key: &str, name: &str, r#gen: u64) -> ContainerId {
        let id = self.brought_in(index, key, name, r#gen).await;
        self.published(id, Some(CopyRotation::In)).await;
        id
    }

    /// A new copy of `name` on `index` brought In, whether or not the cluster has it yet.
    async fn brought_in(&self, index: usize, key: &str, name: &str, r#gen: u64) -> ContainerId {
        let node = self.node(index);
        let created = ensure(
            node,
            ContainerWant::Running {
                container: RunTarget::New(new_copy(key, spec(name), r#gen)),
                rotation: RunRotation::Pending,
            },
        )
        .await;
        let id = present(&created);
        let reply = ensure(node, into(id, None, Some(CopyRotation::Pending))).await;
        assert_eq!(rotation_of(&reply), CopyRotation::In, "{reply:?}");
        id
    }

    async fn read_routes(&self, index: usize, service: &QualifiedService) -> RoutesRead {
        let request = op::ReadRoutes::into_request(ReadRoutesRequest {
            service: service.clone(),
            routes_only: true,
        });
        self.node(index)
            .service
            .read_routes(Request::new(request.encode().unwrap()))
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
            .decode::<op::ReadRoutes>()
            .unwrap()
    }
}

impl Drop for Fleet {
    fn drop(&mut self) {
        self.shutdown.cancel();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

impl Node {
    async fn rotation(&self, container: ContainerId) -> CopyRotation {
        self.runtime
            .rotations(vec![container])
            .await
            .unwrap()
            .remove(&container)
            .unwrap_or_default()
    }

    /// Write `rotation` straight to this Machine's record, as a want that won the
    /// turn would.
    async fn record(&self, container: ContainerId, rotation: CopyRotation) {
        self.runtime
            .rotation_turn(&container)
            .await
            .unwrap()
            .write(&rotation)
            .await
            .unwrap();
    }

    async fn hold_turn(&self, release: Arc<Notify>) {
        let started = Arc::new(Notify::new());
        tokio::spawn({
            let (local, started) = (self.service.local.clone(), Arc::clone(&started));
            async move { local.hold_mutation_for_test(started, release).await }
        });
        started.notified().await;
    }

    /// Hold this Machine's next Container inspect until the gate is waited on twice:
    /// once to see it held, once to release it.
    fn hold_inspect(&self) -> Arc<tokio::sync::Barrier> {
        let gate = Arc::new(tokio::sync::Barrier::new(2));
        *self.fake.inspect_hold.lock().unwrap() = Some(Arc::clone(&gate));
        gate
    }

    /// Answer this Machine's next Container inspect as Docker then sees it, but hold
    /// the answer until the gate is waited on twice.
    fn hold_answered(&self) -> Arc<tokio::sync::Barrier> {
        let gate = Arc::new(tokio::sync::Barrier::new(2));
        *self.fake.answered_hold.lock().unwrap() = Some(Arc::clone(&gate));
        gate
    }

    /// Hold every later inspect of `container` until the guard drops.
    async fn hold_inspects_of(
        &self,
        container: ContainerId,
    ) -> tokio::sync::OwnedRwLockWriteGuard<()> {
        let gate = Arc::clone(&self.fake.inspect_gate).write_owned().await;
        *self.fake.gated_inspects.lock().unwrap() = Some(container.to_string());
        gate
    }

    /// Withdrawn means the stop was planned and is draining or waiting for the turn.
    async fn withdrawn(&self, container: ContainerId) {
        tokio::time::timeout(SETTLE, async {
            while !self.runtime.stopping_ids_for_test().contains(&container) {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap_or_else(|_| panic!("{container} was never withdrawn"));
    }

    fn container(&self, container: ContainerId) -> Option<Value> {
        let id = container.to_string();
        self.fake
            .named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .values()
            .find(|named| named["Id"] == id.as_str())
            .cloned()
    }

    fn running(&self, container: ContainerId) -> bool {
        self.container(container)
            .is_some_and(|named| named["State"]["Status"] == "running")
    }

    fn set_state(&self, container: ContainerId, state: Value) {
        let id = container.to_string();
        for named in self
            .fake
            .named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .values_mut()
        {
            if named["Id"] == id.as_str() {
                named["State"] = state.clone();
            }
        }
    }

    /// Give `container` an address on the Ployz network, where its HTTP check probes.
    fn set_address(&self, container: ContainerId, address: &str) {
        let id = container.to_string();
        for named in self
            .fake
            .named_containers
            .as_ref()
            .unwrap()
            .lock()
            .unwrap()
            .values_mut()
        {
            if named["Id"] == id.as_str() {
                named["NetworkSettings"] = json!({"Networks": {"ployz": {"IPAddress": address}}});
            }
        }
    }

    fn requests(&self) -> Vec<(axum::http::Method, String)> {
        self.fake.requests.lock().unwrap().clone()
    }
}

fn admin_socket(dir: &Path, states: Arc<Mutex<Vec<Value>>>) -> PathBuf {
    let path = dir.join("admin.sock");
    let listener = UnixListener::bind(&path).unwrap();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let mut framed = LengthDelimitedCodec::builder()
                .big_endian()
                .length_field_length(4)
                .new_framed(stream);
            framed.next().await;
            let states = states.lock().unwrap().clone();
            for state in states {
                let frame = serde_json::to_vec(&json!({ "Json": state })).unwrap();
                framed.send(frame.into()).await.unwrap();
            }
            framed
                .send(serde_json::to_vec(&json!("Success")).unwrap().into())
                .await
                .unwrap();
        }
    });
    path
}

fn enrolled(data_dir: &Path) -> LocalMachineStore {
    let mut store = LocalMachineStore::open(data_dir).unwrap();
    store
        .enroll(ployz_core::EnrollRequest {
            role: ployz_core::EnrollAs::Founder {
                initial_policy: Default::default(),
                name: MachineName::parse("local").unwrap(),
                cluster_network: "10.210.0.0/16".parse().unwrap(),
                public_ip: None,
                advertised_endpoints: vec![AdvertisedEndpoint("192.0.2.1:51820".parse().unwrap())],
            },
            wireguard_mtu: None,
            pinned: Default::default(),
        })
        .unwrap();
    store
}

fn machine_row(octet: u8) -> Machine {
    Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: MachineId::random(),
        name: MachineName::parse(format!("machine-{octet}")).unwrap(),
        subnet: format!("10.210.{octet}.0/24").parse().unwrap(),
        public_key: WireGuardPublicKey([octet; 32]),
        public_ip: None,
        advertised_endpoints: vec![AdvertisedEndpoint(
            format!("192.0.2.{octet}:51820").parse().unwrap(),
        )],
        runtime: MachineRuntime::default(),
        build_concurrency: None,
    }
}

fn run(name: &str) -> DeploymentLogId {
    DeploymentLogId::parse(name).unwrap()
}

fn service(name: &str) -> QualifiedService {
    QualifiedService::parse(format!("app/{name}")).unwrap()
}

/// A Service whose health settles the moment its Container runs.
fn spec(name: &str) -> ResolvedServiceSpec {
    let mut spec = spec_with_sources(Vec::new());
    spec.name = ployz_core::ServiceName::parse(name).unwrap();
    spec.update.monitor_millis = Some(0);
    spec
}

/// A Service whose health follows the Docker healthcheck status.
fn checked_spec(name: &str) -> ResolvedServiceSpec {
    let mut spec = spec(name);
    spec.container.healthcheck = Some(HealthcheckSpec::Configured(ConfiguredHealthcheck {
        test: HealthcheckCommand::parse(["CMD", "true"]).unwrap(),
        interval_millis: Some(1_000),
        timeout_millis: Some(1_000),
        start_period_millis: None,
        start_interval_millis: None,
        retries: Some(1),
        deadline_millis: Some(60_000),
    }));
    spec
}

/// The endpoint a copy's HTTP check probes. It answers healthy until `silent` is
/// set, then accepts and never answers.
struct Endpoint {
    silent: Arc<AtomicBool>,
    /// Probes left unanswered.
    hung: Arc<AtomicUsize>,
    /// Whether a prober gave up on an unanswered probe.
    abandoned: Arc<AtomicBool>,
}

/// A Service checked over HTTP at an [`Endpoint`] on 127.0.0.1.
async fn checked_at_endpoint(name: &str, silent: bool) -> (ResolvedServiceSpec, Endpoint) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let endpoint = Endpoint {
        silent: Arc::new(AtomicBool::new(silent)),
        hung: Arc::new(AtomicUsize::new(0)),
        abandoned: Arc::new(AtomicBool::new(false)),
    };
    tokio::spawn({
        let (silent, hung, abandoned) = (
            Arc::clone(&endpoint.silent),
            Arc::clone(&endpoint.hung),
            Arc::clone(&endpoint.abandoned),
        );
        async move {
            while let Ok((mut stream, _)) = listener.accept().await {
                let silent = silent.load(Ordering::SeqCst);
                if silent {
                    hung.fetch_add(1, Ordering::SeqCst);
                }
                let abandoned = Arc::clone(&abandoned);
                tokio::spawn(async move {
                    let mut request = [0; 1024];
                    if !silent {
                        let _ = stream.read(&mut request).await;
                        let _ = stream
                            .write_all(b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\nconnection: close\r\n\r\n")
                            .await;
                        return;
                    }
                    while let Ok(1..) = stream.read(&mut request).await {}
                    abandoned.store(true, Ordering::SeqCst);
                });
            }
        }
    });
    let mut spec = spec(name);
    spec.container.healthcheck = Some(HealthcheckSpec::Http(HttpHealthcheck {
        path: "/health".into(),
        port: NonZeroU16::new(port).unwrap(),
        timeout_seconds: 1,
    }));
    (spec, endpoint)
}

fn new_copy(key: &str, spec: ResolvedServiceSpec, r#gen: u64) -> NewContainer {
    serde_json::from_value(json!({
        "slot": {"namespace": "app", "service": spec.name, "kind": "service_container", "creation_key": key},
        "resolved_spec": spec,
        "gen": r#gen,
    }))
    .unwrap()
}

fn id(container: ContainerId) -> ContainerLocator {
    ContainerLocator::Id(container)
}

fn into(
    container: ContainerId,
    run: Option<DeploymentLogId>,
    expect: Option<CopyRotation>,
) -> ContainerWant {
    ContainerWant::Running {
        container: RunTarget::Existing(id(container)),
        rotation: RunRotation::In { run, expect },
    }
}

fn out(container: ContainerId, allow_empty: bool, expect: Option<CopyRotation>) -> ContainerWant {
    ContainerWant::Out {
        container: id(container),
        allow_empty,
        expect,
    }
}

fn held(container: ContainerId, run: &str, expect: Option<CopyRotation>) -> ContainerWant {
    ContainerWant::Held {
        container: id(container),
        run: self::run(run),
        expect,
    }
}

fn stopped(container: ContainerId) -> ContainerWant {
    serde_json::from_value(json!({"state": "stopped", "container": {"id": container}, "drain": 0}))
        .unwrap()
}

fn gone(container: ContainerId) -> ContainerWant {
    serde_json::from_value(json!({"state": "gone", "container": {"id": container}, "drain": 0}))
        .unwrap()
}

async fn ensure(node: &Node, want: ContainerWant) -> EnsuredContainer {
    let request = op::EnsureContainer::into_request(EnsureContainerRequest { want });
    node.service
        .ensure_container(Request::new(request.encode().unwrap()))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .decode::<op::EnsureContainer>()
        .unwrap()
}

fn present(reply: &EnsuredContainer) -> ContainerId {
    let EnsuredContainer::Ensured {
        now: ContainerNow::Present(observed),
        ..
    } = reply
    else {
        panic!("expected an ensured present Container, got {reply:?}");
    };
    observed.container_id
}

fn rotation_of(reply: &EnsuredContainer) -> CopyRotation {
    let (EnsuredContainer::Ensured {
        now: ContainerNow::Present(observed),
        ..
    }
    | EnsuredContainer::Refused {
        now: ContainerNow::Present(observed),
        ..
    }) = reply
    else {
        panic!("expected a present Container, got {reply:?}");
    };
    observed.rotation().clone()
}

fn ensured(reply: &EnsuredContainer) {
    assert!(
        matches!(reply, EnsuredContainer::Ensured { .. }),
        "expected Ensured, got {reply:?}"
    );
}

fn refused(reply: &EnsuredContainer, reason: ContainerRefusalReason) -> &str {
    let EnsuredContainer::Refused { refusal, .. } = reply else {
        panic!("expected {reason:?}, got {reply:?}");
    };
    assert_eq!(refusal.reason, reason, "{refusal:?}");
    &refusal.message
}

// Lane 2.
#[tokio::test]
async fn out_with_an_acked_sibling() {
    let fleet = Fleet::start().await;
    let api = service("api");
    let a = fleet.routed(0, "a", "api", 1).await;
    let b = fleet.routed(1, "b", "api", 1).await;
    fleet.ack_all(&api, &[a, b]).await;

    let reply = ensure(fleet.node(0), out(a, false, Some(CopyRotation::In))).await;

    ensured(&reply);
    assert_eq!(fleet.node(0).rotation(a).await, CopyRotation::Out);
    assert!(fleet.node(0).running(a), "Out keeps the Container running");
}

// Lane 3.
#[tokio::test]
async fn out_of_the_last_in_copy() {
    let fleet = Fleet::start().await;
    let a = fleet.routed(0, "a", "api", 1).await;
    fleet.ack_all(&service("api"), &[a]).await;

    let reply = ensure(fleet.node(0), out(a, false, Some(CopyRotation::In))).await;

    refused(&reply, ContainerRefusalReason::LastIn);
    assert_eq!(rotation_of(&reply), CopyRotation::In);
    assert_eq!(fleet.node(0).rotation(a).await, CopyRotation::In);
}

// Lane 4.
#[tokio::test]
async fn out_with_allow_empty() {
    let fleet = Fleet::start().await;
    let a = fleet.routed(0, "a", "api", 1).await;

    let reply = ensure(fleet.node(0), out(a, true, Some(CopyRotation::In))).await;

    ensured(&reply);
    assert_eq!(fleet.node(0).rotation(a).await, CopyRotation::Out);
}

// Lane 5.
#[tokio::test]
async fn out_while_a_proxy_has_not_acked() {
    let fleet = Fleet::start().await;
    let api = service("api");
    let a = fleet.routed(0, "a", "api", 1).await;
    let b = fleet.routed(1, "b", "api", 1).await;
    fleet.ack(0, &api, &[a, b]).await;
    fleet.ack(1, &api, &[a, b]).await;
    fleet.ack(2, &api, &[a]).await;

    let reply = ensure(fleet.node(0), out(a, false, Some(CopyRotation::In))).await;

    let message = refused(&reply, ContainerRefusalReason::LastIn);
    assert!(message.contains("machine-3"), "{message}");
    assert!(!message.contains("machine-1"), "{message}");
    assert_eq!(fleet.node(0).rotation(a).await, CopyRotation::In);
}

// Lane 6.
#[tokio::test]
async fn out_while_the_unacked_proxy_is_down() {
    let fleet = Fleet::start().await;
    let api = service("api");
    let a = fleet.routed(0, "a", "api", 1).await;
    let b = fleet.routed(1, "b", "api", 1).await;
    fleet.ack(0, &api, &[a, b]).await;
    fleet.ack(1, &api, &[a, b]).await;
    fleet.ack(2, &api, &[a]).await;
    fleet.down(2);

    let reply = ensure(fleet.node(0), out(a, false, Some(CopyRotation::In))).await;

    ensured(&reply);
    assert_eq!(fleet.node(0).rotation(a).await, CopyRotation::Out);
    let routes = fleet.read_routes(1, &api).await;
    assert_eq!(routes.down, BTreeSet::from([fleet.id(2)]));
    assert!(!routes.routes.contains_key(&fleet.id(2)), "{routes:?}");
}

// Lane 7.
#[tokio::test]
async fn held_then_stopped_then_back_in() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let a = fleet.routed(0, "a", "api", 1).await;

    ensured(&ensure(node, held(a, "run-a", Some(CopyRotation::In))).await);
    assert!(node.running(a), "Held leaves the Container running");

    ensured(&ensure(node, stopped(a)).await);
    assert!(node.container(a).is_some() && !node.running(a));
    assert_eq!(node.rotation(a).await, CopyRotation::Held(run("run-a")));

    let other = ensure(node, into(a, Some(run("run-b")), None)).await;
    refused(&other, ContainerRefusalReason::HeldByOtherRun);
    assert!(!node.running(a));

    let back = ensure(node, into(a, Some(run("run-a")), None)).await;
    assert!(
        matches!(
            back,
            EnsuredContainer::Ensured {
                health: Some(HealthVerdict::Healthy),
                ..
            }
        ),
        "{back:?}"
    );
    assert_eq!(rotation_of(&back), CopyRotation::In);
    assert_eq!(node.rotation(a).await, CopyRotation::In);
    assert!(node.running(a));
}

// Lane 8.
#[tokio::test]
async fn gone_drains_an_out_copy_and_refuses_an_in_copy() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let a = fleet.routed(0, "a", "api", 1).await;
    let b = fleet.routed(0, "b", "api", 1).await;
    ensured(&ensure(node, out(a, true, Some(CopyRotation::In))).await);
    let withdrawn = Arc::new(Mutex::new(Vec::new()));
    *node.fake.on_stop.lock().unwrap() = Some(Box::new({
        let (withdrawn, runtime) = (Arc::clone(&withdrawn), node.runtime.clone());
        move |stopping: &str| {
            let stopping = ContainerId::parse(stopping).unwrap();
            withdrawn
                .lock()
                .unwrap()
                .push(runtime.stopping_ids_for_test().contains(&stopping));
        }
    }));
    let seen = node.requests().len();

    ensured(&ensure(node, gone(a)).await);

    assert_eq!(
        *withdrawn.lock().unwrap(),
        [true],
        "drained before the stop"
    );
    let paths = node
        .requests()
        .split_off(seen)
        .into_iter()
        .map(|(method, path)| format!("{method} {path}"))
        .collect::<Vec<_>>();
    let stop = paths
        .iter()
        .position(|path| *path == format!("POST /containers/{a}/stop"));
    let remove = paths
        .iter()
        .position(|path| path.starts_with(&format!("DELETE /containers/{a}")));
    assert!(
        stop.is_some() && stop < remove,
        "stop before removal: {paths:?}"
    );
    assert!(node.container(a).is_none());

    let reply = ensure(node, gone(b)).await;
    refused(&reply, ContainerRefusalReason::InRotation);
    assert!(node.running(b));
    assert_eq!(node.rotation(b).await, CopyRotation::In);
}

// Lane 9.
#[tokio::test]
async fn older_copy_and_stale_expect() {
    let fleet = Fleet::start().await;
    let newer = fleet.routed(1, "new", "api", 2).await;
    let older = fleet.pending(0, "old", "api", 1).await;

    let reply = ensure(
        fleet.node(0),
        into(older, None, Some(CopyRotation::Pending)),
    )
    .await;
    let message = refused(&reply, ContainerRefusalReason::OlderThanIn);
    assert!(message.contains(&newer.to_string()), "{message}");
    assert_eq!(fleet.node(0).rotation(older).await, CopyRotation::Pending);

    let stale = ensure(fleet.node(1), out(newer, true, Some(CopyRotation::Pending))).await;
    refused(&stale, ContainerRefusalReason::RotationMismatch);
    assert_eq!(fleet.node(1).rotation(newer).await, CopyRotation::In);
}

// Lane 10.
#[tokio::test]
async fn in_for_an_out_copy_then_repeated_out() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let a = fleet.routed(0, "a", "api", 1).await;
    ensured(&ensure(node, out(a, true, Some(CopyRotation::In))).await);

    let back = ensure(node, into(a, None, Some(CopyRotation::Out))).await;
    let message = refused(&back, ContainerRefusalReason::RotationMismatch);
    assert!(message.contains("Out is final"), "{message}");
    assert_eq!(node.rotation(a).await, CopyRotation::Out);

    let repeat = ensure(node, out(a, false, Some(CopyRotation::In))).await;
    ensured(&repeat);
    assert_eq!(rotation_of(&repeat), CopyRotation::Out);
}

/// Deploy 1 read B acked and decided A may leave. Deploy 2 then took B out and
/// removed it. Deploy 1's Out for A still expects A In, which it is, so only the
/// owner's guard keeps the Service from emptying.
#[tokio::test]
async fn stale_out_race() {
    let fleet = Fleet::start().await;
    let api = service("api");
    let a = fleet.routed(0, "a", "api", 1).await;
    let b = fleet.routed(1, "b", "api", 1).await;
    fleet.ack_all(&api, &[a, b]).await;

    ensured(&ensure(fleet.node(1), out(b, true, Some(CopyRotation::In))).await);
    ensured(&ensure(fleet.node(1), gone(b)).await);
    fleet.published(b, None).await;

    let reply = ensure(fleet.node(0), out(a, false, Some(CopyRotation::In))).await;

    refused(&reply, ContainerRefusalReason::LastIn);
    assert_eq!(fleet.node(0).rotation(a).await, CopyRotation::In);
}

/// An In whose health fails answers with the verdict and leaves the copy, and its
/// record, as they were.
#[tokio::test]
async fn unhealthy_in_keeps_rotation() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let unhealthy = json!({"Status": "running", "Health": {"Status": "unhealthy"}});
    let mut copies = Vec::new();
    for (key, rotation) in [
        ("pending", CopyRotation::Pending),
        ("held", CopyRotation::Held(run("run-a"))),
    ] {
        let reply = ensure(
            node,
            ContainerWant::Running {
                container: RunTarget::New(new_copy(key, checked_spec("api"), 1)),
                rotation: RunRotation::Pending,
            },
        )
        .await;
        let copy = present(&reply);
        if let CopyRotation::Held(_) = rotation {
            ensured(&ensure(node, held(copy, "run-a", Some(CopyRotation::Pending))).await);
        }
        node.set_state(copy, unhealthy.clone());
        copies.push((copy, rotation));
    }

    for (copy, rotation) in copies {
        let reply = ensure(node, into(copy, Some(run("run-a")), None)).await;

        let EnsuredContainer::Ensured {
            health: Some(health),
            ..
        } = &reply
        else {
            panic!("expected Ensured with a health verdict, got {reply:?}");
        };
        assert!(matches!(health, HealthVerdict::Failed { .. }), "{health:?}");
        assert_eq!(rotation_of(&reply), rotation);
        assert_eq!(node.rotation(copy).await, rotation);
    }
}

/// A Service without a healthcheck whose copies must stay up `MONITOR` to count
/// as started.
fn monitored_spec(name: &str) -> ResolvedServiceSpec {
    let mut spec = spec(name);
    spec.update.monitor_millis = Some(u64::try_from(MONITOR.as_millis()).unwrap());
    spec
}

const MONITOR: Duration = Duration::from_secs(3);

/// A Pending copy of `spec` on `node` that `deploy` created and waited healthy.
async fn settled_by(
    node: &Node,
    key: &str,
    spec: ResolvedServiceSpec,
    deploy: &str,
) -> ContainerId {
    let mut new = new_copy(key, spec, 1);
    new.answer = AnswerWhen::Settled;
    new.deployment_id = Some(run(deploy));
    let reply = ensure(
        node,
        ContainerWant::Running {
            container: RunTarget::New(new),
            rotation: RunRotation::Pending,
        },
    )
    .await;
    assert!(
        matches!(
            reply,
            EnsuredContainer::Ensured {
                health: Some(HealthVerdict::Healthy),
                ..
            }
        ),
        "{reply:?}"
    );
    present(&reply)
}

/// An In from the run that created a copy and waited it healthy reuses that
/// verdict instead of waiting the monitor window again.
#[tokio::test]
async fn in_reuses_the_health_its_run_settled() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let copy = settled_by(node, "a", monitored_spec("api"), "run-a").await;

    let reply = ensure(
        node,
        into(copy, Some(run("run-a")), Some(CopyRotation::Pending)),
    )
    .await;

    assert_eq!(rotation_of(&reply), CopyRotation::In, "{reply:?}");
    assert_eq!(node.service.local.settles_for_test(&copy), 1);
}

/// A copy another run settled waits out its monitor window again.
#[tokio::test]
async fn in_from_another_run_settles_again() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let copy = settled_by(node, "a", monitored_spec("api"), "run-a").await;

    let started = std::time::Instant::now();
    let reply = ensure(
        node,
        into(copy, Some(run("run-b")), Some(CopyRotation::Pending)),
    )
    .await;

    assert_eq!(rotation_of(&reply), CopyRotation::In, "{reply:?}");
    assert_eq!(node.service.local.settles_for_test(&copy), 2);
    assert!(
        started.elapsed() >= MONITOR * 3 / 4,
        "{:?}",
        started.elapsed()
    );
}

/// A copy restarted since its run settled it is a new Docker run, and settles again.
#[tokio::test]
async fn in_after_a_restart_settles_again() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let copy = settled_by(node, "a", monitored_spec("api"), "run-a").await;
    node.set_state(
        copy,
        json!({"Status": "running", "StartedAt": chrono::Utc::now().to_rfc3339()}),
    );

    let reply = ensure(
        node,
        into(copy, Some(run("run-a")), Some(CopyRotation::Pending)),
    )
    .await;

    assert_eq!(rotation_of(&reply), CopyRotation::In, "{reply:?}");
    assert_eq!(node.service.local.settles_for_test(&copy), 2);
}

/// A copy its run settled healthy but whose check fails now is refused In.
#[tokio::test]
async fn in_for_a_settled_copy_now_unhealthy_is_refused() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let copy = settled_by(node, "a", monitored_spec("api"), "run-a").await;
    let started_at = node.container(copy).unwrap()["State"]["StartedAt"].clone();
    node.set_state(
        copy,
        json!({"Status": "running", "StartedAt": started_at, "Health": {"Status": "unhealthy"}}),
    );

    let reply = ensure(
        node,
        into(copy, Some(run("run-a")), Some(CopyRotation::Pending)),
    )
    .await;

    let EnsuredContainer::Ensured {
        health: Some(health),
        ..
    } = &reply
    else {
        panic!("expected Ensured with a health verdict, got {reply:?}");
    };
    assert!(matches!(health, HealthVerdict::Failed { .. }), "{health:?}");
    assert_eq!(node.rotation(copy).await, CopyRotation::Pending);
}

/// A copy its run settled that has since stopped starts again for the In, and
/// that new Docker run settles again.
#[tokio::test]
async fn in_for_a_settled_copy_since_stopped_settles_again() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let copy = settled_by(node, "a", monitored_spec("api"), "run-a").await;
    node.set_state(copy, json!({"Status": "exited", "ExitCode": 1}));

    let reply = ensure(
        node,
        into(copy, Some(run("run-a")), Some(CopyRotation::Pending)),
    )
    .await;

    assert_eq!(rotation_of(&reply), CopyRotation::In, "{reply:?}");
    assert_eq!(node.service.local.settles_for_test(&copy), 2);
}

/// An In write that lands after a Stopped want planned its stop, but before it took
/// the turn, keeps the copy running.
#[tokio::test]
async fn stopped_rechecks_rotation_under_the_turn() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let a = fleet.pending(0, "a", "api", 1).await;
    let release = Arc::new(Notify::new());
    node.hold_turn(Arc::clone(&release)).await;

    let (reply, ()) = tokio::join!(ensure(node, stopped(a)), async {
        node.withdrawn(a).await;
        node.record(a, CopyRotation::In).await;
        release.notify_one();
    });

    refused(&reply, ContainerRefusalReason::InRotation);
    assert!(node.running(a));
    assert_eq!(node.rotation(a).await, CopyRotation::In);
}

/// A Held write that lands after a Gone want planned its removal keeps the copy.
#[tokio::test]
async fn gone_rechecks_rotation_under_the_turn() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let a = fleet.pending(0, "a", "api", 1).await;
    let release = Arc::new(Notify::new());
    node.hold_turn(Arc::clone(&release)).await;

    let (reply, ()) = tokio::join!(ensure(node, gone(a)), async {
        node.withdrawn(a).await;
        node.record(a, CopyRotation::Held(run("run-a"))).await;
        release.notify_one();
    });

    refused(&reply, ContainerRefusalReason::InRotation);
    assert!(node.running(a));
    assert_eq!(node.rotation(a).await, CopyRotation::Held(run("run-a")));
}

/// A copy that stops after its health settled is not written In.
#[tokio::test]
async fn in_write_rechecks_that_the_copy_runs() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let a = fleet.pending(0, "a", "api", 1).await;
    let before_turn = fleet.cluster.hold_query(CLUSTER_CONTAINERS, 2);

    let (reply, ()) = tokio::join!(
        ensure(node, into(a, None, Some(CopyRotation::Pending))),
        async {
            before_turn.wait().await;
            node.set_state(a, json!({"Status": "exited", "ExitCode": 1}));
            before_turn.wait().await;
        }
    );

    let EnsuredContainer::Ensured { health, .. } = &reply else {
        panic!("expected Ensured, got {reply:?}");
    };
    assert!(
        matches!(health, Some(HealthVerdict::Failed { .. })),
        "{health:?}"
    );
    assert_eq!(rotation_of(&reply), CopyRotation::Pending);
    assert_eq!(node.rotation(a).await, CopyRotation::Pending);
}

/// A copy moved while the LastIn guard read replicated rows is checked again under
/// the turn: the read leaves the turn free, and the fence refuses the stale want.
#[tokio::test]
async fn a_move_after_the_pre_read_is_caught_under_the_turn() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let api = service("api");
    let a = fleet.routed(0, "a", "api", 1).await;
    let b = fleet.routed(1, "b", "api", 1).await;
    fleet.ack_all(&api, &[a, b]).await;
    let reading = fleet.cluster.hold_query(SERVICE_ROUTES, 1);

    let (reply, ()) = tokio::join!(ensure(node, out(a, false, Some(CopyRotation::In))), async {
        reading.wait().await;
        let moved = tokio::time::timeout(
            SETTLE,
            ensure(node, held(a, "run-x", Some(CopyRotation::In))),
        )
        .await
        .expect("the turn is free while the guard reads replicated rows");
        ensured(&moved);
        reading.wait().await;
    });

    refused(&reply, ContainerRefusalReason::RotationMismatch);
    assert_eq!(node.rotation(a).await, CopyRotation::Held(run("run-x")));
}

/// A sibling on a Down Machine serves no one, however the live proxies ack it.
#[tokio::test]
async fn last_in_ignores_a_sibling_on_a_down_machine() {
    let fleet = Fleet::start().await;
    let api = service("api");
    let a = fleet.routed(0, "a", "api", 1).await;
    let b = fleet.routed(2, "b", "api", 1).await;
    fleet.ack(0, &api, &[a, b]).await;
    fleet.ack(1, &api, &[a, b]).await;
    fleet.down(2);

    let reply = ensure(fleet.node(0), out(a, false, Some(CopyRotation::In))).await;

    refused(&reply, ContainerRefusalReason::LastIn);
    assert_eq!(fleet.node(0).rotation(a).await, CopyRotation::In);
}

/// A Machine membership holds no evidence for is unknown, not Down.
#[tokio::test]
async fn stale_evidence_keeps_in_copy_routed() {
    let fleet = Fleet::start().await;
    let api = service("api");
    fleet.membership(0, "Alive");
    fleet.membership(2, "Alive");
    let a = fleet.routed(0, "a", "api", 1).await;
    fleet.addressed(0, a, "10.210.1.2").await;
    let machines = fleet.run_ingress().await;
    fleet.acked_by_all(&api, &[a]).await;
    let mut dns_down = DownMachines::new(fleet.id(2));
    let routed_before = proxy_routes(
        &service_containers(fleet.store.containers().await.unwrap().observations),
        |machine| dns_down.contains(machine),
        &ProxyRoutes::new(),
    );

    let b = fleet.routed(1, "b", "api", 1).await;
    fleet.addressed(1, b, "10.210.2.2").await;

    let down = load_down(&machines, &fleet.admin(), &fleet.id(2))
        .await
        .unwrap();
    assert!(down.is_empty(), "no evidence is not Down: {down:?}");
    dns_down.update(Ok(down));
    let dns_routes = proxy_routes(
        &service_containers(fleet.store.containers().await.unwrap().observations),
        |machine| dns_down.contains(machine),
        &routed_before,
    );
    let dns_routed: BTreeSet<_> = dns_routes[&api]
        .iter()
        .map(|serving| serving.as_observation().container_id)
        .collect();
    assert_eq!(dns_routed, BTreeSet::from([a, b]));
    fleet.acked_by_all(&api, &[a, b]).await;
    let routes = fleet.read_routes(0, &api).await;
    assert!(!routes.down.contains(&fleet.id(1)), "{routes:?}");
    assert!(routes.complete, "{routes:?}");
    assert_eq!(
        routes.routes,
        (0..3)
            .map(|index| (fleet.id(index), Some(BTreeSet::from([a, b]))))
            .collect(),
    );
}

/// A newer local copy brought In while an In want read its guard rows is found under
/// the turn, though the rows read before it never named that copy.
#[tokio::test]
async fn older_than_in_relists_local_copies_under_the_turn() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let a = fleet.pending(0, "a", "api", 1).await;
    let before_turn = fleet.cluster.hold_query(CLUSTER_CONTAINERS, 2);

    let (reply, b) = tokio::join!(
        ensure(node, into(a, None, Some(CopyRotation::Pending))),
        async {
            before_turn.wait().await;
            let b = fleet.routed(0, "b", "api", 2).await;
            before_turn.wait().await;
            b
        }
    );

    let message = refused(&reply, ContainerRefusalReason::OlderThanIn);
    assert!(message.contains(&b.to_string()), "{message}");
    assert_eq!(node.rotation(a).await, CopyRotation::Pending);
    assert_eq!(node.rotation(b).await, CopyRotation::In);
}

/// OlderThanIn reads this Machine's copies from their labels and record, so a local
/// sibling whose health check hangs is never probed while the In want decides.
#[tokio::test]
async fn older_than_in_probes_no_local_copy() {
    // No rescan, so only the wants under test inspect Containers.
    let fleet = Fleet::rescanning(Duration::from_secs(3600)).await;
    let node = fleet.node(0);
    let (hanging, endpoint) = checked_at_endpoint("api", true).await;
    let pending = |key, spec, r#gen| ContainerWant::Running {
        container: RunTarget::New(new_copy(key, spec, r#gen)),
        rotation: RunRotation::Pending,
    };
    let b = present(&ensure(node, pending("b", hanging, 2)).await);
    let a = present(&ensure(node, pending("a", spec("api"), 1)).await);
    node.record(b, CopyRotation::In).await;
    // The publisher re-inspects every copy on a rotation write; let it finish first.
    fleet.published(b, Some(CopyRotation::In)).await;
    node.set_address(b, "127.0.0.1");

    let reply = ensure(node, into(a, None, Some(CopyRotation::Pending))).await;

    let message = refused(&reply, ContainerRefusalReason::OlderThanIn);
    assert!(message.contains(&b.to_string()), "{message}");
    assert_eq!(
        endpoint.hung.load(Ordering::SeqCst),
        0,
        "no health probe ran"
    );
}

/// The In write checks under the turn only that the copy runs, so a health check
/// that hangs once the copy settled holds up no other mutation on the Machine.
#[tokio::test]
async fn in_write_probes_nothing_under_the_turn() {
    // No rescan, so only the wants under test inspect Containers.
    let fleet = Fleet::rescanning(Duration::from_secs(3600)).await;
    let node = fleet.node(0);
    let (checked, endpoint) = checked_at_endpoint("api", false).await;
    let pending = |key, spec| ContainerWant::Running {
        container: RunTarget::New(new_copy(key, spec, 1)),
        rotation: RunRotation::Pending,
    };
    let a = present(&ensure(node, pending("a", checked)).await);
    let b = present(&ensure(node, pending("b", spec("worker"))).await);
    node.set_address(a, "127.0.0.1");
    let before_turn = fleet.cluster.hold_query(CLUSTER_CONTAINERS, 2);

    let (reply, ()) = tokio::join!(
        ensure(node, into(a, None, Some(CopyRotation::Pending))),
        async {
            before_turn.wait().await;
            endpoint.silent.store(true, Ordering::SeqCst);
            let taking = node.hold_inspect();
            before_turn.wait().await;
            // The In takes the turn, which inspects `a` first.
            taking.wait().await;
            let checking = node.hold_inspect();
            taking.wait().await;
            // The In holds the turn and reads whether `a` runs.
            checking.wait().await;
            // Probes after the turn, by the In or the publisher, inspect `a` first.
            let after_turn = node.hold_inspects_of(a).await;
            checking.wait().await;
            ensured(&ensure(node, held(b, "run-b", Some(CopyRotation::Pending))).await);
            // No probe hung under the turn, and none timed out before the Held finished.
            assert_eq!(
                (
                    endpoint.hung.load(Ordering::SeqCst),
                    endpoint.abandoned.load(Ordering::SeqCst)
                ),
                (0, false)
            );
            endpoint.silent.store(false, Ordering::SeqCst);
            drop(after_turn);
        }
    );

    assert_eq!(rotation_of(&reply), CopyRotation::In);
    assert_eq!(node.rotation(b).await, CopyRotation::Held(run("run-b")));
}

/// A Held and a Stopped that land while an In want reads its guard rows leave the
/// copy stopped: the In is refused before it starts anything.
#[tokio::test]
async fn refused_in_starts_nothing() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let a = fleet.pending(0, "a", "api", 1).await;
    let reading = fleet.cluster.hold_query(CLUSTER_CONTAINERS, 1);

    let (reply, ()) = tokio::join!(
        ensure(node, into(a, None, Some(CopyRotation::Pending))),
        async {
            reading.wait().await;
            ensured(&ensure(node, held(a, "run-b", Some(CopyRotation::Pending))).await);
            ensured(&ensure(node, stopped(a)).await);
            reading.wait().await;
        }
    );

    refused(&reply, ContainerRefusalReason::RotationMismatch);
    assert!(!node.running(a), "the refused In started nothing");
    assert_eq!(node.rotation(a).await, CopyRotation::Held(run("run-b")));
}

/// Another run creates the slot, holds and stops the copy while an In want's create
/// still reads the slot as empty. The create adopts that copy and starts nothing.
#[tokio::test]
async fn an_in_that_adopts_a_held_copy_starts_nothing() {
    // No rescan, so only the wants under test inspect the Container.
    let fleet = Fleet::rescanning(Duration::from_secs(3600)).await;
    let node = fleet.node(0);
    let api = spec("api");
    let pending = ContainerWant::Running {
        container: RunTarget::New(new_copy("a", api.clone(), 1)),
        rotation: RunRotation::Pending,
    };
    let routed = ContainerWant::Running {
        container: RunTarget::New(new_copy("a", api, 1)),
        rotation: RunRotation::In {
            run: None,
            expect: Some(CopyRotation::Pending),
        },
    };
    let checking = node.hold_inspect();

    let (reply, a) = tokio::join!(ensure(node, routed), async {
        checking.wait().await;
        let creating = node.hold_answered();
        checking.wait().await;
        creating.wait().await;
        let a = present(&ensure(node, pending).await);
        ensured(&ensure(node, held(a, "run-b", Some(CopyRotation::Pending))).await);
        ensured(&ensure(node, stopped(a)).await);
        creating.wait().await;
        a
    });

    refused(&reply, ContainerRefusalReason::RotationMismatch);
    assert!(!node.running(a), "the refused In started nothing");
    assert_eq!(node.rotation(a).await, CopyRotation::Held(run("run-b")));
}

/// An In that lands after a Stopped want planned its stop, but before it withdrew the
/// copy, keeps the copy routed: the refused stop never withdraws it.
#[tokio::test]
async fn refused_stopped_never_withdraws_the_copy() {
    // No rescan, so only the wants under test inspect the Container.
    let fleet = Fleet::rescanning(Duration::from_secs(3600)).await;
    let node = fleet.node(0);
    let a = present(
        &ensure(
            node,
            ContainerWant::Running {
                container: RunTarget::New(new_copy("a", spec("api"), 1)),
                rotation: RunRotation::Pending,
            },
        )
        .await,
    );
    let stopping = node.runtime.subscribe_stopping_for_test();
    let planning = node.hold_inspect();

    let (reply, ()) = tokio::join!(ensure(node, stopped(a)), async {
        planning.wait().await;
        let stopping_read = node.hold_inspect();
        planning.wait().await;
        stopping_read.wait().await;
        let routed = ensure(node, into(a, None, Some(CopyRotation::Pending))).await;
        assert_eq!(rotation_of(&routed), CopyRotation::In, "{routed:?}");
        stopping_read.wait().await;
    });

    refused(&reply, ContainerRefusalReason::InRotation);
    assert!(node.running(a));
    assert_eq!(node.rotation(a).await, CopyRotation::In);
    assert!(
        !stopping.has_changed().unwrap(),
        "the refused stop never withdrew the copy"
    );
}

#[tokio::test]
async fn in_on_an_in_copy_starts_it() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let a = fleet.routed(0, "a", "api", 1).await;
    node.set_state(a, json!({"Status": "exited", "ExitCode": 1}));

    let reply = ensure(node, into(a, None, None)).await;

    ensured(&reply);
    assert!(node.running(a));
    assert_eq!(rotation_of(&reply), CopyRotation::In);
}

#[tokio::test]
async fn settled_in_settles_health_once() {
    let fleet = Fleet::start().await;
    let node = fleet.node(0);
    let mut spec = spec("api");
    spec.update.monitor_millis = Some(1_000);
    let mut new = new_copy("a", spec, 1);
    new.answer = AnswerWhen::Settled;

    let reply = ensure(
        node,
        ContainerWant::Running {
            container: RunTarget::New(new),
            rotation: RunRotation::In {
                run: None,
                expect: None,
            },
        },
    )
    .await;

    assert!(
        matches!(
            reply,
            EnsuredContainer::Ensured {
                health: Some(HealthVerdict::Healthy),
                ..
            }
        ),
        "{reply:?}"
    );
    assert_eq!(rotation_of(&reply), CopyRotation::In);
    assert_eq!(node.service.local.settles_for_test(&present(&reply)), 1);
}

mod rotation_commands {
    use super::*;

    #[tokio::test]
    async fn allow_empty_takes_the_last_copy_out() {
        let fleet = Fleet::start().await;
        let a = fleet.routed(0, "a", "api", 1).await;

        refused(
            &ensure(fleet.node(0), out(a, false, None)).await,
            ContainerRefusalReason::LastIn,
        );
        ensured(&ensure(fleet.node(0), out(a, true, None)).await);
        assert_eq!(fleet.node(0).rotation(a).await, CopyRotation::Out);
    }

    #[tokio::test]
    async fn gone_refuses_an_in_copy() {
        let fleet = Fleet::start().await;
        let a = fleet.routed(0, "a", "api", 1).await;

        refused(
            &ensure(fleet.node(0), gone(a)).await,
            ContainerRefusalReason::InRotation,
        );
        assert!(fleet.node(0).running(a));
    }

    #[tokio::test]
    async fn stopped_refuses_an_in_copy() {
        let fleet = Fleet::start().await;
        let a = fleet.routed(0, "a", "api", 1).await;

        refused(
            &ensure(fleet.node(0), stopped(a)).await,
            ContainerRefusalReason::InRotation,
        );
        assert!(fleet.node(0).running(a));
    }

    #[tokio::test]
    async fn gone_removes_a_pending_copy() {
        let fleet = Fleet::start().await;
        let a = fleet.pending(0, "a", "api", 1).await;

        ensured(&ensure(fleet.node(0), gone(a)).await);
        assert!(fleet.node(0).container(a).is_none());
    }

    #[tokio::test]
    async fn held_to_out_needs_no_allow_empty() {
        let fleet = Fleet::start().await;
        let a = fleet.routed(0, "a", "api", 1).await;
        ensured(&ensure(fleet.node(0), held(a, "run-a", Some(CopyRotation::In))).await);

        let reply = ensure(
            fleet.node(0),
            out(a, false, Some(CopyRotation::Held(run("run-a")))),
        )
        .await;

        ensured(&reply);
        assert_eq!(fleet.node(0).rotation(a).await, CopyRotation::Out);
    }

    #[tokio::test]
    async fn repeated_held_with_the_same_run_writes_nothing() {
        let fleet = Fleet::start().await;
        let node = fleet.node(0);
        let a = fleet.routed(0, "a", "api", 1).await;
        ensured(&ensure(node, held(a, "run-a", Some(CopyRotation::In))).await);
        let seen = node.requests().len();

        let repeat = ensure(node, held(a, "run-a", Some(CopyRotation::In))).await;

        ensured(&repeat);
        assert_eq!(rotation_of(&repeat), CopyRotation::Held(run("run-a")));
        assert!(
            node.requests()
                .split_off(seen)
                .iter()
                .all(|(method, _)| *method == axum::http::Method::GET),
            "a met Held touches no Docker state"
        );
        assert!(node.running(a));
    }

    #[tokio::test]
    async fn backward_move_is_refused() {
        let fleet = Fleet::start().await;
        let node = fleet.node(0);
        let a = fleet.routed(0, "a", "api", 1).await;
        ensured(&ensure(node, out(a, true, None)).await);

        let reply = ensure(node, held(a, "run-a", None)).await;

        refused(&reply, ContainerRefusalReason::RotationMismatch);
        assert_eq!(node.rotation(a).await, CopyRotation::Out);
        let pending = fleet.pending(0, "b", "api", 1).await;
        let message = refused(
            &ensure(node, out(pending, true, None)).await,
            ContainerRefusalReason::RotationMismatch,
        )
        .to_owned();
        assert!(message.contains("pending"), "{message}");
        assert_eq!(node.rotation(pending).await, CopyRotation::Pending);
    }

    #[tokio::test]
    async fn held_by_another_run_is_refused() {
        let fleet = Fleet::start().await;
        let node = fleet.node(0);
        let a = fleet.routed(0, "a", "api", 1).await;
        ensured(&ensure(node, held(a, "run-a", None)).await);

        refused(
            &ensure(node, held(a, "run-b", None)).await,
            ContainerRefusalReason::HeldByOtherRun,
        );
        refused(
            &ensure(node, into(a, None, None)).await,
            ContainerRefusalReason::HeldByOtherRun,
        );
        assert_eq!(node.rotation(a).await, CopyRotation::Held(run("run-a")));
    }

    #[tokio::test]
    async fn older_than_in_is_refused() {
        let fleet = Fleet::start().await;
        fleet.routed(2, "new", "api", 3).await;
        let older = fleet.pending(0, "old", "api", 2).await;
        fleet.routed(1, "other", "web", 9).await;

        refused(
            &ensure(fleet.node(0), into(older, None, None)).await,
            ContainerRefusalReason::OlderThanIn,
        );
        assert!(fleet.node(0).running(older), "the refusal stops nothing");
        let same_gen = fleet.pending(0, "same", "api", 3).await;
        let reply = ensure(fleet.node(0), into(same_gen, None, None)).await;
        assert_eq!(rotation_of(&reply), CopyRotation::In, "{reply:?}");
    }

    #[tokio::test]
    async fn wrong_expect_is_refused() {
        let fleet = Fleet::start().await;
        let node = fleet.node(0);
        let a = fleet.routed(0, "a", "api", 1).await;

        let reply = ensure(node, held(a, "run-a", Some(CopyRotation::Pending))).await;

        let message = refused(&reply, ContainerRefusalReason::RotationMismatch);
        assert!(
            message.contains("in") && message.contains("pending"),
            "{message}"
        );
        assert_eq!(node.rotation(a).await, CopyRotation::In);
    }

    #[tokio::test]
    async fn repeated_want_is_ensured() {
        let fleet = Fleet::start().await;
        let node = fleet.node(0);
        let a = fleet.routed(0, "a", "api", 1).await;

        let again = ensure(node, into(a, None, Some(CopyRotation::Pending))).await;

        ensured(&again);
        assert_eq!(rotation_of(&again), CopyRotation::In);
        ensured(&ensure(node, out(a, true, Some(CopyRotation::In))).await);
        let repeat = ensure(node, out(a, true, Some(CopyRotation::In))).await;
        ensured(&repeat);
        assert_eq!(rotation_of(&repeat), CopyRotation::Out);
    }

    #[tokio::test]
    async fn held_leaves_the_container_running() {
        let fleet = Fleet::start().await;
        let node = fleet.node(0);
        let a = fleet.routed(0, "a", "api", 1).await;
        let seen = node.requests().len();

        let reply = ensure(node, held(a, "run-a", Some(CopyRotation::In))).await;

        ensured(&reply);
        assert_eq!(rotation_of(&reply), CopyRotation::Held(run("run-a")));
        assert!(node.running(a));
        assert!(
            !node
                .requests()
                .split_off(seen)
                .iter()
                .any(|(_, path)| path.ends_with("/stop")),
            "Held sends no stop"
        );
        fleet
            .published(a, Some(CopyRotation::Held(run("run-a"))))
            .await;
    }

    #[tokio::test]
    async fn stopped_on_a_held_copy_stops_it_and_keeps_held() {
        let fleet = Fleet::start().await;
        let node = fleet.node(0);
        let a = fleet.routed(0, "a", "api", 1).await;
        ensured(&ensure(node, held(a, "run-a", Some(CopyRotation::In))).await);

        let reply = ensure(node, stopped(a)).await;

        ensured(&reply);
        assert!(node.container(a).is_some() && !node.running(a));
        assert_eq!(rotation_of(&reply), CopyRotation::Held(run("run-a")));
        assert_eq!(node.rotation(a).await, CopyRotation::Held(run("run-a")));
    }

    #[tokio::test]
    async fn read_routes_names_a_down_machine() {
        let fleet = Fleet::start().await;
        let api = service("api");
        let a = fleet.routed(0, "a", "api", 1).await;
        fleet.ack(0, &api, &[a]).await;
        fleet.ack(1, &api, &[a]).await;
        fleet.ack(2, &api, &[a]).await;
        fleet.down(2);

        let routes = fleet.read_routes(1, &api).await;

        assert_eq!(routes.down, BTreeSet::from([fleet.id(2)]));
        assert_eq!(
            routes.routes,
            BTreeMap::from([
                (fleet.id(0), Some(BTreeSet::from([a]))),
                (fleet.id(1), Some(BTreeSet::from([a]))),
            ])
        );
        assert!(routes.complete);
        let other = fleet.read_routes(0, &service("web")).await;
        assert_eq!(
            other.routes,
            BTreeMap::from([(fleet.id(0), None), (fleet.id(1), None)]),
            "every live Machine is listed, with no row read"
        );
    }

    #[tokio::test]
    async fn read_routes_keeps_unknown_apart_from_empty() {
        let fleet = Fleet::start().await;
        let api = service("api");
        let a = fleet.routed(0, "a", "api", 1).await;
        fleet.ack(0, &api, &[a]).await;
        fleet
            .cluster
            .write_undecodable_route(&fleet.id(1).to_string(), &api.to_string());

        let routes = fleet.read_routes(0, &api).await;

        assert_eq!(
            routes.routes,
            BTreeMap::from([
                (fleet.id(0), Some(BTreeSet::from([a]))),
                (fleet.id(1), None),
                (fleet.id(2), None),
            ])
        );
        assert!(routes.complete);

        fleet.ack(2, &api, &[]).await;
        let late = MachineId::random();
        fleet.cluster.write_incomplete_machine(late.as_str());
        let routes = fleet.read_routes(0, &api).await;

        assert_eq!(
            routes.routes.get(&fleet.id(2)),
            Some(&Some(BTreeSet::new()))
        );
        assert_eq!(routes.routes.get(&late), Some(&None));
        assert!(!routes.complete, "{routes:?}");
    }
}

/// Times 200 Out wants spread over 50 Services, each with an acked In sibling on
/// another Machine, so every want runs the full LastIn guard and the turn re-check.
#[tokio::test]
#[ignore = "perf probe: prints the p95 of 200 Out wants"]
async fn out_want_p95() {
    let fleet = Fleet::rescanning(Duration::from_secs(1)).await;
    let mut kept = Vec::new();
    let mut wants = Vec::new();
    for index in 0..50 {
        let name = format!("svc-{index}");
        let keep = fleet.brought_in(1, &format!("{name}-keep"), &name, 1).await;
        let mut siblings = vec![keep];
        for copy in 0..4 {
            siblings.push(
                fleet
                    .brought_in(0, &format!("{name}-{copy}"), &name, 1)
                    .await,
            );
        }
        fleet.ack_all(&service(&name), &siblings).await;
        kept.push(keep);
        wants.extend(siblings.into_iter().skip(1));
    }
    for keep in kept {
        fleet.published(keep, Some(CopyRotation::In)).await;
    }
    // Every rotation write wakes a full resync of the 200 Containers, which the
    // in-process fake Docker serves on the same cores; the probe times the want.
    fleet.shutdown.cancel();

    let mut took = Vec::with_capacity(wants.len());
    for copy in wants {
        let started = std::time::Instant::now();
        let reply = ensure(fleet.node(0), out(copy, false, Some(CopyRotation::In))).await;
        took.push(started.elapsed());
        ensured(&reply);
    }

    took.sort_unstable();
    let p95 = *took.get(took.len() * 95 / 100 - 1).unwrap();
    println!("out_want_p95 p95 {:.3} ms", p95.as_secs_f64() * 1e3);
}

#[path = "volume_move_tests.rs"]
mod volume_move;
