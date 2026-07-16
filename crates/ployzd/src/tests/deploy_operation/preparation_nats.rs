use crate::control::intent::machine_roster::MachineRosterStore;
use crate::control::intent::namespace_intent::NamespaceIntentStore;
use crate::control::intent::service::{
    NatsIntentReader, RunningIntentService, start_intent_service,
};
use crate::control::operations::deploy::{
    DeployExecutionCommand, DeployServiceExecutionCommand, load_deploy_execution_facts_from_nats,
    prepare_deploy_execution_command,
};
use crate::control::role_client::machine::NatsMachineFactsReader;
use crate::roles::machine::protocol::{
    MachineDataplaneStatusRpcRequest, MachineDataplaneStatusRpcResponse, MachineRpcResponse,
};
use crate::roles::machine::runner::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    MachineContainerRunner, MachineContainerRunnerError, MachineLogReader, MachineLogReaderError,
    MachineLogTail,
};
use crate::roles::machine::service::{RunningMachineRoleRuntime, start_machine_role_runtime};
use ployz_core::deploy::{
    DeployCleanupContainer, DeployRequest, DeployRoute, DeployRouteTarget, DeployServiceSpec,
    ImageReference, ReplicaCount,
};
use ployz_core::ids::{NamespaceRevisionEntryId, OperationId};
use ployz_core::intent::ActiveMachineState;
use ployz_core::machine::MachineLifecycle;
use ployz_core::machine::MachineName;
use ployz_core::machine::runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerObservation,
};
use ployz_core::operation::RouteHostname;
use ployz_nats::service_runtime::request_json;
use ployz_test_support::containers;
use ployz_test_support::fixtures::serving_target_entry;
use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id, operation_id, route_port,
    service_id,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::tests::support::machine_runtime::{ReadyWireGuardEbpf, test_wireguard_public_key};

