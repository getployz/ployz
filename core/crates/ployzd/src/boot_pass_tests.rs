use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
    time::Duration,
};

use ployz_core::{
    AdvertisedEndpoint, ContainerAddress, ContainerId, ContainerKind, ContainerLocator,
    ContainerNow, ContainerObservation, ContainerRuntimeObservation, ContainerWant, CopyRotation,
    DeploymentLogId, EnsuredContainer, Gen, HealthObservation, Machine, MachineId, MachineName,
    MachineRuntime, MembershipEvidence, Namespace, QualifiedService, ResolvedServiceSpec,
    WireGuardPublicKey,
};
use serde_json::json;
use tokio::{sync::watch, time::Instant};

use super::{BOOT_PASS_WAIT, BootHost, Observed, run};
use crate::{
    corrosion::{
        Error as CorrosionError, IngressRouteAck, IngressRouteRow, ReplicatedObservations,
        ReplicatedStore, store_tests::identity_store, wait_for_catch_up,
    },
    machine::LocalMachineError,
    route_acks::required_ackers,
    test_dir::TestDir,
};

const ACTOR: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

fn machine(name: &str, octet: u8) -> Machine {
    Machine {
        labels: Default::default(),
        accepts_builds: true,
        accepts_services: true,
        accepts_ingress: true,
        id: MachineId::random(),
        name: MachineName::parse(name).unwrap(),
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

fn api() -> QualifiedService {
    QualifiedService::parse("app/api").unwrap()
}

fn copy(suffix: u8, machine: &Machine, r#gen: u64) -> ContainerObservation {
    let resolved_spec: ResolvedServiceSpec = serde_json::from_value(json!({
        "service_id": "a".repeat(32),
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "example.test/image", "pull_policy": "missing" },
    }))
    .unwrap();
    ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        rotation: Some(CopyRotation::In),
        r#gen: Some(Gen::new(r#gen)),
        container_id: ContainerId::parse(format!("{suffix:x}").repeat(64)).unwrap(),
        display_name: format!("api-{suffix}"),
        created_at_unix_nanos: 0,
        machine_id: machine.id,
        namespace: Namespace::parse("app").unwrap(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
        effective_healthcheck: None,
        resolved_spec,
        address: Some(ContainerAddress([10, 210, suffix, 2].into())),
        labels: BTreeMap::new(),
    })
    .unwrap()
}

/// Every Machine in `routing` acks the given copies of `api`.
fn routes(
    routing: &[(&Machine, &[&ContainerObservation])],
) -> BTreeMap<(MachineId, QualifiedService), IngressRouteRow> {
    routing
        .iter()
        .map(|(machine, copies)| {
            let containers = copies.iter().map(|copy| copy.container_id).collect();
            (
                (machine.id, api()),
                IngressRouteRow::Acked(IngressRouteAck {
                    deploy: DeploymentLogId::parse("d2").unwrap(),
                    containers,
                }),
            )
        })
        .collect()
}

fn observed(
    local: &Machine,
    machines: &[&Machine],
    down: &[&Machine],
    containers: Vec<ContainerObservation>,
    routes: BTreeMap<(MachineId, QualifiedService), IngressRouteRow>,
) -> Observed {
    let states = down
        .iter()
        .map(|machine| (machine.management_address(), MembershipEvidence::Down))
        .collect::<BTreeMap<_, _>>();
    let membership = required_ackers(
        ReplicatedObservations {
            observations: machines.iter().map(|machine| (*machine).clone()).collect(),
            incomplete_ids: Vec::new(),
        },
        &local.id,
        Some(&states),
    );
    Observed {
        containers,
        routes,
        membership,
    }
}

fn out(copy: &ContainerObservation) -> ContainerWant {
    ContainerWant::Out {
        container: ContainerLocator::Id(copy.container_id),
        allow_empty: false,
        expect: Some(CopyRotation::In),
    }
}

