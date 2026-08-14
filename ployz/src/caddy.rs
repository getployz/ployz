use std::{collections::BTreeMap, num::NonZeroU16};

use oci_client::{Client, Reference, secrets::RegistryAuth};
use ployz_core::{
    ContainerKind, ContainerObservation, ContainerPath, ContainerResources, HostBind, MachinePath,
    MachineSelector, Placement, PortPublication, PullPolicy, RequestedServiceSpec,
    ServiceContainerSpec, ServiceMode, ServiceMount, ServiceName, ServiceVolume,
    ServiceVolumeReference, TransportProtocol, UpdateConfig, VolumeSource,
};
use semver::Version;

pub const SERVICE_NAME: &str = "caddy";
pub const DATA_PATH: &str = "/var/lib/ployz/caddy";
pub const RUNTIME_PATH: &str = "/run/ployz/caddy";

pub async fn latest_image() -> Result<String, String> {
    let reference: Reference = "docker.io/library/caddy:latest"
        .parse()
        .map_err(|error| format!("parse Caddy image reference: {error}"))?;
    let response = Client::default()
        .list_tags(&reference, &RegistryAuth::Anonymous, None, None)
        .await
        .map_err(|error| format!("list Docker Hub Caddy tags: {error}"))?;
    Ok(select_image(&response.tags))
}

#[must_use]
pub fn select_image(tags: &[String]) -> String {
    tags.iter()
        .filter_map(|tag| {
            Version::parse(tag).ok().filter(|version| {
                version.major == 2
                    && version.pre.is_empty()
                    && version.build.is_empty()
                    && version.to_string() == *tag
            })
        })
        .max()
        .map_or_else(
            || "caddy:latest".into(),
            |version| format!("caddy:{version}"),
        )
}

#[must_use]
pub fn newest_existing_settings(
    observations: &[ContainerObservation],
) -> Option<(String, Vec<MachineSelector>, Option<String>)> {
    observations
        .iter()
        .filter(|container| {
            container.kind == ContainerKind::ServiceContainer
                && container.service_name.as_str() == SERVICE_NAME
        })
        .max_by_key(|container| {
            (
                container.created_at_unix_nanos,
                container.container_id.as_str(),
            )
        })
        .map(|container| {
            (
                container.resolved_spec.container.image.clone(),
                container.resolved_spec.placement.machines.clone(),
                container.resolved_spec.caddy_config.clone(),
            )
        })
}

#[must_use]
pub fn service_spec(
    image: String,
    machines: Vec<MachineSelector>,
    caddy_config: Option<String>,
) -> RequestedServiceSpec {
    let volume = ServiceVolumeReference::parse("caddy-data").expect("static volume is valid");
    let runtime = ServiceVolumeReference::parse("caddy-runtime").expect("static volume is valid");
    let mount = |volume: &ServiceVolumeReference, target: &str| ServiceMount {
        volume: volume.clone(),
        target: ContainerPath::parse(target).expect("static mount path is valid"),
        read_only: false,
    };
    let host_port = |port, protocol| PortPublication::Host {
        bind: HostBind::All,
        published_port: NonZeroU16::new(port).expect("Caddy ports are non-zero"),
        container_port: NonZeroU16::new(port).expect("Caddy ports are non-zero"),
        transport_protocol: protocol,
    };
    RequestedServiceSpec {
        name: ServiceName::parse(SERVICE_NAME).expect("static Service Name is valid"),
        mode: ServiceMode::Global,
        container: ServiceContainerSpec {
            image,
            command: vec![
                "caddy".into(),
                "run".into(),
                "-c".into(),
                "/config/Caddyfile".into(),
            ],
            entrypoint: Vec::new(),
            environment: BTreeMap::from([(
                "CADDY_ADMIN".into(),
                "unix//run/caddy/admin.sock".into(),
            )]),
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
            stop_grace_period_millis: None,
            sysctls: BTreeMap::new(),
            config_mounts: Vec::new(),
        },
        placement: Placement { machines },
        ports: vec![
            host_port(80, TransportProtocol::Tcp),
            host_port(443, TransportProtocol::Tcp),
            host_port(443, TransportProtocol::Udp),
        ],
        volumes: vec![
            ServiceVolume {
                reference: volume.clone(),
                source: VolumeSource::Bind {
                    machine_path: MachinePath::parse(DATA_PATH).expect("static data path is valid"),
                    create_machine_path: true,
                    propagation: None,
                    recursive: None,
                },
            },
            ServiceVolume {
                reference: runtime.clone(),
                source: VolumeSource::Bind {
                    machine_path: MachinePath::parse(RUNTIME_PATH)
                        .expect("static runtime path is valid"),
                    create_machine_path: true,
                    propagation: None,
                    recursive: None,
                },
            },
        ],
        mounts: vec![
            mount(&volume, "/config"),
            mount(&volume, "/data"),
            mount(&runtime, "/run/caddy"),
        ],
        configs: Vec::new(),
        pre_deploy: None,
        caddy_config,
        update: UpdateConfig::default(),
    }
}

