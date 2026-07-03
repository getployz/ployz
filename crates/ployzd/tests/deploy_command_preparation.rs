use ployz_core::deploy::{
    DeployCleanupContainer, DeployRequest, DeployServiceSpec, ImageReference, ReplicaCount,
};
use ployz_core::ids::NamespaceRevisionEntryId;
use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerObservation,
};
use ployz_core::ops::{RouteHostname, RoutePort, RouteTarget};
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
use ployz_test_support::containers;
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
        namespace_route_bindings: Vec::new(),
        namespace_serving_entries: Vec::new(),
        services: vec![DeployServiceExecutionFacts {
            serving_target_entry: None,
            route_bindings: Vec::new(),
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
        namespace_route_bindings: Vec::new(),
        namespace_serving_entries: Vec::new(),
        services: vec![DeployServiceExecutionFacts {
            serving_target_entry: Some(ServingTargetEntry {
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                namespace_revision_entry_id: namespace_revision_entry_id("entry_old"),
            }),
            route_bindings: Vec::new(),
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

#[tokio::test]
async fn manifest_omission_removes_serving_entry_routes_and_containers() {
    // The manifest declares only `svc_api`; `svc_worker` is omitted. Its
    // serving entry is unpublished, its route binding detached, and its
    // running container becomes a cleanup candidate - manifest omission
    // removes a service from the namespace.
    let request = deploy_request();
    let omitted_target = RouteTarget::new(
        RouteHostname::try_new("worker.example.com").expect("valid route hostname"),
        RoutePort::try_new(443).expect("valid route port"),
    );
    let omitted_container = MachineContainerObservationSnapshot::try_new(
        machine_id("machine_a"),
        [observed_service_container_with_service(
            "machine_a",
            "ctr_worker",
            "entry_worker",
            "svc_worker",
        )],
    )
    .expect("valid machine observation snapshot");
    let facts = DeployExecutionFacts {
        namespace_route_bindings: vec![RouteBindingState {
            namespace_id: namespace_id("default"),
            target: omitted_target.clone(),
            endpoint_port: RoutePort::try_new(8080).expect("valid route port"),
            service_id: service_id("svc_worker"),
        }],
        namespace_serving_entries: vec![
            ServingTargetEntry {
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                namespace_revision_entry_id: namespace_revision_entry_id("entry_api"),
            },
            ServingTargetEntry {
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_worker"),
                namespace_revision_entry_id: namespace_revision_entry_id("entry_worker"),
            },
        ],
        services: vec![DeployServiceExecutionFacts {
            serving_target_entry: None,
            route_bindings: Vec::new(),
        }],
        eligible_machines: vec![machine_id("machine_a")],
        dataplane_machines: Vec::new(),
        observed_machines: vec![omitted_container.clone()],
        namespace_cleanup_candidates: ployzd::deploy_worker::namespace_cleanup_candidates(&[
            omitted_container,
        ]),
        step_timeout: Duration::from_secs(5),
    };

    let command = prepare_deploy_execution_command(operation_id("op_123"), request, facts)
        .expect("deploy command preparation succeeds");

    assert_eq!(command.route_binding_removals(), [omitted_target]);
    assert_eq!(
        command.serving_target_removals(),
        [ServingTargetEntry {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_worker"),
            namespace_revision_entry_id: namespace_revision_entry_id("entry_worker"),
        }]
    );
    let [candidate] = command.namespace_cleanup_candidates() else {
        panic!("omitted service container is a cleanup candidate");
    };
    assert_eq!(candidate.identity.service_id, service_id("svc_worker"));
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
    let request = deploy_request();
    let [service] = request.services.as_slice() else {
        panic!("deploy request fixture has one service");
    };
    service.namespace_revision_entry_id(&namespace_id("default"))
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
        identity: containers::identity("svc_api")
            .entry(namespace_revision_entry_id.as_str())
            .operation("op_existing")
            .step(&format!("existing_{container_id}"))
            .build(),
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
    containers::observation(machine_id, container_id)
        .with(
            containers::identity("svc_api")
                .entry(namespace_revision_entry_id.as_str())
                .operation("op_existing")
                .step(&format!("existing_{container_id}")),
        )
        .running_unroutable()
        .build()
}

fn observed_service_container_with_service(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: &str,
    service_id: &str,
) -> ManagedContainerObservation {
    let mut observation =
        observed_service_container(machine_id, container_id, namespace_revision_entry_id);
    observation.identity.service_id = self::service_id(service_id);
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
