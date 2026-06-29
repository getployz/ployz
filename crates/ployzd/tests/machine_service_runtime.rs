use ployz_core::dataplane::{
    DataplanePrepareError, DataplanePrepareRequest, DataplaneProviderFailure, EbpfForwardingReady,
    EbpfForwardingReadyEvidence, PloyzNativeMeshComponent, PloyzNativeMeshMachineReady,
    PloyzNativeMeshReady, WireGuardEbpfPrepareError, WireGuardPublicKey, WireGuardReady,
    WireGuardReadyEvidence,
};
use ployz_core::deploy::ImageReference;
use ployz_core::ids::ContainerId;
use ployz_core::machine_runtime::ManagedContainerKind;
use ployz_core::subjects::{MachineServiceEndpoint, machine_service};
use ployz_nats::observations::AsyncNatsObservationStore;
use ployz_nats::service_runtime::request_json;
use ployz_test_support::ids::{
    container_id, failure_message, machine_id, operation_id, revision_id, service_id, step_id,
};
use ployzd::deploy_worker::{
    DataplanePreparer, MachineContainerRuntime, MachineContainerRuntimeError,
    MachineRuntimeUnavailableReason,
};
use ployzd::docker::labels::{ManagedContainerIdentity, ManagedContainerLabels};
use ployzd::machine_runtime::client::{NatsMachineContainerRuntime, NatsMachineDataplanePreparer};
use ployzd::machine_runtime::protocol::{
    MachineContainerRemoveDomainError, MachineContainerRemoveRpcRequest,
    MachineContainerRemoveRpcResponse, MachineContainerRpcOk, MachineContainerRunRpcRequest,
    MachineContainerRunSpec, MachineContainerStopDomainError, MachineContainerStopRpcRequest,
    MachineContainerStopRpcResponse, MachineDataplanePrepareRpcRequest,
    MachineDataplanePrepareRpcResponse, MachineEnsureEndpointNetworkRpcRequest,
    MachineLogsTailRpcOk, MachineLogsTailRpcRequest, MachineLogsTailRpcResponse,
    MachinePloyzNativeMeshPrepareDomainError, MachinePloyzNativeMeshPrepareRpcRequest,
    MachineRunContainerOutcome,
};
use ployzd::machine_runtime::runner::{
    CreateManagedContainer, ExistingManagedContainer, ExistingManagedContainerState,
    MachineContainerRunner, MachineContainerRunnerError, MachineLogReader, MachineLogReaderError,
    MachineLogTail,
};
use ployzd::machine_runtime::service::{
    MachinePloyzNativeMeshPreparer as LocalWireGuardEbpfPreparer, start_machine_runtime_service,
};
use std::sync::{Arc, Mutex};
use std::time::Duration;

#[tokio::test]
async fn machine_runtime_service_ensures_endpoint_network() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    client
        .ensure_endpoint_network(
            &machine_id("machine_a"),
            MachineEnsureEndpointNetworkRpcRequest {
                operation_id: operation_id("op_123"),
            },
        )
        .await
        .expect("endpoint network ensure succeeds");

    assert_eq!(state.endpoint_networks(), 1);
}

#[tokio::test]
async fn machine_runtime_service_creates_missing_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone()).with_next_container("ctr_created"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let outcome = client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect("container run succeeds");

    assert_eq!(
        outcome,
        MachineRunContainerOutcome::Created {
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
async fn machine_runtime_service_reuses_existing_operation_step_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone())
            .with_existing(existing_container("ctr_existing", managed_labels())),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let outcome = client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect("container run succeeds");

    assert_eq!(
        outcome,
        MachineRunContainerOutcome::ReusedRunning {
            container_id: container_id("ctr_existing"),
        }
    );
    assert!(state.creates().is_empty());
}

#[tokio::test]
async fn machine_runtime_service_starts_existing_stopped_operation_step_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone()).with_existing(existing_container_with_state(
            "ctr_existing",
            managed_labels(),
            ExistingManagedContainerState::StartableStopped,
        )),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let outcome = client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect("container run succeeds");

    assert_eq!(
        outcome,
        MachineRunContainerOutcome::StartedExisting {
            container_id: container_id("ctr_existing"),
        }
    );
    assert_eq!(state.starts(), vec![container_id("ctr_existing")]);
    assert!(state.creates().is_empty());
}

#[tokio::test]
async fn machine_runtime_service_reports_start_failure_with_container_evidence() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state).with_start_failure("ctr_created", "exec format error"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let error = client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect_err("container start failure is returned");

    assert_eq!(
        error,
        MachineContainerRuntimeError::CreatedContainerStartFailed {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_created"),
            message: failure_message("container start failed: exec format error"),
            inspect_hint: inspect_hint("ctr_created"),
        }
    );
}