/// A powered-off Machine back with gen 1 In, while a peer serves gen 2 and a
/// second peer runs only a proxy.
struct Cluster {
    local: Machine,
    peer: Machine,
    proxy: Machine,
    old: ContainerObservation,
    new: ContainerObservation,
}

impl Cluster {
    fn new() -> Self {
        let (local, peer, proxy) = (machine("local", 1), machine("peer", 2), machine("proxy", 3));
        let old = copy(1, &local, 1);
        let new = copy(2, &peer, 2);
        Self {
            local,
            peer,
            proxy,
            old,
            new,
        }
    }

    fn acked_everywhere(&self) -> Observed {
        observed(
            &self.local,
            &[&self.local, &self.peer, &self.proxy],
            &[],
            vec![self.old.clone(), self.new.clone()],
            routes(&[
                (&self.local, &[&self.new]),
                (&self.peer, &[&self.new]),
                (&self.proxy, &[&self.new]),
            ]),
        )
    }
}

#[derive(Clone)]
struct FakeHost {
    local: MachineId,
    targets: watch::Receiver<Option<BTreeMap<String, i64>>>,
    /// `None` is a store already caught up to any target.
    store: Option<ReplicatedStore>,
    observed: Arc<Mutex<Option<Observed>>>,
    wants: Arc<Mutex<Vec<ContainerWant>>>,
}

impl FakeHost {
    fn new(local: &Machine, observed: Observed) -> Self {
        Self {
            local: local.id,
            targets: watch::channel(Some(BTreeMap::new())).1,
            store: None,
            observed: Arc::new(Mutex::new(Some(observed))),
            wants: Arc::default(),
        }
    }

    fn wants(&self) -> Vec<ContainerWant> {
        self.wants.lock().unwrap().clone()
    }
}

impl BootHost for FakeHost {
    fn machine_id(&self) -> MachineId {
        self.local
    }

    async fn peer_targets(&self) -> Option<BTreeMap<String, i64>> {
        self.targets.borrow().clone()
    }

    async fn catch_up(&self, target: &BTreeMap<String, i64>) -> Result<(), CorrosionError> {
        match &self.store {
            Some(store) => wait_for_catch_up(store, target).await,
            None => Ok(()),
        }
    }

    async fn observe(&self) -> Result<Observed, CorrosionError> {
        Ok(self
            .observed
            .lock()
            .unwrap()
            .take()
            .expect("the pass reads the store once"))
    }

    async fn ensure(&self, want: ContainerWant) -> Result<EnsuredContainer, LocalMachineError> {
        let ContainerWant::Out { container, .. } = &want else {
            panic!("the boot pass only sets Out: {want:?}");
        };
        let now = ContainerNow::Absent {
            of: container.clone(),
        };
        self.wants.lock().unwrap().push(want);
        Ok(EnsuredContainer::Ensured { now, health: None })
    }
}

fn published() -> watch::Receiver<bool> {
    watch::channel(true).1
}

async fn pass(local: &Machine, observed: Observed) -> Vec<ContainerWant> {
    let host = FakeHost::new(local, observed);
    run(host.clone(), published()).await;
    host.wants()
}

#[tokio::test(start_paused = true)]
async fn higher_gen_acked_everywhere() {
    let cluster = Cluster::new();

    let wants = pass(&cluster.local, cluster.acked_everywhere()).await;

    assert_eq!(wants, vec![out(&cluster.old)]);
}

