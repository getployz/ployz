use ployz_core::deploy::{DeployCleanupContainer, DeployRequest, ImageReference, ReplicaCount};
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::{
    ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
    NodeContainerObservationSnapshot,
};
use ployz_core::state::{ActiveServiceState, ExpectedActiveService};
use ployzd::deploy_worker::{DeployExecutionFacts, prepare_deploy_execution_command};
use std::time::Duration;

#[tokio::test]
async fn separates_reusable_replicas_from_cleanup_candidates() {
    let request = deploy_request();
    let facts = DeployExecutionFacts {
        active_service: None,
        active_route: None,
        eligible_nodes: vec![node_id("node_a")],
        observed_nodes: vec![
            NodeContainerObservationSnapshot::try_new(
                node_id("node_a"),
                [
                    observed_service_container("node_a", "ctr_old", "rev_old"),
                    observed_service_container_with_service(
                        "node_a",
                        "ctr_other_service",
                        "rev_2",
                        "svc_worker",
                    ),
                    exited_observed_service_container("node_a", "ctr_stopped", "rev_2"),
                ],
            )
            .expect("valid node observation snapshot"),
        ],
        step_timeout: Duration::from_secs(5),
    };

    let command = prepare_deploy_execution_command(operation_id("op_123"), request, facts)
        .expect("deploy command preparation succeeds");

    assert!(command.existing_replicas().is_empty());
    assert_eq!(
        command.cleanup_candidates(),
        [cleanup_container("node_a", "ctr_old", "rev_old")]
    );
    assert_eq!(command.expected_active(), &ExpectedActiveService::Absent);
}

#[tokio::test]
async fn uses_active_service_revision_and_target_replicas() {
    let request = deploy_request();
    let facts = DeployExecutionFacts {
        active_service: Some(ActiveServiceState {
            service_id: service_id("svc_api"),
            active_revision: revision_id("rev_1"),
        }),
        active_route: None,
        eligible_nodes: vec![node_id("node_a")],
        observed_nodes: vec![
            NodeContainerObservationSnapshot::try_new(
                node_id("node_a"),
                [observed_service_container("node_a", "ctr_target", "rev_2")],
            )
            .expect("valid node observation snapshot"),
        ],
        step_timeout: Duration::from_secs(5),
    };

    let command = prepare_deploy_execution_command(operation_id("op_123"), request, facts)
        .expect("deploy command preparation succeeds");

    assert_eq!(
        command.expected_active(),
        &ExpectedActiveService::Revision(revision_id("rev_1"))
    );
    assert_eq!(
        command.existing_replicas(),
        vec![existing_service_replica("node_a", "ctr_target")]
    );
    assert_eq!(
        command.cleanup_candidates(),
        [cleanup_container("node_a", "ctr_target", "rev_2")]
    );
}

#[tokio::test]
async fn rejects_active_state_for_a_different_service() {
    let request = deploy_request();
    let error = prepare_deploy_execution_command(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            active_service: Some(ActiveServiceState {
                service_id: service_id("svc_worker"),
                active_revision: revision_id("rev_1"),
            }),
            active_route: None,
            eligible_nodes: vec![node_id("node_a")],
            observed_nodes: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
    .expect_err("active state for a different service is invalid");

    assert!(matches!(
        error,
        ployzd::deploy_worker::DeployCommandPreparationError::ActiveServiceMismatch {
            expected_service_id,
            actual_service_id,
        } if expected_service_id == service_id("svc_api")
            && actual_service_id == service_id("svc_worker")
    ));
}

fn deploy_request() -> DeployRequest {
    DeployRequest {
        service_id: service_id("svc_api"),
        target_revision: revision_id("rev_2"),
        image: ImageReference::try_new("registry.example/api:rev_2")
            .expect("valid image reference"),
        replicas: ReplicaCount::try_new(1).expect("valid replica count"),
        route: None,
    }
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

fn existing_service_replica(
    node_id: &str,
    container_id: &str,
) -> ployz_core::deploy::ExistingServiceReplica {
    ployz_core::deploy::ExistingServiceReplica {
        node_id: self::node_id(node_id),
        container_id: self::container_id(container_id),
    }
}

fn cleanup_container(
    node_id: &str,
    container_id: &str,
    revision_id: &str,
) -> DeployCleanupContainer {
    DeployCleanupContainer {
        node_id: self::node_id(node_id),
        container_id: self::container_id(container_id),
        service_id: service_id("svc_api"),
        revision_id: self::revision_id(revision_id),
        operation_id: operation_id("op_existing"),
        step_id: StepId::try_new(format!("existing_{container_id}")).expect("valid step id"),
        kind: ManagedContainerKind::Service,
        endpoint_port: None,
    }
}

fn observed_service_container(
    node_id: &str,
    container_id: &str,
    revision_id: &str,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        node_id: self::node_id(node_id),
        container_id: self::container_id(container_id),
        service_id: service_id("svc_api"),
        revision_id: self::revision_id(revision_id),
        operation_id: operation_id("op_existing"),
        step_id: StepId::try_new(format!("existing_{container_id}")).expect("valid step id"),
        kind: ManagedContainerKind::Service,
        state: ContainerRuntimeState::running_unroutable(),
    }
}

fn observed_service_container_with_service(
    node_id: &str,
    container_id: &str,
    revision_id: &str,
    service_id: &str,
) -> ManagedContainerObservation {
    let mut observation = observed_service_container(node_id, container_id, revision_id);
    observation.service_id = self::service_id(service_id);
    observation
}

fn exited_observed_service_container(
    node_id: &str,
    container_id: &str,
    revision_id: &str,
) -> ManagedContainerObservation {
    let mut observation = observed_service_container(node_id, container_id, revision_id);
    observation.state = ContainerRuntimeState::Exited;
    observation
}
