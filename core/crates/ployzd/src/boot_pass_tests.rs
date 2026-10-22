use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    num::NonZeroU32,
    sync::{Arc, Mutex},
    time::Duration,
};

use ployz_core::{
    AdvertisedEndpoint, ContainerAddress, ContainerId, ContainerKind, ContainerLocator,
    ContainerNow, ContainerObservation, ContainerRuntimeObservation, ContainerWant, CopyRotation,
    EnsuredContainer, Gen, HealthObservation, Machine, MachineId, MachineName, MachineRuntime,
    MembershipEvidence, Namespace, PullPolicy, QualifiedService, ResolvedServiceSpec, ServiceMode,
    UpdateOrder, WireGuardPublicKey,
};
use serde_json::json;
use tokio::{sync::watch, time::Instant};

use super::{BOOT_PASS_WAIT, BootHost, Observed, Peers, run};
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
    targets: watch::Receiver<Peers>,
    local_actor: &'static str,
    /// `None` is a store already caught up to any target.
    store: Option<ReplicatedStore>,
    /// The views the store gives, in order; the last one stays.
    observed: Arc<Mutex<VecDeque<Observed>>>,
    reads: Arc<Mutex<usize>>,
    /// What each owner answers for its Containers; a Container missing here gets no answer.
    answers: BTreeMap<ContainerId, ContainerObservation>,
    /// How many more times each Container's owner fails to answer before it does.
    refusals: Arc<Mutex<BTreeMap<ContainerId, usize>>>,
    wants: Arc<Mutex<Vec<ContainerWant>>>,
}

