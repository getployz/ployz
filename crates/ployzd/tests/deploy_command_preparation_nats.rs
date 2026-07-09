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
use ployz_core::state::MachineLifecycle;
use ployz_core::state::{ActiveMachineState, ServingTargetEntry};
use ployz_nats::service_runtime::RunningNatsService;
use ployz_test_support::containers;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id, operation_id, route_port,
    service_id,
};
use ployzd::intent::machine_roster::MachineRosterStore;
use ployzd::intent::namespace_intent::NamespaceIntentStore;
use ployzd::intent::service::{NatsIntentReader, RunningIntentService, start_intent_service};
use ployzd::operations::deploy::{
    DeployExecutionCommand, DeployMachineCandidates, DeployServiceExecutionCommand,
    load_deploy_execution_facts_from_nats, prepare_deploy_execution_command,
};
use ployzd::roles::machine::client::NatsMachineFactsReader;
use ployzd::roles::machine::runner::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    MachineContainerRunner, MachineContainerRunnerError, MachineLogReader, MachineLogReaderError,
    MachineLogTail,
};
use ployzd::roles::machine::service::{MachinePloyzNativeMeshPreparer, start_machine_role_service};
use std::time::Duration;

#[tokio::test]
async fn nats_preparation_loads_active_state_and_observed_target_replicas() {
    let nats = test_nats().await;
    let facts_reader = nats.facts_reader();
    let intent_reader = nats.intent_reader();

    nats.namespace_intent
        .replace_serving_target_entry(ServingTargetEntry {
            namespace_id: namespace_id("default"),
            service_id: service_id("svc_api"),
            namespace_revision_entry_id: namespace_revision_entry_id("entry_old"),
        })
        .await
        .expect("serving target entry stores");
    let _machine_a = nats
        .serve_machine_facts(machine_snapshot(
            "machine_a",
            [managed_observation_with_entry(
                "machine_a",
                "ctr_target",
                "svc_api",
                target_namespace_revision_entry_id(),
                ContainerRuntimeState::running_unroutable(),
            )],
        ))
        .await;
    let _machine_b = nats
        .serve_machine_facts(machine_snapshot(
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
        .await;
    let command = prepare_command_from_nats(
        operation_id("op_123"),
        deploy_request(),
        DeployMachineCandidates::same_machines(vec![
            machine_id("machine_a"),
            machine_id("machine_b"),
            machine_id("machine_missing"),
        ]),
        &intent_reader,
        &facts_reader,
        Duration::from_secs(7),
    )
    .await;

    let service = single_service(&command);
    assert_eq!(
        service.existing_replicas(),
        [ployz_core::deploy::ExistingServiceReplica {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_target")
        }]
    );
    assert_eq!(
        service.cleanup_candidates(),
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
        service.eligible_machines(),
        [machine_id("machine_a"), machine_id("machine_b")]
    );
    assert_eq!(command.step_timeout(), Duration::from_secs(7));
    assert!(command.dataplane_machines().is_empty());
}

#[tokio::test]
async fn nats_preparation_uses_active_machines_as_deploy_scope() {
    let nats = test_nats().await;
    let machine_roster = nats.machine_roster();
    let facts_reader = nats.facts_reader();
    let intent_reader = nats.intent_reader();
    machine_roster
        .replace_active_machine(&active_machine("edge_2"))
        .await
        .expect("active edge stores");
    let _edge_2 = nats
        .serve_machine_facts(machine_snapshot(
            "edge_2",
            [managed_observation_with_entry(
                "edge_2",
                "ctr_target",
                "svc_api",
                target_namespace_revision_entry_id(),
                ContainerRuntimeState::running_unroutable(),
            )],
        ))
        .await;
    let command = prepare_command_from_nats(
        operation_id("op_123"),
        deploy_request(),
        DeployMachineCandidates::same_machines(vec![machine_id("core_1")]),
        &intent_reader,
        &facts_reader,
        Duration::from_secs(7),
    )
    .await;

    let service = single_service(&command);
    assert_eq!(service.eligible_machines(), [machine_id("edge_2")]);
    assert_eq!(
        service.existing_replicas(),
        [ployz_core::deploy::ExistingServiceReplica {
            machine_id: machine_id("edge_2"),
            container_id: container_id("ctr_target")
        }]
    );
    assert!(command.dataplane_machines().is_empty());
}

#[tokio::test]
async fn nats_preparation_excludes_draining_machines_from_placement() {
    let nats = test_nats().await;
    let machine_roster = nats.machine_roster();
    let facts_reader = nats.facts_reader();
    let intent_reader = nats.intent_reader();
    machine_roster
        .replace_active_machine(&active_machine("edge_2"))
        .await
        .expect("active edge stores");
    let mut draining = active_machine("edge_3");
    draining.lifecycle = MachineLifecycle::Draining;
    machine_roster
        .replace_active_machine(&draining)
        .await
        .expect("draining edge stores");
    let _edge_2 = nats
        .serve_machine_facts(machine_snapshot("edge_2", []))
        .await;
    let _edge_3 = nats
        .serve_machine_facts(machine_snapshot("edge_3", []))
        .await;

    let command = prepare_command_from_nats(
        operation_id("op_123"),
        deploy_request(),
        DeployMachineCandidates::same_machines(vec![machine_id("core_1")]),
        &intent_reader,
        &facts_reader,
        Duration::from_secs(7),
    )
    .await;

    assert_eq!(
        single_service(&command).eligible_machines(),
        [machine_id("edge_2")]
    );
    assert_eq!(
        command.unusable_machines(),
        [ployz_core::ops::UnusableMachine {
            machine_id: machine_id("edge_3"),
            reason: ployz_core::state::MachineUsabilityReason::Draining,
        }]
    );
}

#[tokio::test]
async fn routed_nats_preparation_uses_active_machine_scope_for_dataplane() {
    let nats = test_nats().await;
    let machine_roster = nats.machine_roster();
    let facts_reader = nats.facts_reader();
    let intent_reader = nats.intent_reader();
    machine_roster
        .replace_active_machine(&active_machine("edge_2"))
        .await
        .expect("active edge stores");
    let _edge_2 = nats
        .serve_machine_facts(machine_snapshot("edge_2", []))
        .await;
    let command = prepare_command_from_nats(
        operation_id("op_123"),
        routed_deploy_request(),
        DeployMachineCandidates::same_machines(vec![machine_id("core_1")]),
        &intent_reader,
        &facts_reader,
        Duration::from_secs(7),
    )
    .await;

    assert_eq!(
        single_service(&command).eligible_machines(),
        [machine_id("edge_2")]
    );
    assert_eq!(command.dataplane_machines(), [machine_id("edge_2")]);
}

#[tokio::test]
async fn routed_nats_preparation_uses_configured_dataplane_fallback_without_active_machines() {
    let nats = test_nats().await;
    let facts_reader = nats.facts_reader();
    let intent_reader = nats.intent_reader();
    let _core_1 = nats
        .serve_machine_facts(machine_snapshot("core_1", []))
        .await;
    let command = prepare_command_from_nats(
        operation_id("op_123"),
        routed_deploy_request(),
        DeployMachineCandidates::same_machines(vec![machine_id("core_1")]),
        &intent_reader,
        &facts_reader,
        Duration::from_secs(7),
    )
    .await;

    assert_eq!(
        single_service(&command).eligible_machines(),
        [machine_id("core_1")]
    );
    assert_eq!(command.dataplane_machines(), [machine_id("core_1")]);
}

#[tokio::test]
async fn routed_nats_preparation_does_not_require_dataplane_public_ip() {
    let nats = test_nats().await;
    let machine_roster = nats.machine_roster();
    let facts_reader = nats.facts_reader();
    let intent_reader = nats.intent_reader();
    machine_roster
        .replace_active_machine(&active_machine("edge_2"))
        .await
        .expect("active edge stores");
    let _edge_2 = nats
        .serve_machine_facts(machine_snapshot("edge_2", []))
        .await;

    let command = prepare_command_from_nats(
        operation_id("op_123"),
        routed_deploy_request(),
        DeployMachineCandidates::same_machines(vec![machine_id("core_1")]),
        &intent_reader,
        &facts_reader,
        Duration::from_secs(7),
    )
    .await;

    assert_eq!(command.dataplane_machines(), [machine_id("edge_2")]);
}

#[tokio::test]
async fn nats_preparation_uses_absent_active_state_when_service_is_new() {
    let nats = test_nats().await;
    let facts_reader = nats.facts_reader();
    let intent_reader = nats.intent_reader();
    let _machine_a = nats
        .serve_machine_facts(machine_snapshot("machine_a", []))
        .await;

    let command = prepare_command_from_nats(
        operation_id("op_123"),
        deploy_request(),
        DeployMachineCandidates::same_machines(vec![machine_id("machine_a")]),
        &intent_reader,
        &facts_reader,
        Duration::from_secs(7),
    )
    .await;

    assert!(single_service(&command).existing_replicas().is_empty());
}

fn single_service(command: &DeployExecutionCommand) -> &DeployServiceExecutionCommand {
    let [service] = command.services() else {
        panic!("deploy command has one service");
    };
    service
}

async fn prepare_command_from_nats(
    operation_id: OperationId,
    request: DeployRequest,
    machine_scope: DeployMachineCandidates,
    intent_reader: &NatsIntentReader,
    facts_reader: &NatsMachineFactsReader,
    step_timeout: Duration,
) -> ployzd::operations::deploy::DeployExecutionCommand {
    let facts = load_deploy_execution_facts_from_nats(
        &request,
        machine_scope,
        intent_reader,
        facts_reader,
        step_timeout,
    )
    .await
    .expect("deploy facts load from nats");
    prepare_deploy_execution_command(operation_id, request, facts)
}

struct TestNats {
    connected: ployz_test_support::nats::TestNats,
    machine_roster: MachineRosterStore,
    _intent: RunningIntentService,
    _intent_dir: tempfile::TempDir,
    namespace_intent: NamespaceIntentStore,
}

impl TestNats {
    fn machine_roster(&self) -> MachineRosterStore {
        self.machine_roster.clone()
    }

    fn facts_reader(&self) -> NatsMachineFactsReader {
        NatsMachineFactsReader::new(self.connected.controller.clone())
            .with_request_timeout(Duration::from_secs(1))
    }

    fn intent_reader(&self) -> NatsIntentReader {
        NatsIntentReader::new(self.connected.controller.clone())
            .with_request_timeout(Duration::from_secs(1))
    }

    async fn serve_machine_facts(
        &self,
        snapshot: MachineContainerObservationSnapshot,
    ) -> RunningNatsService {
        let machine_id = snapshot.machine_id().clone();
        let machine_client = self.connected.machine_client(&machine_id).await;
        start_machine_role_service(
            machine_client,
            machine_id,
            StaticRunner::from_snapshot(snapshot),
            UnusedPreparer,
            UnusedLogs,
        )
        .await
        .expect("machine facts service starts")
    }
}

#[derive(Clone)]
struct StaticRunner {
    existing: Vec<ExistingManagedContainer>,
}

impl StaticRunner {
    fn from_snapshot(snapshot: MachineContainerObservationSnapshot) -> Self {
        let existing = snapshot
            .containers()
            .iter()
            .map(|container| ExistingManagedContainer {
                container_id: container.container_id.clone(),
                identity: container.identity.clone(),
                state: existing_state(&container.state),
            })
            .collect();
        Self { existing }
    }
}

impl MachineContainerRunner for StaticRunner {
    async fn existing_managed_containers(
        &self,
    ) -> Result<Vec<ExistingManagedContainer>, MachineContainerRunnerError> {
        Ok(self.existing.clone())
    }

    async fn ensure_endpoint_network(&self) -> Result<(), MachineContainerRunnerError> {
        Err(MachineContainerRunnerError::EnsureEndpointNetwork {
            message: "not used".to_owned(),
        })
    }

    async fn create_managed_container(
        &self,
        _command: CreateManagedContainer,
    ) -> Result<ployz_core::ids::ContainerId, MachineContainerRunnerError> {
        Err(MachineContainerRunnerError::Create {
            message: "not used".to_owned(),
        })
    }

    async fn start_managed_container(
        &self,
        container_id: &ployz_core::ids::ContainerId,
    ) -> Result<(), MachineContainerRunnerError> {
        Err(MachineContainerRunnerError::Start {
            container_id: container_id.clone(),
            message: "not used".to_owned(),
        })
    }

    async fn stop_managed_container(
        &self,
        container_id: &ployz_core::ids::ContainerId,
        _expected_identity: &ployz_core::machine_runtime::ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRunnerError> {
        Err(MachineContainerRunnerError::Stop {
            container_id: container_id.clone(),
            message: "not used".to_owned(),
        })
    }

    async fn remove_managed_container(
        &self,
        container_id: &ployz_core::ids::ContainerId,
        _expected_identity: &ployz_core::machine_runtime::ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRunnerError> {
        Err(MachineContainerRunnerError::Remove {
            container_id: container_id.clone(),
            message: "not used".to_owned(),
        })
    }
}

fn existing_state(state: &ContainerRuntimeState) -> ExistingManagedContainerState {
    match state {
        ContainerRuntimeState::Running { ip } => ExistingManagedContainerState::Running { ip: *ip },
        ContainerRuntimeState::Exited => ExistingManagedContainerState::StartableStopped,
    }
}

#[derive(Clone)]
struct UnusedLogs;

impl MachineLogReader for UnusedLogs {
    async fn tail_container_logs(
        &self,
        container_id: &ployz_core::ids::ContainerId,
        _tail_lines: Option<u16>,
    ) -> Result<MachineLogTail, MachineLogReaderError> {
        Err(MachineLogReaderError::NotFound {
            container_id: container_id.clone(),
        })
    }
}

#[derive(Clone)]
struct UnusedPreparer;

impl MachinePloyzNativeMeshPreparer for UnusedPreparer {
    async fn read_wireguard_public_key(
        &self,
    ) -> Result<
        ployz_core::dataplane::WireGuardPublicKey,
        ployz_core::dataplane::WireGuardEbpfPrepareError,
    > {
        Err(
            ployz_core::dataplane::WireGuardEbpfPrepareError::InvalidReport {
                message: ployz_core::ops::FailureMessage::try_new("not used")
                    .expect("static message is valid"),
            },
        )
    }

    async fn prepare_ployz_native_mesh(
        &self,
        _endpoint_routes: &[ployz_core::dataplane::WireGuardEbpfEndpointRoute],
        _peers: &[ployz_core::dataplane::WireGuardPeer],
    ) -> Result<
        ployz_core::dataplane::PloyzNativeMeshReady,
        ployz_core::dataplane::WireGuardEbpfPrepareError,
    > {
        Err(
            ployz_core::dataplane::WireGuardEbpfPrepareError::InvalidReport {
                message: ployz_core::ops::FailureMessage::try_new("not used")
                    .expect("static message is valid"),
            },
        )
    }
}

async fn test_nats() -> TestNats {
    let machine_ids = [
        machine_id("machine_a"),
        machine_id("machine_b"),
        machine_id("edge_2"),
        machine_id("edge_3"),
        machine_id("core_1"),
    ];
    let connected = ployz_test_support::nats::TestNats::start_with_machines(&machine_ids).await;
    let intent_dir = tempfile::tempdir().expect("intent dir");
    let namespace_intent = NamespaceIntentStore::new(
        ployzd::core_store::CoreStore::open_in_memory()
            .await
            .expect("open core store"),
    );
    let machine_roster = MachineRosterStore::new(
        ployzd::core_store::CoreStore::open_in_memory()
            .await
            .expect("open core store"),
    );
    let intent = start_intent_service(
        connected.controller.clone(),
        machine_id("machine_a"),
        machine_roster.clone(),
        namespace_intent.clone(),
        ployzd::core_store::CoreStore::open_in_memory()
            .await
            .expect("core store opens"),
        Duration::from_secs(30),
    )
    .await
    .expect("intent runtime starts");

    TestNats {
        connected,
        machine_roster,
        _intent: intent,
        _intent_dir: intent_dir,
        namespace_intent,
    }
}

fn deploy_request() -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_api"),
            image: ImageReference::try_new("registry.example/api:rev_2")
                .expect("valid image reference"),
            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
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
        control_endpoints: Vec::new(),
        mesh_endpoints: Vec::new(),
        lifecycle: MachineLifecycle::Active,
        machine_id: self::machine_id(machine_id),
        name: MachineName::try_new(machine_id).expect("valid machine name"),
        activated_by: operation_id("op_machine_add"),
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
