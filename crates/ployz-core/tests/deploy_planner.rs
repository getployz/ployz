use ployz_core::deploy::{
    DeployCleanupContainer, DeployPlan, DeployPlanError, DeployPlanStep, DeployPlanningInput,
    DeployPreparationError, DeployPreparationInput, DeployRoute, DeployServicePlan,
    DeployServiceRequest, ExistingServiceReplica, ImageReference, ReplicaCount, ReplicaSlot,
    plan_service_deploy, prepare_deploy,
};
use ployz_core::ids::MachineId;
use ployz_core::machine_runtime::{
    ContainerEndpoint, ContainerRuntimeState, MachineContainerObservationSnapshot,
    ManagedContainerKind, ManagedContainerObservation,
};
use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};
use ployz_core::state::{ActiveRouteState, ActiveServiceState};
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, operation_id, revision_id, service_id, step_id,
};

#[test]
fn new_service_plan_runs_replicas_across_eligible_machines() {
    assert_eq!(
        plan_service_deploy(planning_input(
            3,
            [machine_id("machine_a"), machine_id("machine_b")]
        ))
        .expect("plan succeeds"),
        deploy_plan(
            vec![
                run_step("machine_a", 1),
                run_step("machine_b", 2),
                run_step("machine_a", 3),
            ],
            Vec::new()
        )
    );
}

#[test]
fn service_plan_reuses_running_target_revision_containers() {
    let mut input = planning_input(3, [machine_id("machine_a"), machine_id("machine_b")]);
    input.existing_replicas = vec![existing_replica("machine_b", "ctr_existing")];

    assert_eq!(
        plan_service_deploy(input).expect("plan succeeds"),
        deploy_plan(
            vec![
                use_existing_step("machine_b", "ctr_existing", 1),
                run_step("machine_a", 2),
                run_step("machine_b", 3),
            ],
            Vec::new()
        )
    );
}

#[test]
fn service_plan_counts_duplicate_observations_once() {
    let mut input = planning_input(2, [machine_id("machine_a")]);
    input.existing_replicas = vec![
        existing_replica("machine_b", "ctr_existing"),
        existing_replica("machine_b", "ctr_existing"),
    ];

    assert_eq!(
        plan_service_deploy(input).expect("plan succeeds"),
        deploy_plan(
            vec![
                use_existing_step("machine_b", "ctr_existing", 1),
                run_step("machine_a", 2),
            ],
            Vec::new()
        )
    );
}

#[test]
fn service_plan_does_not_require_eligible_machines_when_reality_already_satisfies_replicas() {
    let mut input = planning_input(1, []);
    input.existing_replicas = vec![existing_replica("machine_b", "ctr_existing")];

    assert_eq!(
        plan_service_deploy(input).expect("existing reality satisfies target"),
        deploy_plan(
            vec![use_existing_step("machine_b", "ctr_existing", 1)],
            Vec::new()
        )
    );
}

#[test]
fn service_plan_cleans_up_unselected_service_containers_after_success() {
    let mut input = planning_input(1, [machine_id("machine_a")]);
    input.existing_replicas = vec![
        existing_replica("machine_b", "ctr_target_keep"),
        existing_replica("machine_b", "ctr_target_extra"),
    ];
    input.cleanup_candidates = vec![
        cleanup_container("machine_b", "ctr_target_keep"),
        cleanup_container("machine_b", "ctr_target_extra"),
        cleanup_container("machine_b", "ctr_old"),
    ];

    assert_eq!(
        plan_service_deploy(input).expect("plan succeeds"),
        deploy_plan(
            vec![use_existing_step("machine_b", "ctr_target_extra", 1)],
            vec![
                cleanup_container("machine_b", "ctr_old"),
                cleanup_container("machine_b", "ctr_target_keep"),
            ],
        )
    );
}

#[test]
fn deploy_plan_requires_eligible_machine() {
    assert_eq!(
        plan_service_deploy(planning_input(1, [])),
        Err(DeployPlanError::NoEligibleMachines)
    );
}

