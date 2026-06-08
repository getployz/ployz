use ployz_core::dataplane::{
    WireGuardEbpfComponent, WireGuardEbpfPrepareError, WireGuardEbpfPrepareRequest,
};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::ManagedContainerKind;
use ployz_core::ops::FailureMessage;
use ployz_core::subjects::NodeServiceEndpoint;
use ployz_nats::service_runtime::request_json;
use ployzd::deploy_worker::{
    NodeContainerRuntime, NodeContainerRuntimeError, NodeRunContainerOutcome,
    NodeRunContainerRequest, NodeRuntimeUnavailableReason, WireGuardEbpfPreparer,
};
use ployzd::docker::labels::ManagedContainerLabels;
use ployzd::node_agent::runtime::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    NodeContainerRunner, NodeContainerRunnerError,
};
use ployzd::node_protocol::{
    NodeWireGuardEbpfPrepareRpcRequest, NodeWireGuardEbpfPrepareRpcResponse,
};
use ployzd::node_rpc::{NatsNodeContainerRuntime, NatsNodeWireGuardEbpfPreparer};
use ployzd::node_service_runtime::{
    NodeWireGuardEbpfPreparer as LocalWireGuardEbpfPreparer, start_node_runtime_service,
};
use ployzd::services::node_endpoint_subject;
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::test]
async fn node_runtime_service_creates_missing_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_node_runtime_service(
        nats.client.clone(),
        node_id("node_a"),
        RecordingRunner::new(state.clone()).with_next_container("ctr_created"),
        ready_wireguard_ebpf(),
    )
    .await
    .expect("node runtime service starts");
    let mut client = NatsNodeContainerRuntime::new(nats.client);

    let outcome = client
        .run_container(run_request("node_a"))
        .await
        .expect("container run succeeds");

    assert_eq!(
        outcome,
        NodeRunContainerOutcome::Created {
            container_id: container_id("ctr_created"),
        }
    );
    assert_eq!(
        state.creates(),
        vec![CreateManagedContainer {
            image: image("registry.example/api:rev_2"),
            labels: managed_labels(),
        }]
    );
}

#[tokio::test]
async fn node_runtime_service_reuses_existing_operation_step_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_node_runtime_service(
        nats.client.clone(),
        node_id("node_a"),
        RecordingRunner::new(state.clone())
            .with_existing(existing_container("ctr_existing", managed_labels())),
        ready_wireguard_ebpf(),
    )
    .await
    .expect("node runtime service starts");
    let mut client = NatsNodeContainerRuntime::new(nats.client);

    let outcome = client
        .run_container(run_request("node_a"))
        .await
        .expect("container run succeeds");

    assert_eq!(
        outcome,
        NodeRunContainerOutcome::ReusedRunning {
            container_id: container_id("ctr_existing"),
        }
    );
    assert!(state.creates().is_empty());
}

#[tokio::test]
async fn node_runtime_service_starts_existing_stopped_operation_step_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_node_runtime_service(
        nats.client.clone(),
        node_id("node_a"),
        RecordingRunner::new(state.clone()).with_existing(existing_container_with_state(
            "ctr_existing",
            managed_labels(),
            ExistingManagedContainerState::StartableStopped,
        )),
        ready_wireguard_ebpf(),
    )
    .await
    .expect("node runtime service starts");
    let mut client = NatsNodeContainerRuntime::new(nats.client);

    let outcome = client
        .run_container(run_request("node_a"))
        .await
        .expect("container run succeeds");

    assert_eq!(
        outcome,
        NodeRunContainerOutcome::StartedExisting {
            container_id: container_id("ctr_existing"),
        }
    );
    assert_eq!(state.starts(), vec![container_id("ctr_existing")]);
    assert!(state.creates().is_empty());
}

#[tokio::test]
async fn node_runtime_service_reports_start_failure_with_container_evidence() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_node_runtime_service(
        nats.client.clone(),
        node_id("node_a"),
        RecordingRunner::new(state).with_start_failure("ctr_created", "exec format error"),
        ready_wireguard_ebpf(),
    )
    .await
    .expect("node runtime service starts");
    let mut client = NatsNodeContainerRuntime::new(nats.client);

    let error = client
        .run_container(run_request("node_a"))
        .await
        .expect_err("container start failure is returned");

    assert_eq!(
        error,
        NodeContainerRuntimeError::CreatedContainerStartFailed {
            node_id: node_id("node_a"),
            container_id: container_id("ctr_created"),
            message: failure_message("container start failed: exec format error"),
            inspect_hint: inspect_hint("ctr_created"),
        }
    );
}

