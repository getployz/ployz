//! Wiring of the reserved Ingress Proxy: Caddy and the holder whose network it joins.

use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU16,
};

use crate::{
    ContainerAddress, ContainerPath, ContainerResources, HostBind, MachinePath, Placement,
    PlacementConstraint, PortPublication, PullPolicy, QualifiedService, RawVolumeSource,
    RequestedServiceSpec, ResolvedServiceSpec, RestartPolicy, ServiceContainerSpec, ServiceMode,
    ServiceMount, ServiceMountGraph, ServiceNetwork, ServiceVolume, ServiceVolumeGraph,
    ServiceVolumeReference, TransportProtocol, UpdateConfig,
};

const CADDY_INGRESS_COMMAND: [&str; 4] = ["caddy", "run", "-c", "/config/caddy/Caddyfile"];
const CADDY_INGRESS_ADMIN: &str = "unix//run/ingress/caddy/admin.sock";
/// Environment variable selecting Caddy's administrative endpoint.
pub const CADDY_ADMIN_ENV: &str = "CADDY_ADMIN";
const INGRESS_PROXY_DATA_PATH: &str = "/var/lib/ployz/ingress";
const INGRESS_PROXY_RUNTIME_PATH: &str = "/run/ployz/ingress";

/// The `ip:port` token an Ingress Proxy config names one upstream by.
#[must_use]
pub fn ingress_upstream(address: ContainerAddress, port: NonZeroU16) -> String {
    format!("{}:{port}", address.0)
}

/// Invalid wiring for a reserved Ingress door identity.
#[derive(Debug, thiserror::Error)]
#[error("reserved Ingress door spec matches no canonical Caddy or holder wiring")]
pub struct IngressProxyServiceSpecError;

/// Whether a Machine's Ingress Proxy answers on host ports 80 and 443.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngressPorts {
    /// Host ports 80 and 443 are published.
    Public,
    /// No host port is published.
    Closed,
}

/// Which canonical wiring a reserved Ingress door spec has.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IngressWiring {
    /// A Caddy that publishes the host ports itself, as before the holder existed.
    LegacyProxy,
    /// A Caddy that joins the holder's network and publishes nothing itself.
    JoinedProxy,
    /// The holder that owns the network namespace and, when public, the host ports.
    Holder(IngressPorts),
}

impl IngressWiring {
    /// The host ports this wiring answers on.
    #[must_use]
    pub fn ports(self) -> IngressPorts {
        match self {
            Self::LegacyProxy | Self::Holder(IngressPorts::Public) => IngressPorts::Public,
            Self::JoinedProxy | Self::Holder(IngressPorts::Closed) => IngressPorts::Closed,
        }
    }
}

/// Build the reserved Caddy that joins the Ingress holder's network.
///
/// The daemon's image pins in `ployzd/src/images.rs` are its only production
/// input. The CLI must not call it.
#[must_use]
pub fn caddy_service_spec(image: &str) -> RequestedServiceSpec {
    let mut spec = legacy_caddy_spec(image.to_owned(), BTreeSet::new());
    spec.container.network = Some(ServiceNetwork::IngressHolder);
    spec.ports = Vec::new();
    spec
}

/// Build the reserved holder that owns the Ingress Proxy's network namespace.
///
/// The daemon's image pins in `ployzd/src/images.rs` are its only production
/// input. The CLI must not call it.
#[must_use]
pub fn holder_service_spec(image: &str, ports: IngressPorts) -> RequestedServiceSpec {
    RequestedServiceSpec {
        name: QualifiedService::system_ingress_holder().name,
        mode: ServiceMode::Global,
        container: holder_container(image.to_owned()),
        placement: Placement::default(),
        ports: match ports {
            IngressPorts::Public => caddy_ports(),
            IngressPorts::Closed => Vec::new(),
        },
        mount_graph: ServiceMountGraph::default(),
        pre_deploy: None,
        update: UpdateConfig::default(),
    }
}

fn legacy_caddy_spec(
    image: String,
    constraints: BTreeSet<PlacementConstraint>,
) -> RequestedServiceSpec {
    RequestedServiceSpec {
        name: QualifiedService::system_ingress().name,
        mode: ServiceMode::Global,
        container: caddy_container(image),
        placement: Placement { constraints },
        ports: caddy_ports(),
        mount_graph: ServiceMountGraph::parse(caddy_volume_graph(), Default::default())
            .expect("built-in Caddy mounts are valid"),
        pre_deploy: None,
        update: UpdateConfig::default(),
    }
}

