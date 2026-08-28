//! Concrete Ingress Proxy backend identity and reserved-Service wiring.

use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
    num::NonZeroU16,
    str::FromStr,
};

use serde::{Deserialize, Serialize};

use crate::{
    ContainerPath, ContainerResources, HostBind, IngressProxyFragment, MachinePath, MachineTarget,
    Placement, PortPublication, PullPolicy, QualifiedService, RequestedServiceSpec,
    ResolvedServiceSpec, RestartPolicy, ServiceContainerSpec, ServiceMode, ServiceMount,
    ServiceVolume, ServiceVolumeGraph, ServiceVolumeReference, TransportProtocol, UpdateConfig,
    UpdateOrder, ValueError, VolumeSource,
};

const CADDY_INGRESS_COMMAND: [&str; 4] = ["caddy", "run", "-c", "/config/caddy/Caddyfile"];
const CADDY_INGRESS_ADMIN: &str = "unix//run/ingress/caddy/admin.sock";
const ZENTINEL_INGRESS_COMMAND: [&str; 2] = ["-c", "/config/zentinel.kdl"];
const ZENTINEL_INGRESS_CAPABILITY: &str = "NET_BIND_SERVICE";
const ENVOY_INGRESS_COMMAND: [&str; 3] = ["envoy", "-c", "/config/bootstrap.yaml"];
const ENVOY_HTTP_CONTAINER_PORT: u16 = 8080;
const ENVOY_HTTPS_CONTAINER_PORT: u16 = 8443;
const INGRESS_PROXY_DATA_PATH: &str = "/var/lib/ployz/ingress";
const INGRESS_PROXY_RUNTIME_PATH: &str = "/run/ployz/ingress";
const SUPPORTED_INGRESS_PROXY_BACKENDS: [IngressProxyBackend; 3] = [
    IngressProxyBackend::Caddy,
    IngressProxyBackend::Zentinel,
    IngressProxyBackend::Envoy,
];

/// Invalid or mixed concrete wiring for the reserved Ingress Proxy Service.
#[derive(Debug, thiserror::Error)]
#[error("reserved Ingress Proxy Service has missing, unknown, or mixed backend wiring")]
pub struct IngressProxyServiceSpecError;

/// Replicated key holding the Cluster's immutable Ingress Proxy Backend.
pub const INGRESS_PROXY_BACKEND_CLUSTER_KEY: &str = "ingress_proxy_backend";
/// Numeric user required by every supported Envoy image override.
pub const ENVOY_RUNTIME_UID: u32 = 101;
/// Numeric group shared by the Envoy process and its private key files.
pub const ENVOY_RUNTIME_GID: u32 = 101;

/// Concrete Ingress Proxy implementation selected when a Cluster is founded.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngressProxyBackend {
    /// Caddy with bridge networking and host-published ports.
    Caddy,
    /// Zentinel with host networking and no published ports.
    Zentinel,
    /// Envoy with bridge networking and unprivileged in-container listeners.
    Envoy,
}

/// Docker network mode required by one canonical Ingress Proxy runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngressProxyNetworkMode {
    /// Attach the container to the Ployz bridge.
    Bridge,
    /// Share the Machine's host network namespace.
    Host,
}

impl IngressProxyBackend {
    /// Parse a replicated Ingress Proxy Backend value.
    ///
    /// # Errors
    ///
    /// Returns [`ValueError`] unless `value` is `caddy`, `zentinel`, or `envoy`.
    pub fn parse(value: &str) -> Result<Self, ValueError> {
        match value {
            "caddy" => Ok(Self::Caddy),
            "zentinel" => Ok(Self::Zentinel),
            "envoy" => Ok(Self::Envoy),
            _ => Err(ValueError::new(
                "Ingress Proxy Backend",
                value,
                "caddy, zentinel, or envoy",
            )),
        }
    }

