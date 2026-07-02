use async_nats::jetstream;
use ployz_core::deploy::{
    DeployCleanupContainer, DeployRequest, DeployRoute, DeployServiceSpec, ImageReference,
    ReplicaCount,
};
use ployz_core::ids::{OperationId, StepId};
use ployz_core::machine::MachineName;
use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerKind,
    ManagedContainerObservation,
};
use ployz_core::ops::{RouteHostname, RouteTarget};
use ployz_core::state::{ActiveMachineState, ActiveServiceState, ActiveServiceStateKey};
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::kv::KV_CORE_BUCKET;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, operation_id, revision_id, route_port, service_id,
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
        .replace_active_service(&ActiveServiceState {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
            active_revision: revision_id("rev_1"),
        })
        .await
        .expect("active service stores");
    observations
        .replace_machine_containers(&machine_snapshot(
            "machine_a",
            [managed_observation(
                "machine_a",
                "ctr_target",
                "svc_api",
                "rev_2",
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
                    "rev_1",
                    ContainerRuntimeState::running_unroutable(),
                ),
                managed_observation(
                    "machine_b",
                    "ctr_stopped",
                    "svc_api",
                    "rev_2",
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
            cleanup_container("machine_a", "ctr_target", "rev_2"),
            cleanup_container("machine_b", "ctr_old", "rev_1"),
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
            [managed_observation(
                "edge_2",
                "ctr_target",
                "svc_api",
                "rev_2",
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
    let key = ActiveServiceStateKey::from_service_id(&service_id("svc_api"));
    let wrong_service_state = ActiveServiceState {
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_worker"),
        active_revision: revision_id("rev_1"),
    };
    nats.jetstream
        .get_key_value(KV_CORE_BUCKET)
        .await
        .expect("open KV_CORE")
        .put(
            key.as_str(),
            serde_json::to_vec(&wrong_service_state)
                .expect("active service state encodes")
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
    .expect_err("wrong active service payload is rejected");

    assert!(matches!(
        error,
        DeployFactLoadError::ActiveServiceRead {
            service_id,
            ref message,
        } if service_id == self::service_id("svc_api")
            && message.contains(key.as_str())
            && message.contains("belongs to svc_worker, not svc_api")
    ));
}

#[tokio::test]
async fn nats_preparation_preserves_decode_failure_message() {
    let nats = test_nats().await;
    let (core_state, observations) = nats.stores();
    let request = deploy_request();
    let key = ActiveServiceStateKey::from_service_id(
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
    .expect_err("malformed active service payload is rejected");

    assert!(matches!(
        error,
        DeployFactLoadError::ActiveServiceRead {
            ref message,
            ..
        } if message.contains("decode active service state")
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
        .expect("deploy command prepares from loaded facts")
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

fn routed_deploy_request() -> DeployRequest {
    let mut request = deploy_request();
    request.services[0].route = Some(DeployRoute {
        target: RouteTarget {
            hostname: RouteHostname::try_new("smoke.local").expect("valid route hostname"),
            port: route_port(8080),
        },
        endpoint_port: route_port(80),
    });
    request
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
    revision_id: &str,
    state: ContainerRuntimeState,
) -> ManagedContainerObservation {
    ManagedContainerObservation {
        machine_id: self::machine_id(machine_id),
        container_id: self::container_id(container_id),
        service_id: self::service_id(service_id),
        revision_id: self::revision_id(revision_id),
        operation_id: operation_id("op_existing"),
        step_id: StepId::try_new(format!("existing_{container_id}")).expect("valid step id"),
        kind: ManagedContainerKind::Service,
        state,
    }
}

fn active_machine(machine_id: &str) -> ActiveMachineState {
    ActiveMachineState {
        machine_id: self::machine_id(machine_id),
        name: MachineName::try_new(machine_id).expect("valid machine name"),
        activated_by: operation_id("op_machine_add"),
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
