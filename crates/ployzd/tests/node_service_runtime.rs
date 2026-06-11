use ployz_core::dataplane::{
    EbpfForwardingReady, EbpfForwardingReadyEvidence, WireGuardEbpfComponent,
    WireGuardEbpfNodeReady, WireGuardEbpfPrepareError, WireGuardEbpfPrepareRequest,
    WireGuardEbpfReady, WireGuardPublicKey, WireGuardReady, WireGuardReadyEvidence,
};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::{ContainerId, NodeId, OperationId, RevisionId, ServiceId, StepId};
use ployz_core::node::ManagedContainerKind;
use ployz_core::ops::FailureMessage;
use ployz_core::subjects::{NodeServiceEndpoint, node_service};
use ployz_nats::service_runtime::request_json;
use ployzd::deploy_worker::{
    NodeContainerRunSpec, NodeContainerRuntime, NodeContainerRuntimeError,
    NodeEnsureEndpointNetworkRequest, NodeRunContainerOutcome, NodeRunContainerRequest,
    NodeRuntimeUnavailableReason, NodeStopContainerRequest, WireGuardEbpfPreparer,
};
use ployzd::docker::labels::{ManagedContainerIdentity, ManagedContainerLabels};
use ployzd::node_agent::runtime::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    NodeContainerRunner, NodeContainerRunnerError, NodeLogReader, NodeLogReaderError, NodeLogTail,
};
use ployzd::node_protocol::{
    NodeContainerRemoveDomainError, NodeContainerRemoveRpcRequest, NodeContainerRemoveRpcResponse,
    NodeContainerStopDomainError, NodeContainerStopRpcRequest, NodeContainerStopRpcResponse,
    NodeLogsTailRpcRequest, NodeLogsTailRpcResponse, NodeWireGuardEbpfPreparePhase,
    NodeWireGuardEbpfPrepareRpcRequest, NodeWireGuardEbpfPrepareRpcResponse,
};
use ployzd::node_rpc::{NatsNodeContainerRuntime, NatsNodeWireGuardEbpfPreparer};
use ployzd::node_service_runtime::{
    NodeWireGuardEbpfPreparer as LocalWireGuardEbpfPreparer, start_node_runtime_service,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::test]
async fn node_runtime_service_ensures_endpoint_network() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_node_runtime_service(
        nats.node_a.clone(),
        node_id("node_a"),
        RecordingRunner::new(state.clone()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("node runtime service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");
    let mut client = NatsNodeContainerRuntime::new(nats.client);

    client
        .ensure_endpoint_network(NodeEnsureEndpointNetworkRequest {
            node_id: node_id("node_a"),
            operation_id: operation_id("op_123"),
        })
        .await
        .expect("endpoint network ensure succeeds");

    assert_eq!(state.endpoint_networks(), 1);
}

#[tokio::test]
async fn node_runtime_service_creates_missing_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_node_runtime_service(
        nats.node_a.clone(),
        node_id("node_a"),
        RecordingRunner::new(state.clone()).with_next_container("ctr_created"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("node runtime service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");
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
            endpoint: None,
            labels: managed_labels(),
        }]
    );
}