#[cfg(test)]
mod tests {
    use ployz_core::{PortPublication, TransportProtocol};

    use super::*;

    #[test]
    fn selects_only_the_greatest_bare_two_x_y_tag() {
        assert_eq!(
            select_image(&[
                "2.9.1".into(),
                "2.10.0".into(),
                "2.11.0-rc.1".into(),
                "2.10".into(),
                "latest".into(),
                "3.0.0".into(),
            ]),
            "caddy:2.10.0"
        );
        assert_eq!(select_image(&["latest".into()]), "caddy:latest");
    }

    #[test]
    fn machine_add_reuses_the_newest_observed_caddy_settings() {
        let mut older: ContainerObservation = serde_json::from_value(serde_json::json!({
            "container_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "display_name": "caddy-old",
            "machine_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "service_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            "service_name": "caddy",
            "kind": "service_container",
            "runtime": { "state": "created" },
            "resolved_spec": {
                "service_id": "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
                "name": "caddy",
                "mode": { "mode": "global" },
                "container": { "image": "caddy:2.9.1", "pull_policy": "missing" }
            }
        }))
        .unwrap();
        older.created_at_unix_nanos = 1;
        let mut newer = older.clone();
        newer.container_id = ployz_core::ContainerId::parse("b".repeat(64)).unwrap();
        newer.created_at_unix_nanos = 2;
        newer.resolved_spec.container.image = "caddy:2.10.2".into();
        newer.resolved_spec.placement.machines = vec![MachineSelector::parse("edge").unwrap()];
        newer.resolved_spec.caddy_config = Some("{ admin off }".into());

        assert_eq!(
            newest_existing_settings(&[newer, older]),
            Some((
                "caddy:2.10.2".into(),
                vec![MachineSelector::parse("edge").unwrap()],
                Some("{ admin off }".into())
            ))
        );
        assert_eq!(newest_existing_settings(&[]), None);
    }

    #[test]
    fn service_spec_owns_caddy_ports_paths_and_admin_socket() {
        let spec = service_spec("caddy:2.10.0".into(), Vec::new(), None);

        assert_eq!(spec.mode, ServiceMode::Global);
        assert_eq!(
            spec.container.command,
            ["caddy", "run", "-c", "/config/Caddyfile"]
        );
        assert_eq!(
            spec.container
                .environment
                .get("CADDY_ADMIN")
                .map(String::as_str),
            Some("unix//run/caddy/admin.sock")
        );
        assert_eq!(spec.ports.len(), 3);
        assert!(matches!(
            spec.ports.get(2),
            Some(PortPublication::Host {
                transport_protocol: TransportProtocol::Udp,
                ..
            })
        ));
        assert_eq!(spec.mounts.len(), 3);
    }
}
