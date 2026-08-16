use std::{collections::BTreeMap, num::NonZeroU32};

use ployz_core::{
    AdvertisedEndpoint, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, Machine, MachineFailure, MachineId, MachineName, MachineSuccess,
    ManagementAddress, MembershipObservation, PartialResult, RequestedServiceSpec,
    ResolvedServiceSpec, RpcError, RpcErrorCode, ServiceId, ServiceMode, WireGuardPublicKey,
};
use serde_json::Value;

use super::*;
use crate::deploy::{DeployOperation, DeploySnapshot};

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
        .operations
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
            | DeployOperation::StopHook { .. } => None,
        });
    assert_eq!(image, Some("v1"));
}

#[test]
fn scale_plan_accepts_only_service_containers() {
    let service_id = ServiceId::random();
    let replicas = |count: u32| NonZeroU32::new(count).unwrap();
    let replicated = ServiceMode::Replicated {
        replicas: replicas(1),
    };
    let mut hook = observation(&service_id, replicated.clone(), "hook", '2');
    hook.kind = ContainerKind::PreDeployHook;
    let snapshot = |containers: Vec<ContainerObservation>| DeploySnapshot {
        machines: vec![machine()],
        containers,
        ..Default::default()
    };

    assert_eq!(
        scale_plan(&snapshot(vec![hook.clone()]), "api", replicas(2))
            .unwrap_err()
            .to_string(),
        "cannot scale a service without regular containers"
    );
    assert!(
        scale_plan(
            &snapshot(vec![observation(&service_id, replicated, "v1", '1'), hook]),
            "api",
            replicas(1),
        )
        .unwrap()
        .is_none()
    );
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
    let mut scaled = requested_from_resolved(&resolved).unwrap();
    scaled.mode = ServiceMode::Replicated {
        replicas: NonZeroU32::new(3).unwrap(),
    };
    let mut expected = requested_from_resolved(&resolved).unwrap();
    expected.mode = scaled.mode.clone();
    assert_eq!(scaled, expected);
}

#[test]
fn observation_warnings_keep_failures_and_omissions_distinct() {
    let result = PartialResult {
        successes: vec![MachineSuccess {
            machine_id: MachineId::parse("a".repeat(32)).unwrap(),
            value: (),
        }],
        failures: vec![MachineFailure {
            machine_id: MachineId::parse("b".repeat(32)).unwrap(),
            error: RpcError {
                code: RpcErrorCode::Unavailable,
                message: "container listing failed".into(),
                details: Value::Null,
            },
        }],
        omissions: vec![MachineId::parse("c".repeat(32)).unwrap()],
    };

    assert_eq!(
        observation_warnings(ObservationKind::Container, &result),
        [
            ObservationWarning::Failed {
                kind: ObservationKind::Container,
                machine_id: MachineId::parse("b".repeat(32)).unwrap(),
                message: "container listing failed".into(),
            },
            ObservationWarning::Omitted {
                kind: ObservationKind::Container,
                machine_id: MachineId::parse("c".repeat(32)).unwrap(),
            },
        ]
    );
}

fn machine() -> ployz_core::MachineObservation {
    ployz_core::MachineObservation {
        machine: Machine {
            id: MachineId::parse("a".repeat(32)).unwrap(),
            name: MachineName::parse("machine-1").unwrap(),
            subnet: "10.210.1.0/24".parse().unwrap(),
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
