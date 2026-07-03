use ployz_core::dataplane::{
    DataplanePrepareError, DataplanePrepareRequest, DataplaneProviderFailure, EbpfForwardingReady,
    EbpfForwardingReadyEvidence, PloyzNativeMeshComponent, PloyzNativeMeshMachineReady,
    PloyzNativeMeshPrepareReport, PloyzNativeMeshReady, WireGuardPublicKey, WireGuardReady,
    WireGuardReadyEvidence,
};
use ployz_core::deploy::{
    DeployCleanupContainer, DeployRequest, DeployRoute, DeployServiceSpec, ImageReference,
    ReplicaCount,
};
use ployz_core::ids::{
    ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, NamespaceRevisionId,
    OperationId, ServiceId, StepId,
};
use ployz_core::machine_runtime::{
    ContainerRuntimeState, MachineContainerObservationSnapshot, ManagedContainerKind,
    ManagedContainerObservation,
};
use ployz_core::ops::{
    DeployCleanupFailure, DeployEvidence, DeployRunningStage, DeployTransition, FailureMessage,
    OperatorHint, RetainedArtifact, RouteHostname, RoutePort, RouteTarget,
};
use ployz_core::state::{RouteBindingState, ServingTargetEntry};
pub(crate) use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id, namespace_revision_id,
    operation_id, service_id,
};
use ployzd::deploy_worker::{
    RouteBindingCommitError, RouteBindingCommitter, ServingTargetCommitError, ServingTargetCommitter,
    DataplanePreparer, DeployExecutionCommand, DeployExecutionFacts, DeployHealthCheckError,
    DeployHealthChecker, DeployOperationRecordError, DeployOperationRecorder,
    DeployServiceExecutionFacts, MachineContainerRuntime, MachineContainerRuntimeError,
    MachineRuntimeUnavailableReason, prepare_deploy_execution_command,
};
use ployzd::machine_runtime::protocol::{
    MachineContainerRemoveRpcRequest, MachineContainerRunRpcRequest,
    MachineContainerStopRpcRequest, MachineEnsureEndpointNetworkRpcRequest,
    MachineRunContainerOutcome,
};
use std::time::Duration;

#[derive(Default)]
pub(super) struct RecordingOperations {
    pub(super) records: Vec<RecordedOperation>,
    fail_completed_transition_remaining: usize,
    fail_cleanup_evidence_remaining: usize,
    pub(super) completed_transition_attempts: usize,
}

impl RecordingOperations {
    pub(super) const fn fail_completed_transition_times(times: usize) -> Self {
        Self {
            records: Vec::new(),
            fail_completed_transition_remaining: times,
            fail_cleanup_evidence_remaining: 0,
            completed_transition_attempts: 0,
        }
    }

