use async_nats::jetstream;
use async_nats::jetstream::stream::StorageType;
use ployz_core::dataplane::{WireGuardEbpfPrepareError, WireGuardEbpfPrepareRequest};
use ployz_core::deploy::{DeployRequest, ImageReference, ReplicaCount};
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId};
use ployz_core::node::{
    ContainerRuntimeState, ManagedContainerKind, ManagedContainerObservation,
    NodeContainerObservationSnapshot,
};
use ployz_core::ops::{
    DeployOperationState, EventSequence, OperationIdempotencyKey, OperationStatus,
};
use ployz_nats::connect::NatsClientUrl;
use ployz_nats::core_state::AsyncNatsCoreStateStore;
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_nats::operation_api_client::OperationApiClient;
use ployz_sdk_types::{DeploySubmitRequest, OpsStatusRequest};
use ployzd::config::ControlProcessConfig;
use ployzd::nats_process::NatsServerRuntime;
use ployzd::node_agent::runtime::{
    CreateManagedContainer, ExistingManagedContainer, NodeContainerRunner, NodeContainerRunnerError,
};
use ployzd::node_service_runtime::{
    NodeWireGuardEbpfPreparer, start_node_runtime_service, start_node_wireguard_ebpf_service,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::test]
async fn control_runtime_bootstraps_nats_and_serves_operation_api() {
    let nats = TestNats::start().await;
    let config = control_config();
    let runtime =
        ployzd::control_runtime::start_control_runtime_with_client(nats.client.clone(), &config)
            .await
            .expect("control runtime starts");
    let api = OperationApiClient::new(nats.client.clone());

    let accepted = api
        .deploy_submit(&DeploySubmitRequest {
            operation_id: operation_id("op_control_runtime"),
            target: deploy_target("svc_api"),
            idempotency_key: idempotency_key("idem_control_runtime"),
        })
        .await
        .expect("operation API accepts deploy");

    assert_eq!(accepted.operation_id, operation_id("op_control_runtime"));
    assert_eq!(accepted.start_sequence, event_sequence(1));
    nats.jetstream
        .get_key_value("KV_CORE")
        .await
        .expect("control runtime created KV_CORE");
    nats.jetstream
        .get_key_value("KV_OPS")
        .await
        .expect("control runtime created KV_OPS");
    nats.jetstream
        .get_stream("PLZ_OPS")
        .await
        .expect("control runtime created PLZ_OPS");
    nats.jetstream
        .get_object_store("PLZ_BUNDLES")
        .await
        .expect("control runtime created PLZ_BUNDLES");

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_launches_deploy_submit_and_commits_active_state() {
    let nats = TestNats::start().await;
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect_err("control has not bootstrapped observations yet");
    assert!(matches!(
        observations,
        ployz_nats::observations::ObservationStoreError::OpenBucket { .. }
    ));

    let config = control_config()
        .with_deploy_nodes(vec![node_id("node_a")])
        .with_deploy_step_timeout(Duration::from_secs(2));
    let runtime =
        ployzd::control_runtime::start_control_runtime_with_client(nats.client.clone(), &config)
            .await
            .expect("control runtime starts");
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open observations");
    let runner = ObservingRunner::new(node_id("node_a"), observations);
    let _container_service =
        start_node_runtime_service(nats.client.clone(), node_id("node_a"), runner.clone())
            .await
            .expect("node container service starts");
    let _wireguard_ebpf_service = start_node_wireguard_ebpf_service(
        nats.client.clone(),
        node_id("node_a"),
        ReadyWireGuardEbpf,
    )
    .await
    .expect("node wireguard ebpf service starts");
    let api = OperationApiClient::new(nats.client.clone());
    let request = DeploySubmitRequest {
        operation_id: operation_id("op_launch"),
        target: deploy_target("svc_api"),
        idempotency_key: idempotency_key("idem_launch"),
    };

    let accepted = api
        .deploy_submit(&request)
        .await
        .expect("operation API accepts deploy");

    assert_eq!(accepted.operation_id, operation_id("op_launch"));
    let status = wait_for_terminal_deploy_status(&api, operation_id("op_launch")).await;
    assert!(matches!(
        status,
        OperationStatus::Deploy {
            state: DeployOperationState::Completed,
            ..
        }
    ));
    let core_state = AsyncNatsCoreStateStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open core state");
    assert_eq!(
        core_state
            .active_service(&service_id("svc_api"))
            .await
            .expect("active service reads")
            .expect("active service committed")
            .active_revision,
        revision_id("rev_2")
    );
    let duplicate = api
        .deploy_submit(&request)
        .await
        .expect("duplicate operation API submit returns original operation");
    assert_eq!(duplicate.operation_id, operation_id("op_launch"));
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert_eq!(runner.created_count(), 1);

    runtime
        .shutdown()
        .await
        .expect("control runtime shuts down");
}

#[tokio::test]
async fn control_runtime_refuses_bootstrap_resource_drift() {
    let nats = TestNats::start().await;
    nats.jetstream
        .create_stream(jetstream::stream::Config {
            name: "PLZ_OPS".to_owned(),
            subjects: vec!["wrong.>".to_owned()],
            storage: StorageType::File,
            ..Default::default()
        })
        .await
        .expect("create drifted PLZ_OPS stream");

    let config = control_config();
    let error = match ployzd::control_runtime::start_control_runtime_with_client(
        nats.client.clone(),
        &config,
    )
    .await
    {
        Ok(runtime) => {
            runtime.shutdown().await.expect("unexpected runtime stops");
            panic!("control runtime should refuse drift");
        }
        Err(error) => error,
    };

    assert!(matches!(
        error,
        ployzd::control_runtime::ControlRuntimeError::AssureBootstrap(
            ployz_nats::bootstrap::BootstrapAssuranceError::RefuseResource { .. }
        )
    ));
}

struct TestNats {
    _server: nats_server::Server,
    client: async_nats::Client,
    jetstream: jetstream::Context,
}

impl TestNats {
    async fn start() -> Self {
        let config = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../ployz-nats/tests/configs/jetstream.conf"
        );
        let server = nats_server::run_server(config);
        let client = async_nats::connect(server.client_url())
            .await
            .expect("connect to test nats");
        let jetstream = jetstream::new(client.clone());

        Self {
            _server: server,
            client,
            jetstream,
        }
    }
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

fn control_config() -> ControlProcessConfig {
    ControlProcessConfig::new(
        NatsServerRuntime::External(NatsClientUrl::loopback(4222)),
        ployz_core::ids::NodeId::try_new("core_1").expect("valid node id"),
    )
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image reference")
}

fn replicas(value: u16) -> ReplicaCount {
    ReplicaCount::try_new(value).expect("valid replica count")
}

fn idempotency_key(value: &str) -> OperationIdempotencyKey {
    OperationIdempotencyKey::try_new(value).expect("valid idempotency key")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn deploy_target(service_id: &str) -> DeployRequest {
    DeployRequest {
        service_id: self::service_id(service_id),
        target_revision: revision_id("rev_2"),
        image: image("ghcr.io/acme/api:rev-2"),
        replicas: replicas(1),
        route: None,
    }
}

async fn wait_for_terminal_deploy_status(
    api: &OperationApiClient,
    operation_id: OperationId,
) -> OperationStatus {
    for _ in 0..80 {
        let status = api
            .ops_status(&OpsStatusRequest {
                operation_id: operation_id.clone(),
            })
            .await
            .expect("status is readable")
            .status;
        let OperationStatus::Deploy { state, .. } = &status else {
            panic!("expected deploy status");
        };
        if state.is_terminal() {
            return status;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    panic!("deploy did not reach terminal status");
}

#[derive(Clone)]
struct ObservingRunner {
    node_id: NodeId,
    observations: AsyncNatsObservationStore,
    state: Arc<Mutex<ObservingRunnerState>>,
}

#[derive(Default)]
struct ObservingRunnerState {
    next_container: u64,
    created_count: u64,
}

impl ObservingRunner {
    fn new(node_id: NodeId, observations: AsyncNatsObservationStore) -> Self {
        Self {
            node_id,
            observations,
            state: Arc::new(Mutex::new(ObservingRunnerState {
                next_container: 1,
                created_count: 0,
            })),
        }
    }

    fn created_count(&self) -> u64 {
        self.state
            .lock()
            .expect("observing runner lock is not poisoned")
            .created_count
    }
}

impl NodeContainerRunner for ObservingRunner {
    async fn existing_managed_containers(
        &self,
    ) -> Result<Vec<ExistingManagedContainer>, NodeContainerRunnerError> {
        Ok(Vec::new())
    }

    async fn create_managed_container(
        &self,
        command: CreateManagedContainer,
    ) -> Result<ContainerId, NodeContainerRunnerError> {
        let container_id = {
            let mut state = self
                .state
                .lock()
                .expect("observing runner lock is not poisoned");
            let container_id = container_id(&format!("ctr_{}", state.next_container));
            state.next_container += 1;
            state.created_count += 1;
            container_id
        };
        let observation = ManagedContainerObservation {
            node_id: self.node_id.clone(),
            container_id: container_id.clone(),
            service_id: command.labels.service_id,
            revision_id: command.labels.revision_id,
            operation_id: command.labels.operation_id,
            step_id: command.labels.step_id,
            kind: ManagedContainerKind::Service,
            state: ContainerRuntimeState::Running,
        };
        let snapshot = self
            .observations
            .node_snapshot(&self.node_id)
            .await
            .map_err(|error| NodeContainerRunnerError::Create {
                message: error.to_string(),
            })?
            .unwrap_or_else(|| {
                NodeContainerObservationSnapshot::try_new(self.node_id.clone(), Vec::new())
                    .expect("empty node snapshot is valid")
            })
            .with_container_replaced(observation)
            .map_err(|error| NodeContainerRunnerError::Create {
                message: error.to_string(),
            })?;
        self.observations
            .replace_node_containers(&snapshot)
            .await
            .map_err(|error| NodeContainerRunnerError::Create {
                message: error.to_string(),
            })?;

        Ok(container_id)
    }
}

#[derive(Clone)]
struct ReadyWireGuardEbpf;

impl NodeWireGuardEbpfPreparer for ReadyWireGuardEbpf {
    async fn prepare_wireguard_ebpf(
        &self,
        _request: WireGuardEbpfPrepareRequest,
    ) -> Result<(), WireGuardEbpfPrepareError> {
        Ok(())
    }
}
