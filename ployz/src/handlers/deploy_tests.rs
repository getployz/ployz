use std::{collections::BTreeMap, num::NonZeroU32};

use ployz_core::{
    AdvertisedEndpoint, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, Machine, MachineId, MachineName, MachineSubnet, ManagementAddress,
    MembershipObservation, PortPublication, ResolvedServiceSpec, RestartPolicy, WireGuardPublicKey,
};

use super::*;

#[test]
fn run_normalizes_supported_inputs_and_rejects_l4_ingress() {
    let command = crate::cli::command();
    let matches = command
        .try_get_matches_from([
            "ployz",
            "run",
            "--name",
            "api",
            "--env",
            "A=b",
            "--publish",
            "8080:80@host",
            "--volume",
            "data:/data:ro",
            "--volume",
            "cache:/cache:volume-nocopy",
            "alpine",
            "echo",
            "hello",
        ])
        .unwrap();
    let spec = run_spec(super::leaf_matches(&matches)).unwrap();
    assert_eq!(spec.name.as_str(), "api");
    assert_eq!(spec.container.command, ["echo", "hello"]);
    assert_eq!(spec.container.restart, RestartPolicy::No);
    assert_eq!(
        spec.container.environment.get("A").map(String::as_str),
        Some("b")
    );
    assert!(matches!(
        spec.ports.first(),
        Some(PortPublication::Host { .. })
    ));
    assert!(spec.mounts.first().is_some_and(|mount| mount.read_only));
    assert!(matches!(
        spec.volumes.get(1).map(|volume| &volume.source),
        Some(ployz_core::VolumeSource::Named { no_copy: true, .. })
    ));

    let matches = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "run",
            "--volume",
            "/tmp:/data:volume-nocopy",
            "alpine",
        ])
        .unwrap();
    assert!(run_spec(super::leaf_matches(&matches)).is_err());

    let matches = crate::cli::command()
        .try_get_matches_from(["ployz", "run", "--publish", "8080:80", "alpine"])
        .unwrap();
    assert!(run_spec(super::leaf_matches(&matches)).is_err());

    let global = crate::cli::command()
        .try_get_matches_from(["ployz", "run", "--mode", "global", "alpine"])
        .unwrap();
    assert_eq!(
        run_spec(super::leaf_matches(&global)).unwrap().mode,
        ServiceMode::Global
    );
    let global_replicas = crate::cli::command()
        .try_get_matches_from([
            "ployz",
            "run",
            "--mode",
            "global",
            "--replicas",
            "2",
            "alpine",
        ])
        .unwrap();
    assert!(run_spec(super::leaf_matches(&global_replicas)).is_err());
}

#[test]
fn run_pulls_untagged_images_as_latest() {
    let image = |image: &str| {
        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "run", "--name", "api", image, "sleep", "30"])
            .unwrap();
        run_spec(super::leaf_matches(&matches))
            .unwrap()
            .container
            .image
    };
    assert_eq!(image("alpine"), "alpine:latest");
    assert_eq!(image("alpine:3.20"), "alpine:3.20");
    let digest = format!("alpine@sha256:{}", "0".repeat(64));
    assert_eq!(image(&digest), digest);
    assert_eq!(image("localhost:5000/foo"), "localhost:5000/foo:latest");
}

#[test]
fn resolved_scale_input_changes_only_replicas() {
    let requested: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "alpine", "pull_policy": "missing" }
    }))
    .unwrap();
    let resolved = ResolvedServiceSpec {
        service_id: ServiceId::random(),
        name: requested.name.clone(),
        mode: requested.mode.clone(),
        container: requested.container.clone(),
        placement: requested.placement.clone(),
        ports: requested.ports.clone(),
        volumes: requested.volumes.clone(),
        mounts: requested.mounts.clone(),
        configs: requested.configs.clone(),
        pre_deploy: None,
        caddy_config: None,
        update: Default::default(),
    };
    let mut scaled = requested_from_resolved(&resolved);
    scaled.mode = ServiceMode::Replicated {
        replicas: NonZeroU32::new(3).unwrap(),
    };
    let mut expected = requested_from_resolved(&resolved);
    expected.mode = scaled.mode.clone();
    assert_eq!(scaled, expected);
}