/// Every canonical door spec for this image and placement, in recognition order.
///
/// Image and placement are free for all three families. Every other field is fixed.
fn canonical_doors(
    image: &str,
    constraints: &BTreeSet<PlacementConstraint>,
) -> [(IngressWiring, RequestedServiceSpec); 4] {
    let placed = |mut spec: RequestedServiceSpec| {
        spec.placement.constraints.clone_from(constraints);
        spec
    };
    [
        (
            IngressWiring::LegacyProxy,
            legacy_caddy_spec(image.to_owned(), constraints.clone()),
        ),
        (
            IngressWiring::JoinedProxy,
            placed(caddy_service_spec(image)),
        ),
        (
            IngressWiring::Holder(IngressPorts::Public),
            placed(holder_service_spec(image, IngressPorts::Public)),
        ),
        (
            IngressWiring::Holder(IngressPorts::Closed),
            placed(holder_service_spec(image, IngressPorts::Closed)),
        ),
    ]
}

/// Recognise a requested spec as one canonical Ingress door wiring.
///
/// # Errors
///
/// Returns [`IngressProxyServiceSpecError`] when the spec matches no canonical wiring.
pub fn validate_requested_ingress_service_spec(
    spec: &RequestedServiceSpec,
) -> Result<IngressWiring, IngressProxyServiceSpecError> {
    canonical_doors(&spec.container.image, &spec.placement.constraints)
        .into_iter()
        .find_map(|(wiring, expected)| (expected == *spec).then_some(wiring))
        .ok_or(IngressProxyServiceSpecError)
}

/// Recognise a resolved spec as one canonical Ingress door wiring.
///
/// # Errors
///
/// Returns [`IngressProxyServiceSpecError`] when the spec matches no canonical
/// wiring or sets a health monitor window.
pub fn validate_ingress_service_spec(
    spec: &ResolvedServiceSpec,
) -> Result<IngressWiring, IngressProxyServiceSpecError> {
    if spec.update.monitor_millis.is_some() {
        return Err(IngressProxyServiceSpecError);
    }
    canonical_doors(&spec.container.image, &spec.placement.constraints)
        .into_iter()
        .find_map(|(wiring, expected)| {
            let expected = expected
                .to_resolved(spec.service_id, spec.update.clone())
                .expect("built-in door mounts are resolved");
            (expected == *spec).then_some(wiring)
        })
        .ok_or(IngressProxyServiceSpecError)
}

fn holder_container(image: String) -> ServiceContainerSpec {
    ServiceContainerSpec {
        command: Vec::new(),
        environment: BTreeMap::new(),
        ..caddy_container(image)
    }
}

