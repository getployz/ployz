use ployz_core::{
    HookContainer, MachineFailure, MachineId, PartialResult, RpcError, RpcErrorCode,
    ServiceContainer, ServiceId, ServiceName, derive_live_services,
};
use serde_json::json;

use super::*;

#[test]
fn process_sort_orders_match_the_cli_contract() {
    let beta = observation(
        'b',
        'b',
        "beta",
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Healthy,
        },
    );
    let alpha = observation(
        'a',
        'c',
        "alpha",
        ContainerRuntimeObservation::Running {
            health: HealthObservation::Unhealthy,
        },
    );
    let gamma = observation('c', 'a', "gamma", ContainerRuntimeObservation::Created);
    let hook = hook_observation(
        'd',
        'd',
        "delta",
        ContainerRuntimeObservation::Exited { code: 0 },
    );

    let beta = ServiceContainer::try_from(beta).unwrap();
    let alpha = ServiceContainer::try_from(alpha).unwrap();
    let gamma = ServiceContainer::try_from(gamma).unwrap();
    let hook = HookContainer::try_from(hook).unwrap();
    let mut containers = vec![
        ContainerRef::Service(&beta),
        ContainerRef::Service(&alpha),
        ContainerRef::Service(&gamma),
        ContainerRef::Hook(&hook),
    ];
    sort_processes(&mut containers, "service");
    assert_eq!(names(&containers), ["alpha", "beta", "delta", "gamma"]);
    sort_processes(&mut containers, "machine");
    assert_eq!(names(&containers), ["gamma", "beta", "alpha", "delta"]);
    sort_processes(&mut containers, "health");
    assert_eq!(names(&containers), ["alpha", "gamma", "beta", "delta"]);
}

#[test]
fn stop_options_are_only_read_for_stop_actions() {
    for (command, action) in [
        ("start", ContainerAction::Start),
        ("rm", ContainerAction::Remove),
    ] {
        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "service", command, "api"])
            .unwrap();
        assert_eq!(
            stop_options(leaf_matches(&matches), &[action]).unwrap(),
            (None, None)
        );
    }

    for (command, actions) in [
        ("stop", &[ContainerAction::Stop][..]),
        ("restart", &[ContainerAction::Stop, ContainerAction::Start]),
    ] {
        let matches = crate::cli::command()
            .try_get_matches_from(["ployz", "service", command, "api"])
            .unwrap();
        assert_eq!(
            stop_options(leaf_matches(&matches), actions).unwrap(),
            (Some("SIGTERM".into()), Some(10))
        );
    }
}

#[test]
fn observation_warnings_come_from_partial_result_failures_and_omissions() {
    let failed_id = MachineId::parse("2".repeat(32)).unwrap();
    let omitted_id = MachineId::parse("3".repeat(32)).unwrap();
    let live = derive_live_services(PartialResult::<Vec<ployz_core::ContainerObservation>, _> {
        successes: Vec::new(),
        failures: vec![MachineFailure {
            machine_id: failed_id,
            error: RpcError {
                code: RpcErrorCode::Unavailable,
                message: "offline".into(),
                details: serde_json::Value::Null,
            },
        }],
        omissions: vec![omitted_id],
    });

    assert_eq!(
        observation_warning_lines(&live),
        vec![
            "WARNING: Live Observation is observer-relative and not globally complete".to_string(),
            format!("WARNING: Machine {failed_id} failed: offline"),
            format!("WARNING: Machine {omitted_id} was omitted"),
        ]
    );
}

#[test]
fn lifecycle_selectors_deduplicate_names_and_ids() {
    let container = observation('a', 'a', "api", ContainerRuntimeObservation::Created);
    let service_id = container.service_id();
    let services = vec![ployz_core::ServiceObservation {
        identity: container.identity(),
        service_id,
        containers: vec![ServiceContainer::try_from(container).unwrap()],
        hook_containers: Vec::new(),
    }];
    let selectors = vec![
        ServiceSelector::parse("api").unwrap(),
        ServiceSelector::from(&service_id),
    ];

    assert_eq!(select_services(&services, &selectors).unwrap().len(), 1);
}

fn names<'a>(containers: &'a [ContainerRef<'a>]) -> Vec<&'a str> {
    containers
        .iter()
        .map(|container| container.as_observation().resolved_spec.name.as_str())
        .collect()
}

fn hook_observation(
    id: char,
    machine: char,
    name: &str,
    runtime: ContainerRuntimeObservation,
) -> ployz_core::ContainerObservation {
    let mut observation = observation(id, machine, name, runtime);
    observation
        .try_update(|parts| parts.kind = ployz_core::ContainerKind::PreDeployHook)
        .unwrap();
    observation
}

fn observation(
    id: char,
    machine: char,
    name: &str,
    runtime: ContainerRuntimeObservation,
) -> ployz_core::ContainerObservation {
    let service_id = ServiceId::parse(id.to_string().repeat(32)).unwrap();
    let service_name = ServiceName::parse(name).unwrap();
    ployz_core::ContainerObservation::try_from(ployz_core::ContainerObservationParts {
        container_id: ployz_core::ContainerId::parse(id.to_string().repeat(64)).unwrap(),
        display_name: name.into(),
        created_at_unix_nanos: 0,
        machine_id: MachineId::parse(machine.to_string().repeat(32)).unwrap(),
        namespace: ployz_core::Namespace::parse("app").unwrap(),
        kind: ployz_core::ContainerKind::ServiceContainer,
        runtime,
        effective_healthcheck: None,
        resolved_spec: serde_json::from_value(json!({
            "service_id": service_id,
            "name": service_name,
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": "alpine:3.23.3", "pull_policy": "missing" }
        }))
        .unwrap(),
        address: None,
        labels: Default::default(),
    })
    .unwrap()
}

#[test]
fn action_result_keeps_machine_failures_apart_from_container_failures() {
    let failed_id = MachineId::parse("2".repeat(32)).unwrap();
    let error = RpcError {
        code: RpcErrorCode::Unavailable,
        message: "offline".into(),
        details: serde_json::Value::Null,
    };
    let live = derive_live_services(PartialResult::<Vec<ployz_core::ContainerObservation>, _> {
        successes: Vec::new(),
        failures: vec![MachineFailure {
            machine_id: failed_id,
            error: error.clone(),
        }],
        omissions: Vec::new(),
    });
    let container_id = ContainerId::parse("c".repeat(64)).unwrap();
    let outcome = ServiceActionOutcome {
        containers: Vec::new(),
        container_failures: vec![ContainerFailure {
            machine_id: failed_id,
            container_id,
            error: error.clone(),
        }],
        wait_error: None,
        partial: true,
    };
    assert_eq!(
        serde_json::to_value(outcome.result(&live)).unwrap(),
        json!({
            "containers": [],
            "container_failures": [{
                "machine_id": failed_id,
                "container_id": container_id,
                "error": error,
            }],
            "failures": [{ "machine_id": failed_id, "error": error }],
            "omitted": [],
        })
    );
}