    /// Stable replicated spelling.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Caddy => "caddy",
            Self::Zentinel => "zentinel",
            Self::Envoy => "envoy",
        }
    }

    /// Docker network mode for the backend's Service Container.
    #[must_use]
    pub const fn network_mode(self) -> IngressProxyNetworkMode {
        match self {
            IngressProxyBackend::Caddy | IngressProxyBackend::Envoy => {
                IngressProxyNetworkMode::Bridge
            }
            IngressProxyBackend::Zentinel => IngressProxyNetworkMode::Host,
        }
    }

    /// Build the complete reserved-Service deploy input for this backend.
    ///
    /// The image, Machine placement, and backend-tagged fragment are deploy
    /// inputs. Every other runtime field is fixed by the backend.
    ///
    /// # Errors
    ///
    /// Returns [`IngressProxyServiceSpecError`] when `fragment` belongs to a
    /// different backend.
    pub fn requested_service_spec(
        self,
        image: String,
        machines: Vec<MachineTarget>,
        fragment: Option<IngressProxyFragment>,
    ) -> Result<RequestedServiceSpec, IngressProxyServiceSpecError> {
        let fragment_matches = match self {
            IngressProxyBackend::Caddy => fragment
                .as_ref()
                .is_none_or(|fragment| fragment.as_caddy().is_some()),
            IngressProxyBackend::Zentinel | IngressProxyBackend::Envoy => fragment.is_none(),
        };
        if !fragment_matches {
            return Err(IngressProxyServiceSpecError);
        }
        let (container, ports, volume_graph, update) = match self {
            IngressProxyBackend::Caddy => (
                caddy_container(image),
                caddy_ports(),
                caddy_volume_graph(),
                UpdateConfig::default(),
            ),
            IngressProxyBackend::Zentinel => (
                zentinel_container(image),
                Vec::new(),
                zentinel_volume_graph(),
                UpdateConfig {
                    order: Some(UpdateOrder::StopFirst),
                    ..Default::default()
                },
            ),
            IngressProxyBackend::Envoy => (
                envoy_container(image),
                envoy_ports(),
                envoy_volume_graph(),
                UpdateConfig::default(),
            ),
        };
        Ok(RequestedServiceSpec {
            name: QualifiedService::system_ingress().name,
            mode: ServiceMode::Global,
            container,
            placement: Placement { machines },
            ports,
            volume_graph,
            config_graph: Default::default(),
            pre_deploy: None,
            ingress_proxy_fragment: fragment,
            update,
        })
    }

    fn matches_requested(self, spec: &RequestedServiceSpec) -> bool {
        self.requested_service_spec(
            spec.container.image.clone(),
            spec.placement.machines.clone(),
            spec.ingress_proxy_fragment.clone(),
        )
        .is_ok_and(|expected| expected == *spec)
    }

    fn matches_resolved(self, spec: &crate::ResolvedServiceSpec) -> bool {
        let update_matches = spec.update.monitor_millis.is_none()
            && match self {
                IngressProxyBackend::Caddy | IngressProxyBackend::Envoy => matches!(
                    spec.update.order,
                    UpdateOrder::StartFirst | UpdateOrder::StopFirst
                ),
                IngressProxyBackend::Zentinel => {
                    matches!(spec.update.order, UpdateOrder::StopFirst)
                }
            };
        update_matches
            && self
                .requested_service_spec(
                    spec.container.image.clone(),
                    spec.placement.machines.clone(),
                    spec.ingress_proxy_fragment.clone(),
                )
                .is_ok_and(|expected| {
                    expected.to_resolved(spec.service_id, spec.update.clone()) == *spec
                })
    }
}

/// Recover the backend encoded by one concrete reserved-service spec.
///
/// # Errors
///
/// Returns [`IngressProxyServiceSpecError`] unless the spec has one exact
/// supported backend topology.
pub fn ingress_proxy_backend(
    spec: &ResolvedServiceSpec,
) -> Result<IngressProxyBackend, IngressProxyServiceSpecError> {
    SUPPORTED_INGRESS_PROXY_BACKENDS
        .into_iter()
        .find(|backend| backend.matches_resolved(spec))
        .ok_or(IngressProxyServiceSpecError)
}

/// Recover the backend encoded by one concrete requested reserved-service spec.
///
/// # Errors
///
/// Returns [`IngressProxyServiceSpecError`] unless the spec has one exact
/// supported backend topology.
pub fn requested_ingress_proxy_backend(
    spec: &RequestedServiceSpec,
) -> Result<IngressProxyBackend, IngressProxyServiceSpecError> {
    SUPPORTED_INGRESS_PROXY_BACKENDS
        .into_iter()
        .find(|backend| backend.matches_requested(spec))
        .ok_or(IngressProxyServiceSpecError)
}

