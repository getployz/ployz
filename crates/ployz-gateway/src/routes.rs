use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::{SocketAddr, SocketAddrV4};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::{AtomicUsize, Ordering};

use pingora::tls::pkey::{PKey, Private};
use pingora::tls::x509::X509;
use ployz_types::model::{
    AcmeChallengeEvent, AcmeChallengeRecord, CertificateEvent, CertificateRecord, InstanceId,
    InstancePhase, InstanceStatusRecord, MachineId, MachineMembership, MachineTopology,
    RoutingEvent, RoutingState, ServiceRelease, ServiceReleaseSlot, ServiceRoutingPolicy,
};
use ployz_types::spec::{Namespace, RouteSpec, ServiceSpec};
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::warn;

#[cfg(test)]
static HTTP_CONFLICT_VALIDATION_VISITS: AtomicUsize = AtomicUsize::new(0);
#[cfg(test)]
static TCP_CONFLICT_VALIDATION_VISITS: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug, Clone)]
pub struct GatewaySnapshot {
    pub http_routes: Vec<HttpRouteView>,
    pub tcp_routes: Vec<TcpRouteView>,
    /// Keyed on `(normalized hostname, token)` for O(1) HTTP-01 lookup.
    pub acme_challenges: HashMap<(String, String), AcmeChallengeView>,
    /// Keyed on normalized hostname for O(1) SNI lookup.
    pub certificates: HashMap<String, CertificateView>,
}