#[test]
fn deploy_preparation_uses_active_revision_and_running_target_replicas() {
    let prepared = prepare_deploy(DeployPreparationInput {
        request: deploy_request(2),
        active_service: Some(ActiveServiceState {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
            active_revision: revision_id("rev_old"),
        }),
        active_route: None,
        eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
        observed_machines: vec![observed_machine(
            "machine_b",
            [
                observed_container(
                    "machine_b",
                    "ctr_target",
                    "svc_api",
                    "rev_1",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::running_unroutable(),
                ),
                observed_container(
                    "machine_b",
                    "ctr_old",
                    "svc_api",
                    "rev_old",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::running_unroutable(),
                ),
                observed_container(
                    "machine_b",
                    "ctr_job",
                    "svc_api",
                    "rev_1",
                    ManagedContainerKind::Job,
                    ContainerRuntimeState::running_unroutable(),
                ),
                observed_container(
                    "machine_b",
                    "ctr_exited",
                    "svc_api",
                    "rev_1",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::Exited,
                ),
            ],
        )],
    })
    .expect("deploy preparation succeeds");

    assert_eq!(prepared.request, deploy_request(2));
    assert_eq!(
        prepared.eligible_machines,
        vec![machine_id("machine_a"), machine_id("machine_b")]
    );
    assert_eq!(
        prepared.existing_replicas,
        vec![existing_replica("machine_b", "ctr_target")]
    );
    assert_eq!(
        prepared.cleanup_candidates,
        vec![
            cleanup_container_with_revision("machine_b", "ctr_target", "rev_1"),
            cleanup_container_with_revision("machine_b", "ctr_old", "rev_old"),
        ]
    );
}

#[test]
fn deploy_preparation_ignores_same_service_id_in_other_namespace() {
    let prepared = prepare_deploy(DeployPreparationInput {
        request: deploy_request(1),
        active_service: None,
        active_route: None,
        eligible_machines: vec![machine_id("machine_a")],
        observed_machines: vec![observed_machine(
            "machine_b",
            [observed_container_in_namespace(
                "other",
                "machine_b",
                "ctr_other_namespace",
                "svc_api",
                "rev_1",
                ManagedContainerKind::Service,
                ContainerRuntimeState::running_unroutable(),
            )],
        )],
    })
    .expect("deploy preparation succeeds");

    assert!(prepared.existing_replicas.is_empty());
    assert!(prepared.cleanup_candidates.is_empty());
}

#[test]
fn routed_deploy_preparation_reuses_only_matching_endpoint_port() {
    let mut request = deploy_request(2);
    request.route = Some(deploy_route("api.example.com", 443, 8080));

    let prepared = prepare_deploy(DeployPreparationInput {
        request,
        active_service: None,
        active_route: None,
        eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
        observed_machines: vec![observed_machine(
            "machine_b",
            [
                observed_container(
                    "machine_b",
                    "ctr_wrong_port",
                    "svc_api",
                    "rev_1",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::running_at(endpoint("10.0.0.2", 3000)),
                ),
                observed_container(
                    "machine_b",
                    "ctr_target",
                    "svc_api",
                    "rev_1",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::running_at(endpoint("10.0.0.3", 8080)),
                ),
            ],
        )],
    })
    .expect("deploy preparation succeeds");

    assert_eq!(
        prepared.existing_replicas,
        vec![existing_replica("machine_b", "ctr_target")]
    );
}

#[test]
fn deploy_preparation_rejects_active_route_for_another_target() {
    let mut request = deploy_request(1);
    request.route = Some(deploy_route("api.example.com", 443, 8080));

    assert_eq!(
        prepare_deploy(DeployPreparationInput {
            request,
            active_service: None,
            active_route: Some(ActiveRouteState {
                namespace_id: namespace_id("default"),
                target: route_target("admin.example.com", 443),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
                revision_id: revision_id("rev_old"),
            }),
            eligible_machines: vec![machine_id("machine_a")],
            observed_machines: Vec::new(),
        }),
        Err(DeployPreparationError::ActiveRouteMismatch {
            expected_route: route_target("api.example.com", 443),
            actual_route: route_target("admin.example.com", 443),
        })
    );
}

#[test]
fn deploy_preparation_builds_route_commit_request_for_routed_deploy() {
    let mut request = deploy_request(1);
    request.route = Some(deploy_route("api.example.com", 443, 8080));

    let prepared = prepare_deploy(DeployPreparationInput {
        request,
        active_service: None,
        active_route: Some(ActiveRouteState {
            namespace_id: namespace_id("default"),
            target: route_target("api.example.com", 443),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_old"),
        }),
        eligible_machines: vec![machine_id("machine_a")],
        observed_machines: Vec::new(),
    })
    .expect("routed deploy preparation succeeds");

    let route_commit = prepared
        .route_commit
        .expect("routed deploy has route state");
    assert_eq!(
        route_commit,
        ActiveRouteState {
            namespace_id: namespace_id("default"),
            target: route_target("api.example.com", 443),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_1"),
        }
    );
}