fn caddy_container(image: String) -> ServiceContainerSpec {
    ServiceContainerSpec {
        image,
        command: CADDY_INGRESS_COMMAND.map(str::to_owned).into(),
        entrypoint: Vec::new(),
        environment: BTreeMap::from([(CADDY_ADMIN_ENV.into(), CADDY_INGRESS_ADMIN.into())]),
        sealed_identity: None,
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
        resources: ContainerResources::default(),
        stop_timeout_secs: None,
        sysctls: BTreeMap::new(),
        restart: RestartPolicy::default(),
        network: None,
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

fn bind_volume(
    reference: ServiceVolumeReference,
    machine_path: impl Into<String>,
) -> ServiceVolume {
    ServiceVolume {
        reference,
        source: RawVolumeSource::Bind {
            machine_path: MachinePath::parse(machine_path).expect("static data path is valid"),
            create_machine_path: true,
            propagation: None,
            recursive: None,
        }
        .admit()
        .expect("built-in Caddy volume is valid"),
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

#[cfg(test)]
mod tests {
    use std::num::NonZeroU32;

    use crate::{
        ConfigSpec, PlacementConstraint, ResolvedUpdateConfig, ServiceConfigGraph, ServiceId,
        ServiceMode, ServiceName, ServiceVolumeGraph, UpdateOrder,
    };

    use super::*;

    fn edge() -> BTreeSet<PlacementConstraint> {
        [PlacementConstraint::parse("node.labels.edge==true").unwrap()].into()
    }

    fn placed(mut spec: RequestedServiceSpec) -> RequestedServiceSpec {
        spec.placement.constraints = edge();
        spec
    }

    fn wiring_of(requested: &RequestedServiceSpec) -> Option<IngressWiring> {
        let wiring = validate_requested_ingress_service_spec(requested).ok();
        for order in [UpdateOrder::StartFirst, UpdateOrder::StopFirst] {
            let resolved = requested
                .to_resolved(
                    ServiceId::random(),
                    ResolvedUpdateConfig {
                        order,
                        monitor_millis: None,
                    },
                )
                .expect("built-in door mounts are resolved");
            assert_eq!(validate_ingress_service_spec(&resolved).ok(), wiring);
        }
        wiring
    }

    #[test]
    fn each_canonical_door_is_recognised_with_any_image_and_placement() {
        let image = "example.test/ingress:override";
        let cases = [
            (
                legacy_caddy_spec(image.into(), edge()),
                IngressWiring::LegacyProxy,
                IngressPorts::Public,
            ),
            (
                placed(caddy_service_spec(image)),
                IngressWiring::JoinedProxy,
                IngressPorts::Closed,
            ),
            (
                placed(holder_service_spec(image, IngressPorts::Public)),
                IngressWiring::Holder(IngressPorts::Public),
                IngressPorts::Public,
            ),
            (
                holder_service_spec(image, IngressPorts::Closed),
                IngressWiring::Holder(IngressPorts::Closed),
                IngressPorts::Closed,
            ),
        ];

        for (spec, wiring, ports) in cases {
            assert_eq!(wiring_of(&spec), Some(wiring));
            assert_eq!(wiring.ports(), ports);
        }
    }

    #[test]
    fn caddy_and_holder_split_the_legacy_wiring() {
        let legacy = legacy_caddy_spec("caddy:test".into(), BTreeSet::new());
        let joined = caddy_service_spec("caddy:test");
        let holder = holder_service_spec("pause:test", IngressPorts::Public);

        assert_eq!(
            joined.container.network,
            Some(ServiceNetwork::IngressHolder)
        );
        assert!(joined.ports.is_empty());
        assert!(joined.container.environment.contains_key(CADDY_ADMIN_ENV));
        assert_eq!(holder.ports, legacy.ports);
        assert_eq!(holder.container.network, None);
        assert!(holder.container.command.is_empty());
        assert!(holder.container.entrypoint.is_empty());
        assert!(holder.container.environment.is_empty());
        assert!(holder.volume_graph().volumes().is_empty());
        assert_eq!(holder.container.pull_policy, PullPolicy::Missing);
        assert!(
            holder_service_spec("pause:test", IngressPorts::Closed)
                .ports
                .is_empty()
        );
    }

    #[test]
    fn a_door_with_mixed_wiring_is_refused() {
        let mut publishing_joined = caddy_service_spec("caddy:test");
        publishing_joined.ports = caddy_ports();
        let mut commanded_holder = holder_service_spec("pause:test", IngressPorts::Public);
        commanded_holder.container.command = vec!["sh".into()];
        let mut holder_named_caddy = holder_service_spec("pause:test", IngressPorts::Public);
        holder_named_caddy.name = QualifiedService::system_ingress().name;
        let mut caddy_named_holder = caddy_service_spec("caddy:test");
        caddy_named_holder.name = QualifiedService::system_ingress_holder().name;

        for malformed in [
            publishing_joined,
            commanded_holder,
            holder_named_caddy,
            caddy_named_holder,
        ] {
            assert_eq!(wiring_of(&malformed), None);
        }
    }

    #[test]
    fn validators_reject_noncanonical_whole_spec_fields() {
        let requested = legacy_caddy_spec("caddy:test".into(), Default::default());

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
        let mut wrong_ports = requested.clone();
        wrong_ports.ports.pop();
        let mut wrong_volume_graph = requested.clone();
        wrong_volume_graph
            .set_volume_graph(ServiceVolumeGraph::default())
            .unwrap();
        let mut wrong_config_graph = requested.clone();
        wrong_config_graph
            .set_config_graph(
                ServiceConfigGraph::parse(
                    vec![ConfigSpec {
                        name: "unexpected".into(),
                        content: Vec::new(),
                    }],
                    Vec::new(),
                )
                .unwrap(),
            )
            .unwrap();
        let mut wrong_requested_update = requested.clone();
        wrong_requested_update.update.order = Some(UpdateOrder::StopFirst);
        let mut unrecognized_network = requested.clone();
        unrecognized_network.container.network = Some(ServiceNetwork::Unrecognized);

        for malformed in [
            wrong_mode,
            wrong_entrypoint,
            wrong_name,
            wrong_environment,
            wrong_ports,
            wrong_volume_graph,
            wrong_config_graph,
            wrong_requested_update,
            unrecognized_network,
        ] {
            assert!(validate_requested_ingress_service_spec(&malformed).is_err());
        }

        for door in [
            requested,
            caddy_service_spec("caddy:test"),
            holder_service_spec("pause:test", IngressPorts::Public),
        ] {
            let mut monitored = door
                .to_resolved(ServiceId::random(), ResolvedUpdateConfig::default())
                .expect("built-in door mounts are resolved");
            monitored.update.monitor_millis = Some(1);
            assert!(validate_ingress_service_spec(&monitored).is_err());
        }
    }
}
