use std::{collections::BTreeMap, num::NonZeroU32};

use ployz_core::{
    AdvertisedEndpoint, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, Machine, MachineFailure, MachineId, MachineName, MachineSuccess,
    ManagementAddress, MembershipObservation, PartialResult, RequestedServiceSpec, RpcError,
    RpcErrorCode, ServiceId, ServiceMode, ServiceSelector, WireGuardPublicKey,
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
        choose_scale_spec(
            &snapshot(vec![observation(
                &service_id,
                ServiceMode::Global,
                "v1",
                '1'
            )]),
            &ServiceSelector::parse("api").unwrap(),
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
        choose_scale_spec(
            &snapshot(vec![observation(
                &service_id,
                replicated.clone(),
                "v1",
                '1'
            )]),
            &ServiceSelector::parse("api").unwrap(),
            replicas(1),
        )
        .unwrap()
        .is_none()
    );

    assert!(
        choose_scale_spec(
            &snapshot(vec![observation(
                &service_id,
                ServiceMode::Replicated {
                    replicas: replicas(3),
                },
                "v1",
                '1',
            )]),
            &ServiceSelector::parse("api").unwrap(),
            replicas(3),
        )
        .unwrap()
        .is_some()
    );

    let mixed_snapshot = snapshot(vec![
        observation(&service_id, replicated.clone(), "v1", '1'),
        observation(&service_id, replicated, "v2", '2'),
    ]);
    let requested = choose_scale_spec(
        &mixed_snapshot,
        &ServiceSelector::parse("api").unwrap(),
        replicas(3),
    )
    .unwrap()
    .unwrap();
    assert_eq!(requested.container.image, "v1");
    let mixed = plan_deploy(
        &DeployIntent::apply_one(requested, PlanOptions::default()),
        &mixed_snapshot,
    )
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
        choose_scale_spec(
            &snapshot(vec![hook.clone()]),
            &ServiceSelector::parse("api").unwrap(),
            replicas(2),
        )
        .unwrap_err()
        .to_string(),
        "cannot scale a service without regular containers"
    );
    assert!(
        choose_scale_spec(
            &snapshot(vec![observation(&service_id, replicated, "v1", '1'), hook]),
            &ServiceSelector::parse("api").unwrap(),
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
    let resolved = requested.to_resolved(ServiceId::random(), Default::default());
    let mut scaled = resolved.to_requested();
    scaled.mode = ServiceMode::Replicated {
        replicas: NonZeroU32::new(3).unwrap(),
    };
    let mut expected = resolved.to_requested();
    expected.mode = scaled.mode.clone();
    assert_eq!(scaled, expected);
}

#[test]
fn deploy_warning_display_is_the_cli_line_body() {
    let spec: RequestedServiceSpec = serde_json::from_value(serde_json::json!({
        "name": "web",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": { "image": "nginx", "pull_policy": "missing" },
        "ports": [{
            "mode": "ingress",
            "hostname": { "kind": "explicit", "hostname": "app.example.com" },
            "load_balancer_port": 443,
            "container_port": 8080,
            "http_protocol": "https"
        }]
    }))
    .unwrap();
    let warning =
        crate::dns::ingress_dns_warnings([&spec], &["192.0.2.1".parse().unwrap()], |_| Vec::new())
            .into_iter()
            .map(DeployWarning::IngressHostname)
            .next()
            .expect("unresolved Ingress Hostname warns");
    assert_eq!(
        warning.to_string(),
        "Ingress Hostname app.example.com does not resolve; it should resolve to 192.0.2.1. A certificate cannot be issued until it points at this Cluster."
    );
    assert_eq!(
        DeployWarning::ObservationOmitted {
            kind: ObservationKind::Volume,
            machine_id: MachineId::parse("c".repeat(32)).unwrap(),
        }
        .to_string(),
        format!(
            "volume observation omitted {}",
            MachineId::parse("c".repeat(32)).unwrap()
        )
    );
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
            DeployWarning::ObservationFailed {
                kind: ObservationKind::Container,
                machine_id: MachineId::parse("b".repeat(32)).unwrap(),
                message: "container listing failed".into(),
            },
            DeployWarning::ObservationOmitted {
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
    let resolved = requested.to_resolved(*service_id, Default::default());
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
