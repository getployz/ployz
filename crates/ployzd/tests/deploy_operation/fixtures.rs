use ployz_core::cert::{
    ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow, ManagedLeaseName,
};
use ployz_core::dataplane::{
    DataplanePrepareError, DataplanePrepareRequest, DataplaneProviderFailure, EbpfForwardingReady,
    EbpfForwardingReadyEvidence, PloyzNativeMeshComponent, PloyzNativeMeshMachineReady,
    PloyzNativeMeshPrepareReport, PloyzNativeMeshReady, WireGuardPublicKey, WireGuardReady,
    WireGuardReadyEvidence,
};
use ployz_core::deploy::{
    ContainerCommand, ContainerHealthcheck, ContainerHealthcheckTest, ContainerMountPath,
    DeployCleanupContainer, DeployRequest, DeployRoute, DeployRouteTarget, DeployServiceSpec,
    HealthcheckShellCommand, ImageReference, PreStartHook, ReplicaCount, ServiceVolumeMount,
    VolumeName,
};
use ployz_core::ids::{
    ContainerId, MachineId, NamespaceRevisionEntryId, NamespaceRevisionId, OperationId, ServiceId,
};
use ployz_core::machine_runtime::{
    MachineContainerObservationSnapshot, ManagedContainerObservation,
};
use ployz_core::ops::{
    CertificateProvisionFailure, DeployCleanupFailure, DeployEvidence, DeployRunningStage,
    DeployTransition, FailureMessage, OperatorHint, RetainedArtifact, RouteHostname, RoutePort,
    RouteTarget,
};
use ployz_core::state::{RouteBindingState, ServingTargetEntry, VolumePinState};
pub(crate) use ployz_test_support::containers;
use ployz_test_support::fixtures::serving_target_entry;
pub(crate) use ployz_test_support::ids::{
    cert_id, container_id, machine_id, namespace_id, namespace_revision_entry_id, operation_id,
    service_id,
};
use ployzd::certificate::GatewayCertificateTarget;
use ployzd::operations::deploy::{
    CertificateProvisioner, DataplanePreparer, DeployExecutionFacts, DeployExecutionInput,
    DeployHealthCheckError, DeployHealthChecker, DeployOperationRecordError,
    DeployOperationRecorder, MachineContainerRuntime, MachineContainerRuntimeError,
    MachineRuntimeUnavailableReason, NamespaceCommitError, NamespaceStateCommitter,
    PreStartHookRuntimeError,
};
use ployzd::roles::machine::client::MachineImageResolveError;
use ployzd::roles::machine::protocol::{
    MachineContainerRemoveRpcRequest, MachineContainerResolveImageRpcRequest,
    MachineContainerRestartRpcRequest, MachineContainerRunHookRpcOk,
    MachineContainerRunHookRpcRequest, MachineContainerRunRpcRequest,
    MachineContainerStopRpcRequest, MachineEnsureEndpointNetworkRpcRequest,
    MachineRunContainerOutcome,
};
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
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
    ImageAvailabilityVerified,
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
            DeployEvidence::ImageResolved { .. } => {}
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
            DeployEvidence::ImageAvailabilityVerified { .. } => {
                self.records
                    .push(RecordedOperation::ImageAvailabilityVerified);
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
    pub(super) resolutions: Vec<(MachineId, MachineContainerResolveImageRpcRequest)>,
    pub(super) endpoint_networks: Vec<(MachineId, MachineEnsureEndpointNetworkRpcRequest)>,
    pub(super) requests: Vec<(MachineId, MachineContainerRunRpcRequest)>,
    pub(super) hook_requests: Vec<(MachineId, MachineContainerRunHookRpcRequest)>,
    pub(super) stops: Vec<(MachineId, MachineContainerStopRpcRequest)>,
    pub(super) removals: Vec<(MachineId, MachineContainerRemoveRpcRequest)>,
    containers: Vec<ContainerId>,
    hook_outcomes: Vec<(ContainerId, i64)>,
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
    certificate_ready: Option<Arc<AtomicBool>>,
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

    pub(super) fn requiring_certificate_ready(certificate_ready: Arc<AtomicBool>) -> Self {
        let mut state = Self::stored();
        state.certificate_ready = Some(certificate_ready);
        state
    }

    fn with_serving_behavior(serving_behavior: ServingCommitBehavior) -> Self {
        Self {
            serving_behavior,
            route_requests: Vec::new(),
            route_removals: Vec::new(),
            serving_requests: Vec::new(),
            serving_removals: Vec::new(),
            volume_pin_requests: Vec::new(),
            certificate_ready: None,
        }
    }
}

