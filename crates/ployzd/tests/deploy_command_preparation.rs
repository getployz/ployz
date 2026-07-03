use ployz_core::deploy::{
    DeployCleanupContainer, DeployRequest, DeployServiceSpec, ImageReference, ReplicaCount,
};
use ployz_core::ids::{NamespaceRevisionEntryId, StepId};
use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerKind,
    ManagedContainerObservation,
};
use ployz_core::state::ActiveServiceState;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id, namespace_revision_id,
    operation_id, service_id,
};
use ployzd::deploy_worker::{
    DeployExecutionFacts, DeployServiceExecutionFacts, prepare_deploy_execution_command,
};
use std::time::Duration;

#[tokio::test]
async fn separates_reusable_replicas_from_cleanup_candidates() {
    let request = deploy_request();
    let facts = DeployExecutionFacts {
        services: vec![DeployServiceExecutionFacts {
            active_service: None,
            active_routes: Vec::new(),
        }],
        eligible_machines: vec![machine_id("machine_a")],
        dataplane_machines: Vec::new(),
        observed_machines: vec![
            MachineContainerObservationSnapshot::try_new(
                machine_id("machine_a"),
                [
                    observed_service_container("machine_a", "ctr_old", "entry_old"),
                    observed_service_container_with_service(
                        "machine_a",
                        "ctr_other_service",
                        "entry_other",
                        "svc_worker",
                    ),
                    exited_observed_service_container("machine_a", "ctr_stopped", "entry_target"),
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
        [cleanup_container("machine_a", "ctr_old", "entry_old")]
    );
}

#[tokio::test]
async fn reuses_running_target_entry_and_marks_service_containers_for_cleanup() {
    let request = deploy_request();
    let facts = DeployExecutionFacts {
        services: vec![DeployServiceExecutionFacts {
            active_service: Some(ActiveServiceState {
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                active_revision: namespace_revision_entry_id("entry_old"),
            }),
            active_routes: Vec::new(),
        }],
        eligible_machines: vec![machine_id("machine_a")],
        dataplane_machines: Vec::new(),
        observed_machines: vec![
            MachineContainerObservationSnapshot::try_new(
                machine_id("machine_a"),
                [observed_service_container_with_entry(
                    "machine_a",
                    "ctr_target",
                    target_namespace_revision_entry_id(),
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
        [cleanup_container_with_entry(
            "machine_a",
            "ctr_target",
            target_namespace_revision_entry_id()
        )]
    );
}

fn deploy_request() -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        namespace_revision_id: namespace_revision_id("rev_2"),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_api"),
            image: ImageReference::try_new("registry.example/api:rev_2")
                .expect("valid image reference"),
            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            routes: Vec::new(),
        }],
    }
}

fn target_namespace_revision_entry_id() -> NamespaceRevisionEntryId {
    deploy_request().services[0].namespace_revision_entry_id()
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
    namespace_revision_entry_id: &str,
) -> DeployCleanupContainer {
    cleanup_container_with_entry(
        machine_id,
        container_id,
        self::namespace_revision_entry_id(namespace_revision_entry_id),
    )
}

fn cleanup_container_with_entry(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: NamespaceRevisionEntryId,
) -> DeployCleanupContainer {
    DeployCleanupContainer {
        machine_id: self::machine_id(machine_id),
        container_id: self::container_id(container_id),
        service_id: service_id("svc_api"),
        revision_id: namespace_revision_entry_id,
        operation_id: operation_id("op_existing"),
        step_id: StepId::try_new(format!("existing_{container_id}")).expect("valid step id"),
        kind: ManagedContainerKind::Service,
        endpoint_port: None,
    }
}

fn observed_service_container(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: &str,
) -> ManagedContainerObservation {
    observed_service_container_with_entry(
        machine_id,
        container_id,
        self::namespace_revision_entry_id(namespace_revision_entry_id),
    )
}

fn observed_service_container_with_entry(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: NamespaceRevisionEntryId,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        machine_id: self::machine_id(machine_id),
        container_id: self::container_id(container_id),
        service_id: service_id("svc_api"),
        revision_id: namespace_revision_entry_id,
        operation_id: operation_id("op_existing"),
        step_id: StepId::try_new(format!("existing_{container_id}")).expect("valid step id"),
        kind: ManagedContainerKind::Service,
        state: ContainerRuntimeState::running_unroutable(),
    }
}

fn observed_service_container_with_service(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: &str,
    service_id: &str,
) -> ManagedContainerObservation {
    let mut observation =
        observed_service_container(machine_id, container_id, namespace_revision_entry_id);
    observation.service_id = self::service_id(service_id);
    observation
}

fn exited_observed_service_container(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: &str,
) -> ManagedContainerObservation {
    let mut observation =
        observed_service_container(machine_id, container_id, namespace_revision_entry_id);
    observation.state = ContainerRuntimeState::Exited;
    observation
}
