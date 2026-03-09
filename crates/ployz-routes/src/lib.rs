use std::cmp::Reverse;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::net::{SocketAddr, SocketAddrV4};

use ployz_sdk::model::{
    InstanceId, InstancePhase, InstanceStatusRecord, MachineId, RoutingState, ServiceSlotRecord,
};
use ployz_sdk::spec::{Namespace, RouteSpec, ServiceSpec};
use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewaySnapshot {
    pub http_routes: Vec<HttpRouteView>,
    pub tcp_routes: Vec<TcpRouteView>,
}

impl GatewaySnapshot {
    #[must_use]
    pub fn empty() -> Self {
        Self {
            http_routes: Vec::new(),
            tcp_routes: Vec::new(),
        }
    }
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
    pub service_port: String,
    pub address: SocketAddr,
}

#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ProjectionError {
    #[error("current head for service '{service}' in namespace '{namespace}' referenced missing revision '{revision_hash}'")]
    MissingRevision {
        namespace: Namespace,
        service: String,
        revision_hash: String,
    },
    #[error("current head for service '{service}' in namespace '{namespace}' had invalid spec json: {message}")]
    InvalidRevisionSpec {
        namespace: Namespace,
        service: String,
        message: String,
    },
    #[error("HTTP route conflict between '{left}' and '{right}' for host '{host}' and path prefix '{path_prefix}'")]
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
}

#[must_use]
pub fn normalize_request_host(host: &str) -> String {
    let trimmed = host.trim().trim_end_matches('.');
    if trimmed.is_empty() {
        return String::new();
    }
    if trimmed.starts_with('[') {
        return trimmed
            .trim_start_matches('[')
            .split(']')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
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
    let mut slots_by_revision = HashMap::new();
    for slot in state.slots {
        slots_by_revision
            .entry((
                slot.namespace.clone(),
                slot.service.clone(),
                slot.revision_hash.clone(),
            ))
            .or_insert_with(Vec::new)
            .push(slot);
    }

    let mut http_routes = Vec::new();
    let mut tcp_routes = Vec::new();
    for head in state.heads {
        let revision_key = (
            head.namespace.clone(),
            head.service.clone(),
            head.current_revision_hash.clone(),
        );
        let Some(revision) = revisions.get(&revision_key) else {
            return Err(ProjectionError::MissingRevision {
                namespace: head.namespace,
                service: head.service,
                revision_hash: head.current_revision_hash,
            });
        };
        let spec: ServiceSpec =
            serde_json::from_str(&revision.spec_json).map_err(|err| ProjectionError::InvalidRevisionSpec {
                namespace: revision.namespace.clone(),
                service: revision.service.clone(),
                message: err.to_string(),
            })?;

        let backends_by_port = routable_backends_by_port(
            &spec,
            &head.current_revision_hash,
            slots_by_revision
                .get(&revision_key)
                .map(Vec::as_slice)
                .unwrap_or(&[]),
            &instances,
        );

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
                        route_id: format!(
                            "http:{}:{}:{}",
                            spec.namespace, spec.name, index
                        ),
                        namespace: spec.namespace.clone(),
                        service: spec.name.clone(),
                        revision_hash: head.current_revision_hash.clone(),
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
                        route_id: format!("tcp:{}:{}:{}", spec.namespace, spec.name, index),
                        namespace: spec.namespace.clone(),
                        service: spec.name.clone(),
                        revision_hash: head.current_revision_hash.clone(),
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
    })
}