#[tokio::test]
async fn node_runtime_service_reports_existing_start_failure_without_created_evidence() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_node_runtime_service(
        nats.client.clone(),
        node_id("node_a"),
        RecordingRunner::new(state).with_existing_start_failure("ctr_existing", "still stopping"),
        ready_wireguard_ebpf(),
    )
    .await
    .expect("node runtime service starts");
    let mut client = NatsNodeContainerRuntime::new(nats.client);

    let error = client
        .run_container(run_request("node_a"))
        .await
        .expect_err("existing container start failure is returned");

    assert_eq!(
        error,
        NodeContainerRuntimeError::ExistingContainerStartFailed {
            node_id: node_id("node_a"),
            container_id: container_id("ctr_existing"),
            message: failure_message("container start failed: still stopping"),
            inspect_hint: inspect_hint("ctr_existing"),
        }
    );
}

#[tokio::test]
async fn node_runtime_service_reports_operation_step_conflict_as_domain_error() {
    let nats = test_nats().await;
    let mut conflicting_labels = managed_labels();
    conflicting_labels.revision_id = revision_id("rev_other");
    let state = RecordingRunnerState::default();
    let service = start_node_runtime_service(
        nats.client.clone(),
        node_id("node_a"),
        RecordingRunner::new(state).with_existing(existing_container(
            "ctr_conflict",
            conflicting_labels.clone(),
        )),
        ready_wireguard_ebpf(),
    )
    .await
    .expect("node runtime service starts");
    let mut client = NatsNodeContainerRuntime::new(nats.client);

    let error = client
        .run_container(run_request("node_a"))
        .await
        .expect_err("container run reports conflict");

    assert_eq!(
        error,
        NodeContainerRuntimeError::OperationStepConflict {
            node_id: node_id("node_a"),
            container_id: container_id("ctr_conflict"),
            expected: managed_labels(),
            actual: conflicting_labels,
        }
    );
    assert_eq!(service.health().domain_failures, 1);
}

#[tokio::test]
async fn node_runtime_service_maps_create_failure_to_unavailable_runtime() {
    let nats = test_nats().await;
    let _service = start_node_runtime_service(
        nats.client.clone(),
        node_id("node_a"),
        RecordingRunner::new(RecordingRunnerState::default()).with_create_failure("disk full"),
        ready_wireguard_ebpf(),
    )
    .await
    .expect("node runtime service starts");
    let mut client = NatsNodeContainerRuntime::new(nats.client);

    let error = client
        .run_container(run_request("node_a"))
        .await
        .expect_err("container create fails");

    assert_eq!(
        error,
        NodeContainerRuntimeError::Unavailable {
            node_id: node_id("node_a"),
            reason: NodeRuntimeUnavailableReason::ServiceInternal {
                message: "container create failed: disk full".to_owned(),
            },
        }
    );
}

#[tokio::test]
async fn node_wireguard_ebpf_service_calls_local_preparer() {
    let nats = test_nats().await;
    let state = RecordingWireGuardEbpfState::default();
    let _service = start_node_runtime_service(
        nats.client.clone(),
        node_id("node_a"),
        idle_runner(),
        RecordingWireGuardEbpf::new(state.clone()),
    )
    .await
    .expect("node wireguard ebpf service starts");
    let mut client = NatsNodeWireGuardEbpfPreparer::new(nats.client);

    client
        .prepare_wireguard_ebpf(wireguard_ebpf_request(&["node_a"]))
        .await
        .expect("wireguard ebpf prepare succeeds");

    assert_eq!(state.prepare_count(), 1);
}