#[tokio::test]
async fn node_runtime_service_reuses_existing_operation_step_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_node_runtime_service(
        nats.node_a.clone(),
        node_id("node_a"),
        RecordingRunner::new(state.clone())
            .with_existing(existing_container("ctr_existing", managed_labels())),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("node runtime service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");
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
        nats.node_a.clone(),
        node_id("node_a"),
        RecordingRunner::new(state.clone()).with_existing(existing_container_with_state(
            "ctr_existing",
            managed_labels(),
            ExistingManagedContainerState::StartableStopped,
        )),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("node runtime service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");
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
        nats.node_a.clone(),
        node_id("node_a"),
        RecordingRunner::new(state).with_start_failure("ctr_created", "exec format error"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("node runtime service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");
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
        nats.node_a.clone(),
        node_id("node_a"),
        RecordingRunner::new(state).with_existing_start_failure("ctr_existing", "still stopping"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("node runtime service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");
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
        nats.node_a.clone(),
        node_id("node_a"),
        RecordingRunner::new(state).with_existing(existing_container(
            "ctr_conflict",
            conflicting_labels.clone(),
        )),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("node runtime service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");
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
        nats.node_a.clone(),
        node_id("node_a"),
        RecordingRunner::new(RecordingRunnerState::default()).with_create_failure("disk full"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("node runtime service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");
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
async fn node_runtime_service_removes_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_node_runtime_service(
        nats.node_a.clone(),
        node_id("node_a"),
        RecordingRunner::new(state.clone()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("node runtime service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");

    let response = request_json::<_, NodeContainerRemoveRpcResponse>(
        &nats.client,
        node_service(&node_id("node_a"), NodeServiceEndpoint::ContainerRemove),
        &NodeContainerRemoveRpcRequest {
            operation_id: operation_id("op_123"),
            container_id: container_id("ctr_old"),
            expected_identity: managed_labels().identity(),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("node service responds");

    assert_eq!(
        response,
        NodeContainerRemoveRpcResponse::Ok {
            node_id: node_id("node_a"),
            container_id: container_id("ctr_old"),
        }
    );
    assert_eq!(state.removes(), vec![container_id("ctr_old")]);
}

#[tokio::test]
async fn node_runtime_service_stops_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_node_runtime_service(
        nats.node_a.clone(),
        node_id("node_a"),
        RecordingRunner::new(state.clone()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("node runtime service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");
    let mut client = NatsNodeContainerRuntime::new(nats.client);

    client
        .stop_container(NodeStopContainerRequest {
            node_id: node_id("node_a"),
            operation_id: operation_id("op_123"),
            container_id: container_id("ctr_failed"),
            expected_identity: managed_labels().identity(),
        })
        .await
        .expect("container stop succeeds");

    assert_eq!(state.stops(), vec![container_id("ctr_failed")]);
}

#[tokio::test]
async fn node_runtime_service_reports_remove_failure_as_domain_error() {
    let nats = test_nats().await;
    let _service = start_node_runtime_service(
        nats.node_a.clone(),
        node_id("node_a"),
        RecordingRunner::new(RecordingRunnerState::default())
            .with_remove_failure("ctr_old", "busy"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("node runtime service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");

    let response = request_json::<_, NodeContainerRemoveRpcResponse>(
        &nats.client,
        node_service(&node_id("node_a"), NodeServiceEndpoint::ContainerRemove),
        &NodeContainerRemoveRpcRequest {
            operation_id: operation_id("op_123"),
            container_id: container_id("ctr_old"),
            expected_identity: managed_labels().identity(),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("node service responds");

    assert_eq!(
        response,
        NodeContainerRemoveRpcResponse::DomainError {
            node_id: node_id("node_a"),
            error: NodeContainerRemoveDomainError::RemoveFailed {
                container_id: container_id("ctr_old"),
                message: failure_message("container remove failed: busy"),
                inspect_hint: inspect_hint("ctr_old"),
            },
        }
    );
}

#[tokio::test]
async fn node_runtime_service_reports_stop_failure_as_domain_error() {
    let nats = test_nats().await;
    let _service = start_node_runtime_service(
        nats.node_a.clone(),
        node_id("node_a"),
        RecordingRunner::new(RecordingRunnerState::default())
            .with_stop_failure("ctr_failed", "permission denied"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("node runtime service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");

    let response = request_json::<_, NodeContainerStopRpcResponse>(
        &nats.client,
        node_service(&node_id("node_a"), NodeServiceEndpoint::ContainerStop),
        &NodeContainerStopRpcRequest {
            operation_id: operation_id("op_123"),
            container_id: container_id("ctr_failed"),
            expected_identity: managed_labels().identity(),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("node service responds");

    assert_eq!(
        response,
        NodeContainerStopRpcResponse::DomainError {
            node_id: node_id("node_a"),
            error: NodeContainerStopDomainError::StopFailed {
                container_id: container_id("ctr_failed"),
                message: failure_message("container stop failed: permission denied"),
                inspect_hint: inspect_hint("ctr_failed"),
            },
        }
    );
}

#[tokio::test]
async fn node_runtime_service_tails_container_logs() {
    let nats = test_nats().await;
    let _service = start_node_runtime_service(
        nats.node_a.clone(),
        node_id("node_a"),
        RecordingRunner::new(RecordingRunnerState::default())
            .with_existing(existing_container("ctr_failed", managed_labels())),
        ready_wireguard_ebpf(),
        RecordingLogReader::new("ctr_failed", "panic: missing DATABASE_URL\n"),
    )
    .await
    .expect("node runtime service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");

    let response = request_json::<_, NodeLogsTailRpcResponse>(
        &nats.client,
        node_service(&node_id("node_a"), NodeServiceEndpoint::LogsTail),
        &NodeLogsTailRpcRequest {
            container_id: container_id("ctr_failed"),
            tail_lines: Some(50),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("node service responds");

    assert_eq!(
        response,
        NodeLogsTailRpcResponse::Ok {
            value: ployzd::node_runtime_types::NodeLogsTailResult {
                node_id: node_id("node_a"),
                container_id: container_id("ctr_failed"),
                text: "panic: missing DATABASE_URL\n".to_owned(),
                truncated: false,
            },
        }
    );
}

#[tokio::test]
async fn node_wireguard_ebpf_service_calls_local_preparer() {
    let nats = test_nats().await;
    let state = RecordingWireGuardEbpfState::default();
    let _service = start_node_runtime_service(
        nats.node_a.clone(),
        node_id("node_a"),
        idle_runner(),
        RecordingWireGuardEbpf::new(state.clone()),
        idle_logs(),
    )
    .await
    .expect("node wireguard ebpf service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");
    let mut client = NatsNodeWireGuardEbpfPreparer::new(nats.client);

    let report = client
        .prepare_wireguard_ebpf(wireguard_ebpf_request(&["node_a"]))
        .await
        .expect("wireguard ebpf prepare succeeds");

    assert_eq!(state.prepare_count(), 1);
    assert_eq!(state.endpoint_routes(), endpoint_routes(&["node_a"]));
    assert_eq!(state.peers(), Vec::new());
    assert_eq!(report.nodes, vec![ready_node("node_a")]);
}

#[tokio::test]
async fn node_wireguard_ebpf_service_rejects_request_not_targeting_this_node() {
    let nats = test_nats().await;
    let state = RecordingWireGuardEbpfState::default();
    let _service = start_node_runtime_service(
        nats.node_a.clone(),
        node_id("node_a"),
        idle_runner(),
        RecordingWireGuardEbpf::new(state.clone()),
        idle_logs(),
    )
    .await
    .expect("node wireguard ebpf service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");
    let response = request_json::<_, NodeWireGuardEbpfPrepareRpcResponse>(
        &nats.client,
        node_service(
            &node_id("node_a"),
            NodeServiceEndpoint::WireGuardEbpfPrepare,
        ),
        &NodeWireGuardEbpfPrepareRpcRequest {
            phase: NodeWireGuardEbpfPreparePhase::PrepareDataplane,
            operation_id: operation_id("op_123"),
            nodes: vec![node_id("node_b")],
            endpoint_routes: endpoint_routes(&["node_b"]),
            peer_endpoints: Vec::new(),
            peers: Vec::new(),
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
        nats.node_a.clone(),
        node_id("node_a"),
        idle_runner(),
        RecordingWireGuardEbpf::new(state).with_failure(WireGuardEbpfPrepareError::Unavailable {
            node_id: node_id("node_a"),
            component: WireGuardEbpfComponent::EbpfForwarding,
            message: failure_message("ebpf program missing"),
        }),
        idle_logs(),
    )
    .await
    .expect("node wireguard ebpf service starts");
    nats.node_a
        .flush()
        .await
        .expect("flush node service subscription");
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
    fn endpoint_networks(&self) -> usize {
        self.inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .endpoint_networks
    }

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

    fn removes(&self) -> Vec<ContainerId> {
        self.inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .removes
            .clone()
    }

    fn stops(&self) -> Vec<ContainerId> {
        self.inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .stops
            .clone()
    }
}

#[derive(Default)]
struct RecordingRunnerInner {
    endpoint_networks: usize,
    creates: Vec<CreateManagedContainer>,
    starts: Vec<ContainerId>,
    stops: Vec<ContainerId>,
    removes: Vec<ContainerId>,
}

#[derive(Clone)]
struct RecordingRunner {
    state: RecordingRunnerState,
    existing: Vec<ExistingManagedContainer>,
    next_container: Option<ContainerId>,
    create_failure: Option<String>,
    start_failure: Option<(ContainerId, String)>,
    stop_failure: Option<(ContainerId, String)>,
    remove_failure: Option<(ContainerId, String)>,
}

impl RecordingRunner {
    fn new(state: RecordingRunnerState) -> Self {
        Self {
            state,
            existing: Vec::new(),
            next_container: None,
            create_failure: None,
            start_failure: None,
            stop_failure: None,
            remove_failure: None,
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

    fn with_remove_failure(mut self, container_id: &str, message: &str) -> Self {
        self.remove_failure = Some((self::container_id(container_id), message.to_owned()));
        self
    }

    fn with_stop_failure(mut self, container_id: &str, message: &str) -> Self {
        self.stop_failure = Some((self::container_id(container_id), message.to_owned()));
        self
    }
}

impl NodeContainerRunner for RecordingRunner {
    async fn existing_managed_containers(
        &self,
    ) -> Result<Vec<ExistingManagedContainer>, NodeContainerRunnerError> {
        Ok(self.existing.clone())
    }

    async fn ensure_endpoint_network(&self) -> Result<(), NodeContainerRunnerError> {
        self.state
            .inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .endpoint_networks += 1;
        Ok(())
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

    async fn remove_managed_container(
        &self,
        container_id: &ContainerId,
        _expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), NodeContainerRunnerError> {
        if let Some((failed_container_id, message)) = self.remove_failure.clone()
            && failed_container_id == *container_id
        {
            return Err(NodeContainerRunnerError::Remove {
                container_id: container_id.clone(),
                message,
            });
        }

        self.state
            .inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .removes
            .push(container_id.clone());
        Ok(())
    }

    async fn stop_managed_container(
        &self,
        container_id: &ContainerId,
        _expected_identity: &ManagedContainerIdentity,
    ) -> Result<(), NodeContainerRunnerError> {
        if let Some((failed_container_id, message)) = self.stop_failure.clone()
            && failed_container_id == *container_id
        {
            return Err(NodeContainerRunnerError::Stop {
                container_id: container_id.clone(),
                message,
            });
        }

        self.state
            .inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .stops
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

    fn endpoint_routes(&self) -> Vec<ployz_core::dataplane::WireGuardEbpfEndpointRoute> {
        self.inner
            .lock()
            .expect("recording wireguard ebpf lock is not poisoned")
            .endpoint_routes
            .clone()
    }

    fn peers(&self) -> Vec<ployz_core::dataplane::WireGuardPeer> {
        self.inner
            .lock()
            .expect("recording wireguard ebpf lock is not poisoned")
            .peers
            .clone()
    }
}

#[derive(Default)]
struct RecordingWireGuardEbpfInner {
    prepare_count: usize,
    endpoint_routes: Vec<ployz_core::dataplane::WireGuardEbpfEndpointRoute>,
    peers: Vec<ployz_core::dataplane::WireGuardPeer>,
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
    async fn read_wireguard_public_key(
        &self,
    ) -> Result<WireGuardPublicKey, WireGuardEbpfPrepareError> {
        Ok(WireGuardPublicKey::try_new("test-public-key").expect("test public key is valid"))
    }

    async fn prepare_wireguard_ebpf(
        &self,
        endpoint_routes: &[ployz_core::dataplane::WireGuardEbpfEndpointRoute],
        peers: &[ployz_core::dataplane::WireGuardPeer],
    ) -> Result<WireGuardEbpfReady, WireGuardEbpfPrepareError> {
        let mut state = self
            .state
            .inner
            .lock()
            .expect("recording wireguard ebpf lock is not poisoned");
        state.prepare_count += 1;
        state.endpoint_routes = endpoint_routes.to_vec();
        state.peers = peers.to_vec();
        drop(state);

        match &self.failure {
            Some(error) => Err(error.clone()),
            None => Ok(ready_components()),
        }
    }
}

fn ready_node(node_id: &str) -> WireGuardEbpfNodeReady {
    WireGuardEbpfNodeReady::new(self::node_id(node_id), ready_components())
}

fn ready_components() -> WireGuardEbpfReady {
    WireGuardEbpfReady {
        wireguard: WireGuardReady {
            public_key: WireGuardPublicKey::try_new("test-public-key")
                .expect("test public key is valid"),
            evidence: vec![WireGuardReadyEvidence::Command {
                program: "wg".to_owned(),
                args: vec!["--version".to_owned()],
            }],
        },
        ebpf_forwarding: EbpfForwardingReady {
            evidence: vec![EbpfForwardingReadyEvidence::PloyzTcBytecode {
                path: "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc".to_owned(),
                symbols: vec!["ployz_egress".to_owned(), "ployz_ingress".to_owned()],
            }],
        },
    }
}

fn idle_runner() -> RecordingRunner {
    RecordingRunner::new(RecordingRunnerState::default())
}

fn idle_logs() -> RecordingLogReader {
    RecordingLogReader::empty()
}

fn ready_wireguard_ebpf() -> RecordingWireGuardEbpf {
    RecordingWireGuardEbpf::new(RecordingWireGuardEbpfState::default())
}

#[derive(Clone)]
struct RecordingLogReader {
    container_id: Option<ContainerId>,
    text: String,
}

impl RecordingLogReader {
    fn empty() -> Self {
        Self {
            container_id: None,
            text: String::new(),
        }
    }

    fn new(container_id: &str, text: &str) -> Self {
        Self {
            container_id: Some(self::container_id(container_id)),
            text: text.to_owned(),
        }
    }
}

impl NodeLogReader for RecordingLogReader {
    async fn tail_container_logs(
        &self,
        container_id: &ContainerId,
        _tail_lines: Option<u16>,
    ) -> Result<NodeLogTail, NodeLogReaderError> {
        match &self.container_id {
            Some(expected) if expected == container_id => Ok(NodeLogTail {
                text: self.text.clone(),
                truncated: false,
            }),
            _ => Err(NodeLogReaderError::NotFound {
                container_id: container_id.clone(),
            }),
        }
    }
}

struct TestNats {
    _nats: ployz_test_support::nats::SecuredTestNats,
    /// Controller principal: the requesting deploy-worker side.
    client: async_nats::Client,
    /// Node principal: the node-runtime service side.
    node_a: async_nats::Client,
}

const TEST_NATS_CONNECT_TIMEOUT: Duration = Duration::from_secs(5);

async fn test_nats() -> TestNats {
    let nats = ployz_test_support::nats::SecuredTestNats::start_with_nodes(&[node_id("node_a")])
        .await
        .expect("secured test nats starts");
    let client = ployz_nats::connect::connect_authenticated(
        &nats.controller_config(),
        TEST_NATS_CONNECT_TIMEOUT,
    )
    .await
    .expect("controller connects");
    let node_a = ployz_nats::connect::connect_authenticated(
        &nats
            .node_config(&node_id("node_a"))
            .expect("fixture minted node_a credentials"),
        TEST_NATS_CONNECT_TIMEOUT,
    )
    .await
    .expect("node_a connects");

    TestNats {
        _nats: nats,
        client,
        node_a,
    }
}

fn run_request(node_id: &str) -> NodeRunContainerRequest {
    NodeRunContainerRequest {
        node_id: self::node_id(node_id),
        image: image("registry.example/api:rev_2"),
        endpoint: None,
        container: managed_container_spec(),
    }
}

fn wireguard_ebpf_request(nodes: &[&str]) -> WireGuardEbpfPrepareRequest {
    WireGuardEbpfPrepareRequest {
        operation_id: operation_id("op_123"),
        nodes: nodes.iter().map(|node| node_id(node)).collect(),
        endpoint_routes: endpoint_routes(nodes),
        peer_endpoints: Vec::new(),
        peers: Vec::new(),
    }
}

fn endpoint_routes(nodes: &[&str]) -> Vec<ployz_core::dataplane::WireGuardEbpfEndpointRoute> {
    nodes
        .iter()
        .map(|node| {
            ployz_core::dataplane::WireGuardEbpfEndpointRoute::default_for_node(&node_id(node))
        })
        .collect()
}

fn failure_message(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("valid failure message")
}

fn inspect_hint(container_id: &str) -> ployz_core::ops::OperatorHint {
    ployz_core::ops::OperatorHint::try_new(format!("ployz container inspect {container_id}"))
        .expect("valid inspect hint")
}

fn managed_container_spec() -> NodeContainerRunSpec {
    NodeContainerRunSpec {
        service_id: service_id("svc_api"),
        revision_id: revision_id("rev_2"),
        operation_id: operation_id("op_123"),
        step_id: step_id("run_1"),
        kind: ManagedContainerKind::Service,
    }
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
