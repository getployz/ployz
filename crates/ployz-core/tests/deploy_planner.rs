use ployz_core::deploy::{
    DeployCleanupContainer, DeployPlan, DeployPlanError, DeployPlanStep, DeployPlanningInput,
    DeployPreparationError, DeployPreparationInput, DeployRequest, DeployRoute,
    ExistingServiceReplica, ImageReference, ReplicaCount, ReplicaSlot, plan_service_deploy,
    prepare_deploy,
};
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::{
    ContainerEndpoint, ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
    NodeContainerObservationSnapshot,
};
use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};
use ployz_core::state::{
    ActiveRouteState, ActiveServiceState, ExpectedActiveRoute, ExpectedActiveService,
};

#[test]
fn new_service_plan_runs_replicas_across_eligible_nodes() {
    assert_eq!(
        plan_service_deploy(planning_input(3, [node_id("node_a"), node_id("node_b")]))
            .expect("plan succeeds"),
        DeployPlan {
            service_id: service_id("svc_api"),
            target_revision: revision_id("rev_1"),
            steps: vec![
                run_step("node_a", 1),
                run_step("node_b", 2),
                run_step("node_a", 3),
            ],
            cleanup_containers: Vec::new(),
        }
    );
}

#[test]
fn service_plan_reuses_running_target_revision_containers() {
    let mut input = planning_input(3, [node_id("node_a"), node_id("node_b")]);
    input.existing_replicas = vec![existing_replica("node_b", "ctr_existing")];

    assert_eq!(
        plan_service_deploy(input).expect("plan succeeds"),
        DeployPlan {
            service_id: service_id("svc_api"),
            target_revision: revision_id("rev_1"),
            steps: vec![
                use_existing_step("node_b", "ctr_existing", 1),
                run_step("node_a", 2),
                run_step("node_b", 3),
            ],
            cleanup_containers: Vec::new(),
        }
    );
}

#[test]
fn service_plan_counts_duplicate_observations_once() {
    let mut input = planning_input(2, [node_id("node_a")]);
    input.existing_replicas = vec![
        existing_replica("node_b", "ctr_existing"),
        existing_replica("node_b", "ctr_existing"),
    ];

    assert_eq!(
        plan_service_deploy(input).expect("plan succeeds"),
        DeployPlan {
            service_id: service_id("svc_api"),
            target_revision: revision_id("rev_1"),
            steps: vec![
                use_existing_step("node_b", "ctr_existing", 1),
                run_step("node_a", 2),
            ],
            cleanup_containers: Vec::new(),
        }
    );
}

#[test]
fn service_plan_does_not_require_eligible_nodes_when_reality_already_satisfies_replicas() {
    let mut input = planning_input(1, []);
    input.existing_replicas = vec![existing_replica("node_b", "ctr_existing")];

    assert_eq!(
        plan_service_deploy(input).expect("existing reality satisfies target"),
        DeployPlan {
            service_id: service_id("svc_api"),
            target_revision: revision_id("rev_1"),
            steps: vec![use_existing_step("node_b", "ctr_existing", 1)],
            cleanup_containers: Vec::new(),
        }
    );
}

#[test]
fn service_plan_cleans_up_unselected_service_containers_after_success() {
    let mut input = planning_input(1, [node_id("node_a")]);
    input.existing_replicas = vec![
        existing_replica("node_b", "ctr_target_keep"),
        existing_replica("node_b", "ctr_target_extra"),
    ];
    input.cleanup_candidates = vec![
        cleanup_container("node_b", "ctr_target_keep"),
        cleanup_container("node_b", "ctr_target_extra"),
        cleanup_container("node_b", "ctr_old"),
    ];

    assert_eq!(
        plan_service_deploy(input).expect("plan succeeds"),
        DeployPlan {
            service_id: service_id("svc_api"),
            target_revision: revision_id("rev_1"),
            steps: vec![use_existing_step("node_b", "ctr_target_extra", 1)],
            cleanup_containers: vec![
                cleanup_container("node_b", "ctr_old"),
                cleanup_container("node_b", "ctr_target_keep"),
            ],
        }
    );
}

#[test]
fn deploy_plan_requires_eligible_node() {
    assert_eq!(
        plan_service_deploy(planning_input(1, [])),
        Err(DeployPlanError::NoEligibleNodes)
    );
}