#[tokio::test]
async fn node_wireguard_ebpf_service_rejects_request_not_targeting_this_node() {
    let nats = test_nats().await;
    let state = RecordingWireGuardEbpfState::default();
    let _service = start_node_runtime_service(
        nats.client.clone(),
        node_id("node_a"),
        idle_runner(),
        RecordingWireGuardEbpf::new(state.clone()),
    )
    .await
    .expect("node wireguard ebpf service starts");
    let response = request_json::<_, NodeWireGuardEbpfPrepareRpcResponse>(
        &nats.client,
        node_endpoint_subject(
            &node_id("node_a"),
            NodeServiceEndpoint::WireGuardEbpfPrepare,
        ),
        &NodeWireGuardEbpfPrepareRpcRequest {
            operation_id: operation_id("op_123"),
            nodes: vec![node_id("node_b")],
        },
        Duration::from_secs(1),
    )
    .await
    .expect("node service responds");

    assert!(matches!(
        response,
        NodeWireGuardEbpfPrepareRpcResponse::DomainError {
            node_id,
            error: ployzd::node_protocol::NodeWireGuardEbpfPrepareDomainError::Unavailable {
                component: WireGuardEbpfComponent::WireGuard,
                ..
            },
            ..
        } if node_id == self::node_id("node_a")
    ));
    assert_eq!(state.prepare_count(), 0);
}

#[tokio::test]
async fn node_wireguard_ebpf_service_preserves_prepare_failure() {
    let nats = test_nats().await;
    let state = RecordingWireGuardEbpfState::default();
    let _service = start_node_runtime_service(
        nats.client.clone(),
        node_id("node_a"),
        idle_runner(),
        RecordingWireGuardEbpf::new(state).with_failure(WireGuardEbpfPrepareError::Unavailable {
            node_id: node_id("node_a"),
            component: WireGuardEbpfComponent::EbpfForwarding,
            message: failure_message("ebpf program missing"),
        }),
    )
    .await
    .expect("node wireguard ebpf service starts");
    let mut client = NatsNodeWireGuardEbpfPreparer::new(nats.client);

    let error = client
        .prepare_wireguard_ebpf(wireguard_ebpf_request(&["node_a"]))
        .await
        .expect_err("wireguard ebpf prepare failure is returned");

    assert_eq!(
        error,
        WireGuardEbpfPrepareError::Unavailable {
            node_id: node_id("node_a"),
            component: WireGuardEbpfComponent::EbpfForwarding,
            message: failure_message("ebpf program missing"),
        }
    );
}

#[derive(Clone, Default)]
struct RecordingRunnerState {
    inner: Arc<Mutex<RecordingRunnerInner>>,
}

impl RecordingRunnerState {
    fn creates(&self) -> Vec<CreateManagedContainer> {
        self.inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .creates
            .clone()
    }

    fn starts(&self) -> Vec<ContainerId> {
        self.inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .starts
            .clone()
    }
}

#[derive(Default)]
struct RecordingRunnerInner {
    creates: Vec<CreateManagedContainer>,
    starts: Vec<ContainerId>,
}

#[derive(Clone)]
struct RecordingRunner {
    state: RecordingRunnerState,
    existing: Vec<ExistingManagedContainer>,
    next_container: Option<ContainerId>,
    create_failure: Option<String>,
    start_failure: Option<(ContainerId, String)>,
}

impl RecordingRunner {
    fn new(state: RecordingRunnerState) -> Self {
        Self {
            state,
            existing: Vec::new(),
            next_container: None,
            create_failure: None,
            start_failure: None,
        }
    }

    fn with_existing(mut self, existing: ExistingManagedContainer) -> Self {
        self.existing.push(existing);
        self
    }

    fn with_next_container(mut self, container_id: &str) -> Self {
        self.next_container = Some(self::container_id(container_id));
        self
    }

    fn with_create_failure(mut self, message: &str) -> Self {
        self.create_failure = Some(message.to_owned());
        self
    }

    fn with_start_failure(mut self, container_id: &str, message: &str) -> Self {
        self.next_container = Some(self::container_id(container_id));
        self.start_failure = Some((self::container_id(container_id), message.to_owned()));
        self
    }

    fn with_existing_start_failure(mut self, container_id: &str, message: &str) -> Self {
        self.existing.push(existing_container_with_state(
            container_id,
            managed_labels(),
            ExistingManagedContainerState::StartableStopped,
        ));
        self.start_failure = Some((self::container_id(container_id), message.to_owned()));
        self
    }
}

impl NodeContainerRunner for RecordingRunner {
    async fn existing_managed_containers(
        &self,
    ) -> Result<Vec<ExistingManagedContainer>, NodeContainerRunnerError> {
        Ok(self.existing.clone())
    }

