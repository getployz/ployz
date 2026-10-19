//! Three real Machine API handlers over one fake Corrosion cluster, each on its own
//! fake Docker. No proxy acks rotation yet, so the tests write the ack rows and the
//! membership by hand; every Container row reaches the cluster through the real
//! observation publisher.

#![expect(clippy::indexing_slicing, reason = "JSON fixture reads")]

use std::{
    collections::{BTreeMap, BTreeSet},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use futures_util::{SinkExt, StreamExt};
use ployz_core::{
    AdvertisedEndpoint, ConfiguredHealthcheck, ContainerId, ContainerLocator, ContainerNow,
    ContainerRefusalReason, ContainerWant, CopyRotation, DeploymentLogId, EnsureContainerRequest,
    EnsuredContainer, HealthVerdict, HealthcheckCommand, HealthcheckSpec, Machine, MachineId,
    MachineName, MachineRpc, MachineRuntime, NewContainer, QualifiedService, ReadRoutesRequest,
    ResolvedServiceSpec, RoutesRead, RunRotation, RunTarget, WireGuardPublicKey, op,
};
use serde_json::{Value, json};
use tokio::net::UnixListener;
use tokio_util::{codec::LengthDelimitedCodec, sync::CancellationToken};
use tonic::Request;

use super::MachineService;
use crate::corrosion::{
    AdminClient, IngressRouteAck, IngressRouteChange, ReplicatedStore, fake_cluster::FakeCluster,
};
use crate::docker::{
    ContainerRuntime,
    test_support::{FakeDocker, fake_runtime_with, spec_with_sources},
};
use crate::machine::{LocalMachineStore, RecordOwner};

const SETTLE: Duration = Duration::from_secs(10);

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
    _cluster: FakeCluster,
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
            _cluster: cluster,
        }
    }

    fn node(&self, index: usize) -> &Node {
        self.nodes.get(index).unwrap()
    }

    fn id(&self, index: usize) -> MachineId {
        self.node(index).row.id
    }

    /// Report `index` Down in every Machine's membership sample.
    fn down(&self, index: usize) {
        let row = &self.node(index).row;
        let address = std::net::SocketAddr::new(
            row.management_address().0.into(),
            ployz_core::CORROSION_GOSSIP_PORT,
        );
        self.states
            .lock()
            .unwrap()
            .push(json!({ "id": { "addr": address }, "state": "Down" }));
    }

    /// Write `index`'s proxy ack for `service`, routing `containers`.
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
        self.runtime.rotation(&container).await.unwrap()
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

fn new_copy(key: &str, spec: ResolvedServiceSpec, r#gen: u64) -> NewContainer {
    serde_json::from_value(json!({
        "slot": {"namespace": "app", "kind": "service_container", "creation_key": key},
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
                (fleet.id(0), BTreeSet::from([a])),
                (fleet.id(1), BTreeSet::from([a])),
            ])
        );
        let other = fleet.read_routes(0, &service("web")).await;
        assert_eq!(
            other.routes,
            BTreeMap::from([
                (fleet.id(0), BTreeSet::new()),
                (fleet.id(1), BTreeSet::new())
            ]),
            "every live Machine is listed, routing nothing"
        );
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
