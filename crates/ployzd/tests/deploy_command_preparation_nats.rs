use async_nats::jetstream;
use ployz_core::deploy::{
    DeployCleanupContainer, DeployRequest, DeployRoute, DeployServiceSpec, ImageReference,
    ReplicaCount,
};
use ployz_core::ids::{NamespaceRevisionEntryId, OperationId};
use ployz_core::machine::MachineName;
use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerObservation,
};
use ployz_core::ops::{RouteHostname, RouteTarget};
use ployz_core::state::{ActiveMachineState, ServingTargetEntry, ServingTargetEntryKey};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::kv::KV_CORE_BUCKET;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_test_support::containers;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id, namespace_revision_id,
    operation_id, route_port, service_id,
};
use ployzd::deploy_worker::{
    DeployExecutionMachineScope, DeployFactLoadError, load_deploy_execution_facts_from_nats,
    prepare_deploy_execution_command,
};
use std::time::Duration;

#[tokio::test]
async fn nats_preparation_loads_active_state_and_observed_target_replicas() {
    let nats = test_nats().await;
    let (core_state, observations) = nats.stores();

    core_state
        .replace_serving_target_entry(&ServingTargetEntry {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
            namespace_revision_entry_id: namespace_revision_entry_id("entry_old"),
        })
        .await
        .expect("serving target entry stores");
    observations
        .replace_machine_containers(&machine_snapshot(
            "machine_a",
            [managed_observation_with_entry(
                "machine_a",
                "ctr_target",
                "svc_api",
                target_namespace_revision_entry_id(),
                ContainerRuntimeState::running_unroutable(),
            )],
        ))
        .await
        .expect("machine_a observations store");
    let machine_b_observations = nats.machine_observations("machine_b").await;
    machine_b_observations
        .replace_machine_containers(&machine_snapshot(
            "machine_b",
            [
                managed_observation(
                    "machine_b",
                    "ctr_old",
                    "svc_api",
                    "entry_old",
                    ContainerRuntimeState::running_unroutable(),
                ),
                managed_observation(
                    "machine_b",
                    "ctr_stopped",
                    "svc_api",
                    "entry_target",
                    ContainerRuntimeState::Exited,
                ),
            ],
        ))
        .await
        .expect("machine_b observations store");
    let command = prepare_command_from_nats(
        operation_id("op_123"),
        deploy_request(),
        DeployExecutionMachineScope::same_machines(vec![
            machine_id("machine_a"),
            machine_id("machine_b"),
            machine_id("machine_missing"),
        ]),
        &core_state,
        &observations,
        Duration::from_secs(7),
    )
    .await;

    assert_eq!(
        command.existing_replicas(),
        [ployz_core::deploy::ExistingServiceReplica {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_target")
        }]
    );
    assert_eq!(
        command.cleanup_candidates(),
        [
            cleanup_container_with_entry(
                "machine_a",
                "ctr_target",
                target_namespace_revision_entry_id()
            ),
            cleanup_container("machine_b", "ctr_old", "entry_old"),
        ]
    );
    assert_eq!(
        command.eligible_machines(),
        [
            machine_id("machine_a"),
            machine_id("machine_b"),
            machine_id("machine_missing")
        ]
    );
    assert_eq!(command.step_timeout(), Duration::from_secs(7));
    assert!(command.dataplane_machines().is_empty());
}

#[tokio::test]
async fn nats_preparation_uses_active_machines_as_deploy_scope() {
    let nats = test_nats().await;
    let (core_state, observations) = nats.stores();
    core_state
        .replace_active_machine(&active_machine("edge_2"))
        .await
        .expect("active edge stores");
    let edge_observations = nats.machine_observations("edge_2").await;
    edge_observations
        .replace_machine_containers(&machine_snapshot(
            "edge_2",
            [managed_observation_with_entry(
                "edge_2",
                "ctr_target",
                "svc_api",
                target_namespace_revision_entry_id(),
                ContainerRuntimeState::running_unroutable(),
            )],
        ))
        .await
        .expect("edge observations store");
    let command = prepare_command_from_nats(
        operation_id("op_123"),
        deploy_request(),
        DeployExecutionMachineScope::same_machines(vec![machine_id("core_1")]),
        &core_state,
        &observations,
        Duration::from_secs(7),
    )
    .await;

    assert_eq!(command.eligible_machines(), [machine_id("edge_2")]);
    assert_eq!(
        command.existing_replicas(),
        [ployz_core::deploy::ExistingServiceReplica {
            machine_id: machine_id("edge_2"),
            container_id: container_id("ctr_target")
        }]
    );
    assert!(command.dataplane_machines().is_empty());
}

