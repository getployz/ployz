use ployz_core::deploy::{
    DeployPlan, DeployPlanError, DeployPlanStep, DeployPlanningInput, DeployRequest,
    ExistingServiceReplica, ImageReference, ReplicaCount, ReplicaSlot, plan_service_deploy,
};
use ployz_core::ids::{ContainerId, NodeId, RevisionId, ServiceId};

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

fn planning_input(
    replicas: u16,
    eligible_nodes: impl IntoIterator<Item = NodeId>,
) -> DeployPlanningInput {
    DeployPlanningInput {
        request: DeployRequest {
            service_id: service_id("svc_api"),
            target_revision: revision_id("rev_1"),
            image: ImageReference::try_new("ghcr.io/acme/api:rev-1").expect("valid image"),
            replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
        },
        eligible_nodes: eligible_nodes.into_iter().collect(),
        existing_replicas: Vec::new(),
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

fn existing_replica(node: &str, container: &str) -> ExistingServiceReplica {
    ExistingServiceReplica {
        node_id: node_id(node),
        container_id: container_id(container),
    }
}