fn planning_input(
    replicas: u16,
    eligible_machines: impl IntoIterator<Item = MachineId>,
) -> DeployPlanningInput {
    DeployPlanningInput {
        request: deploy_request(replicas),
        eligible_machines: eligible_machines.into_iter().collect(),
        existing_replicas: Vec::new(),
        cleanup_candidates: Vec::new(),
    }
}

fn deploy_request(replicas: u16) -> DeployServiceRequest {
    DeployServiceRequest {
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        target_revision: revision_id("rev_1"),
        image: ImageReference::try_new("ghcr.io/acme/api:rev-1").expect("valid image"),
        replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
        route: None,
    }
}

fn deploy_plan(
    steps: Vec<DeployPlanStep>,
    cleanup_containers: Vec<DeployCleanupContainer>,
) -> DeployPlan {
    DeployPlan {
        namespace_id: namespace_id("svc_api"),
        target_revision: revision_id("rev_1"),
        services: vec![DeployServicePlan {
            service_id: service_id("svc_api"),
            steps,
        }],
        cleanup_containers,
    }
}

fn deploy_route(hostname: &str, public_port: u16, endpoint_port: u16) -> DeployRoute {
    DeployRoute {
        target: route_target(hostname, public_port),
        endpoint_port: route_port(endpoint_port),
    }
}

fn use_existing_step(machine: &str, container: &str, slot: u16) -> DeployPlanStep {
    DeployPlanStep::UseExistingContainer {
        machine_id: machine_id(machine),
        container_id: container_id(container),
        slot: ReplicaSlot::try_new(slot).expect("valid replica slot"),
    }
}

fn run_step(machine: &str, slot: u16) -> DeployPlanStep {
    DeployPlanStep::RunContainer {
        machine_id: machine_id(machine),
        slot: ReplicaSlot::try_new(slot).expect("valid replica slot"),
    }
}

fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget {
        hostname: RouteHostname::try_new(hostname).expect("valid route hostname"),
        port: route_port(port),
    }
}

fn route_port(port: u16) -> RoutePort {
    RoutePort::try_new(port).expect("valid route port")
}

fn endpoint(ip: &str, port: u16) -> ContainerEndpoint {
    ContainerEndpoint {
        ip: ip.parse().expect("valid container endpoint ip"),
        port: route_port(port),
    }
}

fn existing_replica(machine: &str, container: &str) -> ExistingServiceReplica {
    ExistingServiceReplica {
        machine_id: machine_id(machine),
        container_id: container_id(container),
    }
}

fn cleanup_container(machine: &str, container: &str) -> DeployCleanupContainer {
    cleanup_container_with_revision(machine, container, "rev_1")
}

fn cleanup_container_with_revision(
    machine: &str,
    container: &str,
    revision: &str,
) -> DeployCleanupContainer {
    DeployCleanupContainer {
        machine_id: machine_id(machine),
        container_id: container_id(container),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        revision_id: revision_id(revision),
        operation_id: operation_id("op_existing"),
        step_id: step_id(container),
        kind: ManagedContainerKind::Service,
        endpoint_port: None,
    }
}

fn observed_machine(
    machine: &str,
    containers: impl IntoIterator<Item = ManagedContainerObservation>,
) -> MachineContainerObservationSnapshot {
    MachineContainerObservationSnapshot::try_new(machine_id(machine), containers)
        .expect("valid observation snapshot")
}

fn observed_container(
    machine: &str,
    container: &str,
    service: &str,
    revision: &str,
    kind: ManagedContainerKind,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    observed_container_in_namespace(
        "default", machine, container, service, revision, kind, state,
    )
}

fn observed_container_in_namespace(
    namespace: &str,
    machine: &str,
    container: &str,
    service: &str,
    revision: &str,
    kind: ManagedContainerKind,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        machine_id: machine_id(machine),
        container_id: container_id(container),
        namespace_id: namespace_id(namespace),
        service_id: service_id(service),
        revision_id: revision_id(revision),
        operation_id: operation_id("op_existing"),
        step_id: step_id(container),
        kind,
        state,
    }
}
