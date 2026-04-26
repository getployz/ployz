use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::net::{SocketAddr, SocketAddrV4};

use ployz_types::model::{
    AcmeChallengeRecord, CertificateRecord, InstanceId, InstancePhase, InstanceStatusRecord,
    MachineId, MachineRecord, MachineTopology, RoutingState, ServiceRelease, ServiceReleaseSlot,
    ServiceRoutingPolicy,
};
use ployz_types::spec::{Namespace, RouteSpec, ServiceSpec};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CertificateView {
    pub hostname: String,
    pub fullchain_pem: String,
    pub private_key_pem: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HttpRouteView {
    pub route_id: String,
    pub namespace: Namespace,
    pub service: String,
    pub revision_hash: String,
    pub hostnames: Vec<String>,
    pub path_prefix: String,
    pub backends: Vec<BackendView>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TcpRouteView {
    pub route_id: String,
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

pub fn project(state: RoutingState) -> Result<GatewaySnapshot, ProjectionError> {
    let revisions = state
        .revisions
        .into_iter()
        .map(|revision| {
            (
                (
                    revision.namespace.clone(),
                    revision.service.clone(),
                    revision.revision_hash.clone(),
                ),
                revision,
            )
        })
        .collect::<HashMap<_, _>>();
    let instances = state
        .instances
        .into_iter()
        .map(|instance| (instance.instance_id.clone(), instance))
        .collect::<HashMap<_, _>>();
    let machines = state
        .machines
        .into_iter()
        .map(|machine| (machine.id.clone(), machine))
        .collect::<HashMap<_, _>>();

    let mut http_routes = Vec::new();
    let mut tcp_routes = Vec::new();
    for release_record in state.releases {
        let routing_revision_hash = routing_revision_hash(&release_record.release);
        let revision_key = (
            release_record.namespace.clone(),
            release_record.service.clone(),
            routing_revision_hash.clone(),
        );
        let Some(revision) = revisions.get(&revision_key) else {
            return Err(ProjectionError::MissingRevision {
                namespace: release_record.namespace,
                service: release_record.service,
                revision_hash: routing_revision_hash,
            });
        };
        let spec: ServiceSpec = serde_json::from_str(&revision.spec_json).map_err(|err| {
            ProjectionError::InvalidRevisionSpec {
                namespace: revision.namespace.clone(),
                service: revision.service.clone(),
                message: err.to_string(),
            }
        })?;

        let backends_by_port = routable_backends_by_port(
            &spec,
            &release_record.namespace,
            &release_record.service,
            &allowed_revision_hashes(&release_record.release),
            &release_record.release.slots,
            &instances,
            &machines,
        )?;

        for (index, route) in spec.routes.iter().enumerate() {
            match route {
                RouteSpec::Http(route) => {
                    let hostnames = route
                        .hostnames
                        .iter()
                        .map(|hostname| normalize_request_host(hostname))
                        .filter(|hostname| !hostname.is_empty())
                        .collect::<BTreeSet<_>>()
                        .into_iter()
                        .collect::<Vec<_>>();
                    http_routes.push(HttpRouteView {
                        route_id: format!("http:{}:{}:{}", revision.namespace, spec.name, index),
                        namespace: revision.namespace.clone(),
                        service: spec.name.clone(),
                        revision_hash: revision.revision_hash.clone(),
                        hostnames,
                        path_prefix: normalize_path_prefix(&route.path_prefix),
                        backends: backends_by_port
                            .get(&route.service_port)
                            .cloned()
                            .unwrap_or_default(),
                    });
                }
                RouteSpec::Tcp(route) => {
                    tcp_routes.push(TcpRouteView {
                        route_id: format!("tcp:{}:{}:{}", revision.namespace, spec.name, index),
                        namespace: revision.namespace.clone(),
                        service: spec.name.clone(),
                        revision_hash: revision.revision_hash.clone(),
                        listen_port: route.listen_port,
                        backends: backends_by_port
                            .get(&route.service_port)
                            .cloned()
                            .unwrap_or_default(),
                    });
                }
            }
        }
    }

    validate_http_conflicts(&http_routes)?;
    validate_tcp_conflicts(&tcp_routes)?;
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

    Ok(GatewaySnapshot {
        http_routes,
        tcp_routes,
        acme_challenges: HashMap::new(),
        certificates: HashMap::new(),
    })
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
            Some((
                hostname.clone(),
                CertificateView {
                    hostname,
                    fullchain_pem: version.fullchain_pem.clone(),
                    private_key_pem: version.private_key_pem.clone(),
                },
            ))
        })
        .collect()
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
    machines: &HashMap<MachineId, MachineRecord>,
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

fn validate_http_conflicts(routes: &[HttpRouteView]) -> Result<(), ProjectionError> {
    let mut seen = HashMap::new();
    for route in routes {
        let hosts = if route.hostnames.is_empty() {
            vec!["*".to_string()]
        } else {
            route.hostnames.clone()
        };
        for host in hosts {
            let path_prefix = route.path_prefix.clone();
            let key = (host.clone(), path_prefix.clone());
            if let Some(existing) = seen.insert(key, route.route_id.clone()) {
                return Err(ProjectionError::HttpRouteConflict {
                    left: existing,
                    right: route.route_id.clone(),
                    host,
                    path_prefix,
                });
            }
        }
    }
    Ok(())
}

fn validate_tcp_conflicts(routes: &[TcpRouteView]) -> Result<(), ProjectionError> {
    let mut seen = HashMap::new();
    for route in routes {
        if let Some(existing) = seen.insert(route.listen_port, route.route_id.clone()) {
            return Err(ProjectionError::TcpRouteConflict {
                left: existing,
                right: route.route_id.clone(),
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

fn normalize_path_prefix(path_prefix: &str) -> String {
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
        ServiceRoutingPolicy, SlotId,
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
        let snapshot = GatewaySnapshot {
            http_routes: vec![
                HttpRouteView {
                    route_id: "specific".into(),
                    namespace: Namespace("prod".into()),
                    service: "specific".into(),
                    revision_hash: "r2".into(),
                    hostnames: vec!["api.example.com".into()],
                    path_prefix: "/v1".into(),
                    backends: vec![backend("specific")],
                },
                HttpRouteView {
                    route_id: "wild".into(),
                    namespace: Namespace("prod".into()),
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
        assert_eq!(route.route_id, "specific");
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
        let machine = MachineRecord {
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
                volumes: Vec::new(),
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

    fn machine_record(id: &str) -> MachineRecord {
        MachineRecord {
            id: MachineId(id.into()),
            public_key: PublicKey([0; 32]),
            overlay_ip: OverlayIp("fd00::1".parse().expect("valid overlay ip")),
            topology: MachineTopology::local(),
            control_target: None,
            subnet: None,
            bridge_ip: None,
            endpoints: Vec::new(),
            lifecycle: MachineLifecycle::Active,
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

    fn active_cert(hostname: &str, version_id: &str) -> CertificateRecord {
        CertificateRecord {
            hostname: hostname.into(),
            issuer_url: "https://acme.test/directory".into(),
            account_id: "acct-test".into(),
            state: CertificateState::Active,
            active_version_id: Some(version_id.into()),
            versions: vec![CertificateVersion {
                version_id: version_id.into(),
                fullchain_pem: format!("fullchain:{hostname}"),
                private_key_pem: format!("key:{hostname}"),
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
        assert_eq!(entry.fullchain_pem, "fullchain:api.example.com");
        assert_eq!(entry.private_key_pem, "key:api.example.com");
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
        assert_eq!(entry.fullchain_pem, "fullchain:api.example.com");
        assert_eq!(entry.private_key_pem, "key:api.example.com");
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
        assert_eq!(entry.fullchain_pem, "fullchain:api.example.com");
        assert_eq!(entry.private_key_pem, "key:api.example.com");
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