impl NamespaceStateCommitter for RecordingNamespaceState {
    async fn replace_route_binding(
        &mut self,
        state: RouteBindingState,
    ) -> Result<(), NamespaceCommitError> {
        if let Some(certificate_ready) = &self.certificate_ready {
            assert!(
                certificate_ready.load(Ordering::SeqCst),
                "custom certificate must be ready before route commit"
            );
        }
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

pub(super) struct RecordingCertificates {
    pub(super) requests: Vec<(OperationId, RouteHostname, Vec<GatewayCertificateTarget>)>,
    result: Result<(), CertificateProvisionFailure>,
    certificate_ready: Arc<AtomicBool>,
}

impl RecordingCertificates {
    pub(super) fn successful() -> Self {
        Self {
            requests: Vec::new(),
            result: Ok(()),
            certificate_ready: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn failing(failure: CertificateProvisionFailure) -> Self {
        Self {
            requests: Vec::new(),
            result: Err(failure),
            certificate_ready: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn readiness(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.certificate_ready)
    }
}

impl CertificateProvisioner for RecordingCertificates {
    async fn ensure(
        &mut self,
        owner_operation_id: &OperationId,
        hostname: &RouteHostname,
        targets: &[GatewayCertificateTarget],
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        self.requests.push((
            owner_operation_id.clone(),
            hostname.clone(),
            targets.to_vec(),
        ));
        self.result.clone()?;
        self.certificate_ready.store(true, Ordering::SeqCst);
        Ok(active_certificate(hostname.clone()))
    }
}

pub(super) fn active_certificate(hostname: RouteHostname) -> ActiveCertState {
    ActiveCertState {
        cert_id: cert_id("cert_api"),
        hostname,
        bundle_ref: CertBundleRef::try_new(format!(
            "sha256:{}:/var/lib/ployz/certs/api.pem",
            "a".repeat(64)
        ))
        .expect("valid certificate bundle reference"),
        validity: CertValidityWindow::try_new(
            CertValidAt::try_new(1).expect("valid not-before timestamp"),
            CertValidAt::try_new(2).expect("valid not-after timestamp"),
        )
        .expect("valid certificate validity window"),
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
            resolutions: Vec::new(),
            endpoint_networks: Vec::new(),
            requests: Vec::new(),
            hook_requests: Vec::new(),
            stops: Vec::new(),
            removals: Vec::new(),
            containers: containers.into_iter().map(container_id).rev().collect(),
            hook_outcomes: Vec::new(),
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
            resolutions: Vec::new(),
            endpoint_networks: Vec::new(),
            requests: Vec::new(),
            hook_requests: Vec::new(),
            stops: Vec::new(),
            removals: Vec::new(),
            containers: containers.into_iter().map(container_id).rev().collect(),
            hook_outcomes: Vec::new(),
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
            resolutions: Vec::new(),
            endpoint_networks: Vec::new(),
            requests: Vec::new(),
            hook_requests: Vec::new(),
            stops: Vec::new(),
            removals: Vec::new(),
            containers: containers.into_iter().map(container_id).rev().collect(),
            hook_outcomes: Vec::new(),
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
            resolutions: Vec::new(),
            endpoint_networks: Vec::new(),
            requests: Vec::new(),
            hook_requests: Vec::new(),
            stops: Vec::new(),
            removals: Vec::new(),
            containers: vec![container_id("ctr_1")],
            hook_outcomes: Vec::new(),
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
            resolutions: Vec::new(),
            endpoint_networks: Vec::new(),
            requests: Vec::new(),
            hook_requests: Vec::new(),
            stops: Vec::new(),
            removals: Vec::new(),
            containers: vec![self::container_id(container_id)],
            hook_outcomes: Vec::new(),
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

    pub(super) fn with_hook_outcome(mut self, container_id: &str, exit_code: i64) -> Self {
        self.hook_outcomes
            .push((self::container_id(container_id), exit_code));
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
    async fn resolve_image(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerResolveImageRpcRequest,
    ) -> Result<ployz_core::image::OciDigest, MachineImageResolveError> {
        let digest = ployz_core::image::OciDigest::sha256(request.reference.as_str().as_bytes());
        self.resolutions.push((machine_id.clone(), request));
        Ok(digest)
    }

    async fn ensure_image(
        &mut self,
        machine_id: &MachineId,
        _request: ployz_core::image::ImageEnsureRequest,
    ) -> Result<
        ployz_core::image::ImageEnsureOk,
        ployzd::roles::machine::client::MachineImageEnsureError,
    > {
        Ok(ployz_core::image::ImageEnsureOk {
            machine_id: machine_id.clone(),
            platform: ployz_core::image::OciPlatform {
                os: "linux".to_owned(),
                architecture: "amd64".to_owned(),
            },
        })
    }

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

    async fn run_pre_start_hook(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRunHookRpcRequest,
    ) -> Result<MachineContainerRunHookRpcOk, PreStartHookRuntimeError> {
        self.hook_requests.push((machine_id.clone(), request));
        let Some((container_id, exit_code)) = self.hook_outcomes.pop() else {
            return Err(PreStartHookRuntimeError::Unavailable {
                machine_id: machine_id.clone(),
                reason: MachineRuntimeUnavailableReason::RequestFailed {
                    message: "synthetic missing hook outcome".to_owned(),
                },
            });
        };
        Ok(MachineContainerRunHookRpcOk {
            machine_id: machine_id.clone(),
            container_id,
            exit_code,
        })
    }

    async fn remove_pre_start_hook(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRemoveRpcRequest,
    ) -> Result<(), PreStartHookRuntimeError> {
        let container_id = request.container_id.clone();
        self.removals.push((machine_id.clone(), request));
        if self.fail_remove {
            return Err(PreStartHookRuntimeError::CleanupFailed {
                machine_id: machine_id.clone(),
                container_id: container_id.clone(),
                message: runtime_failure_message("hook container remove failed: busy"),
                inspect_hint: OperatorHint::try_new(format!(
                    "ployz container inspect {}",
                    container_id.as_str()
                ))
                .expect("valid inspect hint"),
            });
        }
        Ok(())
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

pub(super) fn deploy_command(replicas: u16) -> DeployExecutionInput {
    prepared_deploy_command(
        replicas,
        vec![machine_id("machine_a"), machine_id("machine_b")],
        Vec::new(),
    )
}

pub(super) fn pinned_deploy_command() -> DeployExecutionInput {
    let mut request = target_deploy_request(1);
    let [service] = request.services.as_mut_slice() else {
        panic!("fixture has one service");
    };
    service.image = resolved_registry_image(service.image.as_str());
    deploy_execution_input(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            dataplane_members: Vec::new(),
            eligible_machines: vec![machine_id("machine_a")],
            namespace_cleanup_candidates: Vec::new(),
            observed_machines: Vec::new(),
            managed_lease: None,
            gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn deploy_command_with_healthcheck(replicas: u16) -> DeployExecutionInput {
    let mut request = target_deploy_request(replicas);
    let [service] = request.services.as_mut_slice() else {
        panic!("fixture has one service");
    };
    service.runtime.healthcheck = Some(ContainerHealthcheck {
        test: ContainerHealthcheckTest::Shell(
            HealthcheckShellCommand::try_new("true").expect("valid healthcheck"),
        ),
        interval: None,
        timeout: None,
        retries: None,
        start_period: None,
    });
    deploy_execution_input(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            dataplane_members: Vec::new(),
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            namespace_cleanup_candidates: Vec::new(),
            namespace_volume_pins: Vec::new(),
            observed_machines: Vec::new(),
            managed_lease: None,
            gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn deploy_command_with_pre_start() -> DeployExecutionInput {
    let mut request = target_deploy_request(1);
    let [service] = request.services.as_mut_slice() else {
        panic!("target deploy request declares one service");
    };
    service.runtime.command = Some(
        ContainerCommand::try_new(vec!["sleep".to_owned(), "600".to_owned()])
            .expect("valid service command"),
    );
    service.runtime.healthcheck = Some(ContainerHealthcheck {
        test: ContainerHealthcheckTest::Shell(
            HealthcheckShellCommand::try_new("true").expect("valid healthcheck"),
        ),
        interval: None,
        timeout: None,
        retries: None,
        start_period: None,
    });
    service.pre_start = Some(PreStartHook {
        command: ContainerCommand::try_new(vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "echo ready".to_owned(),
        ])
        .expect("valid hook command"),
    });
    deploy_execution_input(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            dataplane_members: Vec::new(),
            eligible_machines: vec![machine_id("machine_a")],
            namespace_cleanup_candidates: Vec::new(),
            namespace_volume_pins: Vec::new(),
            observed_machines: Vec::new(),
            managed_lease: None,
            gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn routed_deploy_command(replicas: u16) -> DeployExecutionInput {
    deploy_execution_input(
        operation_id("op_123"),
        routed_deploy_request(replicas),
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            dataplane_members: Vec::new(),
            observed_machines: Vec::new(),
            namespace_cleanup_candidates: Vec::new(),
            managed_lease: None,
            gateway_certificate_targets: vec![GatewayCertificateTarget {
                machine_id: machine_id("gateway_a"),
                public_ips: vec![Ipv4Addr::new(203, 0, 113, 10).into()],
            }],
            step_timeout: Duration::from_secs(5),
        },
    )
}

fn routed_deploy_request(replicas: u16) -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_api"),
            image: image("registry.example/api:rev_2"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: vec![DeployRoute {
                target: DeployRouteTarget::Hostname {
                    hostname: RouteHostname::try_new("api.example.com")
                        .expect("valid route hostname"),
                    port: route_port(443),
                },
                endpoint_port: route_port(8080),
            }],
        }],
    }
}

pub(super) fn managed_https_and_custom_http_deploy_command() -> DeployExecutionInput {
    deploy_execution_input(
        operation_id("op_123"),
        DeployRequest {
            namespace_id: namespace_id("default"),
            services: vec![DeployServiceSpec {
                service_id: service_id("svc_api"),
                image: image("registry.example/api:rev_2"),
                image_source: ployz_core::deploy::ImageSource::Registry,
                replicas: ReplicaCount::try_new(1).expect("valid replica count"),
                runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
                pre_start: None,
                depends_on: Vec::new(),
                routes: vec![
                    DeployRoute {
                        target: DeployRouteTarget::Hostname {
                            hostname: RouteHostname::try_new("plain.example.com")
                                .expect("valid route hostname"),
                            port: route_port(80),
                        },
                        endpoint_port: route_port(8080),
                    },
                    DeployRoute {
                        target: DeployRouteTarget::AutoHostname {
                            port: route_port(443),
                        },
                        endpoint_port: route_port(8080),
                    },
                ],
            }],
        },
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            eligible_machines: vec![machine_id("machine_a")],
            dataplane_members: Vec::new(),
            observed_machines: Vec::new(),
            namespace_cleanup_candidates: Vec::new(),
            managed_lease: Some(
                ManagedLeaseName::try_new("cluster-one").expect("valid managed lease name"),
            ),
            gateway_certificate_targets: vec![GatewayCertificateTarget {
                machine_id: machine_id("gateway_a"),
                public_ips: vec![Ipv4Addr::new(203, 0, 113, 10).into()],
            }],
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn route_less_pushed_deploy_command(replicas: u16) -> DeployExecutionInput {
    let request = DeployRequest {
        namespace_id: namespace_id("default"),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_api"),
            image: image("local/api:rev_2"),
            image_source: ployz_core::deploy::ImageSource::PushedToSeed {
                seed: machine_id("machine_seed"),
                manifest_digest: ployz_core::image::OciDigest::try_new(format!(
                    "sha256:{}",
                    "a".repeat(64)
                ))
                .expect("valid manifest digest"),
                image_id: ployz_core::image::OciDigest::try_new(format!(
                    "sha256:{}",
                    "b".repeat(64)
                ))
                .expect("valid image id"),
            },
            replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    };
    let platform = ployz_core::image::OciPlatform {
        os: "linux".to_owned(),
        architecture: "amd64".to_owned(),
    };
    deploy_execution_input(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            machine_platforms: [
                (machine_id("machine_a"), platform.clone()),
                (machine_id("machine_b"), platform),
            ]
            .into_iter()
            .collect(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            dataplane_members: vec![ployz_core::dataplane::DataplaneMember {
                machine_id: machine_id("machine_seed"),
                endpoint_subnet: ployz_core::dataplane::MachineEndpointSubnet::try_new(
                    "10.198.99.0/24",
                )
                .expect("valid seed subnet"),
            }],
            observed_machines: Vec::new(),
            namespace_cleanup_candidates: Vec::new(),
            managed_lease: None,
            gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn deploy_command_without_eligible_machines(replicas: u16) -> DeployExecutionInput {
    prepared_deploy_command(replicas, Vec::new(), Vec::new())
}

pub(super) fn volume_backed_deploy_command(replicas: u16) -> DeployExecutionInput {
    let mut request = target_deploy_request(replicas);
    let [service] = request.services.as_mut_slice() else {
        panic!("deploy request fixture has one service");
    };
    service.runtime.volume_mounts = vec![ServiceVolumeMount {
        volume_name: volume_name("postgres_data"),
        target: ContainerMountPath::try_new("/var/lib/postgresql/data").expect("valid mount path"),
    }];
    deploy_execution_input(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            dataplane_members: Vec::new(),
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            namespace_cleanup_candidates: Vec::new(),
            observed_machines: Vec::new(),
            managed_lease: None,
            gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn deploy_command_with_existing_container(
    replicas: u16,
    machine_id: &str,
    container_id: &str,
) -> DeployExecutionInput {
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
) -> DeployExecutionInput {
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
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    }
}

fn deploy_execution_input(
    operation_id: OperationId,
    request: DeployRequest,
    facts: DeployExecutionFacts,
) -> DeployExecutionInput {
    DeployExecutionInput::new(
        operation_id,
        request,
        facts,
        std::collections::BTreeMap::new(),
    )
}

fn prepared_deploy_command(
    replicas: u16,
    eligible_machines: Vec<MachineId>,
    observed_machines: Vec<MachineContainerObservationSnapshot>,
) -> DeployExecutionInput {
    deploy_execution_input(
        operation_id("op_123"),
        target_deploy_request(replicas),
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            dataplane_members: Vec::new(),
            eligible_machines,
            namespace_cleanup_candidates: namespace_cleanup_candidates(&observed_machines),
            observed_machines,
            managed_lease: None,
            gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn empty_deploy_command_with_running_container(
    machine_id: &str,
    container_id: &str,
) -> DeployExecutionInput {
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
    deploy_execution_input(
        operation_id("op_123"),
        DeployRequest {
            namespace_id: namespace_id("default"),
            services: Vec::new(),
        },
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
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
            dataplane_members: Vec::new(),
            eligible_machines: vec![self::machine_id("machine_a")],
            namespace_cleanup_candidates,
            observed_machines: vec![snapshot],
            managed_lease: None,
            gateway_certificate_targets: Vec::new(),
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
    let mut request = target_deploy_request(replicas);
    let [service] = request.services.as_mut_slice() else {
        panic!("target deploy fixture has one service");
    };
    service.image = resolved_registry_image("registry.example/api:rev_2");
    request.namespace_revision_id()
}

pub(super) fn routed_namespace_revision_id() -> NamespaceRevisionId {
    let mut request = routed_deploy_request(1);
    let [service] = request.services.as_mut_slice() else {
        panic!("routed deploy fixture has one service");
    };
    service.image = resolved_registry_image("registry.example/api:rev_2");
    request.namespace_revision_id()
}

pub(super) fn target_namespace_revision_entry_id() -> NamespaceRevisionEntryId {
    ployz_core::deploy::namespace_revision_entry_id_for(
        &namespace_id("default"),
        &service_id("svc_api"),
        &resolved_registry_image("registry.example/api:rev_2"),
        &ployz_core::deploy::ImageSource::Registry,
        &ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
    )
}

pub(super) fn resolved_registry_image(value: &str) -> ImageReference {
    let requested = image(value);
    requested
        .with_digest(&ployz_core::image::OciDigest::sha256(
            requested.as_str().as_bytes(),
        ))
        .expect("fixture image accepts deterministic digest")
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
