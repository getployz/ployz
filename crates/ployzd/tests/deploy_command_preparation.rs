use ployz_core::deploy::{
    DeployCleanupContainer, DeployPreparationError, DeployRequest, DeployServiceSpec,
    ImageReference, ReplicaCount,
};
use ployz_core::ids::StepId;
use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerKind,
    ManagedContainerObservation,
};
use ployz_core::state::ActiveServiceState;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, operation_id, revision_id, service_id,
};
use ployzd::deploy_worker::{
    DeployCommandPreparationError, DeployExecutionFacts, DeployServiceExecutionFacts,
    prepare_deploy_execution_command,
};
use std::time::Duration;

#[tokio::test]
async fn separates_reusable_replicas_from_cleanup_candidates() {
    let request = deploy_request();
    let facts = DeployExecutionFacts {
        services: vec![DeployServiceExecutionFacts {
            active_service: None,
            active_route: None,
        }],
        eligible_machines: vec![machine_id("machine_a")],
        dataplane_machines: Vec::new(),
        observed_machines: vec![
            MachineContainerObservationSnapshot::try_new(
                machine_id("machine_a"),
                [
                    observed_service_container("machine_a", "ctr_old", "rev_old"),
                    observed_service_container_with_service(
                        "machine_a",
                        "ctr_other_service",
                        "rev_2",
                        "svc_worker",
                    ),
                    exited_observed_service_container("machine_a", "ctr_stopped", "rev_2"),
                ],
            )
            .expect("valid machine observation snapshot"),
        ],
        namespace_cleanup_candidates: Vec::new(),
        step_timeout: Duration::from_secs(5),
    };

    let command = prepare_deploy_execution_command(operation_id("op_123"), request, facts)
        .expect("deploy command preparation succeeds");

    assert!(command.existing_replicas().is_empty());
    assert_eq!(
        command.cleanup_candidates(),
        [cleanup_container("machine_a", "ctr_old", "rev_old")]
    );
}

#[tokio::test]
async fn uses_active_service_revision_and_target_replicas() {
    let request = deploy_request();
    let facts = DeployExecutionFacts {
        services: vec![DeployServiceExecutionFacts {
            active_service: Some(ActiveServiceState {
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                active_revision: revision_id("rev_1"),
            }),
            active_route: None,
        }],
        eligible_machines: vec![machine_id("machine_a")],
        dataplane_machines: Vec::new(),
        observed_machines: vec![
            MachineContainerObservationSnapshot::try_new(
                machine_id("machine_a"),
                [observed_service_container(
                    "machine_a",
                    "ctr_target",
                    "rev_2",
                )],
            )
            .expect("valid machine observation snapshot"),
        ],
        namespace_cleanup_candidates: Vec::new(),
        step_timeout: Duration::from_secs(5),
    };

    let command = prepare_deploy_execution_command(operation_id("op_123"), request, facts)
        .expect("deploy command preparation succeeds");

    assert_eq!(
        command.existing_replicas(),
        vec![existing_service_replica("machine_a", "ctr_target")]
    );
    assert_eq!(
        command.cleanup_candidates(),
        [cleanup_container("machine_a", "ctr_target", "rev_2")]
    );
}

#[tokio::test]
async fn rejects_active_state_for_a_different_service() {
    let request = deploy_request();
    let error = prepare_deploy_execution_command(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            services: vec![DeployServiceExecutionFacts {
                active_service: Some(ActiveServiceState {
                    namespace_id: namespace_id("default"),
                    service_id: service_id("svc_worker"),
                    active_revision: revision_id("rev_1"),
                }),
                active_route: None,
            }],
            eligible_machines: vec![machine_id("machine_a")],
            dataplane_machines: Vec::new(),
            observed_machines: Vec::new(),
            namespace_cleanup_candidates: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
    .expect_err("active state for a different service is invalid");

    assert!(matches!(
        error,
        DeployCommandPreparationError::Service(DeployPreparationError::ActiveServiceMismatch {
            expected_service_id,
            actual_service_id,
        }) if expected_service_id == service_id("svc_api")
            && actual_service_id == service_id("svc_worker")
    ));
}

fn deploy_request() -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        target_revision: revision_id("rev_2"),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_api"),
            image: ImageReference::try_new("registry.example/api:rev_2")
                .expect("valid image reference"),
            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            route: None,
        }],
    }
}

fn existing_service_replica(
    machine_id: &str,
    container_id: &str,
) -> ployz_core::deploy::ExistingServiceReplica {
    ployz_core::deploy::ExistingServiceReplica {
        machine_id: self::machine_id(machine_id),
        container_id: self::container_id(container_id),
    }
}

fn cleanup_container(
    machine_id: &str,
    container_id: &str,
    revision_id: &str,
) -> DeployCleanupContainer {
    DeployCleanupContainer {
        machine_id: self::machine_id(machine_id),
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
    machine_id: &str,
    container_id: &str,
    revision_id: &str,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        machine_id: self::machine_id(machine_id),
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
    machine_id: &str,
    container_id: &str,
    revision_id: &str,
    service_id: &str,
) -> ManagedContainerObservation {
    let mut observation = observed_service_container(machine_id, container_id, revision_id);
    observation.service_id = self::service_id(service_id);
    observation
}

fn exited_observed_service_container(
    machine_id: &str,
    container_id: &str,
    revision_id: &str,
) -> ManagedContainerObservation {
    let mut observation = observed_service_container(machine_id, container_id, revision_id);
    observation.state = ContainerRuntimeState::Exited;
    observation
}
