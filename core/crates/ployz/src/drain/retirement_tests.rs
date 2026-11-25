//! Tests for removing a Server's Ingress Proxy and letting go of the copies a Drain's move
//! left Out there.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Mutex;
use std::time::Duration;

use ployz_core::service::proxy_routes;
use ployz_core::{
    ContainerAddress, ContainerDetails, ContainerId, ContainerLocator, ContainerNow,
    ContainerObservation, ContainerRefusal, ContainerRefusalReason, ContainerWant, CopyRotation,
    EnsuredContainer, HealthVerdict, Machine, MachineId, ProxyRoutes, QualifiedService, RoutesRead,
    RpcError, ServiceContainer,
};

use super::*;
use crate::deploy::Session;
use crate::global_fixtures::{machine, running_on};
use crate::global_slot::retire_ingress_proxy;

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

    async fn ensure_new(
        &self,
        _machine_id: &MachineId,
        want: ContainerWant,
    ) -> Result<ployz_core::EnsuredContainer, RpcError> {
        panic!("a retirement creates nothing: {want:?}")
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
                    verified: None,
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
                    verified: None,
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
        anonymous_volumes: ployz_core::AnonymousVolumes::keep(),
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

/// Whether each Gone a Drain sent deletes the Container's anonymous Volumes.
fn anonymous_volumes_deleted(cluster: &Cluster) -> Vec<bool> {
    cluster
        .wants()
        .into_iter()
        .filter_map(|want| match want {
            ContainerWant::Gone {
                anonymous_volumes, ..
            } => Some(anonymous_volumes.replaced_by().is_some()),
            ContainerWant::Running { .. }
            | ContainerWant::Stopped { .. }
            | ContainerWant::Out { .. }
            | ContainerWant::Held { .. } => None,
        })
        .collect()
}

/// A copy a Drain's move left Out on the drained Server leaves that Server for good: a
/// Service run without a Volume, such as postgres, keeps its data in the anonymous
/// Volume its image declares, so that data stays.
#[tokio::test]
async fn a_drain_keeps_the_anonymous_volumes_of_a_copy_it_lets_go() {
    let (drained, other) = (machine('1', "drained"), machine('2', "other"));
    let cluster = Cluster::of(
        &[&drained, &other],
        vec![
            copy_on(&drained, 'a', CopyRotation::Out),
            copy_on(&other, 'b', CopyRotation::In),
        ],
    );
    let copy = Copy::observed(&copy_on(&drained, 'a', CopyRotation::Out));
    let end = let_go(
        &cluster,
        &copy,
        &names(vec![drained.clone(), other.clone()]),
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(end, OldCopyEnd::Stopped);
    assert_eq!(anonymous_volumes_deleted(&cluster), [false]);
}