    async fn create_managed_container(
        &self,
        command: CreateManagedContainer,
    ) -> Result<ContainerId, NodeContainerRunnerError> {
        if let Some(message) = self.create_failure.clone() {
            return Err(NodeContainerRunnerError::Create { message });
        }

        self.state
            .inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .creates
            .push(command);
        self.next_container
            .clone()
            .ok_or_else(|| NodeContainerRunnerError::Create {
                message: "missing next container id".to_owned(),
            })
    }

    async fn start_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> Result<(), NodeContainerRunnerError> {
        if let Some((failed_container_id, message)) = self.start_failure.clone()
            && failed_container_id == *container_id
        {
            return Err(NodeContainerRunnerError::Start {
                container_id: container_id.clone(),
                message,
            });
        }

        self.state
            .inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .starts
            .push(container_id.clone());
        Ok(())
    }
}

#[derive(Clone, Default)]
struct RecordingWireGuardEbpfState {
    inner: Arc<Mutex<RecordingWireGuardEbpfInner>>,
}

impl RecordingWireGuardEbpfState {
    fn prepare_count(&self) -> usize {
        self.inner
            .lock()
            .expect("recording wireguard ebpf lock is not poisoned")
            .prepare_count
    }
}

#[derive(Default)]
struct RecordingWireGuardEbpfInner {
    prepare_count: usize,
}

#[derive(Clone)]
struct RecordingWireGuardEbpf {
    state: RecordingWireGuardEbpfState,
    failure: Option<WireGuardEbpfPrepareError>,
}

impl RecordingWireGuardEbpf {
    fn new(state: RecordingWireGuardEbpfState) -> Self {
        Self {
            state,
            failure: None,
        }
    }

    fn with_failure(mut self, failure: WireGuardEbpfPrepareError) -> Self {
        self.failure = Some(failure);
        self
    }
}

impl LocalWireGuardEbpfPreparer for RecordingWireGuardEbpf {
    async fn prepare_wireguard_ebpf(&self) -> Result<(), WireGuardEbpfPrepareError> {
        self.state
            .inner
            .lock()
            .expect("recording wireguard ebpf lock is not poisoned")
            .prepare_count += 1;

        match &self.failure {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
}

fn idle_runner() -> RecordingRunner {
    RecordingRunner::new(RecordingRunnerState::default())
}

fn ready_wireguard_ebpf() -> RecordingWireGuardEbpf {
    RecordingWireGuardEbpf::new(RecordingWireGuardEbpfState::default())
}

struct TestNats {
    _server: nats_server::Server,
    client: async_nats::Client,
}

async fn test_nats() -> TestNats {
    let config = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../ployz-nats/tests/configs/jetstream.conf"
    );
    let server = nats_server::run_server(config);
    let client = async_nats::connect(server.client_url())
        .await
        .expect("connect to test nats");

    TestNats {
        _server: server,
        client,
    }
}

fn run_request(node_id: &str) -> NodeRunContainerRequest {
    NodeRunContainerRequest {
        node_id: self::node_id(node_id),
        image: image("registry.example/api:rev_2"),
        labels: managed_labels(),
    }
}

fn wireguard_ebpf_request(nodes: &[&str]) -> WireGuardEbpfPrepareRequest {
    WireGuardEbpfPrepareRequest {
        operation_id: operation_id("op_123"),
        nodes: nodes.iter().map(|node| node_id(node)).collect(),
    }
}

fn failure_message(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("valid failure message")
}

fn inspect_hint(container_id: &str) -> ployz_core::ops::OperatorHint {
    ployz_core::ops::OperatorHint::try_new(format!("ployz container inspect {container_id}"))
        .expect("valid inspect hint")
}

fn existing_container(
    container_id: &str,
    labels: ManagedContainerLabels,
) -> ExistingManagedContainer {
    existing_container_with_state(
        container_id,
        labels,
        ExistingManagedContainerState::Running { endpoint: None },
    )
}

fn existing_container_with_state(
    container_id: &str,
    labels: ManagedContainerLabels,
    state: ExistingManagedContainerState,
) -> ExistingManagedContainer {
    ExistingManagedContainer {
        container_id: self::container_id(container_id),
        labels,
        state,
    }
}

fn managed_labels() -> ManagedContainerLabels {
    ManagedContainerLabels {
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_2"),
        operation_id: operation_id("op_123"),
        step_id: step_id("run_1"),
        kind: ManagedContainerKind::Service,
        endpoint_port: None,
    }
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn step_id(value: &str) -> StepId {
    StepId::try_new(value).expect("valid step id")
}

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image")
}