#[tokio::test(start_paused = true)]
async fn higher_gen_missing_one_ack() {
    let cluster = Cluster::new();
    let observed = observed(
        &cluster.local,
        &[&cluster.local, &cluster.peer, &cluster.proxy],
        &[],
        vec![cluster.old.clone(), cluster.new.clone()],
        routes(&[
            (&cluster.local, &[&cluster.new]),
            (&cluster.peer, &[&cluster.new]),
            (&cluster.proxy, &[&cluster.old]),
        ]),
    );

    assert!(pass(&cluster.local, observed).await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn equal_gen() {
    let cluster = Cluster::new();
    let same = copy(2, &cluster.peer, 1);
    let observed = observed(
        &cluster.local,
        &[&cluster.local, &cluster.peer, &cluster.proxy],
        &[],
        vec![cluster.old.clone(), same.clone()],
        routes(&[
            (&cluster.local, &[&same]),
            (&cluster.peer, &[&same]),
            (&cluster.proxy, &[&same]),
        ]),
    );

    assert!(pass(&cluster.local, observed).await.is_empty());
}

#[tokio::test(start_paused = true)]
async fn down_proxy() {
    let cluster = Cluster::new();
    let observed = observed(
        &cluster.local,
        &[&cluster.local, &cluster.peer, &cluster.proxy],
        &[&cluster.proxy],
        vec![cluster.old.clone(), cluster.new.clone()],
        routes(&[
            (&cluster.local, &[&cluster.new]),
            (&cluster.peer, &[&cluster.new]),
        ]),
    );

    assert_eq!(
        pass(&cluster.local, observed).await,
        vec![out(&cluster.old)]
    );
}

/// A file-backed store, so the test edits the rows the store's server reads.
async fn store_at_version(dir: &TestDir, version: i64) -> (ReplicatedStore, rusqlite::Connection) {
    std::fs::create_dir_all(&dir.0).unwrap();
    let path = dir.0.join("corrosion.db");
    let db = rusqlite::Connection::open(&path).unwrap();
    db.execute_batch(&format!(
        "CREATE TABLE crsql_db_versions (site_id BLOB PRIMARY KEY, db_version INTEGER);
         CREATE TABLE __corro_bookkeeping_gaps (actor_id BLOB, start INTEGER, end INTEGER);
         CREATE TABLE __corro_seq_bookkeeping (site_id BLOB, db_version INTEGER);
         INSERT INTO crsql_db_versions VALUES (X'{ACTOR}', {version});"
    ))
    .unwrap();
    let (store, _server) = identity_store(rusqlite::Connection::open(&path).unwrap()).await;
    (store, db)
}

#[tokio::test]
async fn waits_for_peer_targets_and_own_ack() {
    let dir = TestDir::new("ployzd-boot-pass");
    let (store, db) = store_at_version(&dir, 3).await;
    let cluster = Cluster::new();
    let (peer, targets) = watch::channel(None);
    let host = FakeHost {
        targets,
        store: Some(store),
        ..FakeHost::new(&cluster.local, cluster.acked_everywhere())
    };
    let (first_publish, first_published) = watch::channel(false);
    let running = tokio::spawn(run(host.clone(), first_published));

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(host.wants().is_empty(), "no peer has answered yet");
    peer.send_replace(Some(BTreeMap::from([(ACTOR.to_owned(), 5)])));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(host.wants().is_empty(), "the store is behind the peer");
    db.execute("UPDATE crsql_db_versions SET db_version = 5", [])
        .unwrap();
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(
        host.wants().is_empty(),
        "this Machine's ingress has not published"
    );
    assert!(!running.is_finished());
    first_publish.send_replace(true);
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(host.wants(), vec![out(&cluster.old)]);
}

#[tokio::test(start_paused = true)]
async fn no_reachable_peer_does_nothing() {
    let cluster = Cluster::new();
    let host = FakeHost {
        targets: watch::channel(None).1,
        ..FakeHost::new(&cluster.local, cluster.acked_everywhere())
    };
    let started = Instant::now();

    run(host.clone(), published()).await;

    assert_eq!(started.elapsed(), BOOT_PASS_WAIT);
    assert!(host.wants().is_empty());
}

#[tokio::test(start_paused = true)]
async fn times_out_once_and_does_nothing() {
    let cluster = Cluster::new();
    let host = FakeHost::new(&cluster.local, cluster.acked_everywhere());
    let (_first_publish, never_published) = watch::channel(false);
    let started = Instant::now();

    run(host.clone(), never_published).await;

    assert_eq!(started.elapsed(), BOOT_PASS_WAIT);
    assert!(host.wants().is_empty());
}