#[test]
fn deploy_preparation_uses_active_revision_and_running_target_replicas() {
    let prepared = prepare_deploy(DeployPreparationInput {
        request: deploy_request(2),
        active_service: Some(ActiveServiceState {
            service_id: service_id("svc_api"),
            active_revision: revision_id("rev_old"),
        }),
        active_route: None,
        eligible_nodes: vec![node_id("node_a"), node_id("node_b")],
        observed_nodes: vec![observed_node(
            "node_b",
            [
                observed_container(
                    "node_b",
                    "ctr_target",
                    "svc_api",
                    "rev_1",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::running_unroutable(),
                ),
                observed_container(
                    "node_b",
                    "ctr_old",
                    "svc_api",
                    "rev_old",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::running_unroutable(),
                ),
                observed_container(
                    "node_b",
                    "ctr_job",
                    "svc_api",
                    "rev_1",
                    ManagedContainerKind::Job,
                    ContainerRuntimeState::running_unroutable(),
                ),
                observed_container(
                    "node_b",
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
        prepared.expected_active,
        ExpectedActiveService::Revision(revision_id("rev_old"))
    );
    assert_eq!(
        prepared.eligible_nodes,
        vec![node_id("node_a"), node_id("node_b")]
    );
    assert_eq!(
        prepared.existing_replicas,
        vec![existing_replica("node_b", "ctr_target")]
    );
    assert_eq!(
        prepared.cleanup_candidates,
        vec![
            cleanup_container_with_revision("node_b", "ctr_target", "rev_1"),
            cleanup_container_with_revision("node_b", "ctr_old", "rev_old"),
        ]
    );
}

#[test]
fn routed_deploy_preparation_reuses_only_matching_endpoint_port() {
    let mut request = deploy_request(2);
    request.route = Some(deploy_route("api.example.com", 443, 8080));

    let prepared = prepare_deploy(DeployPreparationInput {
        request,
        active_service: None,
        active_route: None,
        eligible_nodes: vec![node_id("node_a"), node_id("node_b")],
        observed_nodes: vec![observed_node(
            "node_b",
            [
                observed_container(
                    "node_b",
                    "ctr_wrong_port",
                    "svc_api",
                    "rev_1",
                    ManagedContainerKind::Service,
                    ContainerRuntimeState::running_at(endpoint("10.0.0.2", 3000)),
                ),
                observed_container(
                    "node_b",
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
        vec![existing_replica("node_b", "ctr_target")]
    );
}

#[test]
fn deploy_preparation_rejects_active_state_for_another_service() {
    assert_eq!(
        prepare_deploy(DeployPreparationInput {
            request: deploy_request(1),
            active_service: Some(ActiveServiceState {
                service_id: service_id("svc_other"),
                active_revision: revision_id("rev_old"),
            }),
            active_route: None,
            eligible_nodes: vec![node_id("node_a")],
            observed_nodes: Vec::new(),
        }),
        Err(DeployPreparationError::ActiveServiceMismatch {
            expected_service_id: service_id("svc_api"),
            actual_service_id: service_id("svc_other"),
        })
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
                target: route_target("admin.example.com", 443),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
                revision_id: revision_id("rev_old"),
            }),
            eligible_nodes: vec![node_id("node_a")],
            observed_nodes: Vec::new(),
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
            target: route_target("api.example.com", 443),
            endpoint_port: route_port(8080),
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_old"),
        }),
        eligible_nodes: vec![node_id("node_a")],
        observed_nodes: Vec::new(),
    })
    .expect("routed deploy preparation succeeds");

    let route_commit = prepared
        .route_commit
        .expect("routed deploy has route commit request");
    assert_eq!(route_commit.target, route_target("api.example.com", 443));
    assert_eq!(route_commit.endpoint_port, route_port(8080));
    assert_eq!(
        route_commit.expected_current,
        ExpectedActiveRoute::ServiceRevision(ployz_core::state::ExpectedActiveRouteRevision {
            service_id: service_id("svc_api"),
            revision_id: revision_id("rev_old"),
            endpoint_port: route_port(8080),
        })
    );
    assert_eq!(route_commit.service_id, service_id("svc_api"));
    assert_eq!(route_commit.revision_id, revision_id("rev_1"));
}

fn planning_input(
    replicas: u16,
    eligible_nodes: impl IntoIterator<Item = NodeId>,
) -> DeployPlanningInput {
    DeployPlanningInput {
        request: deploy_request(replicas),
        eligible_nodes: eligible_nodes.into_iter().collect(),
        existing_replicas: Vec::new(),
        cleanup_candidates: Vec::new(),
    }
}

fn deploy_request(replicas: u16) -> DeployRequest {
    DeployRequest {
        service_id: service_id("svc_api"),
        target_revision: revision_id("rev_1"),
        image: ImageReference::try_new("ghcr.io/acme/api:rev-1").expect("valid image"),
        replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
        route: None,
    }
}

fn deploy_route(hostname: &str, public_port: u16, endpoint_port: u16) -> DeployRoute {
    DeployRoute {
        target: route_target(hostname, public_port),
        endpoint_port: route_port(endpoint_port),
    }
}

fn use_existing_step(node: &str, container: &str, slot: u16) -> DeployPlanStep {
    DeployPlanStep::UseExistingContainer {
        node_id: node_id(node),
        container_id: container_id(container),
        slot: ReplicaSlot::try_new(slot).expect("valid replica slot"),
    }
}

fn run_step(node: &str, slot: u16) -> DeployPlanStep {
    DeployPlanStep::RunContainer {
        node_id: node_id(node),
        slot: ReplicaSlot::try_new(slot).expect("valid replica slot"),
    }
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn step_id(value: &str) -> StepId {
    StepId::try_new(value).expect("valid step id")
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

fn existing_replica(node: &str, container: &str) -> ExistingServiceReplica {
    ExistingServiceReplica {
        node_id: node_id(node),
        container_id: container_id(container),
    }
}

fn cleanup_container(node: &str, container: &str) -> DeployCleanupContainer {
    cleanup_container_with_revision(node, container, "rev_1")
}

fn cleanup_container_with_revision(
    node: &str,
    container: &str,
    revision: &str,
) -> DeployCleanupContainer {
    DeployCleanupContainer {
        node_id: node_id(node),
        container_id: container_id(container),
        service_id: service_id("svc_api"),
        revision_id: revision_id(revision),
        operation_id: operation_id("op_existing"),
        step_id: step_id(container),
        kind: ManagedContainerKind::Service,
        endpoint_port: None,
    }
}

fn observed_node(
    node: &str,
    containers: impl IntoIterator<Item = ManagedContainerObservation>,
) -> NodeContainerObservationSnapshot {
    NodeContainerObservationSnapshot::try_new(node_id(node), containers)
        .expect("valid observation snapshot")
}

fn observed_container(
    node: &str,
    container: &str,
    service: &str,
    revision: &str,
    kind: ManagedContainerKind,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        node_id: node_id(node),
        container_id: container_id(container),
        service_id: service_id(service),
        revision_id: revision_id(revision),
        operation_id: operation_id("op_existing"),
        step_id: step_id(container),
        kind,
        state,
    }
}