#[tokio::test]
async fn nats_preparation_loads_active_state_and_observed_target_replicas() {
    let nats = test_nats().await;
    nats.declare_active_machine("machine_a").await;
    nats.declare_active_machine("machine_b").await;
    let facts_reader = nats.facts_reader();
    let intent_reader = nats.intent_reader();

    nats.namespace_intent
        .replace_serving_target_entry(promoted_target_entry())
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
            container_id: container_id("ctr_target"),
            creation_gate: ployz_core::deploy::ExistingReplicaCreationGate::AlreadyPassed,
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
            stopped_cleanup_container("machine_b", "ctr_stopped", "entry_target"),
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
    nats.namespace_intent
        .replace_serving_target_entry(promoted_target_entry())
        .await
        .expect("serving target entry stores");
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
            container_id: container_id("ctr_target"),
            creation_gate: ployz_core::deploy::ExistingReplicaCreationGate::AlreadyPassed,
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
        [ployz_core::operation::UnusableMachine {
            machine_id: machine_id("edge_3"),
            reason: ployz_core::machine::MachineUsabilityReason::Draining,
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
    let [gateway] = command.gateway_certificate_targets() else {
        panic!("intent-known gateway remains the only certificate target");
    };
    assert_eq!(gateway.machine_id, machine_id("edge_2"));
}

#[tokio::test]
async fn routed_nats_preparation_ignores_configured_machine_outside_durable_roster() {
    let nats = test_nats().await;
    let facts_reader = nats.facts_reader();
    let intent_reader = nats.intent_reader();
    let _core_1 = nats
        .serve_machine_facts(machine_snapshot("core_1", []))
        .await;
    let command = prepare_command_from_nats(
        operation_id("op_123"),
        routed_deploy_request(),
        &intent_reader,
        &facts_reader,
        Duration::from_secs(7),
    )
    .await;

    assert!(single_service(&command).eligible_machines().is_empty());
    assert!(command.dataplane_machines().is_empty());
    assert!(command.unusable_machines().is_empty());
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
    intent_reader: &NatsIntentReader,
    facts_reader: &NatsMachineFactsReader,
    step_timeout: Duration,
) -> crate::control::operations::deploy::DeployExecutionCommand {
    let request = ployz_core::deploy::VolumeDeclaredDeployRequest::try_new(request)
        .expect("test deploy request normalizes");
    let store = crate::control::store::CoreStore::open_in_memory()
        .await
        .expect("open ingress store");
    let ployz_dns_target =
        crate::control::intent::ingress_intent::PloyzDnsTargetStore::new(store.clone());
    let ingress_projection =
        crate::control::intent::ingress_intent::IngressProjectionStore::new(store);
    let facts = load_deploy_execution_facts_from_nats(
        &request,
        intent_reader,
        facts_reader,
        &ployz_dns_target,
        &ingress_projection,
        step_timeout,
    )
    .await
    .expect("deploy facts load from nats");
    prepare_deploy_execution_command(operation_id, request.into_request(), facts)
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

    async fn declare_active_machine(&self, machine: &str) {
        self.machine_roster
            .replace_active_machine(&active_machine(machine))
            .await
            .expect("active machine stores");
    }

    async fn serve_machine_facts(
        &self,
        snapshot: MachineContainerObservationSnapshot,
    ) -> RunningMachineRoleRuntime {
        let machine_id = snapshot.machine_id().clone();
        let machine_client = self.connected.machine_client(&machine_id).await;
        let runtime = start_machine_role_runtime(
            machine_client,
            machine_id.clone(),
            StaticRunner::from_snapshot(snapshot),
            ReadyWireGuardEbpf::for_machine(&machine_id),
            UnusedLogs,
        )
        .await
        .expect("machine facts service starts");
        if self
            .intent_reader()
            .intent()
            .await
            .expect("intent reads")
            .dataplane_projection
            .declared_members()
            .iter()
            .any(|member| member.machine_id == machine_id)
        {
            self.wait_for_dataplane_projection(&machine_id).await;
        }
        runtime
    }

    async fn wait_for_dataplane_projection(&self, machine_id: &ployz_core::ids::MachineId) {
        let deadline = tokio::time::Instant::now() + Duration::from_secs(2);
        loop {
            let response = request_json::<_, MachineDataplaneStatusRpcResponse>(
                &self.connected.controller,
                ployz_nats::subjects::machine_service(
                    machine_id,
                    ployz_nats::subjects::MachineServiceEndpoint::DataplaneStatus,
                ),
                &MachineDataplaneStatusRpcRequest {
                    mode: ployz_core::network::NetworkStatusMode::Snapshot,
                },
                Duration::from_millis(250),
            )
            .await;
            if matches!(
                response,
                Ok(MachineRpcResponse::Ok(ok))
                    if matches!(
                        ok.value.projection.testimony,
                        ployz_core::network::DataplaneProjectionTestimony::Applied { .. }
                    )
            ) {
                return;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "machine {machine_id:?} did not converge its dataplane projection"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    }
}

#[derive(Clone)]
struct StaticRunner {
    existing: Vec<ExistingManagedContainer>,
    endpoint_subnet: Arc<Mutex<Option<ployz_core::network::MachineEndpointSubnet>>>,
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
                health_status: container.health_status,
                resolved_image_identity: container.resolved_image_identity.clone(),
                created_at_unix_seconds: container.created_at_unix_seconds,
            })
            .collect();
        Self {
            existing,
            endpoint_subnet: Arc::new(Mutex::new(None)),
        }
    }
}

impl crate::roles::machine::runner::MachineImageRemovalRunner for StaticRunner {
    async fn remove_image(
        &self,
        _image_identity: &ployz_core::image::OciDigest,
    ) -> Result<ployz_core::image::ImageRemoveOutcome, String> {
        Ok(ployz_core::image::ImageRemoveOutcome::AlreadyAbsent)
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

    async fn ensure_projection_endpoint_network(
        &self,
        expected_subnet: &ployz_core::network::MachineEndpointSubnet,
    ) -> Result<(), MachineContainerRunnerError> {
        *self
            .endpoint_subnet
            .lock()
            .expect("endpoint subnet lock is not poisoned") = Some(expected_subnet.clone());
        Ok(())
    }

    async fn read_endpoint_network_status(&self) -> ployz_core::network::EndpointBridgeStatus {
        self.endpoint_subnet
            .lock()
            .expect("endpoint subnet lock is not poisoned")
            .clone()
            .map_or(
                ployz_core::network::EndpointBridgeStatus::Missing,
                |subnet| ployz_core::network::EndpointBridgeStatus::Ready { subnet },
            )
    }

    async fn resolve_registry_image(
        &self,
        reference: &ployz_core::deploy::ImageReference,
        _credential: Option<&ployz_core::deploy::RegistryCredential>,
    ) -> Result<ployz_core::image::OciDigest, MachineContainerRunnerError> {
        Ok(ployz_core::image::OciDigest::sha256(
            reference.as_str().as_bytes(),
        ))
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

    async fn wait_managed_container(
        &self,
        _container_id: &ployz_core::ids::ContainerId,
    ) -> Result<i64, MachineContainerRunnerError> {
        Ok(0)
    }

    async fn stop_managed_container(
        &self,
        container_id: &ployz_core::ids::ContainerId,
        _expected_identity: &ployz_core::machine::runtime::ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRunnerError> {
        Err(MachineContainerRunnerError::Stop {
            container_id: container_id.clone(),
            message: "not used".to_owned(),
        })
    }

    async fn restart_managed_container(
        &self,
        container_id: &ployz_core::ids::ContainerId,
        _expected_identity: &ployz_core::machine::runtime::ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRunnerError> {
        Err(MachineContainerRunnerError::Restart {
            container_id: container_id.clone(),
            message: "not used".to_owned(),
        })
    }

    async fn remove_managed_container(
        &self,
        container_id: &ployz_core::ids::ContainerId,
        _expected_identity: &ployz_core::machine::runtime::ManagedContainerIdentity,
    ) -> Result<(), MachineContainerRunnerError> {
        Err(MachineContainerRunnerError::Remove {
            container_id: container_id.clone(),
            message: "not used".to_owned(),
        })
    }

    async fn remove_volume(
        &self,
        docker_volume_name: &str,
    ) -> Result<(), MachineContainerRunnerError> {
        Err(MachineContainerRunnerError::RemoveVolume {
            docker_volume_name: docker_volume_name.to_owned(),
            message: "not used".to_owned(),
        })
    }
}

fn existing_state(state: &ContainerRuntimeState) -> ExistingManagedContainerState {
    match state {
        ContainerRuntimeState::Running {
            ip,
            health,
            started_at_unix_ms,
        } => ExistingManagedContainerState::Running {
            ip: *ip,
            health: *health,
            started_at_unix_ms: *started_at_unix_ms,
        },
        ContainerRuntimeState::Exited => ExistingManagedContainerState::StartableStopped,
    }
}

#[derive(Clone)]
struct UnusedLogs;

impl MachineLogReader for UnusedLogs {
    async fn tail_container_logs(
        &self,
        container_id: &ployz_core::ids::ContainerId,
        _query: crate::roles::machine::runner::MachineLogQuery,
    ) -> Result<MachineLogTail, MachineLogReaderError> {
        Err(MachineLogReaderError::NotFound {
            container_id: container_id.clone(),
        })
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
    let core_store = crate::control::store::CoreStore::open_in_memory()
        .await
        .expect("open core store");
    crate::tests::support::intent::initialize_disabled_ingress(&core_store).await;
    let namespace_intent = NamespaceIntentStore::new(core_store.clone());
    let machine_roster = MachineRosterStore::new(core_store.clone());
    let intent = start_intent_service(
        connected.controller.clone(),
        machine_id("machine_a"),
        namespace_intent.clone(),
        core_store,
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
        origin: None,
        volumes: std::collections::BTreeMap::new(),
        services: vec![DeployServiceSpec {
            keep: None,
            service_id: service_id("svc_api"),
            image: ImageReference::try_new("registry.example/api:rev_2")
                .expect("valid image reference"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
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
        target: DeployRouteTarget::Hostname {
            hostname: RouteHostname::try_new("smoke.local").expect("valid route hostname"),
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

fn promoted_target_entry() -> ployz_core::intent::ServingTargetEntry {
    let mut entry = serving_target_entry("svc_api", "unused");
    entry.namespace_revision_entry_id = target_namespace_revision_entry_id();
    entry
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
    let ordinal = machine_id
        .as_bytes()
        .iter()
        .fold(0_u8, |acc, byte| acc.wrapping_add(*byte))
        .max(1);
    let machine_id = self::machine_id(machine_id);
    ActiveMachineState {
        control_endpoints: Vec::new(),
        mesh_endpoints: vec![std::net::SocketAddr::from((
            [203, 0, 113, ordinal],
            ployz_core::network::DEFAULT_WIREGUARD_LISTEN_PORT,
        ))],
        lifecycle: MachineLifecycle::Active,
        machine_id: machine_id.clone(),
        name: MachineName::try_new(machine_id.as_str()).expect("valid machine name"),
        activated_by: operation_id("op_machine_add"),
        roles: ployz_core::machine::roles::InstallRolePolicy::install_all(),
        endpoint_subnet: ployz_core::network::MachineEndpointSubnet::try_new(
            ployz_core::network::default_endpoint_subnet(&machine_id),
        )
        .expect("valid endpoint subnet"),
        wireguard_public_key: test_wireguard_public_key(&machine_id),
    }
}

fn cleanup_container(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: &str,
) -> ployz_core::deploy::ObservedCleanupCandidate {
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
) -> ployz_core::deploy::ObservedCleanupCandidate {
    ployz_core::deploy::ObservedCleanupCandidate {
        target: DeployCleanupContainer {
            machine_id: self::machine_id(machine_id),
            container_id: self::container_id(container_id),
            identity: containers::identity("svc_api")
                .entry(namespace_revision_entry_id.as_str())
                .operation("op_existing")
                .step(&format!("existing_{container_id}"))
                .build(),
        },
        state: ployz_core::machine::runtime::ContainerRuntimeState::running_unroutable(),
        created_at_unix_seconds: None,
        observed_image_identity: None,
    }
}

fn stopped_cleanup_container(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: &str,
) -> ployz_core::deploy::ObservedCleanupCandidate {
    let mut target = cleanup_container(machine_id, container_id, namespace_revision_entry_id);
    target.state = ployz_core::machine::runtime::ContainerRuntimeState::Exited;
    target
}