impl FakeHost {
    fn new(local: &Machine, observed: Observed) -> Self {
        let answers = observed
            .containers
            .iter()
            .map(|copy| (copy.container_id, copy.clone()))
            .collect();
        Self {
            local: local.id,
            targets: watch::channel(Peers::Answered(BTreeMap::new())).1,
            local_actor: LOCAL_ACTOR,
            store: None,
            observed: Arc::new(Mutex::new(VecDeque::from([observed]))),
            reads: Arc::default(),
            answers,
            refusals: Arc::default(),
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

    async fn peer_targets(&self) -> Peers {
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
        *self.reads.lock().unwrap() += 1;
        let mut views = self.observed.lock().unwrap();
        Ok(if views.len() > 1 {
            views.pop_front()
        } else {
            views.front().cloned()
        }
        .expect("the store has a view"))
    }

    async fn inspect(
        &self,
        machine: MachineId,
        container: ContainerId,
    ) -> Option<ContainerObservation> {
        if let Some(left) = self.refusals.lock().unwrap().get_mut(&container)
            && *left > 0
        {
            *left -= 1;
            return None;
        }
        self.answers
            .get(&container)
            .filter(|copy| copy.machine_id == machine)
            .cloned()
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

mod alone {
    use super::*;
    use crate::boot_pass::alone;

    fn list(
        machines: &[&Machine],
        incomplete_ids: Vec<MachineId>,
    ) -> ReplicatedObservations<Machine, MachineId> {
        ReplicatedObservations {
            observations: machines.iter().map(|machine| (*machine).clone()).collect(),
            incomplete_ids,
        }
    }

    #[test]
    fn only_this_machine() {
        let cluster = Cluster::new();
        let local = &cluster.local.id;

        assert!(alone(&list(&[&cluster.local], Vec::new()), local, &[]));
        assert!(alone(&list(&[], Vec::new()), local, &[]));
        assert!(!alone(
            &list(&[&cluster.local, &cluster.peer], Vec::new()),
            local,
            &[]
        ));
        assert!(!alone(
            &list(&[&cluster.local], vec![cluster.peer.id]),
            local,
            &[]
        ));
    }

    #[test]
    fn a_joiner_before_its_peers_rows_arrive() {
        let cluster = Cluster::new();

        assert!(!alone(
            &list(&[&cluster.local], Vec::new()),
            &cluster.local.id,
            std::slice::from_ref(&cluster.peer)
        ));
    }

    #[tokio::test(start_paused = true)]
    async fn skips_at_once() {
        let cluster = Cluster::new();
        let host = FakeHost {
            targets: watch::channel(Peers::Alone).1,
            ..FakeHost::new(&cluster.local, cluster.acked_everywhere())
        };
        let started = Instant::now();

        run(host.clone(), synced(), published(BTreeMap::new())).await;

        assert_eq!(started.elapsed(), Duration::ZERO);
        assert!(host.wants().is_empty());
        assert!(
            *host.reads.lock().unwrap() == 0,
            "the pass never read the store"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn skips_once_the_last_peer_leaves() {
        let cluster = Cluster::new();
        let (peers, targets) = watch::channel(Peers::Silent);
        let host = FakeHost {
            targets,
            ..FakeHost::new(&cluster.local, cluster.acked_everywhere())
        };
        let started = Instant::now();
        let running = tokio::spawn(run(host.clone(), synced(), published(BTreeMap::new())));

        tokio::time::sleep(Duration::from_secs(10)).await;
        peers.send_replace(Peers::Alone);
        running.await.unwrap();

        assert!(started.elapsed() < Duration::from_secs(12));
        assert!(host.wants().is_empty());
    }
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

fn respec(
    mut copy: ContainerObservation,
    edit: impl FnOnce(&mut ResolvedServiceSpec),
) -> ContainerObservation {
    copy.try_update(|parts| edit(&mut parts.resolved_spec))
        .unwrap();
    copy
}

fn replicas(copy: ContainerObservation, replicas: u32) -> ContainerObservation {
    respec(copy, |spec| {
        spec.mode = ServiceMode::Replicated {
            replicas: NonZeroU32::new(replicas).unwrap(),
        };
    })
}

fn memory(copy: ContainerObservation, bytes: i64) -> ContainerObservation {
    respec(copy, |spec| {
        spec.container.resources.memory_bytes = Some(bytes.try_into().unwrap());
    })
}

fn greeting(copy: ContainerObservation, value: &str) -> ContainerObservation {
    respec(copy, |spec| {
        spec.container
            .environment
            .insert("GREETING".into(), value.into());
    })
}

/// A store row keeps no environment.
fn row(copy: &ContainerObservation) -> ContainerObservation {
    respec(copy.clone(), |spec| spec.container.environment.clear())
}

/// `local` judging `copies`, each routed by every proxy. The store holds their
/// rows, and `answer` turns each full copy into what its owner answers.
fn host_for(
    cluster: &Cluster,
    local: &Machine,
    copies: &[&ContainerObservation],
    answer: impl Fn(ContainerObservation) -> Option<ContainerObservation>,
) -> (FakeHost, BTreeMap<QualifiedService, BTreeSet<ContainerId>>) {
    let observed = observed(
        local,
        &[&cluster.local, &cluster.peer, &cluster.proxy],
        &[],
        copies.iter().map(|copy| row(copy)).collect(),
        routes(&[
            (&cluster.local, copies),
            (&cluster.peer, copies),
            (&cluster.proxy, copies),
        ]),
    );
    let own = own_acks(local, &observed);
    let answers = copies
        .iter()
        .filter_map(|copy| Some((copy.container_id, answer((*copy).clone())?)))
        .collect();
    let host = FakeHost {
        answers,
        ..FakeHost::new(local, observed)
    };
    (host, own)
}

/// The Outs a pass on `host` sends.
async fn outs(
    (host, own): (FakeHost, BTreeMap<QualifiedService, BTreeSet<ContainerId>>),
) -> Vec<ContainerWant> {
    run(host.clone(), synced(), published(own)).await;
    host.wants()
}

/// The local copy beside its siblings, as [`host_for`] has them. True when the
/// pass sets the local copy Out.
async fn set_out_beside(
    cluster: &Cluster,
    older: ContainerObservation,
    newer: Vec<ContainerObservation>,
    answer: impl Fn(ContainerObservation) -> Option<ContainerObservation>,
) -> bool {
    let all = std::iter::once(&older).chain(&newer).collect::<Vec<_>>();
    match outs(host_for(cluster, &cluster.local, &all, answer))
        .await
        .as_slice()
    {
        [] => false,
        [want] if *want == out(&older) => true,
        other => panic!("unexpected wants {other:?}"),
    }
}

/// The local gen-1 copy beside a peer's gen-2 copy, each as `older` and `newer`
/// make it, and every owner answers.
async fn replaced_by(
    older: impl FnOnce(ContainerObservation) -> ContainerObservation,
    newer: impl FnOnce(ContainerObservation) -> ContainerObservation,
) -> bool {
    let cluster = Cluster::new();
    set_out_beside(
        &cluster,
        older(copy(1, &cluster.local, 1)),
        vec![newer(copy(2, &cluster.peer, 2))],
        Some,
    )
    .await
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
    assert!(
        !replaced_by(
            |copy| memory(copy, 1 << 29),
            |copy| replicas(memory(copy, 1 << 30), 2)
        )
        .await
    );
}

/// Deploy N scales from 1 to 2, keeping the local copy at gen 1 and starting the
/// peer's at gen 2. Deploy M raises the limit in place while this Machine is off,
/// so the peer's copy keeps gen 2 under the new spec, and the local update fails.
#[tokio::test(start_paused = true)]
async fn kept_when_an_in_place_deploy_missed_it() {
    let cluster = Cluster::new();
    let kept_by_n = memory(copy(1, &cluster.local, 1), 1 << 29);
    let updated_by_m = replicas(memory(copy(2, &cluster.peer, 2), 3 << 29), 2);

    assert!(!set_out_beside(&cluster, kept_by_n, vec![updated_by_m], Some).await);
}

#[tokio::test(start_paused = true)]
async fn replaced_by_other_environment_values() {
    assert!(replaced_by(|copy| greeting(copy, "old"), |copy| greeting(copy, "new")).await);
}

#[tokio::test(start_paused = true)]
async fn kept_beside_equal_environment_values() {
    assert!(
        !replaced_by(
            |copy| greeting(copy, "same"),
            |copy| replicas(greeting(copy, "same"), 2)
        )
        .await
    );
}

/// A scale from 1 to 2 keeps the gen-1 copy beside the new gen-2 one.
#[tokio::test(start_paused = true)]
async fn kept_by_a_scale_up() {
    assert!(!replaced_by(|copy| copy, |copy| replicas(copy, 2)).await);
}

/// A Deploy while this Machine was Down started the one replica elsewhere.
#[tokio::test(start_paused = true)]
async fn replaced_by_an_identical_failover_copy() {
    assert!(replaced_by(|copy| copy, |copy| copy).await);
}

/// With 2 replicas, a Deploy while this Machine was Down kept the peer's gen-1 copy
/// and started a gen-2 one. The gen-1 copy with the larger id ranks past 2.
#[tokio::test(start_paused = true)]
async fn failover_ranks_out_the_gen_1_copy_with_the_larger_id() {
    let cluster = Cluster::new();
    let kept = copy(5, &cluster.peer, 1);
    let started = replicas(copy(7, &cluster.proxy, 2), 2);

    assert!(
        set_out_beside(
            &cluster,
            copy(6, &cluster.local, 1),
            vec![kept.clone(), started.clone()],
            Some
        )
        .await
    );
    assert!(
        !set_out_beside(
            &cluster,
            copy(4, &cluster.local, 1),
            vec![kept, started],
            Some
        )
        .await
    );
}

/// Two Machines back at once, each judging its own gen-1 copy from the same view,
/// set exactly one of them Out.
#[tokio::test(start_paused = true)]
async fn two_machines_judging_at_once_drop_one_copy() {
    let cluster = Cluster::new();
    let here = copy(4, &cluster.local, 1);
    let there = copy(5, &cluster.peer, 1);
    let started = replicas(copy(7, &cluster.proxy, 2), 2);
    let all = [&here, &there, &started];

    let (here_outs, there_outs) = tokio::join!(
        outs(host_for(&cluster, &cluster.local, &all, Some)),
        outs(host_for(&cluster, &cluster.peer, &all, Some)),
    );

    assert_eq!((here_outs, there_outs), (Vec::new(), vec![out(&there)]));
}

#[tokio::test(start_paused = true)]
async fn kept_when_an_unreadable_sibling_ranks_above_it() {
    let cluster = Cluster::new();
    let unreadable = copy(5, &cluster.peer, 1);
    let started = replicas(copy(7, &cluster.proxy, 2), 2);
    let peer = cluster.peer.id;

    assert!(
        !set_out_beside(
            &cluster,
            copy(6, &cluster.local, 1),
            vec![unreadable, started],
            |copy| (copy.machine_id != peer).then_some(copy)
        )
        .await
    );
}

/// One Machine cannot confirm the other's copy, so their views differ. However
/// either decides, the Service keeps its 2 replicas.
#[tokio::test(start_paused = true)]
async fn two_machines_with_different_answers_keep_the_replica_count() {
    let cluster = Cluster::new();
    let here = copy(4, &cluster.local, 1);
    let there = copy(5, &cluster.peer, 1);
    let started = replicas(copy(7, &cluster.proxy, 2), 2);
    let all = [&here, &there, &started];
    let blind_to = |hidden: MachineId| {
        move |copy: ContainerObservation| (copy.machine_id != hidden).then_some(copy)
    };

    for (here_sees, there_sees) in [
        (blind_to(cluster.peer.id), blind_to(cluster.proxy.id)),
        (blind_to(cluster.proxy.id), blind_to(cluster.local.id)),
    ] {
        let (here_outs, there_outs) = tokio::join!(
            outs(host_for(&cluster, &cluster.local, &all, here_sees)),
            outs(host_for(&cluster, &cluster.peer, &all, there_sees)),
        );

        assert!(all.len() - here_outs.len() - there_outs.len() >= 2);
    }
}

/// Copies one Deploy started differ in update order alone when one replaced a copy
/// holding a host port, which stops first, and the other is new, which starts first.
#[tokio::test(start_paused = true)]
async fn replaced_by_one_deploy_that_both_replaced_and_started_copies() {
    let cluster = Cluster::new();
    let order = |copy, order| respec(copy, |spec| spec.update.order = order);

    assert!(
        set_out_beside(
            &cluster,
            copy(1, &cluster.local, 1),
            vec![
                order(
                    replicas(newer_image(copy(3, &cluster.peer, 2)), 2),
                    UpdateOrder::StopFirst
                ),
                order(
                    replicas(newer_image(copy(4, &cluster.proxy, 2)), 2),
                    UpdateOrder::StartFirst
                ),
            ],
            Some,
        )
        .await
    );
}

#[tokio::test(start_paused = true)]
async fn kept_beside_an_identical_newer_copy_of_a_global_service() {
    let cluster = Cluster::new();
    let global = |copy| respec(copy, |spec| spec.mode = ServiceMode::Global);

    assert!(
        !set_out_beside(
            &cluster,
            global(copy(1, &cluster.local, 1)),
            vec![global(copy(2, &cluster.peer, 2))],
            Some,
        )
        .await
    );
}

/// The pass ranks the local copy third of 2 when it starts fetching specs. Before it
/// sends the Out, a Deploy has `then` happen. This Machine's ingress has routed
/// every copy of both views.
async fn set_out_after(then: impl FnOnce(&Cluster, &mut Vec<ContainerObservation>)) -> bool {
    let cluster = Cluster::new();
    let local = copy(6, &cluster.local, 1);
    let mut copies = vec![
        local.clone(),
        copy(5, &cluster.peer, 1),
        replicas(copy(7, &cluster.proxy, 2), 2),
    ];
    let (host, mut own) = host_for(
        &cluster,
        &cluster.local,
        &copies.iter().collect::<Vec<_>>(),
        Some,
    );
    then(&cluster, &mut copies);
    let (later, later_own) = host_for(
        &cluster,
        &cluster.local,
        &copies.iter().collect::<Vec<_>>(),
        Some,
    );
    for (service, routed) in later_own {
        own.entry(service).or_default().extend(routed);
    }
    let later = later.observed.lock().unwrap().pop_front().unwrap();
    host.observed.lock().unwrap().push_back(later);
    match outs((host, own)).await.as_slice() {
        [] => false,
        [want] if *want == out(&local) => true,
        other => panic!("unexpected wants {other:?}"),
    }
}

#[tokio::test(start_paused = true)]
async fn set_out_when_the_view_holds_until_the_out() {
    assert!(set_out_after(|_, _| {}).await);
}

#[tokio::test(start_paused = true)]
async fn kept_when_a_deploy_retires_the_newer_copy_during_the_fetch() {
    assert!(
        !set_out_after(|_, copies| {
            copies.pop();
        })
        .await
    );
}

#[tokio::test(start_paused = true)]
async fn kept_when_a_newer_gen_appears_during_the_fetch() {
    assert!(
        !set_out_after(|cluster, copies| {
            copies.push(replicas(copy(8, &cluster.proxy, 3), 2));
        })
        .await
    );
}

#[tokio::test(start_paused = true)]
async fn replaced_once_the_owner_answers_on_a_later_try() {
    let cluster = Cluster::new();
    let older = greeting(copy(1, &cluster.local, 1), "old");
    let newer = greeting(copy(2, &cluster.peer, 2), "new");
    let (host, own) = host_for(&cluster, &cluster.local, &[&older, &newer], Some);
    host.refusals.lock().unwrap().insert(newer.container_id, 3);

    assert_eq!(outs((host, own)).await, vec![out(&older)]);
}

#[tokio::test(start_paused = true)]
async fn kept_when_the_owner_reports_another_gen() {
    let cluster = Cluster::new();
    let older = greeting(copy(1, &cluster.local, 1), "old");
    let newer = greeting(copy(2, &cluster.peer, 2), "new");
    let peer = cluster.peer.id;

    assert!(
        !set_out_beside(&cluster, older, vec![newer], |mut copy| {
            if copy.machine_id == peer {
                copy.try_update(|parts| parts.r#gen = Some(Gen::new(3)))
                    .unwrap();
            }
            Some(copy)
        })
        .await
    );
}

#[tokio::test(start_paused = true)]
async fn kept_when_the_newer_copys_owner_does_not_answer() {
    let cluster = Cluster::new();
    let older = greeting(copy(1, &cluster.local, 1), "old");
    let newer = greeting(copy(2, &cluster.peer, 2), "new");
    let local = cluster.local.id;

    assert!(
        !set_out_beside(&cluster, older, vec![newer], |copy| (copy.machine_id
            == local)
            .then_some(copy))
        .await
    );
}

#[test]
fn counts_a_copy_whose_newest_sibling_went_unconfirmed_as_unjudged() {
    let cluster = Cluster::new();
    let observed = cluster.acked_everywhere();
    let own = own_acks(&cluster.local, &observed);
    let candidates = super::candidates(&cluster.local.id, &observed, &own);
    let specs = BTreeMap::from([(cluster.old.container_id, cluster.old.resolved_spec.clone())]);

    assert_eq!(super::superseded(&candidates, &specs), (Vec::new(), 1));
}

#[tokio::test(start_paused = true)]
async fn kept_when_this_machine_cannot_read_its_copy() {
    let cluster = Cluster::new();
    let older = greeting(copy(1, &cluster.local, 1), "old");
    let newer = greeting(copy(2, &cluster.peer, 2), "new");
    let peer = cluster.peer.id;

    assert!(
        !set_out_beside(&cluster, older, vec![newer], |copy| (copy.machine_id
            == peer)
            .then_some(copy))
        .await
    );
}

#[tokio::test(start_paused = true)]
async fn kept_when_the_owner_reports_the_newer_copy_out() {
    let cluster = Cluster::new();
    let older = greeting(copy(1, &cluster.local, 1), "old");
    let newer = greeting(copy(2, &cluster.peer, 2), "new");
    let peer = cluster.peer.id;

    assert!(
        !set_out_beside(&cluster, older, vec![newer], |mut copy| {
            if copy.machine_id == peer {
                copy.try_update(|parts| parts.rotation = Some(CopyRotation::Out))
                    .unwrap();
            }
            Some(copy)
        })
        .await
    );
}

#[tokio::test(start_paused = true)]
async fn kept_beside_an_older_differing_gen_when_the_newest_matches() {
    let cluster = Cluster::new();

    assert!(
        !set_out_beside(
            &cluster,
            copy(1, &cluster.local, 1),
            vec![
                newer_image(copy(3, &cluster.peer, 2)),
                replicas(copy(4, &cluster.peer, 3), 2)
            ],
            Some,
        )
        .await
    );
}

#[tokio::test(start_paused = true)]
async fn replaced_by_the_newest_gen_when_an_older_one_matches() {
    let cluster = Cluster::new();

    assert!(
        set_out_beside(
            &cluster,
            copy(1, &cluster.local, 1),
            vec![
                copy(3, &cluster.peer, 2),
                newer_image(copy(4, &cluster.peer, 3))
            ],
            Some,
        )
        .await
    );
}

#[tokio::test(start_paused = true)]
async fn replaced_by_identical_always_pull_siblings() {
    let cluster = Cluster::new();
    let always = |copy| {
        newer_image(respec(copy, |spec| {
            spec.container.pull_policy = PullPolicy::Always;
        }))
    };

    assert!(
        set_out_beside(
            &cluster,
            copy(1, &cluster.local, 1),
            vec![
                always(copy(3, &cluster.peer, 2)),
                always(copy(4, &cluster.peer, 2))
            ],
            Some,
        )
        .await
    );
}

#[tokio::test(start_paused = true)]
async fn kept_when_the_newest_siblings_disagree() {
    let cluster = Cluster::new();

    assert!(
        !set_out_beside(
            &cluster,
            copy(1, &cluster.local, 1),
            vec![
                newer_image(copy(3, &cluster.peer, 2)),
                respec(copy(4, &cluster.peer, 2), |spec| {
                    spec.container.pull_policy = PullPolicy::Always;
                })
            ],
            Some,
        )
        .await
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
    let (peer, targets) = watch::channel(Peers::Silent);
    let host = FakeHost {
        targets,
        store: Some(store),
        ..FakeHost::new(&cluster.local, observed)
    };
    let running = tokio::spawn(run(host.clone(), synced(), published(own)));

    tokio::time::sleep(Duration::from_millis(1500)).await;
    assert!(host.wants().is_empty(), "no peer has answered yet");
    peer.send_replace(Peers::Answered(BTreeMap::from([(ACTOR.to_owned(), 5)])));
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
        targets: watch::channel(Peers::Answered(BTreeMap::from([(ACTOR.to_owned(), 5)]))).1,
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
        targets: watch::channel(Peers::Answered(BTreeMap::from([
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
        ..PublishedRoutes::default()
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
        targets: watch::channel(Peers::Silent).1,
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