#[tokio::test]
async fn routed_nats_preparation_uses_active_machine_scope_for_dataplane() {
    let nats = test_nats().await;
    let (core_state, observations) = nats.stores();
    core_state
        .replace_active_machine(&active_machine("edge_2"))
        .await
        .expect("active edge stores");
    let command = prepare_command_from_nats(
        operation_id("op_123"),
        routed_deploy_request(),
        DeployExecutionMachineScope::same_machines(vec![machine_id("core_1")]),
        &core_state,
        &observations,
        Duration::from_secs(7),
    )
    .await;

    assert_eq!(command.eligible_machines(), [machine_id("edge_2")]);
    assert_eq!(command.dataplane_machines(), [machine_id("edge_2")]);
}

#[tokio::test]
async fn routed_nats_preparation_uses_configured_dataplane_fallback_without_active_machines() {
    let nats = test_nats().await;
    let (core_state, observations) = nats.stores();
    let command = prepare_command_from_nats(
        operation_id("op_123"),
        routed_deploy_request(),
        DeployExecutionMachineScope::same_machines(vec![machine_id("core_1")]),
        &core_state,
        &observations,
        Duration::from_secs(7),
    )
    .await;

    assert_eq!(command.eligible_machines(), [machine_id("core_1")]);
    assert_eq!(command.dataplane_machines(), [machine_id("core_1")]);
}

#[tokio::test]
async fn routed_nats_preparation_does_not_require_dataplane_public_ip() {
    let nats = test_nats().await;
    let (core_state, observations) = nats.stores();
    core_state
        .replace_active_machine(&active_machine("edge_2"))
        .await
        .expect("active edge stores");

    let command = prepare_command_from_nats(
        operation_id("op_123"),
        routed_deploy_request(),
        DeployExecutionMachineScope::same_machines(vec![machine_id("core_1")]),
        &core_state,
        &observations,
        Duration::from_secs(7),
    )
    .await;

    assert_eq!(command.dataplane_machines(), [machine_id("edge_2")]);
}

#[tokio::test]
async fn nats_preparation_uses_absent_active_state_when_service_is_new() {
    let nats = test_nats().await;
    let (core_state, observations) = nats.stores();

    let command = prepare_command_from_nats(
        operation_id("op_123"),
        deploy_request(),
        DeployExecutionMachineScope::same_machines(vec![machine_id("machine_a")]),
        &core_state,
        &observations,
        Duration::from_secs(7),
    )
    .await;

    assert!(command.existing_replicas().is_empty());
}

#[tokio::test]
async fn nats_preparation_preserves_typed_active_state_read_failure() {
    let nats = test_nats().await;
    let (core_state, observations) = nats.stores();
    let key = ServingTargetEntryKey::from_namespace_service(
        &namespace_id("default"),
        &service_id("svc_api"),
    );
    let wrong_service_state = ServingTargetEntry {
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_worker"),
        namespace_revision_entry_id: namespace_revision_entry_id("entry_old"),
    };
    nats.jetstream
        .get_key_value(KV_CORE_BUCKET)
        .await
        .expect("open KV_CORE")
        .put(
            key.as_str(),
            serde_json::to_vec(&wrong_service_state)
                .expect("serving target entry state encodes")
                .into(),
        )
        .await
        .expect("corrupt active state stores");

    let request = deploy_request();
    let error = load_deploy_execution_facts_from_nats(
        &request,
        DeployExecutionMachineScope::same_machines(vec![machine_id("machine_a")]),
        &core_state,
        &observations,
        Duration::from_secs(7),
    )
    .await
    .expect_err("wrong serving target entry payload is rejected");

    // The namespace-wide serving-target read detects the corruption first
    // (omitted-service reconciliation loads every entry in the namespace).
    assert!(matches!(
        error,
        DeployFactLoadError::ServingTargetEntriesRead { ref message }
            if message.contains(key.as_str())
                && message.contains("does not match canonical key")
    ));
}

