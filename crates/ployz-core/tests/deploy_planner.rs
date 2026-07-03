use ployz_core::deploy::{
    DeployCleanupContainer, DeployPlan, DeployPlanError, DeployPlanStep, DeployPlanningInput,
    DeployPreparationInput, DeployRoute, DeployServicePlan, DeployServiceRequest,
    ExistingServiceReplica, ImageReference, ReplicaCount, ReplicaSlot, plan_service_deploy,
    prepare_deploy,
};
use ployz_core::ids::MachineId;
use ployz_core::machine_runtime::{
    ContainerEndpoint, ContainerRuntimeState, MachineContainerObservationSnapshot,
    ManagedContainerKind, ManagedContainerObservation,
};
use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};
use ployz_core::state::{ActiveRouteState, ActiveServiceState};
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id, namespace_revision_id,
    operation_id, service_id, step_id,
};

#[test]
fn namespace_revision_entry_identity_is_stable_for_same_service_shape() {
    let left = service_spec("svc_api", "ghcr.io/acme/api:rev-1", 1, None);
    let right = service_spec("svc_api", "ghcr.io/acme/api:rev-1", 3, Some((8080, 443)));

    assert_eq!(
        left.namespace_revision_entry_id(),
        right.namespace_revision_entry_id()
    );
    assert_eq!(
        left.namespace_revision_entry_id().as_str(),
        "4352bcaf84b6851f55968256ba6d1b84a1781fa273564663a9d2c5468bc8b14a"
    );
}

#[test]
fn namespace_revision_entry_identity_changes_for_service_or_image_change() {
    let base = service_spec("svc_api", "ghcr.io/acme/api:rev-1", 1, None);

    assert_ne!(
        base.namespace_revision_entry_id(),
        service_spec("svc_web", "ghcr.io/acme/api:rev-1", 1, None).namespace_revision_entry_id()
    );
    assert_eq!(
        service_spec("svc_web", "ghcr.io/acme/api:rev-1", 1, None)
            .namespace_revision_entry_id()
            .as_str(),
        "4b4081f4e61dd4321291177042ce920b4f10ae5b18e4f4c6f590da95555ac5eb"
    );
    assert_ne!(
        base.namespace_revision_entry_id(),
        service_spec("svc_api", "ghcr.io/acme/api:rev-2", 1, None).namespace_revision_entry_id()
    );
    assert_eq!(
        service_spec("svc_api", "ghcr.io/acme/api:rev-2", 1, None)
            .namespace_revision_entry_id()
            .as_str(),
        "3e7c6ae98166d296b94679c6bbe108f6782692c06f8489f0686075d2c3b8501e"
    );
}

