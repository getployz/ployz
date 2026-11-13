//! The answers Internal DNS gives: a projection of the replicated Containers and
//! the plan for each query against it.

use std::{
    collections::HashMap,
    io,
    net::{IpAddr, Ipv4Addr},
    sync::{
        Arc, RwLock,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};

use hickory_server::proto::{
    op::ResponseCode,
    rr::{Name, RData, Record, RecordType, rdata::A},
};
use ipnet::Ipv4Net;
use ployz_core::{
    ClusterName, ContainerObservation, MachineId, Namespace, ProxyRoutes, QualifiedService,
    ServiceId, proxy_routes, reachable_routes, service_containers,
};
use tokio::sync::watch;

use super::query::{InternalQuery, MachineServiceTarget, Query, parse};
use crate::route_acks::DownMachines;

/// How long a caller-relative query from an unknown bridge address waits for a
/// projection that names its caller. A container asks as soon as it starts, before
/// its own observation reaches Corrosion (#3379); a resolver retries after 5 s.
pub(super) const CALLER_WAIT: Duration = Duration::from_secs(3);

#[derive(Debug, PartialEq)]
pub(super) enum ResponsePlan {
    Forward,
    Internal {
        code: ResponseCode,
        answers: Vec<Record>,
    },
    /// A caller-relative name from a bridge address no projection has named yet.
    UnknownCaller,
}

pub(super) struct Projection {
    service_ids: HashMap<ServiceId, Vec<Ipv4Addr>>,
    identities: HashMap<QualifiedService, ServiceAddresses>,
    machine_identities: HashMap<MachineServiceTarget, Vec<Ipv4Addr>>,
    caller_namespaces: HashMap<Ipv4Addr, Namespace>,
    cluster: Option<ClusterName>,
}

pub(super) struct ProjectionInputs {
    pub(super) observations: Vec<ContainerObservation>,
    pub(super) cluster: Option<ClusterName>,
    pub(super) down: DownMachines,
    /// Kept while no copy of a Service is reachable.
    routed: ProxyRoutes,
}

pub(super) struct Snapshot {
    pub(super) observations: Vec<ContainerObservation>,
    pub(super) cluster: Option<ClusterName>,
}

impl ProjectionInputs {
    pub(super) fn load(&mut self, snapshot: Snapshot) {
        self.observations = snapshot.observations;
        self.cluster = snapshot.cluster;
    }

    pub(super) fn new(local_id: MachineId) -> Self {
        Self {
            observations: Vec::new(),
            cluster: None,
            down: DownMachines::new(local_id),
            routed: ProxyRoutes::new(),
        }
    }

    pub(super) fn build(&mut self) -> Projection {
        let containers = service_containers(self.observations.iter().cloned());
        let routes = proxy_routes(
            &containers,
            |machine| self.down.contains(machine),
            &self.routed,
        );
        let reachable = reachable_routes(&routes, &self.observations, |machine| {
            self.down.contains(machine)
        });
        let mut projection = Projection::from_routes(&self.observations, &reachable);
        projection.cluster.clone_from(&self.cluster);
        self.routed = routes;
        projection
    }
}

#[derive(Default)]
struct ServiceAddresses {
    eligible: Vec<Ipv4Addr>,
    next: AtomicUsize,
}

impl ServiceAddresses {
    fn rotated(&self) -> Vec<Ipv4Addr> {
        let mut addresses = self.eligible.clone();
        if !addresses.is_empty() {
            let offset = self.next.fetch_add(1, Ordering::Relaxed) % addresses.len();
            addresses.rotate_left(offset);
        }
        addresses
    }
}

impl Projection {
    fn from_routes(observations: &[ContainerObservation], routes: &ProxyRoutes) -> Self {
        let mut service_ids = HashMap::<ServiceId, Vec<Ipv4Addr>>::new();
        let mut identities = HashMap::<QualifiedService, ServiceAddresses>::new();
        let mut machine_identities = HashMap::<MachineServiceTarget, Vec<Ipv4Addr>>::new();
        for serving in routes.values().flatten() {
            let observation = serving.as_observation();
            let address = serving.address();
            let identity = observation.identity();
            service_ids
                .entry(observation.service_id())
                .or_default()
                .push(address.0);
            identities
                .entry(identity.clone())
                .or_default()
                .eligible
                .push(address.0);
            machine_identities
                .entry(MachineServiceTarget {
                    machine_id: observation.machine_id,
                    identity,
                })
                .or_default()
                .push(address.0);
        }
        Self {
            service_ids,
            identities,
            machine_identities,
            caller_namespaces: unique_caller_namespaces(observations),
            cluster: None,
        }
    }

    pub(super) fn plan(
        &self,
        name: &Name,
        record_type: RecordType,
        local_subnet: Ipv4Net,
        source: IpAddr,
    ) -> ResponsePlan {
        match parse(name, self.cluster.as_ref()) {
            Query::Forward => ResponsePlan::Forward,
            Query::NxDomain => ResponsePlan::Internal {
                code: ResponseCode::NXDomain,
                answers: Vec::new(),
            },
            Query::Internal(query) => {
                self.plan_internal(name, record_type, local_subnet, query, source)
            }
        }
    }

    fn plan_internal(
        &self,
        name: &Name,
        record_type: RecordType,
        local_subnet: Ipv4Net,
        query: InternalQuery,
        source: IpAddr,
    ) -> ResponsePlan {
        if record_type != RecordType::A {
            return ResponsePlan::Internal {
                code: ResponseCode::NoError,
                answers: Vec::new(),
            };
        }
        let (mut addresses, nearest) = match query {
            InternalQuery::Empty | InternalQuery::Regional => (Vec::new(), false),
            InternalQuery::Service(identity) => (
                self.identities
                    .get(&identity)
                    .map(ServiceAddresses::rotated)
                    .unwrap_or_default(),
                false,
            ),
            InternalQuery::CallerService(service) => match self.caller_namespace(source) {
                Some(namespace) => (
                    self.identities
                        .get(&QualifiedService::new(namespace.clone(), service))
                        .map(ServiceAddresses::rotated)
                        .unwrap_or_default(),
                    false,
                ),
                None if may_be_projected(local_subnet, source) => {
                    return ResponsePlan::UnknownCaller;
                }
                None => (Vec::new(), false),
            },
            InternalQuery::Nearest(identity) => (
                self.identities
                    .get(&identity)
                    .map(|addresses| addresses.eligible.clone())
                    .unwrap_or_default(),
                true,
            ),
            InternalQuery::ServiceId(id) => (
                self.service_ids.get(&id).cloned().unwrap_or_default(),
                false,
            ),
            InternalQuery::Machine(target) => (
                self.machine_identities
                    .get(&target)
                    .cloned()
                    .unwrap_or_default(),
                false,
            ),
        };
        if nearest {
            addresses.sort_by_key(|address| !local_subnet.contains(address));
        }
        if addresses.is_empty() {
            return ResponsePlan::Internal {
                code: ResponseCode::NXDomain,
                answers: Vec::new(),
            };
        }
        let answers = addresses
            .into_iter()
            .map(|address| Record::from_rdata(name.clone(), 0, RData::A(A(address))))
            .collect();
        ResponsePlan::Internal {
            code: ResponseCode::NoError,
            answers,
        }
    }

    fn caller_namespace(&self, source: IpAddr) -> Option<&Namespace> {
        let IpAddr::V4(address) = source else {
            return None;
        };
        self.caller_namespaces.get(&address)
    }
}

/// Whether `source` is a container address on this Machine's bridge, which a later
/// projection may name. The gateway is the host itself and never is.
fn may_be_projected(local_subnet: Ipv4Net, source: IpAddr) -> bool {
    let IpAddr::V4(address) = source else {
        return false;
    };
    let gateway = Ipv4Addr::from(u32::from(local_subnet.network()) + 1);
    local_subnet.contains(&address) && address != gateway
}

fn unique_caller_namespaces(containers: &[ContainerObservation]) -> HashMap<Ipv4Addr, Namespace> {
    let mut by_address = HashMap::<Ipv4Addr, Vec<Namespace>>::new();
    for container in containers {
        let Some(address) = container.address else {
            continue;
        };
        by_address
            .entry(address.0)
            .or_default()
            .push(container.namespace.clone());
    }
    by_address
        .into_iter()
        .filter_map(|(address, namespaces)| {
            <[Namespace; 1]>::try_from(namespaces)
                .ok()
                .map(|[namespace]| (address, namespace))
        })
        .collect()
}

#[derive(Clone)]
pub(super) struct Answers(Arc<Shared>);

struct Shared {
    projection: RwLock<Projection>,
    replaced: watch::Sender<()>,
    local_subnet: Ipv4Net,
}

impl Answers {
    pub(super) fn loaded(projection: Projection, local_subnet: Ipv4Net) -> Self {
        Self(Arc::new(Shared {
            projection: RwLock::new(projection),
            replaced: watch::Sender::new(()),
            local_subnet,
        }))
    }

    pub(super) fn replace(&self, projection: Projection) -> io::Result<()> {
        *self
            .0
            .projection
            .write()
            .map_err(|_| io::Error::other("DNS projection lock poisoned"))? = projection;
        self.0.replaced.send_replace(());
        Ok(())
    }

    /// Plans an answer, holding a query from a caller not yet projected until a
    /// projection names it or [`CALLER_WAIT`] passes.
    pub(super) async fn settled_plan(
        &self,
        name: &Name,
        query_type: RecordType,
        caller: IpAddr,
    ) -> ResponsePlan {
        let mut replaced = self.0.replaced.subscribe();
        let deadline = tokio::time::Instant::now() + CALLER_WAIT;
        loop {
            let plan = self.plan(name, query_type, caller);
            if plan != ResponsePlan::UnknownCaller {
                return plan;
            }
            if !matches!(
                tokio::time::timeout_at(deadline, replaced.changed()).await,
                Ok(Ok(()))
            ) {
                return ResponsePlan::UnknownCaller;
            }
        }
    }

    fn plan(&self, name: &Name, query_type: RecordType, caller: IpAddr) -> ResponsePlan {
        self.0
            .projection
            .read()
            .map(|projection| projection.plan(name, query_type, self.0.local_subnet, caller))
            .unwrap_or(ResponsePlan::Internal {
                code: ResponseCode::ServFail,
                answers: Vec::new(),
            })
    }
}