fn routable_backends_by_port(
    spec: &ServiceSpec,
    revision_hash: &str,
    slots: &[ServiceSlotRecord],
    instances: &HashMap<InstanceId, InstanceStatusRecord>,
) -> BTreeMap<String, Vec<BackendView>> {
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
        if !is_routable_instance(instance, slot, revision_hash) {
            continue;
        }
        let Some(overlay_ip) = instance.overlay_ip else {
            continue;
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
    backends
}

fn is_routable_instance(
    instance: &InstanceStatusRecord,
    slot: &ServiceSlotRecord,
    revision_hash: &str,
) -> bool {
    instance.namespace == slot.namespace
        && instance.service == slot.service
        && instance.slot_id == slot.slot_id
        && instance.machine_id == slot.machine_id
        && instance.revision_hash == revision_hash
        && instance.ready
        && instance.phase == InstancePhase::Ready
        && instance.drain_state == ployz_sdk::model::DrainState::None
        && instance.error.is_none()
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
            let key = (host.clone(), route.path_prefix.clone());
            if let Some(existing) = seen.insert(key.clone(), route.route_id.clone()) {
                return Err(ProjectionError::HttpRouteConflict {
                    left: existing,
                    right: route.route_id.clone(),
                    host,
                    path_prefix: key.1,
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
    use std::net::Ipv4Addr;
    use ployz_sdk::model::{DeployId, DrainState, InstanceStatusRecord, ServiceHeadRecord, ServiceRevisionRecord, ServiceSlotRecord, SlotId};
    use ployz_sdk::spec::{
        ContainerSpec, HttpRoute, NetworkMode, Placement, PortProtocol, PullPolicy, Resources,
        RestartPolicy, RouteSpec, ServicePort, ServiceSpec,
    };

    #[test]
    fn project_only_routes_current_head_ready_instances() {
        let namespace = Namespace("prod".into());
        let revision_a = "rev-a".to_string();
        let revision_b = "rev-b".to_string();
        let old = service_spec(&namespace, "api", "v1", revision_a.clone(), vec!["old.example.com".into()]);
        let current = service_spec(&namespace, "api", "v2", revision_b.clone(), vec!["api.example.com".into()]);

        let snapshot = project(RoutingState {
            revisions: vec![revision_record(&old), revision_record(&current)],
            heads: vec![ServiceHeadRecord {
                namespace: namespace.clone(),
                service: "api".into(),
                current_revision_hash: current.revision_hash().expect("revision hash"),
                updated_by_deploy_id: DeployId("dep-1".into()),
                updated_at: 1,
            }],
            slots: vec![
                slot_record(&namespace, "api", "slot-1", "inst-ready", &current),
                slot_record(&namespace, "api", "slot-2", "inst-draining", &current),
            ],
            instances: vec![
                instance_record(&namespace, "api", "slot-1", "inst-ready", true, DrainState::None, &current),
                instance_record(&namespace, "api", "slot-2", "inst-draining", true, DrainState::Requested, &current),
            ],
        })
        .expect("projection succeeds");

        assert_eq!(snapshot.http_routes.len(), 1);
        let route = &snapshot.http_routes[0];
        assert_eq!(route.hostnames, vec!["api.example.com".to_string()]);
        assert_eq!(route.backends.len(), 1);
        assert_eq!(route.backends[0].instance_id.0, "inst-ready");
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
        };

        let route = match_http_route(&snapshot, Some("api.example.com"), "/v1/users")
            .expect("matched route");
        assert_eq!(route.route_id, "specific");
    }

    #[test]
    fn duplicate_http_host_and_path_is_rejected() {
        let namespace = Namespace("prod".into());
        let left = service_spec(&namespace, "one", "v1", "rev-1".into(), vec!["api.example.com".into()]);
        let right = service_spec(&namespace, "two", "v1", "rev-2".into(), vec!["api.example.com".into()]);

        let error = project(RoutingState {
            revisions: vec![revision_record(&left), revision_record(&right)],
            heads: vec![
                ServiceHeadRecord {
                    namespace: namespace.clone(),
                    service: "one".into(),
                    current_revision_hash: left.revision_hash().expect("revision hash"),
                    updated_by_deploy_id: DeployId("dep-1".into()),
                    updated_at: 1,
                },
                ServiceHeadRecord {
                    namespace: namespace.clone(),
                    service: "two".into(),
                    current_revision_hash: right.revision_hash().expect("revision hash"),
                    updated_by_deploy_id: DeployId("dep-1".into()),
                    updated_at: 1,
                },
            ],
            slots: Vec::new(),
            instances: Vec::new(),
        })
        .expect_err("conflict expected");

        match error {
            ProjectionError::HttpRouteConflict { host, path_prefix, .. } => {
                assert_eq!(host, "api.example.com");
                assert_eq!(path_prefix, "/");
            }
            other => panic!("unexpected error: {other}"),
        }
    }

    #[test]
    fn tcp_routes_are_projected_with_no_serving_dependency() {
        let namespace = Namespace("prod".into());
        let mut spec = service_spec(&namespace, "db", "v1", "rev-db".into(), Vec::new());
        spec.routes = vec![RouteSpec::Tcp(ployz_sdk::spec::TcpRoute {
            service_port: "sql".into(),
            listen_port: 5432,
        })];
        spec.service_ports = vec![ServicePort {
            name: "sql".into(),
            container_port: 5432,
            protocol: PortProtocol::Tcp,
        }];

        let snapshot = project(RoutingState {
            revisions: vec![revision_record(&spec)],
            heads: vec![ServiceHeadRecord {
                namespace: namespace.clone(),
                service: "db".into(),
                current_revision_hash: spec.revision_hash().expect("revision hash"),
                updated_by_deploy_id: DeployId("dep-1".into()),
                updated_at: 1,
            }],
            slots: vec![slot_record(&namespace, "db", "slot-1", "inst-db", &spec)],
            instances: vec![instance_record(&namespace, "db", "slot-1", "inst-db", true, DrainState::None, &spec)],
        })
        .expect("projection succeeds");

        assert_eq!(snapshot.http_routes.len(), 0);
        assert_eq!(snapshot.tcp_routes.len(), 1);
        assert_eq!(snapshot.tcp_routes[0].listen_port, 5432);
    }

    fn service_spec(
        namespace: &Namespace,
        service: &str,
        image_tag: &str,
        _seed: String,
        hostnames: Vec<String>,
    ) -> ServiceSpec {
        ServiceSpec {
            name: service.into(),
            namespace: namespace.clone(),
            placement: Placement::Singleton,
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
            routes: vec![RouteSpec::Http(HttpRoute {
                service_port: "http".into(),
                hostnames,
                path_prefix: "/".into(),
            })],
            readiness: None,
            rollout: ployz_sdk::spec::RolloutStrategy::Recreate,
            labels: BTreeMap::new(),
            stop_grace_period: None,
            restart: RestartPolicy::UnlessStopped,
        }
    }

    fn revision_record(spec: &ServiceSpec) -> ServiceRevisionRecord {
        ServiceRevisionRecord {
            namespace: spec.namespace.clone(),
            service: spec.name.clone(),
            revision_hash: spec.revision_hash().expect("revision hash"),
            spec_json: spec
                .canonical_revision_json()
                .expect("canonical revision json"),
            created_by: MachineId("founder".into()),
            created_at: 1,
        }
    }

    fn slot_record(
        namespace: &Namespace,
        service: &str,
        slot_id: &str,
        instance_id: &str,
        spec: &ServiceSpec,
    ) -> ServiceSlotRecord {
        ServiceSlotRecord {
            namespace: namespace.clone(),
            service: service.into(),
            slot_id: SlotId(slot_id.into()),
            machine_id: MachineId("machine-a".into()),
            active_instance_id: InstanceId(instance_id.into()),
            revision_hash: spec.revision_hash().expect("revision hash"),
            updated_by_deploy_id: DeployId("dep-1".into()),
            updated_at: 1,
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
            backend_ports: BTreeMap::from([(String::from("http"), 8080), (String::from("sql"), 5432)]),
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

    fn backend(id: &str) -> BackendView {
        BackendView {
            instance_id: InstanceId(id.into()),
            machine_id: MachineId("machine-a".into()),
            service_port: "http".into(),
            address: SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::new(10, 0, 0, 2), 8080)),
        }
    }
}
