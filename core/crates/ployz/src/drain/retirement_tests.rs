//! Tests for retiring a Drain's chosen Globals on the drained Server and letting go of
//! the copies a move left Out there.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::Duration;

use ployz_core::service::proxy_routes;
use ployz_core::{
    ContainerAddress, ContainerDetails, ContainerId, ContainerLocator, ContainerNow,
    ContainerObservation, ContainerRefusal, ContainerRefusalReason, ContainerWant, CopyRotation,
    EnsuredContainer, HealthVerdict, LiveServices, Machine, MachineId, MachineName,
    ObservedGlobalSlotSpec, Placement, ProxyRoutes, QualifiedService, ResolvedUpdateConfig,
    RoutesRead, RpcError, RpcErrorCode, ServiceContainer, ServiceMode, ServiceObservation,
};

use super::*;
use crate::deploy::Session;
use crate::deploy::cleanup::ACK_WAIT;
use crate::global_fixtures::{
    global_service, grouped, machine, qualified, requested, running_on, service_id,
};
use crate::global_slot::{Sentence, retire_copies, retire_ingress_proxy};

struct Cluster {
    containers: Mutex<Vec<ContainerObservation>>,
    proxies: Vec<MachineId>,
    down: BTreeSet<MachineId>,
    frozen: BTreeSet<MachineId>,
    routes: Mutex<BTreeMap<MachineId, ProxyRoutes>>,
    wants: Mutex<Vec<(MachineId, ContainerWant)>>,
    cancel_on_read: Option<CancellationToken>,
    /// Copies whose removal Docker refuses.
    stuck: BTreeSet<ContainerId>,
}

impl Cluster {
    fn of(proxies: &[&Machine], containers: Vec<ContainerObservation>) -> Self {
        let cluster = Self {
            containers: Mutex::new(containers),
            proxies: proxies.iter().map(|machine| machine.id).collect(),
            down: BTreeSet::new(),
            frozen: BTreeSet::new(),
            routes: Mutex::new(BTreeMap::new()),
            wants: Mutex::new(Vec::new()),
            cancel_on_read: None,
            stuck: BTreeSet::new(),
        };
        for proxy in &cluster.proxies {
            cluster.route(proxy);
        }
        cluster
    }

    fn frozen(mut self, proxy: &Machine) -> Self {
        self.frozen.insert(proxy.id);
        self
    }

    fn route(&self, proxy: &MachineId) -> ProxyRoutes {
        let mut routes = self.routes.lock().unwrap();
        if self.frozen.contains(proxy) {
            return routes.get(proxy).cloned().unwrap_or_default();
        }
        let containers: Vec<ServiceContainer> = self
            .containers
            .lock()
            .unwrap()
            .iter()
            .cloned()
            .map(|observation| ServiceContainer::try_from(observation).unwrap())
            .collect();
        let current = routes.entry(*proxy).or_default();
        *current = proxy_routes(&containers, |machine| self.down.contains(machine), current);
        current.clone()
    }

    fn taken_out(&self, hex: char) -> Copy {
        let mut containers = self.containers.lock().unwrap();
        let copy = containers
            .iter_mut()
            .find(|observation| observation.container_id == container(hex))
            .expect("the copy is on a Machine");
        copy.try_update(|parts| parts.rotation = Some(CopyRotation::Out))
            .unwrap();
        Copy::observed(copy)
    }

    fn wants(&self) -> Vec<ContainerWant> {
        self.wants
            .lock()
            .unwrap()
            .iter()
            .map(|(_, want)| want.clone())
            .collect()
    }

    fn rotations(&self) -> Vec<(ContainerId, CopyRotation)> {
        self.containers
            .lock()
            .unwrap()
            .iter()
            .map(|observation| (observation.container_id, observation.rotation().clone()))
            .collect()
    }
}

fn id_of(locator: &ContainerLocator) -> ContainerId {
    match locator {
        ContainerLocator::Id(id) => *id,
        other => panic!("drain locates copies by id, not {other:?}"),
    }
}

impl Session for Cluster {}