#[test]
fn mutable_tag_repeats_as_same_namespace_revision_entry_identity() {
    assert_eq!(
        service_spec("svc_api", "nginx:latest", 1, None)
            .namespace_revision_entry_id()
            .as_str(),
        "238bbf3691ef5e99ec39743592000604b58356e2568703e01c2e6822a87f3a81"
    );
    assert_eq!(
        service_spec("svc_api", "nginx:latest", 1, None).namespace_revision_entry_id(),
        service_spec("svc_api", "nginx:latest", 3, Some((8080, 443))).namespace_revision_entry_id()
    );
}

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
fn service_plan_reuses_running_target_entry_containers() {
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
            active_revision: namespace_revision_entry_id("entry_old"),
        }),
        active_routes: Vec::new(),
        eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
        observed_machines: vec![observed_machine(
            "machine_b",
            [
                observed_container(
                    "machine_b",
                    "ctr_target",
                    "svc_api",
                    "entry_1",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::running_unroutable(),
                ),
                observed_container(
                    "machine_b",
                    "ctr_old",
                    "svc_api",
                    "entry_old",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::running_unroutable(),
                ),
                observed_container(
                    "machine_b",
                    "ctr_job",
                    "svc_api",
                    "entry_1",
                    ManagedContainerKind::Job,
                    ContainerRuntimeState::running_unroutable(),
                ),
                observed_container(
                    "machine_b",
                    "ctr_exited",
                    "svc_api",
                    "entry_1",
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
            cleanup_container_with_entry("machine_b", "ctr_target", "entry_1"),
            cleanup_container_with_entry("machine_b", "ctr_old", "entry_old"),
        ]
    );
}

#[test]
fn routed_deploy_preparation_reuses_matching_identity_regardless_of_endpoint_port() {
    let mut request = deploy_request(2);
    request.routes = vec![deploy_route("api.example.com", 443, 8080)];

    let prepared = prepare_deploy(DeployPreparationInput {
        request,
        active_service: None,
        active_routes: Vec::new(),
        eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
        observed_machines: vec![observed_machine(
            "machine_b",
            [
                observed_container(
                    "machine_b",
                    "ctr_wrong_port",
                    "svc_api",
                    "entry_1",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::running_at(endpoint("10.0.0.2", 3000)),
                ),
                observed_container(
                    "machine_b",
                    "ctr_target",
                    "svc_api",
                    "entry_1",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::running_at(endpoint("10.0.0.3", 8080)),
                ),
            ],
        )],
    })
    .expect("deploy preparation succeeds");

    assert_eq!(
        prepared.existing_replicas,
        vec![
            existing_replica("machine_b", "ctr_wrong_port"),
            existing_replica("machine_b", "ctr_target"),
        ]
    );
}

#[test]
fn deploy_preparation_commits_multiple_routes_and_removes_omitted_service_routes() {
    let mut request = deploy_request(1);
    request.routes = vec![
        deploy_route("api.example.com", 443, 8080),
        deploy_route("www.example.com", 443, 8080),
    ];

    let prepared = prepare_deploy(DeployPreparationInput {
        request,
        active_service: None,
        active_routes: vec![
            ActiveRouteState {
                namespace_id: namespace_id("default"),
                target: route_target("admin.example.com", 443),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
                revision_id: namespace_revision_entry_id("entry_old"),
            },
            ActiveRouteState {
                namespace_id: namespace_id("default"),
                target: route_target("other.example.com", 443),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_worker"),
                revision_id: namespace_revision_entry_id("entry_worker"),
            },
        ],
        eligible_machines: vec![machine_id("machine_a")],
        observed_machines: Vec::new(),
    })
    .expect("route reconciliation succeeds");

    assert_eq!(
        prepared.route_commits,
        vec![
            ActiveRouteState {
                namespace_id: namespace_id("default"),
                target: route_target("api.example.com", 443),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
                revision_id: namespace_revision_entry_id("entry_1"),
            },
            ActiveRouteState {
                namespace_id: namespace_id("default"),
                target: route_target("www.example.com", 443),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
                revision_id: namespace_revision_entry_id("entry_1"),
            },
        ]
    );
    assert_eq!(
        prepared.route_removals,
        vec![route_target("admin.example.com", 443)]
    );
}

#[test]
fn deploy_preparation_updates_endpoint_port_without_container_plan_changes() {
    let mut request = deploy_request(1);
    request.routes = vec![deploy_route("api.example.com", 443, 8080)];

    let prepared = prepare_deploy(DeployPreparationInput {
        request,
        active_service: None,
        active_routes: vec![ActiveRouteState {
            namespace_id: namespace_id("default"),
            target: route_target("api.example.com", 443),
            endpoint_port: route_port(3000),
            service_id: service_id("svc_api"),
            revision_id: namespace_revision_entry_id("entry_old"),
        }],
        eligible_machines: Vec::new(),
        observed_machines: Vec::new(),
    })
    .expect("routed deploy preparation succeeds");

    assert_eq!(
        prepared.route_commits,
        vec![ActiveRouteState {
            namespace_id: namespace_id("default"),
            target: route_target("api.example.com", 443),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
            revision_id: namespace_revision_entry_id("entry_1"),
        }]
    );
    assert!(prepared.route_removals.is_empty());
    assert!(prepared.existing_replicas.is_empty());
    assert!(prepared.cleanup_candidates.is_empty());
}

fn service_spec(
    service: &str,
    image: &str,
    replicas: u16,
    route: Option<(u16, u16)>,
) -> ployz_core::deploy::DeployServiceSpec {
    ployz_core::deploy::DeployServiceSpec {
        service_id: service_id(service),
        image: ImageReference::try_new(image).expect("valid image"),
        replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
        routes: route
            .map(|(endpoint_port, public_port)| {
                vec![deploy_route("api.example.com", public_port, endpoint_port)]
            })
            .unwrap_or_default(),
    }
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
        namespace_revision_id: namespace_revision_id("rev_1"),
        namespace_revision_entry_id: namespace_revision_entry_id("entry_1"),
        image: ImageReference::try_new("ghcr.io/acme/api:rev-1").expect("valid image"),
        replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
        routes: Vec::new(),
    }
}

fn deploy_plan(
    steps: Vec<DeployPlanStep>,
    cleanup_containers: Vec<DeployCleanupContainer>,
) -> DeployPlan {
    DeployPlan {
        namespace_id: namespace_id("svc_api"),
        namespace_revision_id: namespace_revision_id("rev_1"),
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
    cleanup_container_with_entry(machine, container, "entry_1")
}

fn cleanup_container_with_entry(
    machine: &str,
    container: &str,
    namespace_revision_entry: &str,
) -> DeployCleanupContainer {
    DeployCleanupContainer {
        machine_id: machine_id(machine),
        container_id: container_id(container),
        service_id: service_id("svc_api"),
        revision_id: namespace_revision_entry_id(namespace_revision_entry),
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
    ManagedContainerObservation {
        machine_id: machine_id(machine),
        container_id: container_id(container),
        service_id: service_id(service),
        revision_id: namespace_revision_entry_id(revision),
        operation_id: operation_id("op_existing"),
        step_id: step_id(container),
        kind,
        state,
    }
}