#[tokio::test]
async fn machine_runtime_service_reports_existing_start_failure_without_created_evidence() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state).with_existing_start_failure("ctr_existing", "still stopping"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let error = client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect_err("existing container start failure is returned");

    assert_eq!(
        error,
        MachineContainerRuntimeError::ExistingContainerStartFailed {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_existing"),
            message: failure_message("container start failed: still stopping"),
            inspect_hint: inspect_hint("ctr_existing"),
        }
    );
}

#[tokio::test]
async fn machine_runtime_service_reports_operation_step_conflict_as_domain_error() {
    let nats = test_nats().await;
    let mut conflicting_labels = managed_labels();
    conflicting_labels.revision_id = revision_id("rev_other");
    let state = RecordingRunnerState::default();
    let service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state).with_existing(existing_container(
            "ctr_conflict",
            conflicting_labels.clone(),
        )),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let error = client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect_err("container run reports conflict");

    assert_eq!(
        error,
        MachineContainerRuntimeError::OperationStepConflict {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_conflict"),
            expected: managed_labels(),
            actual: conflicting_labels,
        }
    );
    assert_eq!(service.health().domain_failures, 1);
}

#[tokio::test]
async fn machine_runtime_service_maps_create_failure_to_unavailable_runtime() {
    let nats = test_nats().await;
    let _service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default()).with_create_failure("disk full"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    let error = client
        .run_container(&machine_id("machine_a"), run_request())
        .await
        .expect_err("container create fails");

    assert_eq!(
        error,
        MachineContainerRuntimeError::Unavailable {
            machine_id: machine_id("machine_a"),
            reason: MachineRuntimeUnavailableReason::ServiceInternal {
                message: "container create failed: disk full".to_owned(),
            },
        }
    );
}

#[tokio::test]
async fn machine_runtime_service_removes_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");

    let response = request_json::<_, MachineContainerRemoveRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::ContainerRemove,
        ),
        &MachineContainerRemoveRpcRequest {
            operation_id: operation_id("op_123"),
            container_id: container_id("ctr_old"),
            expected_identity: managed_labels().identity(),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");

    assert_eq!(
        response,
        MachineContainerRemoveRpcResponse::Ok(MachineContainerRpcOk {
            machine_id: machine_id("machine_a"),
            container_id: container_id("ctr_old"),
        })
    );
    assert_eq!(state.removes(), vec![container_id("ctr_old")]);
}

#[tokio::test]
async fn machine_runtime_service_stops_container() {
    let nats = test_nats().await;
    let state = RecordingRunnerState::default();
    let _service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(state.clone()),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineContainerRuntime::new(nats.client);

    client
        .stop_container(
            &machine_id("machine_a"),
            MachineContainerStopRpcRequest {
                operation_id: operation_id("op_123"),
                container_id: container_id("ctr_failed"),
                expected_identity: managed_labels().identity(),
            },
        )
        .await
        .expect("container stop succeeds");

    assert_eq!(state.stops(), vec![container_id("ctr_failed")]);
}

#[tokio::test]
async fn machine_runtime_service_reports_remove_failure_as_domain_error() {
    let nats = test_nats().await;
    let _service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default())
            .with_remove_failure("ctr_old", "busy"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");

    let response = request_json::<_, MachineContainerRemoveRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::ContainerRemove,
        ),
        &MachineContainerRemoveRpcRequest {
            operation_id: operation_id("op_123"),
            container_id: container_id("ctr_old"),
            expected_identity: managed_labels().identity(),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");

    assert_eq!(
        response,
        MachineContainerRemoveRpcResponse::DomainError {
            machine_id: machine_id("machine_a"),
            error: MachineContainerRemoveDomainError::RemoveFailed {
                container_id: container_id("ctr_old"),
                message: failure_message("container remove failed: busy"),
                inspect_hint: inspect_hint("ctr_old"),
            },
        }
    );
}

#[tokio::test]
async fn machine_runtime_service_reports_stop_failure_as_domain_error() {
    let nats = test_nats().await;
    let _service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default())
            .with_stop_failure("ctr_failed", "permission denied"),
        ready_wireguard_ebpf(),
        idle_logs(),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");

    let response = request_json::<_, MachineContainerStopRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::ContainerStop,
        ),
        &MachineContainerStopRpcRequest {
            operation_id: operation_id("op_123"),
            container_id: container_id("ctr_failed"),
            expected_identity: managed_labels().identity(),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");

    assert_eq!(
        response,
        MachineContainerStopRpcResponse::DomainError {
            machine_id: machine_id("machine_a"),
            error: MachineContainerStopDomainError::StopFailed {
                container_id: container_id("ctr_failed"),
                message: failure_message("container stop failed: permission denied"),
                inspect_hint: inspect_hint("ctr_failed"),
            },
        }
    );
}

