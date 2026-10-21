use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::Duration,
};

use ployz_core::{
    AdvertisedEndpoint, ContainerAddress, ContainerId, ContainerKind, ContainerLocator,
    ContainerNow, ContainerObservation, ContainerRuntimeObservation, ContainerWant, CopyRotation,
    EnsuredContainer, Gen, HealthObservation, Machine, MachineId, MachineName, MachineRuntime,
    MembershipEvidence, Namespace, PullPolicy, QualifiedService, ResolvedServiceSpec,
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
    ingress::PublishedRoutes,
    machine::LocalMachineError,
    route_acks::{required_ackers, routed},
    test_dir::TestDir,
};

const ACTOR: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const LOCAL_ACTOR: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

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
        stopped: false,
    })
    .unwrap()
}

fn routes(
    routing: &[(&Machine, &[&ContainerObservation])],
) -> BTreeMap<(MachineId, QualifiedService), IngressRouteRow> {
    routing
        .iter()
        .map(|(machine, copies)| {
            let containers = copies.iter().map(|copy| copy.container_id).collect();
            (
                (machine.id, api()),
                IngressRouteRow::Acked(IngressRouteAck { containers }),
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
        let new = newer_image(copy(2, &peer, 2));
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
    local_actor: &'static str,
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
            local_actor: LOCAL_ACTOR,
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

    async fn local_actor(&self) -> Result<String, CorrosionError> {
        Ok(self.local_actor.to_owned())
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

/// What this Machine's ingress acked per Service, read from its row in `observed`
/// as a process that published after catch-up would have it.
fn own_acks(
    local: &Machine,
    observed: &Observed,
) -> BTreeMap<QualifiedService, BTreeSet<ContainerId>> {
    routed(observed.routes.clone(), &api())
        .remove(&local.id)
        .map(|containers| BTreeMap::from([(api(), containers)]))
        .unwrap_or_default()
}

fn synced() -> watch::Receiver<bool> {
    watch::channel(true).1
}

/// Ingress publishes again on its own every few seconds, so the pass never waits
/// on a fake that has stopped publishing.
fn published(
    routes: BTreeMap<QualifiedService, BTreeSet<ContainerId>>,
) -> watch::Receiver<PublishedRoutes> {
    let (publishes, published) = watch::channel(PublishedRoutes::default());
    tokio::spawn(async move {
        while !publishes.is_closed() {
            publishes.send_modify(|published| {
                published.count += 1;
                published.routes.clone_from(&routes);
            });
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    });
    published
}

async fn pass(local: &Machine, observed: Observed) -> Vec<ContainerWant> {
    let own = own_acks(local, &observed);
    let host = FakeHost::new(local, observed);
    run(host.clone(), synced(), published(own)).await;
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

fn newer_image(mut copy: ContainerObservation) -> ContainerObservation {
    copy.try_update(|parts| parts.resolved_spec.container.image = "example.test/image:2".into())
        .unwrap();
    copy
}

#[tokio::test(start_paused = true)]
async fn kept_beside_a_newer_gen_of_its_image() {
    let cluster = Cluster::new();
    let kept = newer_image(copy(1, &cluster.local, 1));
    let observed = observed(
        &cluster.local,
        &[&cluster.local, &cluster.peer, &cluster.proxy],
        &[],
        vec![kept.clone(), cluster.new.clone()],
        routes(&[
            (&cluster.local, &[&kept, &cluster.new]),
            (&cluster.peer, &[&kept, &cluster.new]),
            (&cluster.proxy, &[&kept, &cluster.new]),
        ]),
    );

    assert!(pass(&cluster.local, observed).await.is_empty());
}

fn respec(
    mut copy: ContainerObservation,
    edit: impl FnOnce(&mut ResolvedServiceSpec),
) -> ContainerObservation {
    copy.try_update(|parts| edit(&mut parts.resolved_spec))
        .unwrap();
    copy
}

fn memory(copy: ContainerObservation, bytes: i64) -> ContainerObservation {
    respec(copy, |spec| {
        spec.container.resources.memory_bytes = Some(bytes.try_into().unwrap());
    })
}

fn digest(mut copy: ContainerObservation, value: &str) -> ContainerObservation {
    copy.try_update(|parts| {
        parts
            .labels
            .insert(crate::docker::LABEL_RECREATE_DIGEST.into(), value.into());
    })
    .unwrap();
    copy
}

fn routed_everywhere(
    cluster: &Cluster,
    older: &ContainerObservation,
    newer: &ContainerObservation,
) -> Observed {
    observed(
        &cluster.local,
        &[&cluster.local, &cluster.peer, &cluster.proxy],
        &[],
        vec![older.clone(), newer.clone()],
        routes(&[
            (&cluster.local, &[older, newer]),
            (&cluster.peer, &[older, newer]),
            (&cluster.proxy, &[older, newer]),
        ]),
    )
}

async fn replaced_by(
    older: impl FnOnce(ContainerObservation) -> ContainerObservation,
    newer: impl FnOnce(ContainerObservation) -> ContainerObservation,
) -> bool {
    let cluster = Cluster::new();
    let older = older(copy(1, &cluster.local, 1));
    let newer = newer(copy(2, &cluster.peer, 2));
    let wants = pass(&cluster.local, routed_everywhere(&cluster, &older, &newer)).await;
    match wants.as_slice() {
        [] => false,
        [want] if *want == out(&older) => true,
        other => panic!("unexpected wants {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn replaced_by_a_new_pre_deploy_hook() {
    assert!(
        replaced_by(
            |copy| copy,
            |copy| respec(copy, |spec| {
                spec.pre_deploy =
                    Some(serde_json::from_value(json!({ "command": ["migrate"] })).unwrap());
            })
        )
        .await
    );
}

#[tokio::test(start_paused = true)]
async fn replaced_by_an_always_pull() {
    assert!(
        replaced_by(
            |copy| copy,
            |copy| respec(copy, |spec| spec.container.pull_policy = PullPolicy::Always)
        )
        .await
    );
}

#[tokio::test(start_paused = true)]
async fn replaced_by_a_lowered_memory_limit() {
    assert!(replaced_by(|copy| memory(copy, 1 << 30), |copy| memory(copy, 1 << 29)).await);
}

#[tokio::test(start_paused = true)]
async fn kept_beside_a_raised_memory_limit() {
    assert!(!replaced_by(|copy| memory(copy, 1 << 29), |copy| memory(copy, 1 << 30)).await);
}

#[tokio::test(start_paused = true)]
async fn replaced_by_other_environment_or_config_content() {
    assert!(replaced_by(|copy| digest(copy, "a"), |copy| digest(copy, "b")).await);
}

#[tokio::test(start_paused = true)]
async fn kept_beside_an_equal_digest() {
    assert!(!replaced_by(|copy| digest(copy, "a"), |copy| digest(copy, "a")).await);
}

#[tokio::test(start_paused = true)]
async fn kept_when_either_copy_predates_the_digest() {
    assert!(!replaced_by(|copy| copy, |copy| copy).await);
    assert!(!replaced_by(|copy| copy, |copy| digest(copy, "b")).await);
    assert!(!replaced_by(|copy| digest(copy, "a"), |copy| copy).await);
}

#[tokio::test(start_paused = true)]
async fn superseded_by_the_newest_replacing_gen() {
    let cluster = Cluster::new();
    let older = copy(1, &cluster.local, 1);
    let kept = copy(3, &cluster.peer, 2);
    let replacing = newer_image(copy(4, &cluster.peer, 3));
    let all = [&older, &kept, &replacing];
    let observed = observed(
        &cluster.local,
        &[&cluster.local, &cluster.peer, &cluster.proxy],
        &[],
        all.iter().map(|copy| (*copy).clone()).collect(),
        routes(&[
            (&cluster.local, &all),
            (&cluster.peer, &all),
            (&cluster.proxy, &all),
        ]),
    );
    let own = own_acks(&cluster.local, &observed);

    assert_eq!(
        super::superseded(&cluster.local.id, &observed, &own),
        vec![super::Superseded {
            service: api(),
            container: older.container_id,
            by: Gen::new(3),
        }]
    );
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

#[tokio::test(start_paused = true)]
async fn stale_own_row_does_not_ack() {
    let cluster = Cluster::new();
    let host = FakeHost::new(&cluster.local, cluster.acked_everywhere());

    run(host.clone(), synced(), published(BTreeMap::new())).await;

    assert!(
        host.wants().is_empty(),
        "this Machine's row predates its power-off, and its ingress has not acked the Service since"
    );
}

#[tokio::test]
async fn waits_for_peer_targets_and_catch_up() {
    let dir = TestDir::new("ployzd-boot-pass");
    let (store, db) = store_at_version(&dir, 3).await;
    let cluster = Cluster::new();
    let observed = cluster.acked_everywhere();
    let own = own_acks(&cluster.local, &observed);
    let (peer, targets) = watch::channel(None);
    let host = FakeHost {
        targets,
        store: Some(store),
        ..FakeHost::new(&cluster.local, observed)
    };
    let running = tokio::spawn(run(host.clone(), synced(), published(own)));

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(host.wants().is_empty(), "no peer has answered yet");
    peer.send_replace(Some(BTreeMap::from([(ACTOR.to_owned(), 5)])));
    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(host.wants().is_empty(), "the store is behind the peer");
    db.execute("UPDATE crsql_db_versions SET db_version = 5", [])
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(host.wants(), vec![out(&cluster.old)]);
}

/// Ingress as it runs at boot: each publish renders the routes from the store as
/// it stood when the render began, and publishing starts before catch-up.
fn rendered_from(
    path: std::path::PathBuf,
    caught_up_at: i64,
    stale: BTreeMap<QualifiedService, BTreeSet<ContainerId>>,
    fresh: BTreeMap<QualifiedService, BTreeSet<ContainerId>>,
) -> watch::Receiver<PublishedRoutes> {
    let (publishes, published) = watch::channel(PublishedRoutes::default());
    tokio::spawn(async move {
        let db = rusqlite::Connection::open(path).unwrap();
        while !publishes.is_closed() {
            let version: i64 = db
                .query_row("SELECT max(db_version) FROM crsql_db_versions", [], |row| {
                    row.get(0)
                })
                .unwrap();
            let routes = if version >= caught_up_at {
                &fresh
            } else {
                &stale
            };
            tokio::time::sleep(Duration::from_millis(100)).await;
            publishes.send_modify(|published| {
                published.count += 1;
                published.routes.clone_from(routes);
            });
        }
    });
    published
}

#[tokio::test]
async fn acks_only_from_a_publish_rendered_after_catch_up() {
    let dir = TestDir::new("ployzd-boot-pass-render");
    let (store, db) = store_at_version(&dir, 3).await;
    let cluster = Cluster::new();
    // This Machine's row predates the power-off and routes only its old copy.
    let observed = observed(
        &cluster.local,
        &[&cluster.local, &cluster.peer, &cluster.proxy],
        &[],
        vec![cluster.old.clone(), cluster.new.clone()],
        routes(&[
            (&cluster.local, &[&cluster.old]),
            (&cluster.peer, &[&cluster.new]),
            (&cluster.proxy, &[&cluster.new]),
        ]),
    );
    let stale = BTreeMap::from([(api(), BTreeSet::from([cluster.old.container_id]))]);
    let fresh = BTreeMap::from([(
        api(),
        BTreeSet::from([cluster.old.container_id, cluster.new.container_id]),
    )]);
    let host = FakeHost {
        targets: watch::channel(Some(BTreeMap::from([(ACTOR.to_owned(), 5)]))).1,
        store: Some(store),
        ..FakeHost::new(&cluster.local, observed)
    };
    let published = rendered_from(dir.0.join("corrosion.db"), 5, stale, fresh);
    let running = tokio::spawn(run(host.clone(), synced(), published));

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(host.wants().is_empty(), "the store is behind the peer");
    db.execute("UPDATE crsql_db_versions SET db_version = 5", [])
        .unwrap();
    tokio::time::timeout(Duration::from_secs(5), running)
        .await
        .unwrap()
        .unwrap();

    assert_eq!(host.wants(), vec![out(&cluster.old)]);
}

#[tokio::test]
async fn catch_up_skips_this_machines_own_actor() {
    const PEER_ACTOR: &str = "cccccccccccccccccccccccccccccccc";
    let dir = TestDir::new("ployzd-boot-pass-own-actor");
    let (store, db) = store_at_version(&dir, 25).await;
    db.execute(
        &format!("INSERT INTO crsql_db_versions VALUES (X'{PEER_ACTOR}', 4)"),
        [],
    )
    .unwrap();
    let cluster = Cluster::new();
    let observed = cluster.acked_everywhere();
    let own = own_acks(&cluster.local, &observed);
    // A power cut lost this Machine's last two commits, which the peer still holds.
    let host = FakeHost {
        targets: watch::channel(Some(BTreeMap::from([
            (ACTOR.to_owned(), 27),
            (PEER_ACTOR.to_owned(), 4),
        ])))
        .1,
        local_actor: ACTOR,
        store: Some(store),
        ..FakeHost::new(&cluster.local, observed)
    };

    tokio::time::timeout(
        Duration::from_secs(5),
        run(host.clone(), synced(), published(own)),
    )
    .await
    .expect("catch-up waited for this Machine's own lost writes");

    assert_eq!(host.wants(), vec![out(&cluster.old)]);
}

#[tokio::test(start_paused = true)]
async fn waits_for_two_publishes_after_catch_up() {
    let cluster = Cluster::new();
    let observed = cluster.acked_everywhere();
    let own = own_acks(&cluster.local, &observed);
    let host = FakeHost::new(&cluster.local, observed);
    let (publishes, published) = watch::channel(PublishedRoutes {
        count: 1,
        routes: own.clone(),
    });
    let running = tokio::spawn(run(host.clone(), synced(), published));
    let publish = || {
        publishes.send_modify(|published| {
            published.count += 1;
            published.routes.clone_from(&own);
        });
    };

    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        host.wants().is_empty(),
        "a publish before catch-up read the store before it"
    );
    publish();
    tokio::time::sleep(Duration::from_secs(1)).await;
    assert!(
        host.wants().is_empty(),
        "the first publish since catch-up may have read the store before it"
    );
    publish();
    running.await.unwrap();

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

    run(host.clone(), synced(), published(BTreeMap::new())).await;

    assert_eq!(started.elapsed(), BOOT_PASS_WAIT);
    assert!(host.wants().is_empty());
}

#[tokio::test(start_paused = true)]
async fn times_out_once_and_does_nothing() {
    let cluster = Cluster::new();
    let host = FakeHost::new(&cluster.local, cluster.acked_everywhere());
    let (_publishes, never_published) = watch::channel(PublishedRoutes::default());
    let started = Instant::now();

    run(host.clone(), synced(), never_published).await;

    assert_eq!(started.elapsed(), BOOT_PASS_WAIT);
    assert!(host.wants().is_empty());
}

#[tokio::test(start_paused = true)]
async fn first_sync_counts_toward_the_deadline() {
    let cluster = Cluster::new();
    let observed = cluster.acked_everywhere();
    let own = own_acks(&cluster.local, &observed);
    let host = FakeHost::new(&cluster.local, observed);
    let (_sync, never_synced) = watch::channel(false);
    let started = Instant::now();

    run(host.clone(), never_synced, published(own)).await;

    assert_eq!(started.elapsed(), BOOT_PASS_WAIT);
    assert!(host.wants().is_empty());
}