/// Environment keys emitted by reserved Ingress Proxy backend profiles.
#[must_use]
pub fn ingress_proxy_profile_environment_keys() -> BTreeSet<String> {
    SUPPORTED_INGRESS_PROXY_BACKENDS
        .into_iter()
        .filter_map(|backend| {
            backend
                .requested_service_spec(String::new(), Vec::new(), None)
                .ok()
        })
        .flat_map(|spec| spec.container.environment.into_keys())
        .collect()
}

fn base_container(image: String) -> ServiceContainerSpec {
    ServiceContainerSpec {
        image,
        command: Vec::new(),
        entrypoint: Vec::new(),
        environment: BTreeMap::new(),
        labels: Default::default(),
        hostname: None,
        extra_hosts: Vec::new(),
        cap_add: Vec::new(),
        cap_drop: Vec::new(),
        healthcheck: None,
        pull_policy: PullPolicy::Missing,
        init: None,
        user: None,
        working_directory: None,
        tty: false,
        open_stdin: false,
        privileged: false,
        pid_mode: None,
        log_driver: None,
        resources: ContainerResources::default(),
        stop_timeout_secs: None,
        sysctls: BTreeMap::new(),
        restart: RestartPolicy::default(),
    }
}

fn caddy_container(image: String) -> ServiceContainerSpec {
    ServiceContainerSpec {
        command: CADDY_INGRESS_COMMAND.map(str::to_owned).into(),
        environment: BTreeMap::from([("CADDY_ADMIN".into(), CADDY_INGRESS_ADMIN.into())]),
        ..base_container(image)
    }
}

fn zentinel_container(image: String) -> ServiceContainerSpec {
    ServiceContainerSpec {
        command: ZENTINEL_INGRESS_COMMAND.map(str::to_owned).into(),
        cap_add: vec![ZENTINEL_INGRESS_CAPABILITY.into()],
        cap_drop: vec!["ALL".into()],
        ..base_container(image)
    }
}

fn envoy_container(image: String) -> ServiceContainerSpec {
    ServiceContainerSpec {
        command: ENVOY_INGRESS_COMMAND.map(str::to_owned).into(),
        user: Some(format!("{ENVOY_RUNTIME_UID}:{ENVOY_RUNTIME_GID}")),
        ..base_container(image)
    }
}

fn caddy_ports() -> Vec<PortPublication> {
    let host_port = |port, transport_protocol| PortPublication::Host {
        bind: HostBind::All,
        published_port: NonZeroU16::new(port).expect("Caddy ports are non-zero"),
        container_port: NonZeroU16::new(port).expect("Caddy ports are non-zero"),
        transport_protocol,
    };
    vec![
        host_port(80, TransportProtocol::Tcp),
        host_port(443, TransportProtocol::Tcp),
        host_port(443, TransportProtocol::Udp),
    ]
}

fn caddy_volume_graph() -> ServiceVolumeGraph {
    let data = ServiceVolumeReference::parse("ingress-data").expect("static volume is valid");
    let runtime = ServiceVolumeReference::parse("ingress-runtime").expect("static volume is valid");
    ServiceVolumeGraph::parse(
        vec![
            bind_volume(data.clone(), INGRESS_PROXY_DATA_PATH),
            bind_volume(runtime.clone(), INGRESS_PROXY_RUNTIME_PATH),
        ],
        vec![
            mount(&data, "/config"),
            mount(&data, "/data"),
            mount(&runtime, "/run/ingress"),
        ],
    )
    .expect("static Caddy Volume graph is valid")
}

fn zentinel_volume_graph() -> ServiceVolumeGraph {
    let data = ServiceVolumeReference::parse("ingress-data").expect("static volume is valid");
    ServiceVolumeGraph::parse(
        vec![bind_volume(
            data.clone(),
            format!("{INGRESS_PROXY_DATA_PATH}/zentinel"),
        )],
        vec![mount(&data, "/config")],
    )
    .expect("static Zentinel Volume graph is valid")
}

fn envoy_ports() -> Vec<PortPublication> {
    let host_port = |published, container| PortPublication::Host {
        bind: HostBind::All,
        published_port: NonZeroU16::new(published).expect("Envoy host ports are non-zero"),
        container_port: NonZeroU16::new(container).expect("Envoy container ports are non-zero"),
        transport_protocol: TransportProtocol::Tcp,
    };
    vec![
        host_port(80, ENVOY_HTTP_CONTAINER_PORT),
        host_port(443, ENVOY_HTTPS_CONTAINER_PORT),
    ]
}