#[tokio::test]
async fn machine_runtime_service_tails_container_logs() {
    let nats = test_nats().await;
    let _service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        RecordingRunner::new(RecordingRunnerState::default())
            .with_existing(existing_container("ctr_failed", managed_labels())),
        ready_wireguard_ebpf(),
        RecordingLogReader::new("ctr_failed", "panic: missing DATABASE_URL\n"),
    )
    .await
    .expect("machine runtime service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");

    let response = request_json::<_, MachineLogsTailRpcResponse>(
        &nats.client,
        machine_service(&machine_id("machine_a"), MachineServiceEndpoint::LogsTail),
        &MachineLogsTailRpcRequest {
            container_id: container_id("ctr_failed"),
            tail_lines: Some(50),
        },
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");

    assert_eq!(
        response,
        MachineLogsTailRpcResponse::Ok(MachineLogsTailRpcOk {
            value: ployzd::machine_runtime::protocol::MachineLogsTailResult {
                machine_id: machine_id("machine_a"),
                container_id: container_id("ctr_failed"),
                text: "panic: missing DATABASE_URL\n".to_owned(),
                truncated: false,
            },
        })
    );
}

#[tokio::test]
async fn machine_wireguard_ebpf_service_calls_local_preparer() {
    let nats = test_nats().await;
    let state = RecordingWireGuardEbpfState::default();
    let _service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        idle_runner(),
        RecordingWireGuardEbpf::new(state.clone()),
        idle_logs(),
    )
    .await
    .expect("machine wireguard ebpf service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineDataplanePreparer::new(nats.client, nats.observations);

    let report = client
        .prepare_dataplane(dataplane_request(&["machine_a"]))
        .await
        .expect("dataplane prepare succeeds");
    assert_eq!(state.prepare_count(), 1);
    assert_eq!(state.endpoint_routes(), endpoint_routes(&["machine_a"]));
    assert_eq!(state.peers(), Vec::new());
    assert_eq!(report.machines, vec![ready_machine("machine_a")]);
}

#[tokio::test]
async fn machine_wireguard_ebpf_service_rejects_request_not_targeting_this_machine() {
    let nats = test_nats().await;
    let state = RecordingWireGuardEbpfState::default();
    let _service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        idle_runner(),
        RecordingWireGuardEbpf::new(state.clone()),
        idle_logs(),
    )
    .await
    .expect("machine wireguard ebpf service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let response = request_json::<_, MachineDataplanePrepareRpcResponse>(
        &nats.client,
        machine_service(
            &machine_id("machine_a"),
            MachineServiceEndpoint::DataplanePrepare,
        ),
        &MachineDataplanePrepareRpcRequest::ployz_native_mesh(
            operation_id("op_123"),
            vec![machine_id("machine_b")],
            MachinePloyzNativeMeshPrepareRpcRequest::PrepareDataplane {
                endpoint_routes: endpoint_routes(&["machine_b"]),
                peers: Vec::new(),
            },
        ),
        Duration::from_secs(1),
    )
    .await
    .expect("machine service responds");

    assert!(matches!(
        response,
        MachineDataplanePrepareRpcResponse::DomainError {
            machine_id,
            error: MachinePloyzNativeMeshPrepareDomainError::Unavailable {
                component: PloyzNativeMeshComponent::WireGuard,
                ..
            },
        } if machine_id == self::machine_id("machine_a")
    ));
    assert_eq!(state.prepare_count(), 0);
}