#[tokio::test]
async fn nats_preparation_preserves_decode_failure_message() {
    let nats = test_nats().await;
    let (core_state, observations) = nats.stores();
    let request = deploy_request();
    let key = ServingTargetEntryKey::from_namespace_service(
        &namespace_id("default"),
        request
            .primary_service_id()
            .expect("test deploy request has a service"),
    );
    nats.jetstream
        .get_key_value(KV_CORE_BUCKET)
        .await
        .expect("open KV_CORE")
        .put(key.as_str(), br#"{"service_id":"svc_api""#.to_vec().into())
        .await
        .expect("malformed active state stores");

    let error = load_deploy_execution_facts_from_nats(
        &request,
        DeployExecutionMachineScope::same_machines(vec![machine_id("machine_a")]),
        &core_state,
        &observations,
        Duration::from_secs(7),
    )
    .await
    .expect_err("malformed serving target entry payload is rejected");

    assert!(matches!(
        error,
        DeployFactLoadError::ServingTargetEntriesRead { ref message }
            if message.contains("decode serving target entry state")
    ));
}

async fn prepare_command_from_nats(
    operation_id: OperationId,
    request: DeployRequest,
    machine_scope: DeployExecutionMachineScope,
    core_state: &AsyncNatsCoreStateStore,
    observations: &AsyncNatsObservationStore,
    step_timeout: Duration,
) -> ployzd::deploy_worker::DeployExecutionCommand {
    let facts = load_deploy_execution_facts_from_nats(
        &request,
        machine_scope,
        core_state,
        observations,
        step_timeout,
    )
    .await
    .expect("deploy facts load from nats");
    prepare_deploy_execution_command(operation_id, request, facts)
}

struct TestNats {
    connected: ployz_test_support::nats::TestNats,
    jetstream: jetstream::Context,
    core_state: AsyncNatsCoreStateStore,
    observations: AsyncNatsObservationStore,
}

impl TestNats {
    fn stores(&self) -> (AsyncNatsCoreStateStore, AsyncNatsObservationStore) {
        (self.core_state.clone(), self.observations.clone())
    }

    /// The observation store connected as the given machine — each machine may
    /// only write its own observation keys.
    async fn machine_observations(&self, machine_id_value: &str) -> AsyncNatsObservationStore {
        let machine_client = self
            .connected
            .machine_client(&machine_id(machine_id_value))
            .await;
        AsyncNatsObservationStore::from_jetstream(&jetstream::new(machine_client))
            .await
            .expect("open machine observation store")
    }
}

async fn test_nats() -> TestNats {
    let machine_ids = [
        machine_id("machine_a"),
        machine_id("machine_b"),
        machine_id("edge_2"),
        machine_id("core_1"),
    ];
    let connected = ployz_test_support::nats::TestNats::start_with_machines(&machine_ids).await;
    connected.bootstrap_resources().await;
    let jetstream = connected.jetstream.clone();
    let machine_client = connected.machine_client(&machine_id("machine_a")).await;
    let machine_jetstream = jetstream::new(machine_client);
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&jetstream)
        .await
        .expect("open core state store");
    let observations = AsyncNatsObservationStore::from_jetstream(&machine_jetstream)
        .await
        .expect("open observation store");

    TestNats {
        connected,
        jetstream,
        core_state,
        observations,
    }
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

fn routed_deploy_request() -> DeployRequest {
    let mut request = deploy_request();
    let [service] = request.services.as_mut_slice() else {
        panic!("deploy request fixture has one service");
    };
    service.routes = vec![DeployRoute {
        target: RouteTarget {
            hostname: RouteHostname::try_new("smoke.local").expect("valid route hostname"),
            port: route_port(8080),
        },
        endpoint_port: route_port(80),
    }];
    request
}

fn target_namespace_revision_entry_id() -> NamespaceRevisionEntryId {
    let request = deploy_request();
    let [service] = request.services.as_slice() else {
        panic!("deploy request fixture has one service");
    };
    service.namespace_revision_entry_id(&namespace_id("default"))
}

fn machine_snapshot(
    machine_id: &str,
    containers: impl IntoIterator<Item = ManagedContainerObservation>,
) -> MachineContainerObservationSnapshot {
    MachineContainerObservationSnapshot::try_new(self::machine_id(machine_id), containers)
        .expect("valid machine observation snapshot")
}

fn managed_observation(
    machine_id: &str,
    container_id: &str,
    service_id: &str,
    namespace_revision_entry_id: &str,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    managed_observation_with_entry(
        machine_id,
        container_id,
        service_id,
        self::namespace_revision_entry_id(namespace_revision_entry_id),
        state,
    )
}

fn managed_observation_with_entry(
    machine_id: &str,
    container_id: &str,
    service_id: &str,
    namespace_revision_entry_id: NamespaceRevisionEntryId,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    containers::observation(machine_id, container_id)
        .with(
            containers::identity(service_id)
                .entry(namespace_revision_entry_id.as_str())
                .operation("op_existing")
                .step(&format!("existing_{container_id}")),
        )
        .state(state)
        .build()
}

fn active_machine(machine_id: &str) -> ActiveMachineState {
    ActiveMachineState {
        machine_id: self::machine_id(machine_id),
        name: MachineName::try_new(machine_id).expect("valid machine name"),
        activated_by: operation_id("op_machine_add"),
        substrate_versions: None,
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