fn envoy_volume_graph() -> ServiceVolumeGraph {
    let data = ServiceVolumeReference::parse("ingress-data").expect("static volume is valid");
    ServiceVolumeGraph::parse(
        vec![bind_volume(
            data.clone(),
            format!("{INGRESS_PROXY_DATA_PATH}/envoy"),
        )],
        vec![mount(&data, "/config")],
    )
    .expect("static Envoy Volume graph is valid")
}

fn bind_volume(
    reference: ServiceVolumeReference,
    machine_path: impl Into<String>,
) -> ServiceVolume {
    ServiceVolume {
        reference,
        source: VolumeSource::Bind {
            machine_path: MachinePath::parse(machine_path).expect("static data path is valid"),
            create_machine_path: true,
            propagation: None,
            recursive: None,
        },
    }
}

fn mount(volume: &ServiceVolumeReference, target: &str) -> ServiceMount {
    ServiceMount {
        volume: volume.clone(),
        target: ContainerPath::parse(target).expect("static mount path is valid"),
        read_only: false,
        no_copy: false,
        subpath: None,
    }
}

impl fmt::Display for IngressProxyBackend {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl FromStr for IngressProxyBackend {
    type Err = ValueError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        Self::parse(value)
    }
}

#[cfg(test)]
mod tests {
    use std::num::{NonZeroU16, NonZeroU32};

    use crate::{
        ConfigSpec, MachineTarget, ResolvedUpdateConfig, ServiceConfigGraph, ServiceId,
        ServiceMode, ServiceName, UpdateOrder,
    };

    use super::*;

    #[test]
    fn backend_builds_and_recognizes_each_canonical_runtime() {
        let machines = vec![MachineTarget::parse("edge").unwrap()];
        for backend in SUPPORTED_INGRESS_PROXY_BACKENDS {
            let requested = backend
                .requested_service_spec(
                    "example.test/ingress:override".into(),
                    machines.clone(),
                    None,
                )
                .unwrap();
            let resolved = requested.to_resolved(
                ServiceId::random(),
                ResolvedUpdateConfig {
                    order: requested.update.order.unwrap_or(UpdateOrder::StartFirst),
                    monitor_millis: None,
                },
            );

            assert_eq!(
                requested_ingress_proxy_backend(&requested).unwrap(),
                backend
            );
            assert_eq!(ingress_proxy_backend(&resolved).unwrap(), backend);
            assert_eq!(requested.placement.machines, machines);
        }
    }

    #[test]
    fn profile_environment_keys_include_caddy_admin() {
        assert!(ingress_proxy_profile_environment_keys().contains("CADDY_ADMIN"));
    }

    #[test]
    fn resolved_update_validation_matches_planner_outcomes() {
        let resolved = |backend: IngressProxyBackend, order: UpdateOrder| {
            backend
                .requested_service_spec("ingress:test".into(), Vec::new(), None)
                .unwrap()
                .to_resolved(
                    ServiceId::random(),
                    ResolvedUpdateConfig {
                        order,
                        monitor_millis: None,
                    },
                )
        };

        for (backend, order, accepted) in [
            (IngressProxyBackend::Caddy, UpdateOrder::StartFirst, true),
            (IngressProxyBackend::Caddy, UpdateOrder::StopFirst, true),
            (IngressProxyBackend::Envoy, UpdateOrder::StartFirst, true),
            (IngressProxyBackend::Envoy, UpdateOrder::StopFirst, true),
            (IngressProxyBackend::Zentinel, UpdateOrder::StopFirst, true),
            (
                IngressProxyBackend::Zentinel,
                UpdateOrder::StartFirst,
                false,
            ),
        ] {
            assert_eq!(
                ingress_proxy_backend(&resolved(backend, order)).is_ok(),
                accepted
            );
        }
    }