#[tokio::test]
async fn machine_wireguard_ebpf_service_preserves_prepare_failure() {
    let nats = test_nats().await;
    let state = RecordingWireGuardEbpfState::default();
    let _service = start_machine_runtime_service(
        nats.machine_a.clone(),
        machine_id("machine_a"),
        idle_runner(),
        RecordingWireGuardEbpf::new(state).with_failure(WireGuardEbpfPrepareError::Unavailable {
            machine_id: machine_id("machine_a"),
            component: PloyzNativeMeshComponent::EbpfForwarding,
            message: failure_message("ebpf program missing"),
        }),
        idle_logs(),
    )
    .await
    .expect("machine wireguard ebpf service starts");
    nats.machine_a
        .flush()
        .await
        .expect("flush machine service subscription");
    let mut client = NatsMachineDataplanePreparer::new(nats.client, nats.observations);

    let error = client
        .prepare_dataplane(dataplane_request(&["machine_a"]))
        .await
        .expect_err("dataplane prepare failure is returned");

    assert_eq!(
        error,
        DataplanePrepareError::Unavailable {
            machine_id: machine_id("machine_a"),
            provider: DataplaneProviderFailure::PloyzNativeMesh {
                component: PloyzNativeMeshComponent::EbpfForwarding,
            },
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

impl MachineContainerRunner for RecordingRunner {
    async fn existing_managed_containers(
        &self,
    ) -> Result<Vec<ExistingManagedContainer>, MachineContainerRunnerError> {
        Ok(self.existing.clone())
    }

    async fn ensure_endpoint_network(&self) -> Result<(), MachineContainerRunnerError> {
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
    ) -> Result<ContainerId, MachineContainerRunnerError> {
        if let Some(message) = self.create_failure.clone() {
            return Err(MachineContainerRunnerError::Create { message });
        }

        self.state
            .inner
            .lock()
            .expect("recording runner lock is not poisoned")
            .creates
            .push(command);
        self.next_container
            .clone()
            .ok_or_else(|| MachineContainerRunnerError::Create {
                message: "missing next container id".to_owned(),
            })
    }

    async fn start_managed_container(
        &self,
        container_id: &ContainerId,
    ) -> Result<(), MachineContainerRunnerError> {
        if let Some((failed_container_id, message)) = self.start_failure.clone()
            && failed_container_id == *container_id
        {
            return Err(MachineContainerRunnerError::Start {
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
    ) -> Result<(), MachineContainerRunnerError> {
        if let Some((failed_container_id, message)) = self.remove_failure.clone()
            && failed_container_id == *container_id
        {
            return Err(MachineContainerRunnerError::Remove {
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
    ) -> Result<(), MachineContainerRunnerError> {
        if let Some((failed_container_id, message)) = self.stop_failure.clone()
            && failed_container_id == *container_id
        {
            return Err(MachineContainerRunnerError::Stop {
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

    async fn prepare_ployz_native_mesh(
        &self,
        endpoint_routes: &[ployz_core::dataplane::WireGuardEbpfEndpointRoute],
        peers: &[ployz_core::dataplane::WireGuardPeer],
    ) -> Result<PloyzNativeMeshReady, WireGuardEbpfPrepareError> {
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

fn ready_machine(machine_id: &str) -> PloyzNativeMeshMachineReady {
    PloyzNativeMeshMachineReady {
        machine_id: self::machine_id(machine_id),
        ready: ready_components(),
    }
}

fn ready_components() -> PloyzNativeMeshReady {
    PloyzNativeMeshReady {
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

impl MachineLogReader for RecordingLogReader {
    async fn tail_container_logs(
        &self,
        container_id: &ContainerId,
        _tail_lines: Option<u16>,
    ) -> Result<MachineLogTail, MachineLogReaderError> {
        match &self.container_id {
            Some(expected) if expected == container_id => Ok(MachineLogTail {
                text: self.text.clone(),
                truncated: false,
            }),
            _ => Err(MachineLogReaderError::NotFound {
                container_id: container_id.clone(),
            }),
        }
    }
}

struct TestNats {
    _nats: ployz_test_support::nats::TestNats,
    /// Controller principal: the requesting deploy-worker side.
    client: async_nats::Client,
    observations: AsyncNatsObservationStore,
    /// Machine principal: the machine-runtime service side.
    machine_a: async_nats::Client,
}

async fn test_nats() -> TestNats {
    let nats =
        ployz_test_support::nats::TestNats::start_with_machines(&[machine_id("machine_a")]).await;
    nats.bootstrap_resources().await;
    let client = nats.controller.clone();
    let observations = AsyncNatsObservationStore::from_jetstream(&nats.jetstream)
        .await
        .expect("open controller observation store");
    let machine_a = nats.machine_client(&machine_id("machine_a")).await;

    TestNats {
        _nats: nats,
        client,
        observations,
        machine_a,
    }
}

fn run_request() -> MachineContainerRunRpcRequest {
    MachineContainerRunRpcRequest {
        image: image("registry.example/api:rev_2"),
        endpoint: None,
        container: managed_container_spec(),
    }
}

fn dataplane_request(machines: &[&str]) -> DataplanePrepareRequest {
    DataplanePrepareRequest::for_machines(
        operation_id("op_123"),
        machines.iter().map(|machine| machine_id(machine)).collect(),
    )
}

fn endpoint_routes(machines: &[&str]) -> Vec<ployz_core::dataplane::WireGuardEbpfEndpointRoute> {
    machines
        .iter()
        .map(|machine| {
            ployz_core::dataplane::WireGuardEbpfEndpointRoute::default_for_machine(&machine_id(
                machine,
            ))
        })
        .collect()
}

fn inspect_hint(container_id: &str) -> ployz_core::ops::OperatorHint {
    ployz_core::ops::OperatorHint::try_new(format!("ployz container inspect {container_id}"))
        .expect("valid inspect hint")
}

fn managed_container_spec() -> MachineContainerRunSpec {
    MachineContainerRunSpec {
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

fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image")
}