    pub(super) const fn fail_cleanup_evidence_times(times: usize) -> Self {
        Self {
            records: Vec::new(),
            fail_completed_transition_remaining: 0,
            fail_cleanup_evidence_remaining: times,
            completed_transition_attempts: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RecordedOperation {
    Transition(DeployTransition),
    PlanCreated {
        replica_count: usize,
    },
    DataplanePrepared {
        machine_count: usize,
    },
    HealthCheckStarted,
    ContainerStarted {
        machine_id: MachineId,
        container_id: ContainerId,
    },
    CleanupFinished {
        removed: Vec<DeployCleanupContainer>,
        failed: Vec<DeployCleanupFailure>,
    },
}

impl DeployOperationRecorder for RecordingOperations {
    async fn record_deploy_transition(
        &mut self,
        recorded_operation_id: &OperationId,
        transition: DeployTransition,
    ) -> Result<(), DeployOperationRecordError> {
        assert_eq!(recorded_operation_id, &operation_id("op_123"));
        if self.fail_completed_transition_remaining > 0
            && matches!(transition, DeployTransition::Completed { .. })
        {
            self.fail_completed_transition_remaining -= 1;
            self.completed_transition_attempts += 1;
            return Err(DeployOperationRecordError::Synthetic {
                message: "completion record failed",
            });
        }
        self.records.push(RecordedOperation::Transition(transition));
        Ok(())
    }

    async fn record_deploy_evidence(
        &mut self,
        recorded_operation_id: &OperationId,
        evidence: DeployEvidence,
    ) -> Result<(), DeployOperationRecordError> {
        assert_eq!(recorded_operation_id, &operation_id("op_123"));
        match evidence {
            DeployEvidence::PlanCreated { plan } => {
                self.records.push(RecordedOperation::PlanCreated {
                    replica_count: plan
                        .services
                        .iter()
                        .map(|service| service.steps.len())
                        .sum(),
                });
            }
            DeployEvidence::DataplanePrepared { report } => {
                self.records.push(RecordedOperation::DataplanePrepared {
                    machine_count: report.machines.len(),
                });
            }
            DeployEvidence::ContainerStarted {
                machine_id,
                container_id,
            } => {
                self.records.push(RecordedOperation::ContainerStarted {
                    machine_id,
                    container_id,
                });
            }
            DeployEvidence::HealthCheckStarted => {
                self.records.push(RecordedOperation::HealthCheckStarted);
            }
            DeployEvidence::CleanupFinished { removed, failed } => {
                if self.fail_cleanup_evidence_remaining > 0 {
                    self.fail_cleanup_evidence_remaining -= 1;
                    return Err(DeployOperationRecordError::Synthetic {
                        message: "cleanup evidence record failed",
                    });
                }
                self.records
                    .push(RecordedOperation::CleanupFinished { removed, failed });
            }
        }
        Ok(())
    }
}

pub(super) struct RecordingRuntime {
    pub(super) endpoint_networks: Vec<(MachineId, MachineEnsureEndpointNetworkRpcRequest)>,
    pub(super) requests: Vec<(MachineId, MachineContainerRunRpcRequest)>,
    pub(super) stops: Vec<(MachineId, MachineContainerStopRpcRequest)>,
    pub(super) removals: Vec<(MachineId, MachineContainerRemoveRpcRequest)>,
    containers: Vec<ContainerId>,
    fail_after_first: bool,
    fail_endpoint_network: bool,
    reuse_existing: bool,
    fail_start: bool,
    fail_remove: bool,
    fail_stop: bool,
}

#[derive(Default)]
pub(super) struct RecordingWireGuardEbpf {
    pub(super) requests: Vec<DataplanePrepareRequest>,
    failure: Option<DataplanePrepareError>,
}

impl RecordingWireGuardEbpf {
    pub(super) fn ready() -> Self {
        Self::default()
    }

    pub(super) fn wireguard_failed(machine_id: &str) -> Self {
        Self {
            requests: Vec::new(),
            failure: Some(DataplanePrepareError::Unavailable {
                machine_id: self::machine_id(machine_id),
                provider: DataplaneProviderFailure::PloyzNativeMesh {
                    component: PloyzNativeMeshComponent::WireGuard,
                },
                message: ployz_core::ops::FailureMessage::try_new("wireguard interface failed")
                    .expect("valid failure message"),
            }),
        }
    }
}

impl DataplanePreparer for RecordingWireGuardEbpf {
    async fn prepare_dataplane(
        &mut self,
        request: DataplanePrepareRequest,
    ) -> Result<PloyzNativeMeshPrepareReport, DataplanePrepareError> {
        let ready_machines = request
            .membership
            .iter()
            .map(|member| ready_machine(member.machine_id.clone()))
            .collect::<Vec<_>>();
        self.requests.push(request);
        match &self.failure {
            Some(error) => Err(error.clone()),
            None => Ok(PloyzNativeMeshPrepareReport::from_machines(ready_machines)
                .expect("recording report has unique machines")),
        }
    }
}

fn ready_machine(machine_id: MachineId) -> PloyzNativeMeshMachineReady {
    let public_key = wireguard_public_key(format!("public-{}", machine_id.as_str()));
    PloyzNativeMeshMachineReady {
        machine_id,
        ready: PloyzNativeMeshReady {
            wireguard: WireGuardReady {
                public_key,
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
        },
    }
}

pub(super) struct RecordingActiveState {
    pub(super) requests: Vec<ServingTargetEntry>,
    pub(super) removals: Vec<ServiceId>,
}

pub(super) struct RecordingRouteState {
    pub(super) requests: Vec<RouteBindingState>,
    pub(super) removals: Vec<RouteTarget>,
}

impl RecordingRouteState {
    pub(super) fn stored() -> Self {
        Self {
            requests: Vec::new(),
            removals: Vec::new(),
        }
    }
}

impl RouteBindingCommitter for RecordingRouteState {
    async fn replace_route_binding(
        &mut self,
        state: RouteBindingState,
    ) -> Result<(), RouteBindingCommitError> {
        self.requests.push(state);
        Ok(())
    }

    async fn remove_route_binding(
        &mut self,
        target: RouteTarget,
    ) -> Result<(), RouteBindingCommitError> {
        self.removals.push(target);
        Ok(())
    }
}

impl RecordingActiveState {
    pub(super) fn stored() -> Self {
        Self {
            requests: Vec::new(),
            removals: Vec::new(),
        }
    }
}

impl ServingTargetCommitter for RecordingActiveState {
    async fn replace_serving_target_entry(
        &mut self,
        state: ServingTargetEntry,
    ) -> Result<(), ServingTargetCommitError> {
        self.requests.push(state);
        Ok(())
    }

    async fn remove_serving_target_entry(
        &mut self,
        _namespace_id: NamespaceId,
        service_id: ServiceId,
    ) -> Result<(), ServingTargetCommitError> {
        self.removals.push(service_id);
        Ok(())
    }
}

pub(super) struct HangingActiveState;

impl ServingTargetCommitter for HangingActiveState {
    async fn replace_serving_target_entry(
        &mut self,
        _state: ServingTargetEntry,
    ) -> Result<(), ServingTargetCommitError> {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Ok(())
    }

    async fn remove_serving_target_entry(
        &mut self,
        _namespace_id: NamespaceId,
        _service_id: ServiceId,
    ) -> Result<(), ServingTargetCommitError> {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Ok(())
    }
}

pub(super) struct LostLockActiveState;

impl ServingTargetCommitter for LostLockActiveState {
    async fn replace_serving_target_entry(
        &mut self,
        _state: ServingTargetEntry,
    ) -> Result<(), ServingTargetCommitError> {
        Err(ServingTargetCommitError::NamespaceLockLost)
    }

    async fn remove_serving_target_entry(
        &mut self,
        _namespace_id: NamespaceId,
        _service_id: ServiceId,
    ) -> Result<(), ServingTargetCommitError> {
        Err(ServingTargetCommitError::NamespaceLockLost)
    }
}

#[derive(Default)]
pub(super) struct RecordingHealth {
    pub(super) checked: Vec<Vec<DeployContainerForAssert>>,
    failure: Option<DeployHealthCheckError>,
}

impl RecordingHealth {
    pub(super) fn healthy() -> Self {
        Self::default()
    }

    pub(super) fn unhealthy(machine_id: &str, container_id: &str) -> Self {
        Self {
            checked: Vec::new(),
            failure: Some(DeployHealthCheckError::Unhealthy {
                machine_id: self::machine_id(machine_id),
                container_id: self::container_id(container_id),
                message: ployz_core::ops::FailureMessage::try_new("probe failed")
                    .expect("valid failure message"),
                log_hint: ployz_core::ops::OperatorHint::try_new(format!(
                    "ployz logs {container_id}"
                ))
                .expect("valid log hint"),
            }),
        }
    }
}

impl DeployHealthChecker for RecordingHealth {
    async fn wait_healthy(
        &mut self,
        containers: &[ployzd::deploy_worker::DeployContainer],
    ) -> Result<(), DeployHealthCheckError> {
        self.checked.push(
            containers
                .iter()
                .map(|container| {
                    DeployContainerForAssert::from_container(
                        container.machine_id.clone(),
                        container.container_id.clone(),
                    )
                })
                .collect(),
        );

        match &self.failure {
            Some(error) => Err(error.clone()),
            None => Ok(()),
        }
    }
}

pub(super) struct HangingHealth;

impl DeployHealthChecker for HangingHealth {
    async fn wait_healthy(
        &mut self,
        _containers: &[ployzd::deploy_worker::DeployContainer],
    ) -> Result<(), DeployHealthCheckError> {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeployContainerForAssert {
    machine_id: MachineId,
    container_id: ContainerId,
}

impl DeployContainerForAssert {
    pub(super) fn new(machine_id: &str, container_id: &str) -> Self {
        Self::from_container(
            self::machine_id(machine_id),
            self::container_id(container_id),
        )
    }

    pub(super) fn routed(machine_id: &str, container_id: &str) -> Self {
        Self::from_container(
            self::machine_id(machine_id),
            self::container_id(container_id),
        )
    }

    const fn from_container(machine_id: MachineId, container_id: ContainerId) -> Self {
        Self {
            machine_id,
            container_id,
        }
    }
}

impl RecordingRuntime {
    pub(super) fn with_containers<const N: usize>(containers: [&str; N]) -> Self {
        Self {
            endpoint_networks: Vec::new(),
            requests: Vec::new(),
            stops: Vec::new(),
            removals: Vec::new(),
            containers: containers.into_iter().map(container_id).rev().collect(),
            fail_after_first: false,
            fail_endpoint_network: false,
            reuse_existing: false,
            fail_start: false,
            fail_remove: false,
            fail_stop: false,
        }
    }

    pub(super) fn reusing_containers<const N: usize>(containers: [&str; N]) -> Self {
        Self {
            endpoint_networks: Vec::new(),
            requests: Vec::new(),
            stops: Vec::new(),
            removals: Vec::new(),
            containers: containers.into_iter().map(container_id).rev().collect(),
            fail_after_first: false,
            fail_endpoint_network: false,
            reuse_existing: true,
            fail_start: false,
            fail_remove: false,
            fail_stop: false,
        }
    }

    pub(super) fn failing_after_first_container() -> Self {
        Self {
            endpoint_networks: Vec::new(),
            requests: Vec::new(),
            stops: Vec::new(),
            removals: Vec::new(),
            containers: vec![container_id("ctr_1")],
            fail_after_first: true,
            fail_endpoint_network: false,
            reuse_existing: false,
            fail_start: false,
            fail_remove: false,
            fail_stop: false,
        }
    }

    pub(super) fn failing_start(container_id: &str) -> Self {
        Self {
            endpoint_networks: Vec::new(),
            requests: Vec::new(),
            stops: Vec::new(),
            removals: Vec::new(),
            containers: vec![self::container_id(container_id)],
            fail_after_first: false,
            fail_endpoint_network: false,
            reuse_existing: false,
            fail_start: true,
            fail_remove: false,
            fail_stop: false,
        }
    }

    pub(super) fn with_remove_failure(mut self) -> Self {
        self.fail_remove = true;
        self
    }

    pub(super) fn with_stop_failure(mut self) -> Self {
        self.fail_stop = true;
        self
    }

    pub(super) fn with_endpoint_network_failure(mut self) -> Self {
        self.fail_endpoint_network = true;
        self
    }
}

impl MachineContainerRuntime for RecordingRuntime {
    async fn ensure_endpoint_network(
        &mut self,
        machine_id: &MachineId,
        request: MachineEnsureEndpointNetworkRpcRequest,
    ) -> Result<(), MachineContainerRuntimeError> {
        self.endpoint_networks.push((machine_id.clone(), request));
        if self.fail_endpoint_network {
            return Err(MachineContainerRuntimeError::Unavailable {
                machine_id: machine_id.clone(),
                reason: MachineRuntimeUnavailableReason::RequestFailed {
                    message: "synthetic endpoint network failure".to_owned(),
                },
            });
        }
        Ok(())
    }

    async fn run_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRunRpcRequest,
    ) -> Result<MachineRunContainerOutcome, MachineContainerRuntimeError> {
        self.requests.push((machine_id.clone(), request));
        if self.fail_after_first && self.requests.len() > 1 {
            return Err(MachineContainerRuntimeError::Unavailable {
                machine_id: machine_id.clone(),
                reason: MachineRuntimeUnavailableReason::RequestFailed {
                    message: "synthetic runtime failure".to_owned(),
                },
            });
        }

        let Some(container_id) = self.containers.pop() else {
            return Err(MachineContainerRuntimeError::Unavailable {
                machine_id: machine_id.clone(),
                reason: MachineRuntimeUnavailableReason::RequestFailed {
                    message: "synthetic missing container id".to_owned(),
                },
            });
        };

        if self.fail_start {
            return Err(MachineContainerRuntimeError::CreatedContainerStartFailed {
                machine_id: machine_id.clone(),
                container_id,
                message: runtime_failure_message("container start failed: exec format error"),
                inspect_hint: inspect_hint("ctr_created"),
            });
        }

        if self.reuse_existing {
            Ok(MachineRunContainerOutcome::ReusedRunning { container_id })
        } else {
            Ok(MachineRunContainerOutcome::Created { container_id })
        }
    }

    async fn remove_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRemoveRpcRequest,
    ) -> Result<(), MachineContainerRuntimeError> {
        let container_id = request.container_id.clone();
        self.removals.push((machine_id.clone(), request));
        if self.fail_remove {
            return Err(MachineContainerRuntimeError::RemoveContainerFailed {
                machine_id: machine_id.clone(),
                container_id: container_id.clone(),
                message: runtime_failure_message("container remove failed: busy"),
                inspect_hint: OperatorHint::try_new(format!(
                    "ployz container inspect {}",
                    container_id.as_str()
                ))
                .expect("valid inspect hint"),
            });
        }

        Ok(())
    }

    async fn stop_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerStopRpcRequest,
    ) -> Result<(), MachineContainerRuntimeError> {
        let container_id = request.container_id.clone();
        self.stops.push((machine_id.clone(), request));
        if self.fail_stop {
            return Err(MachineContainerRuntimeError::StopContainerFailed {
                machine_id: machine_id.clone(),
                container_id: container_id.clone(),
                message: runtime_failure_message("container stop failed: permission denied"),
                inspect_hint: OperatorHint::try_new(format!(
                    "ployz container inspect {}",
                    container_id.as_str()
                ))
                .expect("valid inspect hint"),
            });
        }
        Ok(())
    }
}

pub(super) fn deploy_command(replicas: u16) -> DeployExecutionCommand {
    prepared_deploy_command(
        replicas,
        vec![machine_id("machine_a"), machine_id("machine_b")],
        Vec::new(),
    )
}

pub(super) fn routed_deploy_command(replicas: u16) -> DeployExecutionCommand {
    prepare_deploy_execution_command(
        operation_id("op_123"),
        DeployRequest {
            namespace_id: namespace_id("default"),
            namespace_revision_id: target_namespace_revision_id(),
            services: vec![DeployServiceSpec {
                service_id: service_id("svc_api"),
                image: image("registry.example/api:rev_2"),
                replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
                routes: vec![DeployRoute {
                    target: route_target("api.example.com", 443),
                    endpoint_port: route_port(8080),
                }],
            }],
        },
        DeployExecutionFacts {
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            services: vec![DeployServiceExecutionFacts {
                serving_target_entry: None,
                route_bindings: Vec::new(),
            }],
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            dataplane_machines: Vec::new(),
            observed_machines: Vec::new(),
            namespace_cleanup_candidates: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
    .expect("routed deploy command preparation succeeds")
}

pub(super) fn deploy_command_without_eligible_machines(replicas: u16) -> DeployExecutionCommand {
    prepared_deploy_command(replicas, Vec::new(), Vec::new())
}

pub(super) fn deploy_command_with_existing_container(
    replicas: u16,
    machine_id: &str,
    container_id: &str,
) -> DeployExecutionCommand {
    let snapshot = MachineContainerObservationSnapshot::try_new(
        self::machine_id(machine_id),
        [observed_service_container_with_entry(
            machine_id,
            container_id,
            target_namespace_revision_entry_id(),
        )],
    )
    .expect("valid machine observation snapshot");
    prepared_deploy_command(
        replicas,
        vec![self::machine_id("machine_a"), self::machine_id("machine_b")],
        vec![snapshot],
    )
}

pub(super) fn deploy_command_replacing_old_container(
    replicas: u16,
    machine_id: &str,
    container_id: &str,
) -> DeployExecutionCommand {
    let snapshot = MachineContainerObservationSnapshot::try_new(
        self::machine_id(machine_id),
        [observed_service_container(
            machine_id,
            container_id,
            "entry_old",
        )],
    )
    .expect("valid machine observation snapshot");
    prepared_deploy_command(
        replicas,
        vec![self::machine_id("machine_a"), self::machine_id("machine_b")],
        vec![snapshot],
    )
}

fn prepared_deploy_command(
    replicas: u16,
    eligible_machines: Vec<MachineId>,
    observed_machines: Vec<MachineContainerObservationSnapshot>,
) -> DeployExecutionCommand {
    prepare_deploy_execution_command(
        operation_id("op_123"),
        DeployRequest {
            namespace_id: namespace_id("default"),
            namespace_revision_id: target_namespace_revision_id(),
            services: vec![DeployServiceSpec {
                service_id: service_id("svc_api"),
                image: image("registry.example/api:rev_2"),
                replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
                routes: Vec::new(),
            }],
        },
        DeployExecutionFacts {
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            services: vec![DeployServiceExecutionFacts {
                serving_target_entry: None,
                route_bindings: Vec::new(),
            }],
            dataplane_machines: Vec::new(),
            eligible_machines,
            namespace_cleanup_candidates: namespace_cleanup_candidates(&observed_machines),
            observed_machines,
            step_timeout: Duration::from_secs(5),
        },
    )
    .expect("deploy command preparation succeeds")
}

pub(super) fn empty_deploy_command_with_running_container(
    machine_id: &str,
    container_id: &str,
) -> DeployExecutionCommand {
    let snapshot = MachineContainerObservationSnapshot::try_new(
        self::machine_id(machine_id),
        [observed_service_container(
            machine_id,
            container_id,
            "entry_old",
        )],
    )
    .expect("valid machine observation snapshot");
    let namespace_cleanup_candidates = namespace_cleanup_candidates(&[snapshot.clone()]);
    prepare_deploy_execution_command(
        operation_id("op_123"),
        DeployRequest {
            namespace_id: namespace_id("default"),
            namespace_revision_id: namespace_revision_id("rev_3"),
            services: Vec::new(),
        },
        DeployExecutionFacts {
            namespace_route_bindings: vec![RouteBindingState {
                namespace_id: namespace_id("default"),
                target: RouteTarget::new(
                    RouteHostname::try_new("api.example.com").expect("valid route hostname"),
                    route_port(443),
                ),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
            }],
            namespace_serving_entries: vec![ServingTargetEntry {
                namespace_id: namespace_id("default"),
                service_id: service_id("svc_api"),
                namespace_revision_entry_id: namespace_revision_entry_id("entry_old"),
            }],
            services: Vec::new(),
            dataplane_machines: Vec::new(),
            eligible_machines: vec![self::machine_id("machine_a")],
            namespace_cleanup_candidates,
            observed_machines: vec![snapshot],
            step_timeout: Duration::from_secs(5),
        },
    )
    .expect("empty deploy command preparation succeeds")
}

fn namespace_cleanup_candidates(
    observed_machines: &[MachineContainerObservationSnapshot],
) -> Vec<DeployCleanupContainer> {
    ployzd::deploy_worker::namespace_cleanup_candidates(observed_machines)
}

fn wireguard_public_key(value: impl Into<String>) -> WireGuardPublicKey {
    WireGuardPublicKey::try_new(value).expect("valid wireguard public key")
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
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        namespace_revision_entry_id: namespace_revision_entry_id,
        operation_id: operation_id("op_existing"),
        step_id: StepId::try_new(format!("existing_{container_id}")).expect("valid step id"),
        kind: ManagedContainerKind::Service,
        state: ContainerRuntimeState::running_unroutable(),
    }
}

pub(super) fn active_service_running() -> DeployRunningStage {
    DeployRunningStage::ServingTargetCommit
}

pub(super) fn target_namespace_revision_id() -> NamespaceRevisionId {
    namespace_revision_id("rev_2")
}

pub(super) fn target_namespace_revision_entry_id() -> NamespaceRevisionEntryId {
    ployz_core::deploy::namespace_revision_entry_id_for(
        &namespace_id("default"),
        &service_id("svc_api"),
        &image("registry.example/api:rev_2"),
    )
}

pub(super) fn image(value: &str) -> ImageReference {
    ImageReference::try_new(value).expect("valid image")
}

pub(super) fn route_target(hostname: &str, port: u16) -> RouteTarget {
    RouteTarget {
        hostname: RouteHostname::try_new(hostname).expect("valid route hostname"),
        port: route_port(port),
    }
}

pub(super) fn route_port(port: u16) -> RoutePort {
    RoutePort::try_new(port).expect("valid route port")
}

pub(super) fn retained_container(machine_id: &str, container_id: &str) -> RetainedArtifact {
    RetainedArtifact::StartedContainer {
        machine_id: self::machine_id(machine_id),
        container_id: self::container_id(container_id),
        log_hint: OperatorHint::try_new(format!("ployz logs {container_id}"))
            .expect("valid log hint"),
    }
}

pub(super) fn cleanup_container(
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

pub(super) fn cleanup_container_with_entry(
    machine_id: &str,
    container_id: &str,
    namespace_revision_entry_id: NamespaceRevisionEntryId,
) -> DeployCleanupContainer {
    DeployCleanupContainer {
        machine_id: self::machine_id(machine_id),
        container_id: self::container_id(container_id),
        namespace_id: namespace_id("default"),
        service_id: service_id("svc_api"),
        namespace_revision_entry_id: namespace_revision_entry_id,
        operation_id: operation_id("op_existing"),
        step_id: StepId::try_new(format!("existing_{container_id}")).expect("valid step id"),
        kind: ManagedContainerKind::Service,
    }
}

pub(super) fn retained_created_container(machine_id: &str, container_id: &str) -> RetainedArtifact {
    RetainedArtifact::CreatedContainer {
        machine_id: self::machine_id(machine_id),
        container_id: self::container_id(container_id),
        inspect_hint: inspect_hint(container_id),
    }
}

pub(super) fn retained_stop_failed_container(
    machine_id: &str,
    container_id: &str,
) -> RetainedArtifact {
    RetainedArtifact::ContainerStopFailed {
        machine_id: self::machine_id(machine_id),
        container_id: self::container_id(container_id),
        message: runtime_failure_message("container stop failed: permission denied"),
        inspect_hint: inspect_hint(container_id),
    }
}

pub(super) fn inspect_hint(container_id: &str) -> OperatorHint {
    OperatorHint::try_new(format!("ployz container inspect {container_id}"))
        .expect("valid inspect hint")
}

fn runtime_failure_message(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("valid failure message")
}