    #[test]
    fn classifiers_reject_noncanonical_whole_spec_fields() {
        let requested = IngressProxyBackend::Caddy
            .requested_service_spec("caddy:test".into(), Vec::new(), None)
            .unwrap();

        let mut wrong_mode = requested.clone();
        wrong_mode.mode = ServiceMode::Replicated {
            replicas: NonZeroU32::new(1).unwrap(),
        };
        let mut wrong_entrypoint = requested.clone();
        wrong_entrypoint.container.entrypoint.push("proxy".into());
        let mut wrong_name = requested.clone();
        wrong_name.name = ServiceName::parse("proxy").unwrap();
        let mut wrong_environment = requested.clone();
        wrong_environment
            .container
            .environment
            .insert("UNEXPECTED".into(), "1".into());
        let mut wrong_capability = requested.clone();
        wrong_capability.container.cap_add.push("NET_ADMIN".into());
        let mut wrong_ports = requested.clone();
        wrong_ports.ports.pop();
        let mut wrong_volume_graph = requested.clone();
        wrong_volume_graph.volume_graph = ServiceVolumeGraph::default();
        let mut wrong_config_graph = requested.clone();
        wrong_config_graph.config_graph = ServiceConfigGraph::parse(
            vec![ConfigSpec {
                name: "unexpected".into(),
                content: Vec::new(),
            }],
            Vec::new(),
        )
        .unwrap();
        let mut wrong_requested_update = requested.clone();
        wrong_requested_update.update.order = Some(UpdateOrder::StopFirst);

        for malformed in [
            wrong_mode,
            wrong_entrypoint,
            wrong_name,
            wrong_environment,
            wrong_capability,
            wrong_ports,
            wrong_volume_graph,
            wrong_config_graph,
            wrong_requested_update,
        ] {
            assert!(requested_ingress_proxy_backend(&malformed).is_err());
        }

        let mut wrong_resolved_update =
            requested.to_resolved(ServiceId::random(), ResolvedUpdateConfig::default());
        wrong_resolved_update.update.monitor_millis = Some(1);
        assert!(ingress_proxy_backend(&wrong_resolved_update).is_err());

        assert!(
            IngressProxyBackend::Zentinel
                .requested_service_spec(
                    "zentinel:test".into(),
                    Vec::new(),
                    Some(IngressProxyFragment::parse_caddy("respond ok").unwrap()),
                )
                .is_err()
        );
        assert!(
            IngressProxyBackend::Envoy
                .requested_service_spec(
                    "envoy:test".into(),
                    Vec::new(),
                    Some(IngressProxyFragment::parse_caddy("respond ok").unwrap()),
                )
                .is_err()
        );
    }

    #[test]
    fn envoy_uses_bridge_unprivileged_published_ports() {
        let requested = IngressProxyBackend::Envoy
            .requested_service_spec("envoy:test".into(), Vec::new(), None)
            .unwrap();

        assert_eq!(
            requested.container.command,
            ["envoy", "-c", "/config/bootstrap.yaml"]
        );
        assert_eq!(requested.container.image, "envoy:test");
        assert_eq!(requested.container.user.as_deref(), Some("101:101"));
        assert!(requested.container.cap_add.is_empty());
        assert!(requested.container.cap_drop.is_empty());
        assert_eq!(requested.update, UpdateConfig::default());
        assert_eq!(
            requested.ports,
            [
                PortPublication::Host {
                    bind: HostBind::All,
                    published_port: NonZeroU16::new(80).unwrap(),
                    container_port: NonZeroU16::new(ENVOY_HTTP_CONTAINER_PORT).unwrap(),
                    transport_protocol: TransportProtocol::Tcp,
                },
                PortPublication::Host {
                    bind: HostBind::All,
                    published_port: NonZeroU16::new(443).unwrap(),
                    container_port: NonZeroU16::new(ENVOY_HTTPS_CONTAINER_PORT).unwrap(),
                    transport_protocol: TransportProtocol::Tcp,
                },
            ]
        );
        assert!(
            requested
                .volume_graph
                .volumes()
                .iter()
                .filter_map(|volume| match &volume.source {
                    VolumeSource::Bind { machine_path, .. } => Some(machine_path.as_str()),
                    VolumeSource::External { .. }
                    | VolumeSource::Ordinary { .. }
                    | VolumeSource::Provisioned { .. }
                    | VolumeSource::Tmpfs { .. } => None,
                })
                .eq(["/var/lib/ployz/ingress/envoy"])
        );
    }

    #[test]
    fn parse_seam_accepts_envoy_and_rejects_an_unknown_spelling() {
        assert_eq!(
            IngressProxyBackend::parse("envoy").unwrap(),
            IngressProxyBackend::Envoy
        );
        assert!(IngressProxyBackend::parse("traefik").is_err());
        assert_eq!(
            serde_json::to_value(IngressProxyBackend::Envoy).unwrap(),
            serde_json::json!("envoy")
        );
    }
}