impl CopyLifecycle for Cluster {
    async fn service_containers(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError> {
        Ok(self
            .containers
            .lock()
            .unwrap()
            .iter()
            .filter(|observation| observation.identity() == *service)
            .cloned()
            .collect())
    }

    async fn replicated_containers(&self) -> Result<Vec<ContainerObservation>, RpcError> {
        Ok(self.containers.lock().unwrap().clone())
    }

    async fn ensure_container(
        &self,
        _machine_id: &MachineId,
        want: ContainerWant,
    ) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError> {
        unreachable!("retirement settles nothing: {want:?}")
    }

    async fn ensure_unsettled(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        _timeout: Option<Duration>,
    ) -> Result<EnsuredContainer, RpcError> {
        self.wants.lock().unwrap().push((*machine_id, want.clone()));
        let mut containers = self.containers.lock().unwrap();
        match want {
            ContainerWant::Out {
                container,
                allow_empty,
                expect,
            } => {
                let id = id_of(&container);
                let service = containers
                    .iter()
                    .find(|observation| observation.container_id == id)
                    .expect("an Out names a copy the Machine runs")
                    .identity();
                let another_in = containers.iter().any(|observation| {
                    observation.container_id != id
                        && observation.identity() == service
                        && *observation.rotation() == CopyRotation::In
                });
                let copy = containers
                    .iter_mut()
                    .find(|observation| observation.container_id == id)
                    .expect("an Out names a copy the Machine runs");
                assert_eq!(expect.as_ref(), Some(copy.rotation()));
                if !allow_empty && !another_in {
                    return Ok(EnsuredContainer::Refused {
                        now: ContainerNow::Present(copy.clone()),
                        refusal: ContainerRefusal {
                            reason: ContainerRefusalReason::LastIn,
                            message: "it is the last copy in rotation".into(),
                            capability: None,
                        },
                    });
                }
                copy.try_update(|parts| parts.rotation = Some(CopyRotation::Out))
                    .unwrap();
                Ok(EnsuredContainer::Ensured {
                    now: ContainerNow::Present(copy.clone()),
                    health: None,
                })
            }
            ContainerWant::Gone { container, .. } => {
                let id = id_of(&container);
                if self.stuck.contains(&id) {
                    return Err(RpcError::internal("Docker refused the removal"));
                }
                containers.retain(|observation| observation.container_id != id);
                Ok(EnsuredContainer::Ensured {
                    now: ContainerNow::Absent { of: container },
                    health: None,
                })
            }
            other @ (ContainerWant::Running { .. }
            | ContainerWant::Stopped { .. }
            | ContainerWant::Held { .. }) => {
                unreachable!("retirement sends Out and Gone only, not {other:?}")
            }
        }
    }

    async fn inspect_container(
        &self,
        _machine_id: &MachineId,
        _container_id: &ContainerId,
    ) -> Result<ContainerDetails, RpcError> {
        unreachable!("retirement inspects nothing")
    }

    async fn read_routes(
        &self,
        _machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<RoutesRead, RpcError> {
        if let Some(cancellation) = &self.cancel_on_read {
            cancellation.cancel();
        }
        let routes = self
            .proxies
            .iter()
            .map(|proxy| {
                let routed = self
                    .route(proxy)
                    .get(service)
                    .map(|route| {
                        route
                            .iter()
                            .map(|serving| serving.as_observation().container_id)
                            .collect()
                    })
                    .unwrap_or_default();
                (*proxy, Some(routed))
            })
            .collect();
        Ok(RoutesRead {
            routes,
            complete: true,
            down: self.down.clone(),
        })
    }

    async fn read_entry_routes(&self, _service: &QualifiedService) -> Result<RoutesRead, RpcError> {
        unreachable!("retirement reads routes through the copy's own Machine")
    }
}

fn copy_on(machine: &Machine, hex: char, rotation: CopyRotation) -> ContainerObservation {
    let mut observation = running_on(machine, hex);
    observation
        .try_update(|parts| {
            parts.rotation = Some(rotation);
            parts.address = Some(ContainerAddress(
                format!("10.210.{}.{}", hex.to_digit(16).unwrap(), 2)
                    .parse()
                    .unwrap(),
            ));
        })
        .unwrap();
    observation
}

fn ingress_on(machine: &Machine, hex: char) -> ContainerObservation {
    door_on(machine, hex, QualifiedService::system_ingress())
}

fn holder_on(machine: &Machine, hex: char) -> ContainerObservation {
    door_on(machine, hex, QualifiedService::system_ingress_holder())
}

fn door_on(machine: &Machine, hex: char, door: QualifiedService) -> ContainerObservation {
    let mut observation = running_on(machine, hex);
    observation
        .try_update(|parts| {
            parts.namespace = door.namespace;
            parts.resolved_spec.name = door.name;
        })
        .unwrap();
    observation
}

fn container(hex: char) -> ContainerId {
    crate::global_fixtures::container_id(hex)
}

fn gone(hex: char) -> ContainerWant {
    ContainerWant::Gone {
        container: ContainerLocator::Id(container(hex)),
        drain: ployz_core::Drain::DEFAULT,
        anonymous_volumes: AnonymousVolumes::Remove,
    }
}

fn out(hex: char, expect: CopyRotation) -> ContainerWant {
    ContainerWant::Out {
        container: ContainerLocator::Id(container(hex)),
        allow_empty: false,
        expect: Some(expect),
    }
}

fn names(machines: Vec<Machine>) -> impl Fn(&MachineId) -> MachineRef {
    move |id| {
        machines
            .iter()
            .find(|machine| machine.id == *id)
            .map(MachineRef::from)
            .expect("every proxy is a named Machine")
    }
}

#[tokio::test]
async fn retire_copies_takes_an_in_copy_out_waits_it_unlisted_then_removes_it() {
    let (drained, other, third) = (
        machine('1', "drained"),
        machine('2', "other"),
        machine('3', "third"),
    );
    let cluster = Cluster::of(
        &[&drained, &other, &third],
        vec![
            copy_on(&drained, 'a', CopyRotation::In),
            copy_on(&other, 'b', CopyRotation::In),
        ],
    );
    retire_copies(
        &cluster,
        &drained.id,
        &qualified("app", "api"),
        &CancellationToken::new(),
        &Sentence::Drain(&ServerNames::default()),
    )
    .await
    .expect("another In copy lets the drained one go Out");
    assert_eq!(cluster.wants(), [out('a', CopyRotation::In), gone('a')]);
    assert_eq!(cluster.rotations(), [(container('b'), CopyRotation::In)]);
}

#[tokio::test]
async fn retire_copies_refuses_the_last_in_copy_and_leaves_it_serving() {
    let (drained, other) = (machine('1', "drained"), machine('2', "other"));
    let cluster = Cluster::of(
        &[&drained, &other],
        vec![copy_on(&drained, 'a', CopyRotation::In)],
    );
    let error = retire_copies(
        &cluster,
        &drained.id,
        &qualified("app", "api"),
        &CancellationToken::new(),
        &Sentence::Drain(&ServerNames::default()),
    )
    .await
    .expect_err("the last copy In refuses to go Out");
    assert_eq!(error.code, RpcErrorCode::Conflict);
    assert_eq!(error.message, "it is the last copy in rotation");
    assert_eq!(cluster.wants(), [out('a', CopyRotation::In)]);
    assert_eq!(cluster.rotations(), [(container('a'), CopyRotation::In)]);
}

#[tokio::test]
async fn retire_copies_removes_pending_and_out_copies_with_a_plain_gone() {
    let (drained, other) = (machine('1', "drained"), machine('2', "other"));
    let cluster = Cluster::of(
        &[&drained, &other],
        vec![
            copy_on(&drained, 'a', CopyRotation::Pending),
            copy_on(&drained, 'b', CopyRotation::Out),
            copy_on(&other, 'c', CopyRotation::In),
        ],
    );
    retire_copies(
        &cluster,
        &drained.id,
        &qualified("app", "api"),
        &CancellationToken::new(),
        &Sentence::Drain(&ServerNames::default()),
    )
    .await
    .expect("no copy was routed");
    assert_eq!(cluster.wants(), [gone('a'), gone('b')]);
    assert_eq!(cluster.rotations(), [(container('c'), CopyRotation::In)]);
}

#[tokio::test(start_paused = true)]
async fn retire_copies_names_the_proxy_that_did_not_confirm_and_keeps_the_copy() {
    let (drained, other, stuck) = (
        machine('1', "drained"),
        machine('2', "other"),
        machine('3', "stuck"),
    );
    let cluster = Cluster::of(
        &[&drained, &other, &stuck],
        vec![
            copy_on(&drained, 'a', CopyRotation::In),
            copy_on(&other, 'b', CopyRotation::In),
        ],
    )
    .frozen(&stuck);
    let error = retire_copies(
        &cluster,
        &drained.id,
        &qualified("app", "api"),
        &CancellationToken::new(),
        &Sentence::Drain(&ServerNames::listing([(stuck.id, stuck.name.clone())])),
    )
    .await
    .expect_err("a proxy still listing the copy keeps it");
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(
        error.message,
        format!(
            "not unlisted after {} s; the proxies on {} did not confirm",
            ACK_WAIT.as_secs(),
            stuck.name
        )
    );
    assert_eq!(cluster.wants(), [out('a', CopyRotation::In)]);
    assert_eq!(
        cluster.rotations(),
        [
            (container('a'), CopyRotation::Out),
            (container('b'), CopyRotation::In),
        ]
    );
}

/// A retirement the owner refuses gives the owner's reason as the error; one the drain's
/// cancel ends says so, and neither removes the copy.
#[tokio::test(start_paused = true)]
async fn retire_copies_gives_the_refusal_or_the_cancel_as_its_error() {
    let (drained, other, stuck) = (
        machine('1', "drained"),
        machine('2', "other"),
        machine('3', "stuck"),
    );
    let refused = Cluster::of(
        &[&drained, &other],
        vec![copy_on(&drained, 'a', CopyRotation::In)],
    );
    let error = retire_copies(
        &refused,
        &drained.id,
        &qualified("app", "api"),
        &CancellationToken::new(),
        &Sentence::Drain(&ServerNames::default()),
    )
    .await
    .expect_err("the last copy in rotation stays");
    assert_eq!(error.message, "it is the last copy in rotation");
    assert_eq!(
        error,
        ContainerRefusal {
            reason: ContainerRefusalReason::LastIn,
            message: "it is the last copy in rotation".into(),
            capability: None,
        }
        .into_error()
    );
    assert_eq!(refused.wants(), [out('a', CopyRotation::In)]);

    let cancellation = CancellationToken::new();
    let mut cancelled = Cluster::of(
        &[&drained, &other, &stuck],
        vec![
            copy_on(&drained, 'a', CopyRotation::In),
            copy_on(&other, 'b', CopyRotation::In),
        ],
    )
    .frozen(&stuck);
    cancelled.cancel_on_read = Some(cancellation.clone());
    let error = retire_copies(
        &cancelled,
        &drained.id,
        &qualified("app", "api"),
        &cancellation,
        &Sentence::Drain(&ServerNames::default()),
    )
    .await
    .expect_err("a cancelled drain retires nothing more");
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(error.message, "the drain was cancelled while it retired");
    assert_eq!(cancelled.wants(), [out('a', CopyRotation::In)]);
}

#[tokio::test]
async fn the_ingress_proxy_goes_caddy_first_then_its_holder() {
    let (drained, other) = (machine('1', "drained"), machine('2', "other"));
    let cluster = Cluster::of(
        &[&drained, &other],
        vec![
            holder_on(&drained, 'c'),
            ingress_on(&drained, 'a'),
            ingress_on(&other, 'b'),
        ],
    );
    retire_ingress_proxy(&cluster, &drained.id, &CancellationToken::new())
        .await
        .expect("a Pending-for-life copy needs no rotation step");
    assert_eq!(
        cluster.wants(),
        [gone('a'), gone('c')],
        "Docker removes no holder while a Caddy joined to it exists"
    );
    assert_eq!(
        cluster.rotations(),
        [(container('b'), CopyRotation::Pending)]
    );
}

#[tokio::test]
async fn a_caddy_that_stays_keeps_its_holder() {
    let drained = machine('1', "drained");
    let mut cluster = Cluster::of(
        &[&drained],
        vec![holder_on(&drained, 'c'), ingress_on(&drained, 'a')],
    );
    cluster.stuck.insert(container('a'));
    let error = retire_ingress_proxy(&cluster, &drained.id, &CancellationToken::new())
        .await
        .unwrap_err();
    assert!(error.message.contains("refused"), "{error:?}");
    assert_eq!(cluster.wants(), [gone('a')], "the holder is never asked");
    assert_eq!(cluster.rotations().len(), 2);
}

#[tokio::test]
async fn let_go_waits_an_out_copy_unlisted_then_removes_it() {
    let (drained, other, third) = (
        machine('1', "drained"),
        machine('2', "other"),
        machine('3', "third"),
    );
    let cluster = Cluster::of(
        &[&drained, &other, &third],
        vec![
            copy_on(&drained, 'a', CopyRotation::Out),
            copy_on(&other, 'b', CopyRotation::In),
        ],
    );
    let copy = Copy::observed(&copy_on(&drained, 'a', CopyRotation::Out));
    let end = let_go(
        &cluster,
        &copy,
        &names(vec![drained.clone(), other.clone(), third.clone()]),
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(end, OldCopyEnd::Stopped);
    assert_eq!(cluster.wants(), [gone('a')], "an Out copy is never moved");
    assert_eq!(cluster.rotations(), [(container('b'), CopyRotation::In)]);
}

#[tokio::test(start_paused = true)]
async fn let_go_leaves_the_copy_running_when_a_proxy_does_not_confirm() {
    let (drained, other, stuck) = (
        machine('1', "drained"),
        machine('2', "other"),
        machine('3', "stuck"),
    );
    let cluster = Cluster::of(
        &[&drained, &other, &stuck],
        vec![
            copy_on(&drained, 'a', CopyRotation::In),
            copy_on(&other, 'b', CopyRotation::In),
        ],
    )
    .frozen(&stuck);
    let copy = cluster.taken_out('a');
    let end = let_go(
        &cluster,
        &copy,
        &names(vec![drained.clone(), other.clone(), stuck.clone()]),
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        end,
        OldCopyEnd::Running {
            unconfirmed: Unconfirmed::Proxies {
                servers: vec![MachineRef::from(&stuck)],
            },
        }
    );
    assert!(
        cluster.wants().is_empty(),
        "nothing is sent while a proxy lists it"
    );
    assert_eq!(
        cluster.rotations(),
        [
            (container('a'), CopyRotation::Out),
            (container('b'), CopyRotation::In),
        ]
    );
}

#[tokio::test]
async fn let_go_leaves_the_copy_running_when_the_drain_is_cancelled() {
    let (drained, other, stuck) = (
        machine('1', "drained"),
        machine('2', "other"),
        machine('3', "stuck"),
    );
    let cancellation = CancellationToken::new();
    let mut cluster = Cluster::of(
        &[&drained, &other, &stuck],
        vec![
            copy_on(&drained, 'a', CopyRotation::In),
            copy_on(&other, 'b', CopyRotation::In),
        ],
    )
    .frozen(&stuck);
    cluster.cancel_on_read = Some(cancellation.clone());
    let copy = cluster.taken_out('a');
    let end = let_go(
        &cluster,
        &copy,
        &names(vec![drained.clone(), other.clone(), stuck.clone()]),
        &cancellation,
    )
    .await;
    assert_eq!(
        end,
        OldCopyEnd::Running {
            unconfirmed: Unconfirmed::Cancelled,
        }
    );
    assert!(cluster.wants().is_empty());
}

struct FakeRetireClient {
    machine_id: MachineId,
    services: Vec<ServiceObservation>,
    retire_calls: Vec<QualifiedService>,
    retire_error: Option<RpcError>,
    cancel_on_retire: Option<CancellationToken>,
    silent: Option<(MachineId, MachineName)>,
}

impl RetireClient for FakeRetireClient {
    async fn live_services(&mut self) -> Result<Live, ConnectError> {
        let services = LiveServices {
            containers: ployz_core::PartialResult {
                successes: vec![ployz_core::MachineSuccess {
                    machine_id: self.machine_id,
                    value: self
                        .services
                        .iter()
                        .flat_map(ServiceObservation::members)
                        .map(|container| container.as_observation().clone())
                        .collect(),
                }],
                failures: Vec::new(),
                omissions: self.silent.iter().map(|(id, _)| *id).collect(),
            },
        };
        Ok(Live {
            services,
            names: ServerNames::listing(self.silent.iter().cloned()),
        })
    }

    async fn retire_slot(
        &mut self,
        _machine_id: &MachineId,
        slot: &ObservedGlobalSlotSpec,
        _names: &ServerNames,
        _cancellation: &CancellationToken,
    ) -> Result<(), RpcError> {
        self.retire_calls.push(slot.identity().clone());
        if let Some(cancellation) = &self.cancel_on_retire {
            cancellation.cancel();
        }
        self.retire_error.clone().map_or(Ok(()), Err)
    }
}

fn retiring(drained: &Machine, services: Vec<ServiceObservation>) -> FakeRetireClient {
    FakeRetireClient {
        machine_id: drained.id,
        services,
        retire_calls: Vec::new(),
        retire_error: None,
        cancel_on_retire: None,
        silent: None,
    }
}

fn global_on(drained: &Machine, namespace: &str, hex: char) -> ServiceObservation {
    global_service(
        qualified(namespace, "api"),
        hex,
        Placement::default(),
        running_on(drained, hex),
    )
}

#[tokio::test]
async fn retirement_touches_only_the_chosen_globals() {
    let drained = machine('1', "drained");
    let mut client = retiring(
        &drained,
        vec![
            global_on(&drained, "app", 'a'),
            global_on(&drained, "other", 'b'),
        ],
    );
    let chosen = qualified("app", "api");
    let never = CancellationToken::new();
    let retired =
        retire_globals(&mut client, &drained, std::slice::from_ref(&chosen), &never).await;
    assert_eq!(retired, [(chosen.clone(), Retirement::Retired)]);
    assert_eq!(client.retire_calls, [chosen]);

    client.retire_calls.clear();
    assert!(
        retire_globals(&mut client, &drained, &[], &never)
            .await
            .is_empty()
    );
    assert!(client.retire_calls.is_empty());
}

#[tokio::test]
async fn retirement_names_the_server_that_left_the_observation_partial() {
    let drained = machine('1', "drained");
    let beta = machine('2', "beta");
    let mut client = retiring(&drained, vec![global_on(&drained, "app", 'a')]);
    client.silent = Some((beta.id, beta.name.clone()));
    let chosen = qualified("app", "api");
    let outcomes = retire_globals(
        &mut client,
        &drained,
        std::slice::from_ref(&chosen),
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        outcomes,
        [(
            chosen,
            Retirement::NotRetired(
                "cannot retire from partial Service observations: beta: no terminal response"
                    .into()
            )
        )]
    );
    assert!(client.retire_calls.is_empty());
}

#[tokio::test]
async fn retirement_reports_every_global_and_stops_at_the_next_once_cancelled() {
    let drained = machine('1', "drained");
    let globals = [qualified("app", "api"), qualified("other", "api")];
    let services = vec![
        global_on(&drained, "app", 'a'),
        global_on(&drained, "other", 'b'),
    ];

    let cancellation = CancellationToken::new();
    let mut client = retiring(&drained, services.clone());
    client.cancel_on_retire = Some(cancellation.clone());
    assert_eq!(
        retire_globals(&mut client, &drained, &globals, &cancellation).await,
        [
            (globals[0].clone(), Retirement::Retired),
            (globals[1].clone(), Retirement::NotAttempted),
        ]
    );
    assert_eq!(client.retire_calls, [globals[0].clone()]);

    let mut client = retiring(&drained, services);
    client.retire_error = Some(RpcError::conflict("the Server accepts it again"));
    let gone = qualified("gone", "api");
    let outcomes = retire_globals(
        &mut client,
        &drained,
        &[globals[0].clone(), gone.clone()],
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        outcomes,
        [
            (
                globals[0].clone(),
                Retirement::NotRetired("the Server accepts it again".into())
            ),
            (gone, Retirement::Retired),
        ],
        "a Global a full observation no longer finds has nothing left to retire"
    );
}

#[tokio::test]
async fn a_global_whose_newest_container_is_not_global_is_reported_not_retired() {
    let drained = machine('1', "drained");
    let identity = qualified("app", "api");
    let mut spec = requested(ServiceMode::Replicated {
        replicas: std::num::NonZeroU32::MIN,
    });
    spec.name = identity.name.clone();
    let replicated = grouped(
        identity.clone(),
        spec.to_resolved(service_id('a'), ResolvedUpdateConfig::default())
            .expect("volume graph is scoped"),
        running_on(&drained, 'a'),
    );
    let mut client = retiring(&drained, vec![replicated]);
    let outcomes = retire_globals(
        &mut client,
        &drained,
        std::slice::from_ref(&identity),
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(
        outcomes,
        [(
            identity,
            Retirement::NotRetired("its newest Container is not Global; deploy it first".into())
        )]
    );
    assert!(client.retire_calls.is_empty());
}