impl GatewaySnapshot {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            http_routes: Vec::new(),
            tcp_routes: Vec::new(),
            acme_challenges: HashMap::new(),
            certificates: HashMap::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcmeChallengeView {
    pub hostname: String,
    pub token: String,
    pub key_authorization: String,
}

/// Parsed, ready-to-install TLS material. Built once at projection time so the
/// handshake hot path is a snapshot lookup + `Arc::clone` — no PEM parsing, no
/// `X509::clone`, no `Vec` allocation per connection.
#[derive(Debug)]
pub struct ProjectedTlsMaterial {
    pub leaf: X509,
    pub chain: Arc<[X509]>,
    pub private_key: PKey<Private>,
}

#[derive(Debug, Clone)]
pub struct CertificateView {
    pub hostname: String,
    pub tls: Arc<ProjectedTlsMaterial>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRouteView {
    pub route_id: RouteId,
    pub namespace: Namespace,
    pub service: String,
    pub revision_hash: String,
    pub hostnames: Vec<String>,
    pub path_prefix: String,
    pub backends: Vec<BackendView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TcpRouteView {
    pub route_id: RouteId,
    pub namespace: Namespace,
    pub service: String,
    pub revision_hash: String,
    pub listen_port: u16,
    pub backends: Vec<BackendView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BackendView {
    pub instance_id: InstanceId,
    pub machine_id: MachineId,
    pub topology: MachineTopology,
    pub service_port: String,
    pub address: SocketAddr,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ServiceKey {
    pub namespace: Namespace,
    pub service: String,
}

impl ServiceKey {
    #[must_use]
    pub fn new(namespace: Namespace, service: String) -> Self {
        Self { namespace, service }
    }

    #[must_use]
    pub fn from_release(record: &ployz_types::model::ServiceReleaseRecord) -> Self {
        Self::new(record.namespace.clone(), record.service.clone())
    }

    #[must_use]
    pub fn from_instance(record: &InstanceStatusRecord) -> Self {
        Self::new(record.namespace.clone(), record.service.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct RevisionKey {
    pub namespace: Namespace,
    pub service: String,
    pub revision_hash: String,
}

impl RevisionKey {
    #[must_use]
    pub fn new(namespace: Namespace, service: String, revision_hash: String) -> Self {
        Self {
            namespace,
            service,
            revision_hash,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct RouteId(String);

impl RouteId {
    #[must_use]
    pub fn http(service: &ServiceKey, index: usize) -> Self {
        Self(format!(
            "http:{}:{}:{}",
            service.namespace, service.service, index
        ))
    }

    #[must_use]
    pub fn tcp(service: &ServiceKey, index: usize) -> Self {
        Self(format!(
            "tcp:{}:{}:{}",
            service.namespace, service.service, index
        ))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for RouteId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct NormalizedHostname(String);

impl NormalizedHostname {
    #[must_use]
    pub fn parse(hostname: &str) -> Option<Self> {
        let normalized = normalize_request_host(hostname);
        if normalized.is_empty() {
            None
        } else {
            Some(Self(normalized))
        }
    }

    #[must_use]
    pub fn into_string(self) -> String {
        self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct BackendKey {
    pub instance_id: InstanceId,
    pub service_port: String,
}

#[derive(Debug, Clone)]
pub enum GatewayProjectionEvent {
    Routing(RoutingEvent),
    Certificate(CertificateEvent),
    AcmeChallenge(AcmeChallengeEvent),
}

#[derive(Debug, Clone)]
pub enum ProjectionDelta {
    RoutesChanged {
        removed_route_ids: Vec<RouteId>,
        upserted_http: Vec<HttpRouteView>,
        upserted_tcp: Vec<TcpRouteView>,
    },
    CertificateChanged {
        hostname: String,
        value: Option<CertificateView>,
    },
    ChallengeChanged {
        key: (String, String),
        value: Option<AcmeChallengeView>,
    },
    Empty,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProjectionError {
    #[error(
        "service release for '{service}' in namespace '{namespace}' referenced missing revision '{revision_hash}'"
    )]
    MissingRevision {
        namespace: Namespace,
        service: String,
        revision_hash: String,
    },
    #[error(
        "service release for '{service}' in namespace '{namespace}' had invalid spec json: {message}"
    )]
    InvalidRevisionSpec {
        namespace: Namespace,
        service: String,
        message: String,
    },
    #[error(
        "HTTP route conflict between '{left}' and '{right}' for host '{host}' and path prefix '{path_prefix}'"
    )]
    HttpRouteConflict {
        left: String,
        right: String,
        host: String,
        path_prefix: String,
    },
    #[error("TCP route conflict between '{left}' and '{right}' for listen port {listen_port}")]
    TcpRouteConflict {
        left: String,
        right: String,
        listen_port: u16,
    },
    #[error(
        "routable instance '{instance_id}' for service '{service}' in namespace '{namespace}' referenced missing machine '{machine_id}'"
    )]
    MissingMachineForInstance {
        namespace: Namespace,
        service: String,
        instance_id: InstanceId,
        machine_id: MachineId,
    },
}

#[must_use]
pub fn normalize_request_host(host: &str) -> String {
    let trimmed = host.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        return String::new();
    }
    if let Some(ipv6) = trimmed.strip_prefix('[') {
        let bare = match ipv6.split_once(']') {
            Some((addr, _)) => addr,
            None => ipv6,
        };
        return bare.to_ascii_lowercase();
    }
    match trimmed.rsplit_once(':') {
        Some((left, right)) if right.chars().all(|char| char.is_ascii_digit()) => {
            left.to_ascii_lowercase()
        }
        _ => trimmed.to_ascii_lowercase(),
    }
}

#[must_use]
pub fn match_http_route<'a>(
    snapshot: &'a GatewaySnapshot,
    host: Option<&str>,
    path: &str,
) -> Option<&'a HttpRouteView> {
    let host = host
        .map(normalize_request_host)
        .filter(|value| !value.is_empty());
    let path = normalize_path_prefix(path);
    snapshot.http_routes.iter().find(|route| {
        route_matches_host(route, host.as_deref()) && path.starts_with(route.path_prefix.as_str())
    })
}

#[derive(Clone)]
pub struct GatewayProjector {
    machines: HashMap<MachineId, MachineMembership>,
    revisions: HashMap<RevisionKey, ployz_types::model::ServiceRevisionRecord>,
    specs: HashMap<RevisionKey, ServiceSpec>,
    releases: HashMap<ServiceKey, ployz_types::model::ServiceReleaseRecord>,
    instances: HashMap<InstanceId, InstanceStatusRecord>,
    instances_by_machine: HashMap<MachineId, HashSet<InstanceId>>,
    service_routes: HashMap<ServiceKey, BTreeSet<RouteId>>,
    http_routes: BTreeMap<RouteId, HttpRouteView>,
    tcp_routes: BTreeMap<RouteId, TcpRouteView>,
    http_conflicts: HashMap<(String, String), RouteId>,
    tcp_conflicts: HashMap<u16, RouteId>,
    acme_challenges: HashMap<(String, String), AcmeChallengeView>,
    certificates: HashMap<String, CertificateView>,
    #[cfg(test)]
    project_counts: HashMap<ServiceKey, usize>,
}

struct ServiceRouteSnapshot {
    service: ServiceKey,
    route_ids: Option<BTreeSet<RouteId>>,
    http_routes: Vec<(RouteId, HttpRouteView)>,
    tcp_routes: Vec<(RouteId, TcpRouteView)>,
}

impl GatewayProjector {
    pub fn new(state: RoutingState) -> Result<Self, ProjectionError> {
        let mut projector = Self {
            machines: state
                .machines
                .into_iter()
                .map(|machine| (machine.id.clone(), machine))
                .collect(),
            revisions: state
                .revisions
                .into_iter()
                .map(|revision| {
                    (
                        RevisionKey::new(
                            revision.namespace.clone(),
                            revision.service.clone(),
                            revision.revision_hash.clone(),
                        ),
                        revision,
                    )
                })
                .collect(),
            specs: HashMap::new(),
            releases: state
                .releases
                .into_iter()
                .map(|release| (ServiceKey::from_release(&release), release))
                .collect(),
            instances: HashMap::new(),
            instances_by_machine: HashMap::new(),
            service_routes: HashMap::new(),
            http_routes: BTreeMap::new(),
            tcp_routes: BTreeMap::new(),
            http_conflicts: HashMap::new(),
            tcp_conflicts: HashMap::new(),
            acme_challenges: HashMap::new(),
            certificates: HashMap::new(),
            #[cfg(test)]
            project_counts: HashMap::new(),
        };

        for instance in state.instances {
            projector.insert_instance(instance);
        }
        let services = projector.releases.keys().cloned().collect::<Vec<_>>();
        projector.reproject_services(services)?;
        Ok(projector)
    }

    pub fn apply(
        &mut self,
        event: GatewayProjectionEvent,
    ) -> Result<ProjectionDelta, ProjectionError> {
        match event {
            GatewayProjectionEvent::Routing(event) => self.apply_routing(event),
            GatewayProjectionEvent::Certificate(event) => Ok(self.apply_certificate(event)),
            GatewayProjectionEvent::AcmeChallenge(event) => Ok(self.apply_acme(event)),
        }
    }

    #[must_use]
    pub fn snapshot_value(&self) -> GatewaySnapshot {
        let mut http_routes = self.http_routes.values().cloned().collect::<Vec<_>>();
        let mut tcp_routes = self.tcp_routes.values().cloned().collect::<Vec<_>>();
        http_routes.sort_by_key(|route| {
            (
                route.hostnames.is_empty(),
                Reverse(route.path_prefix.len()),
                route.namespace.0.clone(),
                route.service.clone(),
                route.route_id.clone(),
            )
        });
        tcp_routes.sort_by_key(|route| (route.listen_port, route.route_id.clone()));
        GatewaySnapshot {
            http_routes,
            tcp_routes,
            acme_challenges: self.acme_challenges.clone(),
            certificates: self.certificates.clone(),
        }
    }

    fn apply_routing(&mut self, event: RoutingEvent) -> Result<ProjectionDelta, ProjectionError> {
        let mut services = BTreeSet::new();
        match event {
            RoutingEvent::MachineUpsert(machine) => {
                let machine_id = machine.id.clone();
                self.machines.insert(machine.id.clone(), machine);
                services.extend(self.services_for_machine(&machine_id));
            }
            RoutingEvent::MachineRemoved { id } => {
                services.extend(self.services_for_machine(&id));
                self.machines.remove(&id);
            }
            RoutingEvent::RevisionUpsert(revision) => {
                let key = RevisionKey::new(
                    revision.namespace.clone(),
                    revision.service.clone(),
                    revision.revision_hash.clone(),
                );
                self.specs.remove(&key);
                self.revisions.insert(key.clone(), revision);
                services.extend(self.services_using_revision(&key));
            }
            RoutingEvent::RevisionRemoved {
                namespace,
                service,
                revision_hash,
            } => {
                let key = RevisionKey::new(namespace, service, revision_hash);
                self.revisions.remove(&key);
                self.specs.remove(&key);
                services.extend(self.services_using_revision(&key));
            }
            RoutingEvent::ReleaseUpsert(release) => {
                let service = ServiceKey::from_release(&release);
                self.releases.insert(service.clone(), release);
                services.insert(service);
            }
            RoutingEvent::ReleaseRemoved { namespace, service } => {
                let service = ServiceKey::new(namespace, service);
                self.releases.remove(&service);
                services.insert(service);
            }
            RoutingEvent::InstanceUpsert(instance) => {
                services.insert(ServiceKey::from_instance(&instance));
                if let Some(old) = self.instances.get(&instance.instance_id) {
                    services.insert(ServiceKey::from_instance(old));
                }
                self.insert_instance(instance);
            }
            RoutingEvent::InstanceRemoved { instance_id } => {
                if let Some(instance) = self.instances.get(&instance_id) {
                    services.insert(ServiceKey::from_instance(instance));
                }
                self.remove_instance(&instance_id);
            }
        }
        self.reproject_services(services.into_iter().collect())
    }

    fn apply_certificate(&mut self, event: CertificateEvent) -> ProjectionDelta {
        let hostname = match &event {
            CertificateEvent::Upsert(record) => {
                NormalizedHostname::parse(&record.hostname).map(NormalizedHostname::into_string)
            }
            CertificateEvent::Removed { hostname } => {
                NormalizedHostname::parse(hostname).map(NormalizedHostname::into_string)
            }
        };
        match event {
            CertificateEvent::Upsert(record) => {
                self.project_certificate(record);
            }
            CertificateEvent::Removed { hostname } => {
                if let Some(hostname) = NormalizedHostname::parse(&hostname) {
                    self.certificates.remove(&hostname.into_string());
                }
            }
        }
        hostname.map_or(ProjectionDelta::Empty, |hostname| {
            ProjectionDelta::CertificateChanged {
                value: self.certificates.get(&hostname).cloned(),
                hostname,
            }
        })
    }

    fn apply_acme(&mut self, event: AcmeChallengeEvent) -> ProjectionDelta {
        let key = match &event {
            AcmeChallengeEvent::Upsert(record) => NormalizedHostname::parse(&record.hostname)
                .map(NormalizedHostname::into_string)
                .map(|hostname| (hostname, record.token.clone())),
            AcmeChallengeEvent::Removed { hostname, token } => NormalizedHostname::parse(hostname)
                .map(NormalizedHostname::into_string)
                .map(|hostname| (hostname, token.clone())),
        };
        match event {
            AcmeChallengeEvent::Upsert(record) => {
                self.project_acme(record);
            }
            AcmeChallengeEvent::Removed { hostname, token } => {
                if let Some(hostname) = NormalizedHostname::parse(&hostname) {
                    self.acme_challenges
                        .remove(&(hostname.into_string(), token));
                }
            }
        }
        key.map_or(ProjectionDelta::Empty, |key| {
            ProjectionDelta::ChallengeChanged {
                value: self.acme_challenges.get(&key).cloned(),
                key,
            }
        })
    }

    fn reproject_services(
        &mut self,
        services: Vec<ServiceKey>,
    ) -> Result<ProjectionDelta, ProjectionError> {
        let services = services.into_iter().collect::<BTreeSet<_>>();
        if services.is_empty() {
            return Ok(ProjectionDelta::Empty);
        }
        let affected_services = services.clone();
        let saved_routes = self.save_service_routes(&services);
        let removed_route_ids = saved_routes
            .iter()
            .flat_map(|snapshot| snapshot.route_ids.iter().flatten().cloned())
            .collect::<Vec<_>>();
        for service in services {
            self.clear_service_routes(&service);
            if self.releases.contains_key(&service) {
                if let Err(err) = self.project_service(&service) {
                    self.restore_service_routes(saved_routes);
                    return Err(err);
                }
            }
        }
        let removed = removed_route_ids.into_iter().collect::<BTreeSet<_>>();
        let mut upserted_http = Vec::new();
        let mut upserted_tcp = Vec::new();
        for service in affected_services {
            if let Some(route_ids) = self.service_routes.get(&service) {
                for route_id in route_ids {
                    if let Some(route) = self.http_routes.get(route_id) {
                        upserted_http.push(route.clone());
                    }
                    if let Some(route) = self.tcp_routes.get(route_id) {
                        upserted_tcp.push(route.clone());
                    }
                }
            }
        }
        Ok(ProjectionDelta::RoutesChanged {
            removed_route_ids: removed.into_iter().collect(),
            upserted_http,
            upserted_tcp,
        })
    }

    fn project_service(&mut self, service: &ServiceKey) -> Result<(), ProjectionError> {
        #[cfg(test)]
        {
            *self.project_counts.entry(service.clone()).or_default() += 1;
        }
        let Some(release_record) = self.releases.get(service).cloned() else {
            return Ok(());
        };
        let routing_revision_hash = routing_revision_hash(&release_record.release);
        let revision_key = RevisionKey::new(
            service.namespace.clone(),
            service.service.clone(),
            routing_revision_hash.clone(),
        );
        let spec = self.spec_for_revision(&revision_key)?.clone();
        let backends_by_port = self.project_backends(&spec, &release_record)?;

        for (index, route) in spec.routes.iter().enumerate() {
            match route {
                RouteSpec::Http(route) => {
                    self.project_http(
                        service,
                        &spec,
                        &routing_revision_hash,
                        index,
                        route,
                        &backends_by_port,
                    )?;
                }
                RouteSpec::Tcp(route) => {
                    self.project_tcp(
                        service,
                        &spec,
                        &routing_revision_hash,
                        index,
                        route,
                        &backends_by_port,
                    )?;
                }
            }
        }
        Ok(())
    }

    fn project_http(
        &mut self,
        service: &ServiceKey,
        spec: &ServiceSpec,
        revision_hash: &str,
        index: usize,
        route: &ployz_types::spec::HttpRoute,
        backends_by_port: &BTreeMap<String, Vec<BackendView>>,
    ) -> Result<(), ProjectionError> {
        let route_id = RouteId::http(service, index);
        let hostnames = route
            .hostnames
            .iter()
            .filter_map(|hostname| {
                NormalizedHostname::parse(hostname).map(NormalizedHostname::into_string)
            })
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect::<Vec<_>>();
        self.service_routes
            .entry(service.clone())
            .or_default()
            .insert(route_id.clone());
        let route = HttpRouteView {
            route_id: route_id.clone(),
            namespace: service.namespace.clone(),
            service: spec.name.clone(),
            revision_hash: revision_hash.to_string(),
            hostnames,
            path_prefix: normalize_path_prefix(&route.path_prefix),
            backends: backends_by_port
                .get(&route.service_port)
                .cloned()
                .unwrap_or_default(),
        };
        self.insert_projected_http_route(route_id, route)?;
        Ok(())
    }

    fn project_tcp(
        &mut self,
        service: &ServiceKey,
        spec: &ServiceSpec,
        revision_hash: &str,
        index: usize,
        route: &ployz_types::spec::TcpRoute,
        backends_by_port: &BTreeMap<String, Vec<BackendView>>,
    ) -> Result<(), ProjectionError> {
        let route_id = RouteId::tcp(service, index);
        self.service_routes
            .entry(service.clone())
            .or_default()
            .insert(route_id.clone());
        let route = TcpRouteView {
            route_id: route_id.clone(),
            namespace: service.namespace.clone(),
            service: spec.name.clone(),
            revision_hash: revision_hash.to_string(),
            listen_port: route.listen_port,
            backends: backends_by_port
                .get(&route.service_port)
                .cloned()
                .unwrap_or_default(),
        };
        self.insert_projected_tcp_route(route_id, route)?;
        Ok(())
    }

    fn project_backends(
        &self,
        spec: &ServiceSpec,
        release_record: &ployz_types::model::ServiceReleaseRecord,
    ) -> Result<BTreeMap<String, Vec<BackendView>>, ProjectionError> {
        let allowed_revision_hashes = allowed_revision_hashes(&release_record.release);
        routable_backends_by_port(
            spec,
            &release_record.namespace,
            &release_record.service,
            &allowed_revision_hashes,
            &release_record.release.slots,
            &self.instances,
            &self.machines,
        )
    }

    fn insert_projected_http_route(
        &mut self,
        route_id: RouteId,
        route: HttpRouteView,
    ) -> Result<(), ProjectionError> {
        let hosts = conflict_hosts(&route);
        for host in &hosts {
            let key = (host.clone(), route.path_prefix.clone());
            if let Some(existing) = self.http_conflicts.get(&key)
                && existing != &route_id
            {
                return Err(ProjectionError::HttpRouteConflict {
                    left: existing.to_string(),
                    right: route_id.to_string(),
                    host: host.clone(),
                    path_prefix: route.path_prefix.clone(),
                });
            }
        }
        for host in hosts {
            self.http_conflicts
                .insert((host, route.path_prefix.clone()), route_id.clone());
        }
        self.http_routes.insert(route_id, route);
        Ok(())
    }

    fn insert_projected_tcp_route(
        &mut self,
        route_id: RouteId,
        route: TcpRouteView,
    ) -> Result<(), ProjectionError> {
        if let Some(existing) = self.tcp_conflicts.get(&route.listen_port)
            && existing != &route_id
        {
            return Err(ProjectionError::TcpRouteConflict {
                left: existing.to_string(),
                right: route_id.to_string(),
                listen_port: route.listen_port,
            });
        }
        self.tcp_conflicts
            .insert(route.listen_port, route_id.clone());
        self.tcp_routes.insert(route_id, route);
        Ok(())
    }

    fn project_certificate(&mut self, record: CertificateRecord) {
        let Some(hostname) = NormalizedHostname::parse(&record.hostname) else {
            return;
        };
        let hostname = hostname.into_string();
        let Some(version) = record.installed_version() else {
            self.certificates.remove(&hostname);
            return;
        };
        match parse_tls_material(&hostname, version) {
            Some(tls) => {
                self.certificates.insert(
                    hostname.clone(),
                    CertificateView {
                        hostname,
                        tls: Arc::new(tls),
                    },
                );
            }
            None => {
                self.certificates.remove(&hostname);
            }
        }
    }

    fn project_acme(&mut self, record: AcmeChallengeRecord) {
        let Some(hostname) = NormalizedHostname::parse(&record.hostname) else {
            return;
        };
        let hostname = hostname.into_string();
        self.acme_challenges.insert(
            (hostname.clone(), record.token.clone()),
            AcmeChallengeView {
                hostname,
                token: record.token,
                key_authorization: record.key_authorization,
            },
        );
    }

    fn spec_for_revision(&mut self, key: &RevisionKey) -> Result<&ServiceSpec, ProjectionError> {
        use std::collections::hash_map::Entry;
        match self.specs.entry(key.clone()) {
            Entry::Occupied(entry) => Ok(entry.into_mut()),
            Entry::Vacant(entry) => {
                let Some(revision) = self.revisions.get(key) else {
                    return Err(ProjectionError::MissingRevision {
                        namespace: key.namespace.clone(),
                        service: key.service.clone(),
                        revision_hash: key.revision_hash.clone(),
                    });
                };
                let spec = serde_json::from_str(&revision.spec_json).map_err(|err| {
                    ProjectionError::InvalidRevisionSpec {
                        namespace: revision.namespace.clone(),
                        service: revision.service.clone(),
                        message: err.to_string(),
                    }
                })?;
                Ok(entry.insert(spec))
            }
        }
    }

    fn clear_service_routes(&mut self, service: &ServiceKey) {
        let Some(routes) = self.service_routes.remove(service) else {
            return;
        };
        for route_id in routes {
            self.remove_projected_route(&route_id);
        }
    }

    fn remove_projected_route(&mut self, route_id: &RouteId) {
        if let Some(route) = self.http_routes.remove(route_id) {
            for host in conflict_hosts(&route) {
                self.http_conflicts
                    .remove(&(host, route.path_prefix.clone()));
            }
        }
        if let Some(route) = self.tcp_routes.remove(route_id) {
            self.tcp_conflicts.remove(&route.listen_port);
        }
    }

    fn save_service_routes(&self, services: &BTreeSet<ServiceKey>) -> Vec<ServiceRouteSnapshot> {
        services
            .iter()
            .map(|service| {
                let route_ids = self.service_routes.get(service).cloned();
                let mut http_routes = Vec::new();
                let mut tcp_routes = Vec::new();
                if let Some(route_ids) = &route_ids {
                    for route_id in route_ids {
                        if let Some(route) = self.http_routes.get(route_id) {
                            http_routes.push((route_id.clone(), route.clone()));
                        }
                        if let Some(route) = self.tcp_routes.get(route_id) {
                            tcp_routes.push((route_id.clone(), route.clone()));
                        }
                    }
                }
                ServiceRouteSnapshot {
                    service: service.clone(),
                    route_ids,
                    http_routes,
                    tcp_routes,
                }
            })
            .collect()
    }

    fn restore_service_routes(&mut self, snapshots: Vec<ServiceRouteSnapshot>) {
        for snapshot in &snapshots {
            self.clear_service_routes(&snapshot.service);
        }
        for snapshot in snapshots {
            if let Some(route_ids) = snapshot.route_ids {
                self.service_routes.insert(snapshot.service, route_ids);
            }
            for (route_id, route) in snapshot.http_routes {
                self.restore_projected_http_route(route_id, route);
            }
            for (route_id, route) in snapshot.tcp_routes {
                self.restore_projected_tcp_route(route_id, route);
            }
        }
    }

    fn restore_projected_http_route(&mut self, route_id: RouteId, route: HttpRouteView) {
        for host in conflict_hosts(&route) {
            self.http_conflicts
                .insert((host, route.path_prefix.clone()), route_id.clone());
        }
        self.http_routes.insert(route_id, route);
    }

    fn restore_projected_tcp_route(&mut self, route_id: RouteId, route: TcpRouteView) {
        self.tcp_conflicts
            .insert(route.listen_port, route_id.clone());
        self.tcp_routes.insert(route_id, route);
    }

    fn insert_instance(&mut self, instance: InstanceStatusRecord) {
        self.remove_instance(&instance.instance_id);
        self.instances_by_machine
            .entry(instance.machine_id.clone())
            .or_default()
            .insert(instance.instance_id.clone());
        self.instances
            .insert(instance.instance_id.clone(), instance);
    }

    fn remove_instance(&mut self, instance_id: &InstanceId) {
        let Some(instance) = self.instances.remove(instance_id) else {
            return;
        };
        if let Some(instances) = self.instances_by_machine.get_mut(&instance.machine_id) {
            instances.remove(instance_id);
            if instances.is_empty() {
                self.instances_by_machine.remove(&instance.machine_id);
            }
        }
    }

    fn services_for_machine(&self, machine_id: &MachineId) -> BTreeSet<ServiceKey> {
        self.instances_by_machine
            .get(machine_id)
            .into_iter()
            .flat_map(|instances| instances.iter())
            .filter_map(|instance_id| self.instances.get(instance_id))
            .map(ServiceKey::from_instance)
            .collect()
    }

    fn services_using_revision(&self, revision: &RevisionKey) -> BTreeSet<ServiceKey> {
        self.releases
            .iter()
            .filter_map(|(service, release)| {
                if service.namespace == revision.namespace
                    && service.service == revision.service
                    && release
                        .release
                        .referenced_revision_hashes
                        .iter()
                        .any(|hash| hash == &revision.revision_hash)
                {
                    Some(service.clone())
                } else {
                    None
                }
            })
            .collect()
    }

    #[cfg(test)]
    fn project_count(&self, service: &ServiceKey) -> usize {
        self.project_counts.get(service).copied().unwrap_or(0)
    }

    #[must_use]
    pub fn certificate_count(&self) -> usize {
        self.certificates.len()
    }

    #[must_use]
    pub fn has_certificate(&self, hostname: &str) -> bool {
        NormalizedHostname::parse(hostname)
            .map(NormalizedHostname::into_string)
            .is_some_and(|hostname| self.certificates.contains_key(&hostname))
    }

    #[must_use]
    pub fn acme_challenge_count(&self) -> usize {
        self.acme_challenges.len()
    }

    #[must_use]
    pub fn has_acme_challenge(&self, hostname: &str, token: &str) -> bool {
        NormalizedHostname::parse(hostname)
            .map(NormalizedHostname::into_string)
            .is_some_and(|hostname| {
                self.acme_challenges
                    .contains_key(&(hostname, token.to_string()))
            })
    }
}

pub fn project(state: RoutingState) -> Result<GatewaySnapshot, ProjectionError> {
    Ok(GatewayProjector::new(state)?.snapshot_value())
}

#[must_use]
pub fn with_managed_tls(
    mut snapshot: GatewaySnapshot,
    challenges: &[AcmeChallengeRecord],
    certificates: &[CertificateRecord],
) -> GatewaySnapshot {
    snapshot.acme_challenges = project_acme_challenges(challenges);
    snapshot.certificates = project_certificates(certificates);
    snapshot
}

#[must_use]
pub fn project_acme_challenges(
    challenges: &[AcmeChallengeRecord],
) -> HashMap<(String, String), AcmeChallengeView> {
    challenges
        .iter()
        .map(|challenge| {
            let hostname = normalize_request_host(&challenge.hostname);
            (
                (hostname.clone(), challenge.token.clone()),
                AcmeChallengeView {
                    hostname,
                    token: challenge.token.clone(),
                    key_authorization: challenge.key_authorization.clone(),
                },
            )
        })
        .collect()
}

#[must_use]
pub fn project_certificates(
    certificates: &[CertificateRecord],
) -> HashMap<String, CertificateView> {
    certificates
        .iter()
        .filter_map(|record| {
            let version = record.installed_version()?;
            let hostname = normalize_request_host(&record.hostname);
            let tls = parse_tls_material(&hostname, version)?;
            Some((
                hostname.clone(),
                CertificateView {
                    hostname,
                    tls: Arc::new(tls),
                },
            ))
        })
        .collect()
}

/// Parse a record's PEM material into ready-to-install X509 + chain + private
/// key. Returns `None` and logs a warning for malformed records so they never
/// reach the handshake-path callback. PEM parsing happens here (cold path) and
/// is intentionally synchronous.
fn parse_tls_material(
    hostname: &str,
    version: &ployz_types::model::CertificateVersion,
) -> Option<ProjectedTlsMaterial> {
    let stack = match X509::stack_from_pem(version.fullchain_pem.as_bytes()) {
        Ok(stack) => stack,
        Err(error) => {
            warn!(
                hostname,
                reason = %error,
                "managed TLS fullchain PEM did not parse; dropping certificate from snapshot"
            );
            return None;
        }
    };
    let mut iter = stack.into_iter();
    let Some(leaf) = iter.next() else {
        warn!(
            hostname,
            "managed TLS fullchain PEM contained no leaf certificate; dropping from snapshot"
        );
        return None;
    };
    let chain: Arc<[X509]> = iter.collect();
    let private_key = match PKey::private_key_from_pem(version.private_key_pem.as_bytes()) {
        Ok(key) => key,
        Err(error) => {
            warn!(
                hostname,
                reason = %error,
                "managed TLS private key PEM did not parse; dropping certificate from snapshot"
            );
            return None;
        }
    };
    Some(ProjectedTlsMaterial {
        leaf,
        chain,
        private_key,
    })
}

#[must_use]
pub fn match_acme_challenge<'a>(
    snapshot: &'a GatewaySnapshot,
    host: Option<&str>,
    path: &str,
) -> Option<&'a AcmeChallengeView> {
    let host = host
        .map(normalize_request_host)
        .filter(|value| !value.is_empty())?;
    let token = path.strip_prefix("/.well-known/acme-challenge/")?;
    snapshot.acme_challenges.get(&(host, token.to_string()))
}

fn routable_backends_by_port(
    spec: &ServiceSpec,
    namespace: &Namespace,
    service: &str,
    allowed_revision_hashes: &HashSet<String>,
    slots: &[ServiceReleaseSlot],
    instances: &HashMap<InstanceId, InstanceStatusRecord>,
    machines: &HashMap<MachineId, MachineMembership>,
) -> Result<BTreeMap<String, Vec<BackendView>>, ProjectionError> {
    let service_ports = spec
        .service_ports
        .iter()
        .map(|port| (port.name.clone(), port.clone()))
        .collect::<HashMap<_, _>>();
    let mut backends = BTreeMap::new();
    for slot in slots {
        let Some(instance) = instances.get(&slot.active_instance_id) else {
            continue;
        };
        if !is_routable_instance(instance, slot, namespace, service, allowed_revision_hashes) {
            continue;
        }
        let Some(overlay_ip) = instance.overlay_ip else {
            continue;
        };
        let Some(machine) = machines.get(&instance.machine_id) else {
            return Err(ProjectionError::MissingMachineForInstance {
                namespace: namespace.clone(),
                service: service.to_string(),
                instance_id: instance.instance_id.clone(),
                machine_id: instance.machine_id.clone(),
            });
        };
        for port_name in service_ports.keys() {
            let Some(port_number) = instance.backend_ports.get(port_name) else {
                continue;
            };
            backends
                .entry(port_name.clone())
                .or_insert_with(Vec::new)
                .push(BackendView {
                    instance_id: instance.instance_id.clone(),
                    machine_id: instance.machine_id.clone(),
                    topology: machine.topology.clone(),
                    service_port: port_name.clone(),
                    address: SocketAddr::V4(SocketAddrV4::new(overlay_ip, *port_number)),
                });
        }
    }
    for values in backends.values_mut() {
        values.sort_by_key(|backend| {
            (
                backend.machine_id.0.clone(),
                backend.instance_id.0.clone(),
                backend.address,
            )
        });
    }
    Ok(backends)
}

fn is_routable_instance(
    instance: &InstanceStatusRecord,
    slot: &ServiceReleaseSlot,
    namespace: &Namespace,
    service: &str,
    allowed_revision_hashes: &HashSet<String>,
) -> bool {
    instance.namespace == *namespace
        && instance.service == service
        && instance.slot_id == slot.slot_id
        && instance.machine_id == slot.machine_id
        && instance.revision_hash == slot.revision_hash
        && allowed_revision_hashes.contains(&instance.revision_hash)
        && instance.ready
        && instance.phase == InstancePhase::Ready
        && instance.drain_state == ployz_types::model::DrainState::None
        && instance.error.is_none()
}

fn routing_revision_hash(release: &ServiceRelease) -> String {
    match &release.routing {
        ServiceRoutingPolicy::Direct { revision_hash } => revision_hash.clone(),
        ServiceRoutingPolicy::Split { .. } => release.primary_revision_hash.clone(),
    }
}

fn allowed_revision_hashes(release: &ServiceRelease) -> HashSet<String> {
    match &release.routing {
        ServiceRoutingPolicy::Direct { revision_hash } => HashSet::from([revision_hash.clone()]),
        ServiceRoutingPolicy::Split { allocations } => {
            let hashes = allocations
                .iter()
                .map(|allocation| allocation.revision_hash.clone())
                .collect::<HashSet<_>>();
            if hashes.is_empty() {
                release.referenced_revision_hashes.iter().cloned().collect()
            } else {
                hashes
            }
        }
    }
}

fn conflict_hosts(route: &HttpRouteView) -> Vec<String> {
    if route.hostnames.is_empty() {
        vec!["*".to_string()]
    } else {
        route.hostnames.clone()
    }
}

#[cfg(test)]
#[allow(dead_code)]
fn validate_http_conflicts<'a>(
    routes: impl IntoIterator<Item = &'a HttpRouteView>,
) -> Result<(), ProjectionError> {
    let mut seen = HashMap::new();
    for route in routes {
        #[cfg(test)]
        HTTP_CONFLICT_VALIDATION_VISITS.fetch_add(1, Ordering::Relaxed);
        for host in conflict_hosts(route) {
            let path_prefix = route.path_prefix.clone();
            let key = (host.clone(), path_prefix.clone());
            if let Some(existing) = seen.insert(key, route.route_id.clone()) {
                return Err(ProjectionError::HttpRouteConflict {
                    left: existing.to_string(),
                    right: route.route_id.to_string(),
                    host,
                    path_prefix,
                });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
#[allow(dead_code)]
fn validate_tcp_conflicts<'a>(
    routes: impl IntoIterator<Item = &'a TcpRouteView>,
) -> Result<(), ProjectionError> {
    let mut seen = HashMap::new();
    for route in routes {
        #[cfg(test)]
        TCP_CONFLICT_VALIDATION_VISITS.fetch_add(1, Ordering::Relaxed);
        if let Some(existing) = seen.insert(route.listen_port, route.route_id.clone()) {
            return Err(ProjectionError::TcpRouteConflict {
                left: existing.to_string(),
                right: route.route_id.to_string(),
                listen_port: route.listen_port,
            });
        }
    }
    Ok(())
}

fn route_matches_host(route: &HttpRouteView, host: Option<&str>) -> bool {
    if route.hostnames.is_empty() {
        return true;
    }
    let Some(host) = host else {
        return false;
    };
    route.hostnames.iter().any(|candidate| candidate == host)
}

#[cfg(test)]
fn reset_conflict_validation_visits() {
    HTTP_CONFLICT_VALIDATION_VISITS.store(0, Ordering::Relaxed);
    TCP_CONFLICT_VALIDATION_VISITS.store(0, Ordering::Relaxed);
}

#[cfg(test)]
fn http_conflict_validation_visits() -> usize {
    HTTP_CONFLICT_VALIDATION_VISITS.load(Ordering::Relaxed)
}

pub(crate) fn normalize_path_prefix(path_prefix: &str) -> String {
    let trimmed = path_prefix.trim();
    if trimmed.is_empty() {
        return "/".into();
    }
    if trimmed.starts_with('/') {
        trimmed.to_string()
    } else {
        format!("/{trimmed}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_types::model::{
        CertificateState, DeployId, DrainState, InstanceStatusRecord, MachineLifecycle, OverlayIp,
        PublicKey, ServiceRelease, ServiceReleaseRecord, ServiceReleaseSlot, ServiceRevisionRecord,
        ServiceRoutingPolicy, SlotId, StorageParticipation,
    };
    use ployz_types::spec::{
        ContainerSpec, NetworkMode, Placement, PortProtocol, PullPolicy, Resources, RestartPolicy,
        RouteSpec, ServicePort, ServiceSpec,
    };
    use std::net::Ipv4Addr;

    #[test]
    fn project_only_routes_release_ready_instances() {
        let namespace = Namespace("prod".into());
        let old = service_spec(&namespace, "api", "v1", vec!["old.example.com".into()]);
        let current = service_spec(&namespace, "api", "v2", vec!["api.example.com".into()]);

        let snapshot = project(RoutingState {
            machines: vec![machine_record("machine-a")],
            revisions: vec![revision_record(&old), revision_record(&current)],
            releases: vec![release_record(
                &namespace,
                "api",
                &current.revision_hash().expect("revision hash"),
                vec![
                    slot_record("slot-1", "inst-ready", &current),
                    slot_record("slot-2", "inst-draining", &current),
                ],
            )],
            instances: vec![
                instance_record(
                    &namespace,
                    "api",
                    "slot-1",
                    "inst-ready",
                    true,
                    DrainState::None,
                    &current,
                ),
                instance_record(
                    &namespace,
                    "api",
                    "slot-2",
                    "inst-draining",
                    true,
                    DrainState::Requested,
                    &current,
                ),
            ],
        })
        .expect("projection succeeds");

        let [route] = snapshot.http_routes.as_slice() else {
            panic!("expected one http route");
        };
        assert_eq!(route.hostnames, vec!["api.example.com".to_string()]);
        let [backend] = route.backends.as_slice() else {
            panic!("expected one backend");
        };
        assert_eq!(backend.instance_id.0, "inst-ready");
    }

    #[test]
    fn split_release_includes_backends_from_multiple_revisions() {
        let namespace = Namespace("prod".into());
        let stable = service_spec(&namespace, "api", "v1", vec!["api.example.com".into()]);
        let canary = service_spec(&namespace, "api", "v2", vec!["api.example.com".into()]);
        let stable_hash = stable.revision_hash().expect("stable revision hash");
        let canary_hash = canary.revision_hash().expect("canary revision hash");

        let snapshot = project(RoutingState {
            machines: vec![machine_record("machine-a")],
            revisions: vec![revision_record(&stable), revision_record(&canary)],
            releases: vec![ServiceReleaseRecord {
                namespace: namespace.clone(),
                service: String::from("api"),
                release: ServiceRelease {
                    primary_revision_hash: stable_hash.clone(),
                    referenced_revision_hashes: vec![stable_hash.clone(), canary_hash.clone()],
                    routing: ServiceRoutingPolicy::Split {
                        allocations: vec![
                            ployz_types::model::ServiceTrafficAllocation {
                                revision_hash: stable_hash.clone(),
                                percent: 90,
                                label: Some(String::from("stable")),
                            },
                            ployz_types::model::ServiceTrafficAllocation {
                                revision_hash: canary_hash.clone(),
                                percent: 10,
                                label: Some(String::from("canary")),
                            },
                        ],
                    },
                    slots: vec![
                        slot_record("slot-stable", "inst-stable", &stable),
                        slot_record("slot-canary", "inst-canary", &canary),
                    ],
                    updated_by_deploy_id: DeployId(String::from("dep-1")),
                    updated_at: 1,
                },
            }],
            instances: vec![
                instance_record(
                    &namespace,
                    "api",
                    "slot-stable",
                    "inst-stable",
                    true,
                    DrainState::None,
                    &stable,
                ),
                instance_record(
                    &namespace,
                    "api",
                    "slot-canary",
                    "inst-canary",
                    true,
                    DrainState::None,
                    &canary,
                ),
            ],
        })
        .expect("projection succeeds");

        let [route] = snapshot.http_routes.as_slice() else {
            panic!("expected one http route");
        };
        assert_eq!(route.backends.len(), 2);
    }

    #[test]
    fn specific_host_beats_wildcard_and_longer_path_beats_shorter() {
        let namespace = Namespace("prod".into());
        let specific_route_id =
            RouteId::http(&ServiceKey::new(namespace.clone(), "specific".into()), 0);
        let snapshot = GatewaySnapshot {
            http_routes: vec![
                HttpRouteView {
                    route_id: specific_route_id.clone(),
                    namespace: namespace.clone(),
                    service: "specific".into(),
                    revision_hash: "r2".into(),
                    hostnames: vec!["api.example.com".into()],
                    path_prefix: "/v1".into(),
                    backends: vec![backend("specific")],
                },
                HttpRouteView {
                    route_id: RouteId::http(&ServiceKey::new(namespace.clone(), "wild".into()), 0),
                    namespace,
                    service: "wild".into(),
                    revision_hash: "r1".into(),
                    hostnames: Vec::new(),
                    path_prefix: "/".into(),
                    backends: vec![backend("wild")],
                },
            ],
            tcp_routes: Vec::new(),
            acme_challenges: HashMap::new(),
            certificates: HashMap::new(),
        };

        let route = match_http_route(&snapshot, Some("api.example.com"), "/v1/users")
            .expect("matched route");
        assert_eq!(route.route_id, specific_route_id);
    }

    #[test]
    fn duplicate_http_host_and_path_is_rejected() {
        let namespace = Namespace("prod".into());
        let left = service_spec(&namespace, "one", "v1", vec!["api.example.com".into()]);
        let right = service_spec(&namespace, "two", "v1", vec!["api.example.com".into()]);

        let error = project(RoutingState {
            machines: Vec::new(),
            revisions: vec![revision_record(&left), revision_record(&right)],
            releases: vec![
                release_record(
                    &namespace,
                    "one",
                    &left.revision_hash().expect("revision hash"),
                    Vec::new(),
                ),
                release_record(
                    &namespace,
                    "two",
                    &right.revision_hash().expect("revision hash"),
                    Vec::new(),
                ),
            ],
            instances: Vec::new(),
        })
        .expect_err("conflict expected");

        match error {
            ProjectionError::HttpRouteConflict {
                host, path_prefix, ..
            } => {
                assert_eq!(host, "api.example.com");
                assert_eq!(path_prefix, "/");
            }
            ProjectionError::MissingRevision { .. }
            | ProjectionError::InvalidRevisionSpec { .. }
            | ProjectionError::TcpRouteConflict { .. }
            | ProjectionError::MissingMachineForInstance { .. } => panic!("unexpected error"),
        }
    }

    #[test]
    fn tcp_routes_are_projected_with_no_serving_dependency() {
        let namespace = Namespace("prod".into());
        let mut spec = service_spec(&namespace, "db", "v1", Vec::new());
        spec.routes = vec![RouteSpec::Tcp(ployz_types::spec::TcpRoute {
            service_port: "sql".into(),
            listen_port: 5432,
        })];
        spec.service_ports = vec![ServicePort {
            name: "sql".into(),
            container_port: 5432,
            protocol: PortProtocol::Tcp,
        }];

        let snapshot = project(RoutingState {
            machines: vec![machine_record("machine-a")],
            revisions: vec![revision_record(&spec)],
            releases: vec![release_record(
                &namespace,
                "db",
                &spec.revision_hash().expect("revision hash"),
                vec![slot_record("slot-1", "inst-db", &spec)],
            )],
            instances: vec![instance_record(
                &namespace,
                "db",
                "slot-1",
                "inst-db",
                true,
                DrainState::None,
                &spec,
            )],
        })
        .expect("projection succeeds");

        assert!(snapshot.http_routes.is_empty());
        let [route] = snapshot.tcp_routes.as_slice() else {
            panic!("expected one tcp route");
        };
        assert_eq!(route.listen_port, 5432);
    }

    #[test]
    fn projected_backend_includes_machine_topology() {
        let namespace = Namespace("prod".into());
        let spec = service_spec(&namespace, "api", "v1", vec!["api.example.com".into()]);
        let machine = MachineMembership {
            topology: MachineTopology::new("us-east", Some("use1-a"))
                .expect("topology should parse"),
            ..machine_record("machine-a")
        };

        let snapshot = project(RoutingState {
            machines: vec![machine.clone()],
            revisions: vec![revision_record(&spec)],
            releases: vec![release_record(
                &namespace,
                "api",
                &spec.revision_hash().expect("revision hash"),
                vec![slot_record("slot-1", "inst-ready", &spec)],
            )],
            instances: vec![instance_record(
                &namespace,
                "api",
                "slot-1",
                "inst-ready",
                true,
                DrainState::None,
                &spec,
            )],
        })
        .expect("projection succeeds");

        let [route] = snapshot.http_routes.as_slice() else {
            panic!("expected one http route");
        };
        let [backend] = route.backends.as_slice() else {
            panic!("expected one backend");
        };
        assert_eq!(backend.topology, machine.topology);
    }

    #[test]
    fn projection_fails_when_routable_instance_references_missing_machine() {
        let namespace = Namespace("prod".into());
        let spec = service_spec(&namespace, "api", "v1", vec!["api.example.com".into()]);

        let error = project(RoutingState {
            machines: Vec::new(),
            revisions: vec![revision_record(&spec)],
            releases: vec![release_record(
                &namespace,
                "api",
                &spec.revision_hash().expect("revision hash"),
                vec![slot_record("slot-1", "inst-ready", &spec)],
            )],
            instances: vec![instance_record(
                &namespace,
                "api",
                "slot-1",
                "inst-ready",
                true,
                DrainState::None,
                &spec,
            )],
        })
        .expect_err("missing machine should fail projection");

        match error {
            ProjectionError::MissingMachineForInstance {
                instance_id,
                machine_id,
                ..
            } => {
                assert_eq!(instance_id.0, "inst-ready");
                assert_eq!(machine_id.0, "machine-a");
            }
            ProjectionError::MissingRevision { .. }
            | ProjectionError::InvalidRevisionSpec { .. }
            | ProjectionError::HttpRouteConflict { .. }
            | ProjectionError::TcpRouteConflict { .. } => panic!("unexpected error"),
        }
    }

    #[test]
    fn route_id_and_hostname_keys_are_stable_and_normalized() {
        let service = ServiceKey::new(Namespace("prod".into()), "api".into());
        assert_eq!(RouteId::http(&service, 2).as_str(), "http:prod:api:2");
        assert_eq!(RouteId::tcp(&service, 1).as_str(), "tcp:prod:api:1");
        assert_eq!(
            NormalizedHostname::parse("API.Example.Com.")
                .expect("hostname")
                .into_string(),
            "api.example.com"
        );
        assert_eq!(
            NormalizedHostname::parse("API.Example.Com:443")
                .expect("hostname")
                .into_string(),
            "api.example.com"
        );
        assert!(NormalizedHostname::parse("   .").is_none());
    }

    #[test]
    fn instance_event_reprojects_only_the_affected_service() {
        let namespace = Namespace("prod".into());
        let api = service_spec(&namespace, "api", "v1", vec!["api.example.com".into()]);
        let web = service_spec(&namespace, "web", "v1", vec!["web.example.com".into()]);
        let api_service = ServiceKey::new(namespace.clone(), "api".into());
        let web_service = ServiceKey::new(namespace.clone(), "web".into());
        let old_api_instance = instance_record(
            &namespace,
            "api",
            "slot-api",
            "inst-api",
            true,
            DrainState::None,
            &api,
        );
        let mut new_api_instance = old_api_instance.clone();
        new_api_instance
            .backend_ports
            .insert(String::from("http"), 8081);

        let mut projector = GatewayProjector::new(RoutingState {
            machines: vec![machine_record("machine-a")],
            revisions: vec![revision_record(&api), revision_record(&web)],
            releases: vec![
                release_record(
                    &namespace,
                    "api",
                    &api.revision_hash().expect("api revision hash"),
                    vec![slot_record("slot-api", "inst-api", &api)],
                ),
                release_record(
                    &namespace,
                    "web",
                    &web.revision_hash().expect("web revision hash"),
                    vec![slot_record("slot-web", "inst-web", &web)],
                ),
            ],
            instances: vec![
                old_api_instance.clone(),
                instance_record(
                    &namespace,
                    "web",
                    "slot-web",
                    "inst-web",
                    true,
                    DrainState::None,
                    &web,
                ),
            ],
        })
        .expect("initial projection");

        assert_eq!(projector.project_count(&api_service), 1);
        assert_eq!(projector.project_count(&web_service), 1);

        projector
            .apply(GatewayProjectionEvent::Routing(
                RoutingEvent::InstanceUpsert(new_api_instance),
            ))
            .expect("apply instance update");

        assert_eq!(projector.project_count(&api_service), 2);
        assert_eq!(projector.project_count(&web_service), 1);
    }

    #[test]
    fn thousand_route_release_update_touches_one_route() {
        let namespace = Namespace("prod".into());
        let mut revisions = Vec::new();
        let mut releases = Vec::new();
        let mut service_keys = Vec::new();
        for index in 0..1_000 {
            let service = format!("svc-{index}");
            let spec = service_spec(
                &namespace,
                &service,
                "v1",
                vec![format!("{service}.example.com")],
            );
            let revision_hash = spec.revision_hash().expect("revision hash");
            revisions.push(revision_record(&spec));
            releases.push(release_record(
                &namespace,
                &service,
                &revision_hash,
                Vec::new(),
            ));
            service_keys.push(ServiceKey::new(namespace.clone(), service));
        }

        let mut projector = GatewayProjector::new(RoutingState {
            machines: Vec::new(),
            revisions,
            releases: releases.clone(),
            instances: Vec::new(),
        })
        .expect("initial projection");

        let target = 421;
        let target_service = service_keys[target].clone();
        let neighbor_service = service_keys[target + 1].clone();
        let delta = projector
            .apply(GatewayProjectionEvent::Routing(
                RoutingEvent::ReleaseUpsert(releases[target].clone()),
            ))
            .expect("release update should apply");

        assert_eq!(projector.project_count(&target_service), 2);
        assert_eq!(projector.project_count(&neighbor_service), 1);
        match delta {
            ProjectionDelta::RoutesChanged {
                removed_route_ids,
                upserted_http,
                upserted_tcp,
            } => {
                assert_eq!(removed_route_ids.len(), 1);
                assert_eq!(upserted_http.len(), 1);
                assert!(upserted_tcp.is_empty());
            }
            ProjectionDelta::CertificateChanged { .. }
            | ProjectionDelta::ChallengeChanged { .. }
            | ProjectionDelta::Empty => panic!("expected route delta"),
        }
    }

    #[test]
    fn single_route_update_does_not_run_global_http_validation() {
        let namespace = Namespace("prod".into());
        let mut revisions = Vec::new();
        let mut releases = Vec::new();
        for index in 0..1_000 {
            let service = format!("svc-{index}");
            let spec = service_spec(
                &namespace,
                &service,
                "v1",
                vec![format!("{service}.example.com")],
            );
            let revision_hash = spec.revision_hash().expect("revision hash");
            revisions.push(revision_record(&spec));
            releases.push(release_record(
                &namespace,
                &service,
                &revision_hash,
                Vec::new(),
            ));
        }

        let mut projector = GatewayProjector::new(RoutingState {
            machines: Vec::new(),
            revisions,
            releases: releases.clone(),
            instances: Vec::new(),
        })
        .expect("initial projection");

        reset_conflict_validation_visits();
        projector
            .apply(GatewayProjectionEvent::Routing(
                RoutingEvent::ReleaseUpsert(releases[421].clone()),
            ))
            .expect("release update should apply");

        let visits = http_conflict_validation_visits();
        assert_eq!(visits, 0, "single route update ran global validation");
    }

    #[test]
    fn failed_reprojection_restores_existing_routes() {
        let namespace = Namespace("prod".into());
        let spec = service_spec(&namespace, "api", "v1", vec!["api.example.com".into()]);
        let mut projector = GatewayProjector::new(RoutingState {
            machines: vec![machine_record("machine-a")],
            revisions: vec![revision_record(&spec)],
            releases: vec![release_record(
                &namespace,
                "api",
                &spec.revision_hash().expect("revision hash"),
                vec![slot_record("slot-1", "inst-ready", &spec)],
            )],
            instances: vec![instance_record(
                &namespace,
                "api",
                "slot-1",
                "inst-ready",
                true,
                DrainState::None,
                &spec,
            )],
        })
        .expect("initial projection");
        let before = projector.snapshot_value();
        let missing_release = release_record(&namespace, "api", "missing-rev", Vec::new());

        let error = projector
            .apply(GatewayProjectionEvent::Routing(
                RoutingEvent::ReleaseUpsert(missing_release),
            ))
            .expect_err("missing revision should fail");
        assert!(matches!(error, ProjectionError::MissingRevision { .. }));

        assert_eq!(
            projector.snapshot_value().http_routes,
            before.http_routes,
            "failed reprojection must restore previously published routes"
        );
        assert_eq!(projector.snapshot_value().tcp_routes, before.tcp_routes);
    }

    #[test]
    fn release_removed_emits_route_removal_delta() {
        let namespace = Namespace("prod".into());
        let spec = service_spec(&namespace, "api", "v1", vec!["api.example.com".into()]);
        let release = release_record(
            &namespace,
            "api",
            &spec.revision_hash().expect("revision hash"),
            Vec::new(),
        );
        let mut projector = GatewayProjector::new(RoutingState {
            machines: Vec::new(),
            revisions: vec![revision_record(&spec)],
            releases: vec![release.clone()],
            instances: Vec::new(),
        })
        .expect("initial projection");

        let delta = projector
            .apply(GatewayProjectionEvent::Routing(
                RoutingEvent::ReleaseRemoved {
                    namespace: release.namespace,
                    service: release.service,
                },
            ))
            .expect("release removal should apply");

        assert!(projector.snapshot_value().http_routes.is_empty());
        match delta {
            ProjectionDelta::RoutesChanged {
                removed_route_ids,
                upserted_http,
                upserted_tcp,
            } => {
                assert_eq!(removed_route_ids.len(), 1);
                assert!(upserted_http.is_empty());
                assert!(upserted_tcp.is_empty());
            }
            ProjectionDelta::CertificateChanged { .. }
            | ProjectionDelta::ChallengeChanged { .. }
            | ProjectionDelta::Empty => panic!("expected route removal delta"),
        }
    }

    fn service_spec(
        _namespace: &Namespace,
        service: &str,
        image_tag: &str,
        hostnames: Vec<String>,
    ) -> ServiceSpec {
        ServiceSpec {
            name: service.into(),
            placement: Placement::Replicated { count: 1 },
            template: ContainerSpec {
                image: format!("example:{image_tag}"),
                command: None,
                entrypoint: None,
                env: BTreeMap::new(),
                mounts: Vec::new(),
                cap_add: Vec::new(),
                cap_drop: Vec::new(),
                privileged: false,
                user: None,
                stop_grace_period: None,
                pid_mode: None,
                pull_policy: PullPolicy::IfNotPresent,
                resources: Resources::empty(),
                sysctls: BTreeMap::new(),
            },
            network: NetworkMode::Overlay,
            service_ports: vec![ServicePort {
                name: "http".into(),
                container_port: 8080,
                protocol: PortProtocol::Tcp,
            }],
            publish: Vec::new(),
            routes: vec![RouteSpec::Http(ployz_types::spec::HttpRoute {
                service_port: "http".into(),
                hostnames,
                path_prefix: "/".into(),
            })],
            readiness: None,
            rollout: ployz_types::spec::RolloutStrategy::Recreate,
            labels: BTreeMap::new(),
            restart: RestartPolicy::UnlessStopped,
        }
    }

    fn revision_record(spec: &ServiceSpec) -> ServiceRevisionRecord {
        ServiceRevisionRecord {
            namespace: Namespace("prod".into()),
            service: spec.name.clone(),
            revision_hash: spec.revision_hash().expect("revision hash"),
            spec_json: spec
                .canonical_revision_json()
                .expect("canonical revision json"),
            created_by: MachineId("founder".into()),
            created_at: 1,
        }
    }

    fn release_record(
        namespace: &Namespace,
        service: &str,
        revision_hash: &str,
        slots: Vec<ServiceReleaseSlot>,
    ) -> ServiceReleaseRecord {
        ServiceReleaseRecord {
            namespace: namespace.clone(),
            service: service.into(),
            release: ServiceRelease {
                primary_revision_hash: revision_hash.into(),
                referenced_revision_hashes: vec![revision_hash.into()],
                routing: ServiceRoutingPolicy::Direct {
                    revision_hash: revision_hash.into(),
                },
                slots,
                updated_by_deploy_id: DeployId("dep-1".into()),
                updated_at: 1,
            },
        }
    }

    fn slot_record(slot_id: &str, instance_id: &str, spec: &ServiceSpec) -> ServiceReleaseSlot {
        ServiceReleaseSlot {
            slot_id: SlotId(slot_id.into()),
            machine_id: MachineId("machine-a".into()),
            active_instance_id: InstanceId(instance_id.into()),
            revision_hash: spec.revision_hash().expect("revision hash"),
        }
    }

    fn instance_record(
        namespace: &Namespace,
        service: &str,
        slot_id: &str,
        instance_id: &str,
        ready: bool,
        drain_state: DrainState,
        spec: &ServiceSpec,
    ) -> InstanceStatusRecord {
        InstanceStatusRecord {
            instance_id: InstanceId(instance_id.into()),
            namespace: namespace.clone(),
            service: service.into(),
            slot_id: SlotId(slot_id.into()),
            machine_id: MachineId("machine-a".into()),
            revision_hash: spec.revision_hash().expect("revision hash"),
            deploy_id: DeployId("dep-1".into()),
            docker_container_id: "container".into(),
            overlay_ip: Some(Ipv4Addr::new(10, 0, 0, 2)),
            backend_ports: BTreeMap::from([
                (String::from("http"), 8080),
                (String::from("sql"), 5432),
            ]),
            phase: if ready {
                InstancePhase::Ready
            } else {
                InstancePhase::Starting
            },
            ready,
            drain_state,
            error: None,
            started_at: 1,
            updated_at: 1,
        }
    }

    fn machine_record(id: &str) -> MachineMembership {
        MachineMembership {
            id: MachineId(id.into()),
            public_key: PublicKey([0; 32]),
            overlay_ip: OverlayIp("fd00::1".parse().expect("valid overlay ip")),
            topology: MachineTopology::local(),
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: MachineLifecycle::Active,
            storage: true,
            storage_participation: StorageParticipation::default_authority(),
            created_at: 1,
            updated_at: 1,
            labels: BTreeMap::new(),
        }
    }

    fn backend(id: &str) -> BackendView {
        BackendView {
            instance_id: InstanceId(id.into()),
            machine_id: MachineId("machine-a".into()),
            topology: MachineTopology::local(),
            service_port: "http".into(),
            address: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 2), 8080)),
        }
    }

    // ---------------------------------------------------------------------
    // with_managed_tls + match_acme_challenge
    // ---------------------------------------------------------------------

    use ployz_types::model::CertificateVersion;

    /// Generate a self-signed PEM bundle for tests. project_certificates parses
    /// PEM at projection time and drops malformed records, so test fixtures
    /// must produce real PEM.
    fn self_signed_pem(hostname: &str) -> (String, String) {
        let key = rcgen::KeyPair::generate().expect("keypair");
        let params = rcgen::CertificateParams::new(vec![hostname.into()]).expect("cert params");
        let cert = params.self_signed(&key).expect("self-signed cert");
        (cert.pem(), key.serialize_pem())
    }

    fn active_cert(hostname: &str, version_id: &str) -> CertificateRecord {
        let (fullchain_pem, private_key_pem) = self_signed_pem(hostname);
        CertificateRecord {
            hostname: hostname.into(),
            issuer_url: "https://acme.test/directory".into(),
            account_id: "acct-test".into(),
            state: CertificateState::Active,
            active_version_id: Some(version_id.into()),
            versions: vec![CertificateVersion {
                version_id: version_id.into(),
                fullchain_pem,
                private_key_pem,
                not_before: Some(0),
                not_after: Some(100),
                issued_at: 0,
            }],
            order_url: None,
            last_error: None,
            requested_at: 0,
            updated_at: 0,
            next_renewal_at: None,
        }
    }

    fn challenge(hostname: &str, token: &str) -> AcmeChallengeRecord {
        AcmeChallengeRecord {
            hostname: hostname.into(),
            token: token.into(),
            key_authorization: format!("{token}.keyauth"),
            expires_at: 100,
            created_at: 0,
        }
    }

    #[test]
    fn tls_events_do_not_reproject_routes_and_challenge_events_reuse_cert_material() {
        let namespace = Namespace("prod".into());
        let spec = service_spec(&namespace, "api", "v1", vec!["api.example.com".into()]);
        let service = ServiceKey::new(namespace.clone(), "api".into());
        let mut projector = GatewayProjector::new(RoutingState {
            machines: vec![machine_record("machine-a")],
            revisions: vec![revision_record(&spec)],
            releases: vec![release_record(
                &namespace,
                "api",
                &spec.revision_hash().expect("revision hash"),
                vec![slot_record("slot-1", "inst-ready", &spec)],
            )],
            instances: vec![instance_record(
                &namespace,
                "api",
                "slot-1",
                "inst-ready",
                true,
                DrainState::None,
                &spec,
            )],
        })
        .expect("initial projection");
        assert_eq!(projector.project_count(&service), 1);

        projector
            .apply(GatewayProjectionEvent::Certificate(
                CertificateEvent::Upsert(active_cert("api.example.com", "v1")),
            ))
            .expect("certificate projection");
        assert_eq!(
            projector.project_count(&service),
            1,
            "certificate events must not route-project services"
        );
        let before = projector
            .certificates
            .get("api.example.com")
            .expect("cert projected")
            .tls
            .clone();

        projector
            .apply(GatewayProjectionEvent::AcmeChallenge(
                AcmeChallengeEvent::Upsert(challenge("api.example.com", "tok-1")),
            ))
            .expect("challenge projection");
        assert_eq!(
            projector.project_count(&service),
            1,
            "challenge events must not route-project services"
        );
        let after = projector
            .certificates
            .get("api.example.com")
            .expect("cert still projected")
            .tls
            .clone();
        assert!(
            Arc::ptr_eq(&before, &after),
            "challenge events must reuse unchanged parsed TLS material"
        );
    }

    #[test]
    fn with_managed_tls_projects_active_cert_into_hostname_keyed_map() {
        let snapshot = with_managed_tls(
            GatewaySnapshot::empty(),
            &[],
            &[active_cert("api.example.com", "v1")],
        );
        let entry = snapshot
            .certificates
            .get("api.example.com")
            .expect("certificate present under normalized hostname");
        assert_eq!(entry.hostname, "api.example.com");
        // Self-signed fullchain has only the leaf — chain must be empty —
        // and the parsed private key must match the leaf's public key.
        assert!(entry.tls.chain.is_empty());
        assert!(
            entry
                .tls
                .leaf
                .public_key()
                .expect("leaf pubkey")
                .public_eq(&entry.tls.private_key)
        );
    }

    #[test]
    fn with_managed_tls_serves_certs_with_installed_version_regardless_of_state() {
        // Whether a cert is projected is governed by `installed_version()`,
        // not by `state`. Renewal flips a healthy row to RenewalDue→Issuing
        // (and possibly Failed on a non-retryable finalize) while keeping
        // `active_version_id` pointing at the previous valid leaf, so all of
        // those states must remain serviceable. Only `Pending` with no prior
        // issuance — i.e. `active_version_id == None` — should drop out.
        let mut pending_no_version = active_cert("pending.example.com", "v1");
        pending_no_version.state = CertificateState::Pending;
        pending_no_version.active_version_id = None;
        pending_no_version.versions.clear();

        let mut issuing_renewal = active_cert("issuing.example.com", "v1");
        issuing_renewal.state = CertificateState::Issuing;
        let mut renewal_due = active_cert("renewal.example.com", "v1");
        renewal_due.state = CertificateState::RenewalDue;
        let mut failed_with_fallback = active_cert("failed.example.com", "v1");
        failed_with_fallback.state = CertificateState::Failed;

        let snapshot = with_managed_tls(
            GatewaySnapshot::empty(),
            &[],
            &[
                pending_no_version,
                issuing_renewal,
                renewal_due,
                failed_with_fallback,
                active_cert("ok.example.com", "v1"),
            ],
        );
        // pending-without-version is dropped; the other four are served.
        assert_eq!(snapshot.certificates.len(), 4);
        assert!(snapshot.certificates.contains_key("ok.example.com"));
        assert!(snapshot.certificates.contains_key("issuing.example.com"));
        assert!(snapshot.certificates.contains_key("renewal.example.com"));
        assert!(snapshot.certificates.contains_key("failed.example.com"));
        assert!(!snapshot.certificates.contains_key("pending.example.com"));
    }

    #[test]
    fn with_managed_tls_keeps_serving_old_leaf_during_in_flight_renewal() {
        // Concrete renewal scenario: cert was Active with version v1, the
        // ticker promoted it to RenewalDue, start_one moved it to Issuing
        // for the new order, but finalize hasn't completed. The gateway must
        // keep handing out v1 — not blackhole TLS for the duration of the
        // ACME round trip.
        let mut renewing = active_cert("api.example.com", "v1");
        renewing.state = CertificateState::Issuing;
        renewing.order_url = Some("https://acme.test/orders/42".into());

        let snapshot = with_managed_tls(GatewaySnapshot::empty(), &[], &[renewing]);
        let entry = snapshot
            .certificates
            .get("api.example.com")
            .expect("renewing cert should still serve the previous leaf");
        // Material is the previously-issued v1, untouched by the in-flight order.
        assert!(
            entry
                .tls
                .leaf
                .public_key()
                .expect("leaf pubkey")
                .public_eq(&entry.tls.private_key)
        );
    }

    #[test]
    fn with_managed_tls_keeps_serving_old_leaf_after_failed_renewal() {
        // finalize_one's non-retryable error path explicitly restores
        // `previous_active_version_id` before downgrading to Failed, so the
        // gateway can keep using the prior leaf until the next reconcile pass
        // retries. The projection must respect that fallback contract.
        let mut failed = active_cert("api.example.com", "v1");
        failed.state = CertificateState::Failed;
        failed.last_error = Some("orderInvalid: rateLimited".into());

        let snapshot = with_managed_tls(GatewaySnapshot::empty(), &[], &[failed]);
        let entry = snapshot
            .certificates
            .get("api.example.com")
            .expect("failed-with-fallback cert should still serve previous leaf");
        assert!(
            entry
                .tls
                .leaf
                .public_key()
                .expect("leaf pubkey")
                .public_eq(&entry.tls.private_key)
        );
    }

    #[test]
    fn project_certificates_drops_records_with_malformed_fullchain_pem() {
        // PEM envelope present, body is garbage → real parse error. The bad
        // record must be silently dropped from the snapshot (with a warn) so
        // the handshake-path callback never sees it.
        let mut bad_chain = active_cert("bad.example.com", "v1");
        bad_chain.versions[0].fullchain_pem =
            "-----BEGIN CERTIFICATE-----\nnot-valid-base64!!!\n-----END CERTIFICATE-----\n"
                .to_string();
        let good = active_cert("ok.example.com", "v1");
        let snapshot = with_managed_tls(GatewaySnapshot::empty(), &[], &[bad_chain, good]);
        assert_eq!(snapshot.certificates.len(), 1);
        assert!(snapshot.certificates.contains_key("ok.example.com"));
        assert!(!snapshot.certificates.contains_key("bad.example.com"));
    }

    #[test]
    fn project_certificates_drops_records_with_malformed_private_key() {
        let mut bad_key = active_cert("bad.example.com", "v1");
        bad_key.versions[0].private_key_pem = "not a pem".to_string();
        let good = active_cert("ok.example.com", "v1");
        let snapshot = with_managed_tls(GatewaySnapshot::empty(), &[], &[bad_key, good]);
        assert_eq!(snapshot.certificates.len(), 1);
        assert!(snapshot.certificates.contains_key("ok.example.com"));
        assert!(!snapshot.certificates.contains_key("bad.example.com"));
    }

    #[test]
    fn project_certificates_drops_records_with_empty_fullchain_pem() {
        // boring's `X509::stack_from_pem` is permissive and returns an empty
        // vec for content with no PEM envelopes; we treat that the same as
        // malformed and drop the record.
        let mut empty_chain = active_cert("bad.example.com", "v1");
        empty_chain.versions[0].fullchain_pem = "this is not a pem".to_string();
        let snapshot = with_managed_tls(GatewaySnapshot::empty(), &[], &[empty_chain]);
        assert!(snapshot.certificates.is_empty());
    }

    #[test]
    fn with_managed_tls_skips_active_cert_without_active_version_id() {
        let mut record = active_cert("api.example.com", "v1");
        record.active_version_id = None;
        let snapshot = with_managed_tls(GatewaySnapshot::empty(), &[], &[record]);
        assert!(snapshot.certificates.is_empty());
    }

    #[test]
    fn with_managed_tls_skips_when_active_version_id_points_at_missing_version() {
        let mut record = active_cert("api.example.com", "v1");
        record.active_version_id = Some("vmissing".into());
        let snapshot = with_managed_tls(GatewaySnapshot::empty(), &[], &[record]);
        assert!(snapshot.certificates.is_empty());
    }

    #[test]
    fn with_managed_tls_normalizes_cert_hostname_case_and_trailing_dot() {
        let snapshot = with_managed_tls(
            GatewaySnapshot::empty(),
            &[],
            &[active_cert("API.Example.Com.", "v1")],
        );
        assert!(snapshot.certificates.contains_key("api.example.com"));
        assert!(!snapshot.certificates.contains_key("API.Example.Com."));
    }

    #[test]
    fn with_managed_tls_projects_all_challenges_keyed_by_host_and_token() {
        let snapshot = with_managed_tls(
            GatewaySnapshot::empty(),
            &[
                challenge("api.example.com", "tok-a"),
                challenge("api.example.com", "tok-b"),
                challenge("other.example.com", "tok-c"),
            ],
            &[],
        );
        assert_eq!(snapshot.acme_challenges.len(), 3);
        let one = snapshot
            .acme_challenges
            .get(&("api.example.com".into(), "tok-a".into()))
            .expect("first challenge for api.example.com");
        assert_eq!(one.key_authorization, "tok-a.keyauth");
        assert!(
            snapshot
                .acme_challenges
                .contains_key(&("api.example.com".into(), "tok-b".into()))
        );
        assert!(
            snapshot
                .acme_challenges
                .contains_key(&("other.example.com".into(), "tok-c".into()))
        );
    }

    #[test]
    fn with_managed_tls_normalizes_challenge_hostname() {
        let snapshot = with_managed_tls(
            GatewaySnapshot::empty(),
            &[challenge("API.Example.Com.", "tok")],
            &[],
        );
        assert!(
            snapshot
                .acme_challenges
                .contains_key(&("api.example.com".into(), "tok".into()))
        );
    }

    #[test]
    fn match_acme_challenge_returns_view_on_exact_path_and_host() {
        let snapshot = with_managed_tls(
            GatewaySnapshot::empty(),
            &[challenge("api.example.com", "tok-1")],
            &[],
        );
        let hit = match_acme_challenge(
            &snapshot,
            Some("api.example.com"),
            "/.well-known/acme-challenge/tok-1",
        )
        .expect("challenge should match");
        assert_eq!(hit.key_authorization, "tok-1.keyauth");
    }

    #[test]
    fn match_acme_challenge_returns_none_on_unknown_host() {
        let snapshot = with_managed_tls(
            GatewaySnapshot::empty(),
            &[challenge("api.example.com", "tok-1")],
            &[],
        );
        assert!(
            match_acme_challenge(
                &snapshot,
                Some("other.example.com"),
                "/.well-known/acme-challenge/tok-1"
            )
            .is_none()
        );
    }

    #[test]
    fn match_acme_challenge_returns_none_on_wrong_token() {
        let snapshot = with_managed_tls(
            GatewaySnapshot::empty(),
            &[challenge("api.example.com", "tok-1")],
            &[],
        );
        assert!(
            match_acme_challenge(
                &snapshot,
                Some("api.example.com"),
                "/.well-known/acme-challenge/tok-wrong"
            )
            .is_none()
        );
    }

    #[test]
    fn match_acme_challenge_returns_none_without_well_known_prefix() {
        let snapshot = with_managed_tls(
            GatewaySnapshot::empty(),
            &[challenge("api.example.com", "tok-1")],
            &[],
        );
        assert!(match_acme_challenge(&snapshot, Some("api.example.com"), "/tok-1").is_none());
        assert!(match_acme_challenge(&snapshot, Some("api.example.com"), "/").is_none());
    }

    #[test]
    fn match_acme_challenge_requires_host_header() {
        let snapshot = with_managed_tls(
            GatewaySnapshot::empty(),
            &[challenge("api.example.com", "tok-1")],
            &[],
        );
        assert!(
            match_acme_challenge(&snapshot, None, "/.well-known/acme-challenge/tok-1").is_none()
        );
        assert!(
            match_acme_challenge(&snapshot, Some(""), "/.well-known/acme-challenge/tok-1")
                .is_none()
        );
    }

    #[test]
    fn match_acme_challenge_normalizes_host_case_and_port() {
        let snapshot = with_managed_tls(
            GatewaySnapshot::empty(),
            &[challenge("api.example.com", "tok-1")],
            &[],
        );
        let hit = match_acme_challenge(
            &snapshot,
            Some("API.Example.Com:8080"),
            "/.well-known/acme-challenge/tok-1",
        )
        .expect("challenge should match with normalized host");
        assert_eq!(hit.token, "tok-1");
    }
}
