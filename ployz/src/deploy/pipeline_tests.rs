use std::{collections::BTreeMap, num::NonZeroU32};

use ployz_core::{
    AdvertisedEndpoint, ContainerId, ContainerKind, ContainerObservation,
    ContainerRuntimeObservation, Machine, MachineFailure, MachineId, MachineName, MachineSuccess,
    MembershipObservation, PartialResult, ProjectName, RequestedServiceSpec, RpcError,
    RpcErrorCode, ServiceId, ServiceMode, ServiceSelector, WireGuardPublicKey,
};
use serde_json::Value;

use super::*;
use crate::deploy::{DeployOperation, DeploySnapshot, IngressContext, preview_deploy};

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
    let matching = choose_scale_spec(
        &snapshot(vec![observation(
            &service_id,
            replicated.clone(),
            "v1",
            '1',
        )]),
        &ServiceSelector::parse("api").unwrap(),
        replicas(1),
    )
    .unwrap();
    assert_eq!(matching.project_name.as_str(), "app");
    assert!(matching.requested.is_none());

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
        .requested
        .is_some()
    );

    let mixed_snapshot = snapshot(vec![
        observation(&service_id, replicated.clone(), "v1", '1'),
        observation(&service_id, replicated, "v2", '2'),
    ]);
    let choice = choose_scale_spec(
        &mixed_snapshot,
        &ServiceSelector::parse("api").unwrap(),
        replicas(3),
    )
    .unwrap();
    let requested = choice.requested.unwrap();
    assert_eq!(choice.project_name.as_str(), "app");
    assert_eq!(requested.container.image, "v1");
    let mixed = preview_deploy(
        &DeployIntent::apply_one(choice.project_name, requested, PlanOptions::default()),
        &mixed_snapshot,
        IngressContext::default(),
    )
    .unwrap();
    let image = mixed
        .operations
        .iter()
        .find_map(|row| match &row.operation {
            DeployOperation::RunContainer { spec, .. } | DeployOperation::RunHook { spec, .. } => {
                Some(spec.container.image.as_str())
            }
            DeployOperation::ReplaceContainer(replacement) => {
                Some(replacement.spec.container.image.as_str())
            }
            DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::StopHook { .. }
            | DeployOperation::RemoveVolume { .. } => None,
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
    hook.try_update(|parts| parts.kind = ContainerKind::PreDeployHook)
        .unwrap();
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
        .requested
        .is_none()
    );
}

#[test]
fn scale_does_not_select_a_service_owned_by_another_project() {
    let service_id = ServiceId::random();
    let replicated = ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    };
    let mut system = observation(&service_id, replicated, "v1", '1');
    system
        .try_update(|parts| parts.project_name = ProjectName::system())
        .unwrap();
    let snapshot = DeploySnapshot {
        machines: vec![machine()],
        containers: vec![system],
        ..Default::default()
    };
    assert_eq!(
        choose_scale_spec(
            &snapshot,
            &ServiceSelector::parse("shop/api").unwrap(),
            NonZeroU32::new(2).unwrap(),
        )
        .unwrap_err()
        .to_string(),
        "Service \"shop/api\" was not found"
    );
    let choice = choose_scale_spec(
        &snapshot,
        &ServiceSelector::parse("api").unwrap(),
        NonZeroU32::new(2).unwrap(),
    )
    .unwrap();
    assert_eq!(choice.project_name.as_str(), "ployz-system");
    assert!(choice.requested.is_some());
}

#[test]
fn scale_uses_the_selected_qualified_service_project() {
    let replicas = |count: u32| NonZeroU32::new(count).unwrap();
    let replicated = ServiceMode::Replicated {
        replicas: replicas(1),
    };
    let mut staging = observation(&ServiceId::random(), replicated.clone(), "v1", '1');
    staging
        .try_update(|parts| parts.project_name = ProjectName::parse("shop-staging").unwrap())
        .unwrap();
    staging
        .try_update(|parts| {
            parts.resolved_spec.name = ployz_core::ServiceName::parse("web").unwrap()
        })
        .unwrap();

    let mut prod = observation(&ServiceId::random(), replicated, "v2", '2');
    prod.try_update(|parts| parts.project_name = ProjectName::parse("shop-prod").unwrap())
        .unwrap();
    prod.try_update(|parts| {
        parts.resolved_spec.name = ployz_core::ServiceName::parse("web").unwrap()
    })
    .unwrap();

    let snapshot = DeploySnapshot {
        machines: vec![machine()],
        containers: vec![staging, prod],
        ..Default::default()
    };

    let choice = choose_scale_spec(
        &snapshot,
        &ServiceSelector::parse("shop-staging/web").unwrap(),
        replicas(2),
    )
    .unwrap();
    let requested = choice.requested.unwrap();
    assert_eq!(choice.project_name.as_str(), "shop-staging");
    assert_eq!(requested.name.as_str(), "web");
    assert_eq!(requested.container.image, "v1");
    assert!(
        choose_scale_spec(
            &snapshot,
            &ServiceSelector::parse("web").unwrap(),
            replicas(2),
        )
        .is_err()
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
            .map(DeployWarning::from)
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
    assert_eq!(
        DeployWarning::ObserverRelativeHostnameConflict.to_string(),
        "Hostname conflict detection is observer-relative to this Machine's current visible fan-out and does not claim uniqueness."
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
        observation_warnings(
            ObservationKind::Container,
            &result.failures,
            &result.omissions,
        ),
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
    ployz_core::MachineObservation::new(
        Machine {
            id: MachineId::parse("a".repeat(32)).unwrap(),
            name: MachineName::parse("machine-1").unwrap(),
            subnet: "10.210.1.0/24".parse().unwrap(),
            public_key: WireGuardPublicKey([1; 32]),
            public_ip: None,
            advertised_endpoints: Vec::<AdvertisedEndpoint>::new(),
            runtime: Default::default(),
        },
        MembershipObservation::Up,
    )
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
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
        display_name: format!("api-{id}"),
        created_at_unix_nanos: 0,
        machine_id: machine().machine.id,
        project_name: ProjectName::parse("app").unwrap(),
        kind: ContainerKind::ServiceContainer,
        runtime: ContainerRuntimeObservation::Running {
            health: ployz_core::HealthObservation::NotConfigured,
        },
        effective_healthcheck: None,
        resolved_spec: resolved,
        address: None,
        labels: BTreeMap::new(),
    })
    .unwrap()
}
