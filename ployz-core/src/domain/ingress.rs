//! Caddy's reserved Ingress Proxy Service wiring.

use std::{collections::BTreeMap, num::NonZeroU16};

use crate::{
    ContainerPath, ContainerResources, HostBind, IngressProxyFragment, MachinePath, MachineTarget,
    Placement, PortPublication, PullPolicy, QualifiedService, RawVolumeSource,
    RequestedServiceSpec, ResolvedServiceSpec, RestartPolicy, ServiceContainerSpec, ServiceMode,
    ServiceMount, ServiceVolume, ServiceVolumeGraph, ServiceVolumeReference, TransportProtocol,
    UpdateConfig,
};

const CADDY_INGRESS_COMMAND: [&str; 4] = ["caddy", "run", "-c", "/config/caddy/Caddyfile"];
const CADDY_INGRESS_ADMIN: &str = "unix//run/ingress/caddy/admin.sock";
/// Environment variable selecting Caddy's administrative endpoint.
pub const CADDY_ADMIN_ENV: &str = "CADDY_ADMIN";
const INGRESS_PROXY_DATA_PATH: &str = "/var/lib/ployz/ingress";
const INGRESS_PROXY_RUNTIME_PATH: &str = "/run/ployz/ingress";

/// Invalid wiring for the reserved Ingress Proxy Service.
#[derive(Debug, thiserror::Error)]
#[error("reserved Ingress Proxy Service does not have canonical Caddy wiring")]
pub struct IngressProxyServiceSpecError;

/// Build the complete reserved Caddy Service deploy input.
///
/// The image, Machine placement, and optional Caddy fragment are deploy inputs.
/// Every other runtime field is fixed by Ployz.
#[must_use]
pub fn caddy_service_spec(
    image: String,
    machines: Vec<MachineTarget>,
    fragment: Option<IngressProxyFragment>,
) -> RequestedServiceSpec {
    RequestedServiceSpec {
        name: QualifiedService::system_ingress().name,
        mode: ServiceMode::Global,
        container: caddy_container(image),
        placement: Placement { machines },
        ports: caddy_ports(),
        mount_graph: crate::ServiceMountGraph::new(caddy_volume_graph(), Default::default())
            .expect("built-in Caddy mounts are valid"),
        pre_deploy: None,
        ingress_proxy_fragment: fragment,
        update: UpdateConfig::default(),
    }
}

/// Validate that a requested spec is the reserved Caddy Service.
///
/// # Errors
///
/// Returns [`IngressProxyServiceSpecError`] when any reserved Caddy field differs.
pub fn validate_requested_ingress_service_spec(
    spec: &RequestedServiceSpec,
) -> Result<(), IngressProxyServiceSpecError> {
    let expected = caddy_service_spec(
        spec.container.image.clone(),
        spec.placement.machines.clone(),
        spec.ingress_proxy_fragment.clone(),
    );
    (expected == *spec)
        .then_some(())
        .ok_or(IngressProxyServiceSpecError)
}

/// Validate that a resolved spec is the reserved Caddy Service.
///
/// # Errors
///
/// Returns [`IngressProxyServiceSpecError`] when any reserved Caddy field differs.
pub fn validate_ingress_service_spec(
    spec: &ResolvedServiceSpec,
) -> Result<(), IngressProxyServiceSpecError> {
    if spec.update.monitor_millis.is_some() {
        return Err(IngressProxyServiceSpecError);
    }
    let expected = caddy_service_spec(
        spec.container.image.clone(),
        spec.placement.machines.clone(),
        spec.ingress_proxy_fragment.clone(),
    )
    .to_resolved(spec.service_id, spec.update.clone())
    .expect("built-in Caddy mounts are resolved");
    (expected == *spec)
        .then_some(())
        .ok_or(IngressProxyServiceSpecError)
}

fn caddy_container(image: String) -> ServiceContainerSpec {
    ServiceContainerSpec {
        image,
        command: CADDY_INGRESS_COMMAND.map(str::to_owned).into(),
        entrypoint: Vec::new(),
        environment: BTreeMap::from([(CADDY_ADMIN_ENV.into(), CADDY_INGRESS_ADMIN.into())]),
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
        ConfigSpec, MachineTarget, ResolvedUpdateConfig, ServiceConfigGraph, ServiceId,
        ServiceMode, ServiceName, ServiceVolumeGraph, UpdateOrder,
    };

    use super::*;

    #[test]
    fn caddy_wiring_round_trips_through_both_validators() {
        let machines = vec![MachineTarget::parse("edge").unwrap()];
        let requested = caddy_service_spec("example.test/ingress:override".into(), machines, None);

        assert!(validate_requested_ingress_service_spec(&requested).is_ok());
        for order in [UpdateOrder::StartFirst, UpdateOrder::StopFirst] {
            let resolved = requested
                .to_resolved(
                    ServiceId::random(),
                    ResolvedUpdateConfig {
                        order,
                        monitor_millis: None,
                    },
                )
                .expect("built-in Caddy mounts are resolved");
            assert!(validate_ingress_service_spec(&resolved).is_ok());
        }
        assert!(
            requested
                .container
                .environment
                .contains_key(CADDY_ADMIN_ENV)
        );
    }

    #[test]
    fn validators_reject_noncanonical_whole_spec_fields() {
        let requested = caddy_service_spec("caddy:test".into(), Vec::new(), None);

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

        for malformed in [
            wrong_mode,
            wrong_entrypoint,
            wrong_name,
            wrong_environment,
            wrong_ports,
            wrong_volume_graph,
            wrong_config_graph,
            wrong_requested_update,
        ] {
            assert!(validate_requested_ingress_service_spec(&malformed).is_err());
        }

        let mut wrong_resolved_update = requested
            .to_resolved(ServiceId::random(), ResolvedUpdateConfig::default())
            .expect("built-in Caddy mounts are resolved");
        wrong_resolved_update.update.monitor_millis = Some(1);
        assert!(validate_ingress_service_spec(&wrong_resolved_update).is_err());
    }
}