#[test]
fn run_forms_share_normalization() {
    let root = crate::cli::command()
        .try_get_matches_from(["ployz", "run", "--name", "api", "alpine"])
        .unwrap();
    let nested = crate::cli::command()
        .try_get_matches_from(["ployz", "service", "run", "--name", "api", "alpine"])
        .unwrap();
    assert_eq!(
        run_spec(super::leaf_matches(&root)).unwrap(),
        run_spec(super::leaf_matches(&nested)).unwrap()
    );
}

#[test]
fn scale_plan_rejects_global_noops_matching_and_uses_one_mixed_spec() {
    let service_id = ServiceId::random();
    let replicas = |count: u32| NonZeroU32::new(count).unwrap();
    let snapshot = |containers: Vec<ContainerObservation>| DeploySnapshot {
        machines: vec![machine()],
        containers,
        ..Default::default()
    };

    assert_eq!(
        scale_plan(
            &snapshot(vec![observation(
                &service_id,
                ServiceMode::Global,
                "v1",
                '1'
            )]),
            "api",
            replicas(2),
        )
        .unwrap_err()
        .to_string(),
        "global services cannot be scaled"
    );

    let replicated = ServiceMode::Replicated {
        replicas: replicas(1),
    };
    assert!(
        scale_plan(
            &snapshot(vec![observation(
                &service_id,
                replicated.clone(),
                "v1",
                '1'
            )]),
            "api",
            replicas(1),
        )
        .unwrap()
        .is_none()
    );

    assert!(
        scale_plan(
            &snapshot(vec![observation(
                &service_id,
                ServiceMode::Replicated {
                    replicas: replicas(3),
                },
                "v1",
                '1',
            )]),
            "api",
            replicas(3),
        )
        .unwrap()
        .is_some()
    );

    let mixed = scale_plan(
        &snapshot(vec![
            observation(&service_id, replicated.clone(), "v1", '1'),
            observation(&service_id, replicated, "v2", '2'),
        ]),
        "api",
        replicas(3),
    )
    .unwrap()
    .unwrap();
    let image = mixed
        .operations()
        .iter()
        .find_map(|operation| match operation {
            DeployOperation::RunContainer { spec, .. } | DeployOperation::RunHook { spec, .. } => {
                Some(spec.container.image.as_str())
            }
            DeployOperation::ReplaceContainer(replacement) => {
                Some(replacement.spec.container.image.as_str())
            }
            DeployOperation::CreateVolume { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::StopHook { .. }
            | DeployOperation::Sequence { .. } => None,
        });
    assert_eq!(image, Some("v1"));
}

fn machine() -> ployz_core::MachineObservation {
    ployz_core::MachineObservation {
        machine: Machine {
            id: MachineId::parse("a".repeat(32)).unwrap(),
            name: MachineName::parse("machine-1").unwrap(),
            subnet: MachineSubnet("10.210.1.0/24".parse().unwrap()),
            management_address: ManagementAddress("::1".parse().unwrap()),
            public_key: WireGuardPublicKey([1; 32]),
            public_ip: None,
            advertised_endpoints: Vec::<AdvertisedEndpoint>::new(),
            runtime: Default::default(),
        },
        membership: MembershipObservation::Up,
        selected_endpoint: None,
    }
}

fn observation(
    service_id: &ServiceId,
    mode: ServiceMode,
    image: &str,
    id: char,
) -> ContainerObservation {
    let requested: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "api",
        "mode": mode,
        "container": { "image": image, "pull_policy": "missing" }
    }))
    .unwrap();
    let resolved = ResolvedServiceSpec {
        service_id: *service_id,
        name: requested.name.clone(),
        mode: requested.mode,
        container: requested.container,
        placement: requested.placement,
        ports: requested.ports,
        volumes: requested.volumes,
        mounts: requested.mounts,
        configs: requested.configs,
        pre_deploy: None,
        caddy_config: None,
        update: Default::default(),
    };
    ContainerObservation {
        container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
        display_name: format!("api-{id}"),
        created_at_unix_nanos: 0,
        machine_id: machine().machine.id,
        service_id: *service_id,
        service_name: requested.name,
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: ployz_core::HealthObservation::NotConfigured,
        },
        effective_healthcheck: None,
        resolved_spec: resolved,
        address: None,
        labels: BTreeMap::new(),
    }
}
