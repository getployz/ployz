use ployz_core::dataplane::{
    DataplanePrepareError, DataplanePrepareRequest, DataplaneProviderFailure, EbpfForwardingReady,
    EbpfForwardingReadyEvidence, PloyzNativeMeshComponent, PloyzNativeMeshMachineReady,
    PloyzNativeMeshPrepareReport, PloyzNativeMeshReady, WireGuardPublicKey, WireGuardReady,
    WireGuardReadyEvidence,
};
use ployz_core::deploy::{
    ContainerHealthcheck, ContainerHealthcheckTest, ContainerMountPath, DeployCleanupContainer,
    DeployRequest, DeployRoute, DeployRouteTarget, DeployServiceSpec, HealthcheckShellCommand,
    ImageReference, ReplicaCount, ServiceVolumeMount, VolumeName,
};
use ployz_core::ids::{
    ContainerId, MachineId, NamespaceRevisionEntryId, NamespaceRevisionId, OperationId, ServiceId,
};
use ployz_core::machine_runtime::{
    MachineContainerObservationSnapshot, ManagedContainerObservation,
};
use ployz_core::ops::{
    DeployCleanupFailure, DeployEvidence, DeployRunningStage, DeployTransition, FailureMessage,
    OperatorHint, RetainedArtifact, RouteHostname, RoutePort, RouteTarget,
};
use ployz_core::state::{RouteBindingState, ServingTargetEntry, VolumePinState};
pub(crate) use ployz_test_support::containers;
use ployz_test_support::fixtures::serving_target_entry;
pub(crate) use ployz_test_support::ids::{
    container_id, machine_id, namespace_id, namespace_revision_entry_id, operation_id, service_id,
};
use ployzd::operations::deploy::{
    DataplanePreparer, DeployExecutionCommand, DeployExecutionFacts, DeployHealthCheckError,
    DeployHealthChecker, DeployOperationRecordError, DeployOperationRecorder,
    MachineContainerRuntime, MachineContainerRuntimeError, MachineRuntimeUnavailableReason,
    NamespaceCommitError, NamespaceStateCommitter, prepare_deploy_execution_command,
};
use ployzd::roles::machine::protocol::{
    MachineContainerRemoveRpcRequest, MachineContainerRestartRpcRequest,
    MachineContainerRunRpcRequest, MachineContainerStopRpcRequest,
    MachineEnsureEndpointNetworkRpcRequest, MachineRunContainerOutcome,
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
    start_existing: bool,
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

/// How the fake handles serving-target commits; routes always record.
pub(super) enum ServingCommitBehavior {
    Commit,
    Hang,
    LoseLock,
}

pub(super) struct RecordingNamespaceState {
    pub(super) serving_behavior: ServingCommitBehavior,
    pub(super) route_requests: Vec<RouteBindingState>,
    pub(super) route_removals: Vec<RouteTarget>,
    pub(super) serving_requests: Vec<ServingTargetEntry>,
    pub(super) serving_removals: Vec<ServiceId>,
    pub(super) volume_pin_requests: Vec<VolumePinState>,
}

impl RecordingNamespaceState {
    pub(super) fn stored() -> Self {
        Self::with_serving_behavior(ServingCommitBehavior::Commit)
    }

    pub(super) fn hanging_serving_commits() -> Self {
        Self::with_serving_behavior(ServingCommitBehavior::Hang)
    }

    pub(super) fn lost_lock_serving_commits() -> Self {
        Self::with_serving_behavior(ServingCommitBehavior::LoseLock)
    }

    fn with_serving_behavior(serving_behavior: ServingCommitBehavior) -> Self {
        Self {
            serving_behavior,
            route_requests: Vec::new(),
            route_removals: Vec::new(),
            serving_requests: Vec::new(),
            serving_removals: Vec::new(),
            volume_pin_requests: Vec::new(),
        }
    }
}

impl NamespaceStateCommitter for RecordingNamespaceState {
    async fn replace_route_binding(
        &mut self,
        state: RouteBindingState,
    ) -> Result<(), NamespaceCommitError> {
        self.route_requests.push(state);
        Ok(())
    }

    async fn remove_route_binding(
        &mut self,
        target: RouteTarget,
    ) -> Result<(), NamespaceCommitError> {
        self.route_removals.push(target);
        Ok(())
    }

    async fn replace_serving_target_entry(
        &mut self,
        state: ServingTargetEntry,
    ) -> Result<(), NamespaceCommitError> {
        match self.serving_behavior {
            ServingCommitBehavior::Commit => {
                self.serving_requests.push(state);
                Ok(())
            }
            ServingCommitBehavior::Hang => {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(())
            }
            ServingCommitBehavior::LoseLock => Err(NamespaceCommitError::ServingTargetLockLost {
                scope: ployz_core::ops::ControlPlaneCommitScope::ServiceEntry {
                    service_id: state.service_id,
                    namespace_revision_entry_id: state.namespace_revision_entry_id,
                },
            }),
        }
    }

    async fn replace_volume_pin(
        &mut self,
        state: VolumePinState,
    ) -> Result<(), NamespaceCommitError> {
        self.volume_pin_requests.push(state);
        Ok(())
    }

    async fn remove_serving_target_entry(
        &mut self,
        entry: ServingTargetEntry,
    ) -> Result<(), NamespaceCommitError> {
        match self.serving_behavior {
            ServingCommitBehavior::Commit => {
                self.serving_removals.push(entry.service_id);
                Ok(())
            }
            ServingCommitBehavior::Hang => {
                tokio::time::sleep(Duration::from_secs(60)).await;
                Ok(())
            }
            ServingCommitBehavior::LoseLock => Err(NamespaceCommitError::ServingTargetLockLost {
                scope: ployz_core::ops::ControlPlaneCommitScope::ServiceEntry {
                    service_id: entry.service_id,
                    namespace_revision_entry_id: entry.namespace_revision_entry_id,
                },
            }),
        }
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
        containers: &[ployzd::operations::deploy::DeployContainer],
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
        _containers: &[ployzd::operations::deploy::DeployContainer],
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
            start_existing: false,
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
            start_existing: false,
            fail_start: false,
            fail_remove: false,
            fail_stop: false,
        }
    }

    pub(super) fn starting_existing_containers<const N: usize>(containers: [&str; N]) -> Self {
        Self {
            endpoint_networks: Vec::new(),
            requests: Vec::new(),
            stops: Vec::new(),
            removals: Vec::new(),
            containers: containers.into_iter().map(container_id).rev().collect(),
            fail_after_first: false,
            fail_endpoint_network: false,
            reuse_existing: false,
            start_existing: true,
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
            start_existing: false,
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
            start_existing: false,
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
        } else if self.start_existing {
            Ok(MachineRunContainerOutcome::StartedExisting { container_id })
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

    async fn restart_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRestartRpcRequest,
    ) -> Result<(), MachineContainerRuntimeError> {
        let container_id = request.container_id.clone();
        self.stops.push((
            machine_id.clone(),
            MachineContainerStopRpcRequest {
                operation_id: request.operation_id,
                container_id,
                expected_identity: request.expected_identity,
            },
        ));
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

pub(super) fn deploy_command_with_healthcheck(replicas: u16) -> DeployExecutionCommand {
    let mut request = target_deploy_request(replicas);
    request.services[0].runtime.healthcheck = Some(ContainerHealthcheck {
        test: ContainerHealthcheckTest::Shell(
            HealthcheckShellCommand::try_new("true").expect("valid healthcheck"),
        ),
        interval: None,
        timeout: None,
        retries: None,
        start_period: None,
    });
    prepare_deploy_execution_command(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            dataplane_machines: Vec::new(),
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            namespace_cleanup_candidates: Vec::new(),
            namespace_volume_pins: Vec::new(),
            observed_machines: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn routed_deploy_command(replicas: u16) -> DeployExecutionCommand {
    prepare_deploy_execution_command(
        operation_id("op_123"),
        DeployRequest {
            namespace_id: namespace_id("default"),
            services: vec![DeployServiceSpec {
                service_id: service_id("svc_api"),
                image: image("registry.example/api:rev_2"),
                replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
                runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
                routes: vec![DeployRoute {
                    target: DeployRouteTarget::Hostname {
                        hostname: RouteHostname::try_new("api.example.com")
                            .expect("valid route hostname"),
                        port: route_port(443),
                    },
                    endpoint_port: route_port(8080),
                }],
            }],
        },
        DeployExecutionFacts {
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            dataplane_machines: Vec::new(),
            observed_machines: Vec::new(),
            namespace_cleanup_candidates: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn deploy_command_without_eligible_machines(replicas: u16) -> DeployExecutionCommand {
    prepared_deploy_command(replicas, Vec::new(), Vec::new())
}

pub(super) fn volume_backed_deploy_command(replicas: u16) -> DeployExecutionCommand {
    let mut request = target_deploy_request(replicas);
    let [service] = request.services.as_mut_slice() else {
        panic!("deploy request fixture has one service");
    };
    service.runtime.volume_mounts = vec![ServiceVolumeMount {
        volume_name: volume_name("postgres_data"),
        target: ContainerMountPath::try_new("/var/lib/postgresql/data").expect("valid mount path"),
    }];
    prepare_deploy_execution_command(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            dataplane_machines: Vec::new(),
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            namespace_cleanup_candidates: Vec::new(),
            observed_machines: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
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

pub(super) fn target_deploy_request(replicas: u16) -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_api"),
            image: image("registry.example/api:rev_2"),
            replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            routes: Vec::new(),
        }],
    }
}

fn prepared_deploy_command(
    replicas: u16,
    eligible_machines: Vec<MachineId>,
    observed_machines: Vec<MachineContainerObservationSnapshot>,
) -> DeployExecutionCommand {
    prepare_deploy_execution_command(
        operation_id("op_123"),
        target_deploy_request(replicas),
        DeployExecutionFacts {
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            dataplane_machines: Vec::new(),
            eligible_machines,
            namespace_cleanup_candidates: namespace_cleanup_candidates(&observed_machines),
            observed_machines,
            step_timeout: Duration::from_secs(5),
        },
    )
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
    let namespace_cleanup_candidates =
        namespace_cleanup_candidates(std::slice::from_ref(&snapshot));
    prepare_deploy_execution_command(
        operation_id("op_123"),
        DeployRequest {
            namespace_id: namespace_id("default"),
            services: Vec::new(),
        },
        DeployExecutionFacts {
            unusable_machines: Vec::new(),
            namespace_route_bindings: vec![RouteBindingState {
                namespace_id: namespace_id("default"),
                target: RouteTarget::new(
                    RouteHostname::try_new("api.example.com").expect("valid route hostname"),
                    route_port(443),
                ),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
            }],
            namespace_serving_entries: vec![serving_target_entry("svc_api", "entry_old")],
            namespace_volume_pins: Vec::new(),
            dataplane_machines: Vec::new(),
            eligible_machines: vec![self::machine_id("machine_a")],
            namespace_cleanup_candidates,
            observed_machines: vec![snapshot],
            step_timeout: Duration::from_secs(5),
        },
    )
}

fn namespace_cleanup_candidates(
    observed_machines: &[MachineContainerObservationSnapshot],
) -> Vec<DeployCleanupContainer> {
    ployzd::operations::deploy::namespace_cleanup_candidates(
        &namespace_id("default"),
        observed_machines,
    )
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

pub(super) fn active_service_running() -> DeployRunningStage {
    DeployRunningStage::ServingTargetCommit
}

pub(super) fn target_namespace_revision_id(replicas: u16) -> NamespaceRevisionId {
    target_deploy_request(replicas).namespace_revision_id()
}

pub(super) fn target_namespace_revision_entry_id() -> NamespaceRevisionEntryId {
    ployz_core::deploy::namespace_revision_entry_id_for(
        &namespace_id("default"),
        &service_id("svc_api"),
        &image("registry.example/api:rev_2"),
        &ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
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

pub(super) fn volume_name(value: &str) -> VolumeName {
    VolumeName::try_new(value).expect("valid volume name")
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
        identity: containers::identity("svc_api")
            .entry(namespace_revision_entry_id.as_str())
            .operation("op_existing")
            .step(&format!("existing_{container_id}"))
            .build(),
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
