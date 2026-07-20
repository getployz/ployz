use ployz_core::certificate::{ActiveCertState, CertBundleRef, CertValidAt, CertValidityWindow};
use ployz_core::deploy::{
    ContainerCommand, ContainerHealthcheck, ContainerHealthcheckTest, ContainerMountPath,
    DatasetName, DependencyCondition, DeployCleanupContainer, DeployRequest, DeployRoute,
    DeployRouteTarget, DeployServiceSpec, DeployVolumeHandoffApplied, HealthcheckShellCommand,
    ImageReference, PreStartHook, ReplicaCount, ServiceDependency, ServiceMode, ServiceVolumeMount,
    VolumeMaxSizeBytes, VolumeName, VolumeSpec, ZfsPoolName,
};
use ployz_core::ids::{
    ContainerId, MachineId, NamespaceRevisionEntryId, NamespaceRevisionId, OperationId,
    RouteBindingId, ServiceId,
};

use ployz_core::ingress::{AutomaticHostnameLabel, RouteBindingOrigin};
use ployz_core::intent::{RouteBindingState, ServingTargetEntry, VolumeKind, VolumePinState};
use ployz_core::machine::runtime::{
    MachineContainerObservationSnapshot, ManagedContainerObservation,
};
use ployz_core::operation::{
    CertificateProvisionFailure, DeployCleanupFailure, DeployEvidence, DeployRunningStage,
    DeployTransition, DeployVolumeHandoffRollbackContainerOutcome, FailureMessage, OperatorHint,
    RetainedArtifact, RouteHostname, RoutePort, RouteTarget, UnusableMachine,
};
pub(crate) use ployz_test_support::containers;
use ployz_test_support::fixtures::serving_target_entry;
pub(crate) use ployz_test_support::ids::{
    cert_id, container_id, machine_id, namespace_id, namespace_revision_entry_id, operation_id,
    service_id,
};

pub(super) fn phase_number(value: u16) -> ployz_core::operation::DeployPhaseNumber {
    ployz_core::operation::DeployPhaseNumber::try_new(value).expect("positive phase number")
}
use crate::certificate::GatewayCertificateTarget;
use crate::control::operations::deploy::{
    AutomaticHostnameMode, CertificateProvisioner, DeployExecutionFacts, DeployExecutionInput,
    DeployHealthCheckError, DeployHealthChecker, DeployOperationRecordError,
    DeployOperationRecorder, DeployPhasePromotion, MachineContainerRuntime,
    MachineContainerRuntimeError, MachineRuntimeUnavailableReason, NamespaceCommitError,
    NamespaceStateCommitter, PreStartHookRuntimeError,
};
use crate::control::role_client::machine::MachineImageResolveError;
use crate::roles::machine::protocol::{
    MachineContainerRemoveRpcRequest, MachineContainerResolveImageRpcRequest,
    MachineContainerRestartRpcRequest, MachineContainerRunHookRpcOk,
    MachineContainerRunHookRpcRequest, MachineContainerRunRpcRequest, MachineContainerStopOutcome,
    MachineContainerStopRpcRequest, MachineRunContainerOutcome,
};

pub(super) fn environment_revision_key() -> ployz_core::deploy::EnvironmentRevisionKey {
    let seed = ployz_core::nats_config::NatsUserSeed::try_new(
        "SUAIZ5LKGG2Y4WC7ZPKS46LSLLJQIFTO6KMSWSU2VN3TC7YRRIKH5WRXJQ",
    )
    .expect("valid deterministic controller seed");
    ployz_core::deploy::EnvironmentRevisionKey::derive_from_controller_seed(&seed)
}
use std::net::Ipv4Addr;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

pub(super) struct RecordingOperations {
    pub(super) records: Vec<RecordedOperation>,
    pub(super) phase_records: Vec<DeployEvidence>,
    fail_completed_transition_remaining: usize,
    fail_cleanup_evidence_remaining: usize,
    fail_phase_finished_evidence_remaining: usize,
    fail_handoff_applied_evidence_remaining: usize,
    pub(super) completed_transition_attempts: usize,
    expected_operation_id: OperationId,
}

impl Default for RecordingOperations {
    fn default() -> Self {
        Self::for_operation(operation_id("op_123"))
    }
}

impl RecordingOperations {
    pub(super) fn for_operation(expected_operation_id: OperationId) -> Self {
        Self {
            records: Vec::new(),
            phase_records: Vec::new(),
            fail_completed_transition_remaining: 0,
            fail_cleanup_evidence_remaining: 0,
            fail_phase_finished_evidence_remaining: 0,
            fail_handoff_applied_evidence_remaining: 0,
            completed_transition_attempts: 0,
            expected_operation_id,
        }
    }

    pub(super) fn fail_completed_transition_times(times: usize) -> Self {
        Self {
            records: Vec::new(),
            phase_records: Vec::new(),
            fail_completed_transition_remaining: times,
            fail_cleanup_evidence_remaining: 0,
            fail_phase_finished_evidence_remaining: 0,
            fail_handoff_applied_evidence_remaining: 0,
            completed_transition_attempts: 0,
            expected_operation_id: operation_id("op_123"),
        }
    }

    pub(super) fn fail_cleanup_evidence_times(times: usize) -> Self {
        Self {
            records: Vec::new(),
            phase_records: Vec::new(),
            fail_completed_transition_remaining: 0,
            fail_cleanup_evidence_remaining: times,
            fail_phase_finished_evidence_remaining: 0,
            fail_handoff_applied_evidence_remaining: 0,
            completed_transition_attempts: 0,
            expected_operation_id: operation_id("op_123"),
        }
    }

    pub(super) fn fail_phase_finished_evidence_times(times: usize) -> Self {
        Self {
            records: Vec::new(),
            phase_records: Vec::new(),
            fail_completed_transition_remaining: 0,
            fail_cleanup_evidence_remaining: 0,
            fail_phase_finished_evidence_remaining: times,
            fail_handoff_applied_evidence_remaining: 0,
            completed_transition_attempts: 0,
            expected_operation_id: operation_id("op_123"),
        }
    }

    pub(super) fn fail_handoff_applied_evidence_once() -> Self {
        Self {
            fail_handoff_applied_evidence_remaining: 1,
            ..Self::default()
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RecordedOperation {
    Transition(DeployTransition),
    PlanCreated {
        replica_count: usize,
    },
    GlobalPlacement {
        candidates: Vec<MachineId>,
        selected: Vec<MachineId>,
        deferred: Vec<UnusableMachine>,
        draining: Vec<MachineId>,
    },
    ImageAvailabilityVerified,
    HealthCheckStarted,
    ContainerStarted {
        machine_id: MachineId,
        container_id: ContainerId,
    },
    VolumeHandoffApplied {
        handoff: DeployVolumeHandoffApplied,
    },
    VolumeHandoffRollbackFinished {
        outcomes: Vec<DeployVolumeHandoffRollbackContainerOutcome>,
    },
    CleanupFinished {
        removed: Vec<DeployCleanupContainer>,
        failed: Vec<DeployCleanupFailure>,
        images: Vec<ployz_core::operation::DeployImageCleanup>,
    },
}

impl DeployOperationRecorder for RecordingOperations {
    async fn record_deploy_transition(
        &mut self,
        recorded_operation_id: &OperationId,
        transition: DeployTransition,
    ) -> Result<(), DeployOperationRecordError> {
        assert_eq!(recorded_operation_id, &self.expected_operation_id);
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
        assert_eq!(recorded_operation_id, &self.expected_operation_id);
        match evidence {
            DeployEvidence::ImageResolved { .. } => {}
            DeployEvidence::PlanCreated { plan } => {
                self.records.push(RecordedOperation::PlanCreated {
                    replica_count: plan
                        .phases
                        .iter()
                        .flat_map(|phase| &phase.services)
                        .map(|service| service.work.steps().count())
                        .sum(),
                });
                if let Some(ployz_core::deploy::DeployServicePlacement::Global {
                    candidates,
                    selected,
                    deferred,
                    draining,
                }) = plan
                    .phases
                    .iter()
                    .flat_map(|phase| &phase.services)
                    .map(|service| &service.placement)
                    .find(|placement| {
                        matches!(
                            placement,
                            ployz_core::deploy::DeployServicePlacement::Global { .. }
                        )
                    })
                {
                    self.records.push(RecordedOperation::GlobalPlacement {
                        candidates: candidates.clone(),
                        selected: selected.clone(),
                        deferred: deferred.clone(),
                        draining: draining.clone(),
                    });
                }
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
            DeployEvidence::VolumeHandoffApplied { handoff, .. } => {
                if self.fail_handoff_applied_evidence_remaining > 0 {
                    self.fail_handoff_applied_evidence_remaining -= 1;
                    return Err(DeployOperationRecordError::Synthetic {
                        message: "volume handoff evidence record failed",
                    });
                }
                self.records
                    .push(RecordedOperation::VolumeHandoffApplied { handoff });
            }
            DeployEvidence::VolumeHandoffRollbackFinished { outcomes, .. } => {
                self.records
                    .push(RecordedOperation::VolumeHandoffRollbackFinished { outcomes });
            }
            DeployEvidence::HealthCheckStarted => {
                self.records.push(RecordedOperation::HealthCheckStarted);
            }
            evidence @ (DeployEvidence::PhaseStarted { .. }
            | DeployEvidence::PhaseFinished { .. }) => {
                if self.fail_phase_finished_evidence_remaining > 0
                    && matches!(evidence, DeployEvidence::PhaseFinished { .. })
                {
                    self.fail_phase_finished_evidence_remaining -= 1;
                    return Err(DeployOperationRecordError::Synthetic {
                        message: "phase evidence record failed",
                    });
                }
                self.phase_records.push(evidence);
            }
            DeployEvidence::CleanupFinished {
                removed,
                failed,
                images,
            } => {
                if self.fail_cleanup_evidence_remaining > 0 {
                    self.fail_cleanup_evidence_remaining -= 1;
                    return Err(DeployOperationRecordError::Synthetic {
                        message: "cleanup evidence record failed",
                    });
                }
                self.records.push(RecordedOperation::CleanupFinished {
                    removed,
                    failed,
                    images,
                });
            }
        }
        Ok(())
    }
}

pub(super) struct RecordingRuntime {
    pub(super) actions: Vec<RuntimeAction>,
    pub(super) resolutions: Vec<(MachineId, MachineContainerResolveImageRpcRequest)>,
    pub(super) requests: Vec<(MachineId, MachineContainerRunRpcRequest)>,
    pub(super) hook_requests: Vec<(MachineId, MachineContainerRunHookRpcRequest)>,
    pub(super) stops: Vec<(MachineId, MachineContainerStopRpcRequest)>,
    pub(super) restarts: Vec<(MachineId, MachineContainerRestartRpcRequest)>,
    pub(super) removals: Vec<(MachineId, MachineContainerRemoveRpcRequest)>,
    pub(super) image_removals: Vec<(MachineId, ployz_core::image::ImageRemoveRequest)>,
    pub(super) image_ensures: Vec<(MachineId, ployz_core::image::ImageEnsureRequest)>,
    pub(super) volume_ensures: Vec<(MachineId, VolumePinState)>,
    volume_ensure_failure: Option<crate::control::operations::deploy::MachineVolumeEnsureError>,
    required_pin_commit: Option<Arc<AtomicBool>>,
    containers: Vec<ContainerId>,
    hook_outcomes: Vec<(ContainerId, i64)>,
    fail_after_first: bool,
    hang_after_first: Option<Arc<tokio::sync::Notify>>,
    reuse_existing: bool,
    start_existing: bool,
    fail_start: bool,
    fail_start_after_first: bool,
    fail_remove: bool,
    fail_stop: bool,
    fail_stop_for: Vec<ContainerId>,
    stop_outcomes: Vec<(ContainerId, SyntheticStopResult)>,
    hang_stop_for: Vec<(ContainerId, Option<Arc<tokio::sync::Notify>>)>,
    fail_restart: bool,
    run_failure: Option<SyntheticRunFailure>,
}

#[derive(Clone)]
enum SyntheticRunFailure {
    Ambiguous(Vec<ContainerId>),
    Unavailable,
    Hang,
}

#[derive(Clone)]
enum SyntheticStopResult {
    Outcome(MachineContainerStopOutcome),
    UnavailableAfterDelivery {
        late_completion: Arc<tokio::sync::Notify>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum RuntimeAction {
    Run(ContainerId),
    Stop(ContainerId),
    Restart(ContainerId),
    Remove(ContainerId),
}

/// How the fake handles serving-target commits; routes always record.
pub(super) enum ServingCommitBehavior {
    Commit,
    Slow(Duration),
    LoseLock,
}

pub(super) struct RecordingNamespaceState {
    pub(super) serving_behavior: ServingCommitBehavior,
    pub(super) route_requests: Vec<RouteBindingState>,
    pub(super) route_removals: Vec<RouteTarget>,
    pub(super) serving_requests: Vec<ServingTargetEntry>,
    pub(super) phase_requests: Vec<(
        Vec<RouteBindingState>,
        Vec<RouteTarget>,
        Vec<ServingTargetEntry>,
    )>,
    pub(super) serving_removals: Vec<ServiceId>,
    pub(super) volume_pin_requests: Vec<VolumePinState>,
    pin_commit_signal: Option<Arc<AtomicBool>>,
    certificate_ready: Option<Arc<AtomicBool>>,
}

impl RecordingNamespaceState {
    pub(super) fn stored() -> Self {
        Self::with_serving_behavior(ServingCommitBehavior::Commit)
    }

    pub(super) fn slow_serving_commits(delay: Duration) -> Self {
        Self::with_serving_behavior(ServingCommitBehavior::Slow(delay))
    }

    pub(super) fn lost_lock_serving_commits() -> Self {
        Self::with_serving_behavior(ServingCommitBehavior::LoseLock)
    }

    pub(super) fn requiring_certificate_ready(certificate_ready: Arc<AtomicBool>) -> Self {
        let mut state = Self::stored();
        state.certificate_ready = Some(certificate_ready);
        state
    }

    pub(super) fn signaling_pin_commit(signal: Arc<AtomicBool>) -> Self {
        let mut state = Self::stored();
        state.pin_commit_signal = Some(signal);
        state
    }

    fn with_serving_behavior(serving_behavior: ServingCommitBehavior) -> Self {
        Self {
            serving_behavior,
            route_requests: Vec::new(),
            route_removals: Vec::new(),
            serving_requests: Vec::new(),
            phase_requests: Vec::new(),
            serving_removals: Vec::new(),
            volume_pin_requests: Vec::new(),
            pin_commit_signal: None,
            certificate_ready: None,
        }
    }
}

impl NamespaceStateCommitter for RecordingNamespaceState {
    async fn commit_deploy_phase(
        &mut self,
        promotion: DeployPhasePromotion,
    ) -> Result<(), NamespaceCommitError> {
        let DeployPhasePromotion {
            scope,
            route_bindings,
            route_binding_removals,
            first_serving_target_entry,
            remaining_serving_target_entries,
        } = promotion;
        let serving_target_entries = std::iter::once(first_serving_target_entry)
            .chain(remaining_serving_target_entries)
            .collect::<Vec<_>>();
        if let Some(certificate_ready) = &self.certificate_ready {
            assert!(
                certificate_ready.load(Ordering::SeqCst),
                "custom certificate must be ready before phase commit"
            );
        }
        match self.serving_behavior {
            ServingCommitBehavior::Commit => {}
            ServingCommitBehavior::Slow(delay) => {
                tokio::time::sleep(delay).await;
            }
            ServingCommitBehavior::LoseLock => {
                return Err(NamespaceCommitError::ServingTargetLockLost { scope });
            }
        }
        self.route_requests.extend(route_bindings.iter().cloned());
        self.route_removals
            .extend(route_binding_removals.iter().cloned());
        self.serving_requests
            .extend(serving_target_entries.iter().cloned());
        self.phase_requests.push((
            route_bindings,
            route_binding_removals,
            serving_target_entries,
        ));
        Ok(())
    }

    async fn remove_route_binding(
        &mut self,
        target: RouteTarget,
    ) -> Result<(), NamespaceCommitError> {
        self.route_removals.push(target);
        Ok(())
    }

    async fn replace_volume_pin(
        &mut self,
        state: VolumePinState,
    ) -> Result<(), NamespaceCommitError> {
        self.volume_pin_requests.push(state);
        if let Some(signal) = &self.pin_commit_signal {
            signal.store(true, Ordering::SeqCst);
        }
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
            ServingCommitBehavior::Slow(delay) => {
                tokio::time::sleep(delay).await;
                Ok(())
            }
            ServingCommitBehavior::LoseLock => Err(NamespaceCommitError::ServingTargetLockLost {
                scope: ployz_core::operation::ControlPlaneCommitScope::ServiceEntry {
                    service_id: entry.service_id,
                    namespace_revision_entry_id: entry.namespace_revision_entry_id,
                },
            }),
        }
    }
}

pub(super) struct RecordingCertificates {
    pub(super) requests: Vec<(OperationId, RouteHostname, Vec<GatewayCertificateTarget>)>,
    pub(super) ployz_wildcard_requests: usize,
    pub(super) ployz_operation_ids: Vec<OperationId>,
    pub(super) ployz_target_requests: Vec<Vec<GatewayCertificateTarget>>,
    result: Result<(), CertificateProvisionFailure>,
    certificate_ready: Arc<AtomicBool>,
    ployz_wildcard_ready: Arc<AtomicBool>,
}

impl RecordingCertificates {
    pub(super) fn successful() -> Self {
        Self {
            requests: Vec::new(),
            ployz_wildcard_requests: 0,
            ployz_operation_ids: Vec::new(),
            ployz_target_requests: Vec::new(),
            result: Ok(()),
            certificate_ready: Arc::new(AtomicBool::new(false)),
            ployz_wildcard_ready: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn failing(failure: CertificateProvisionFailure) -> Self {
        Self {
            requests: Vec::new(),
            ployz_wildcard_requests: 0,
            ployz_operation_ids: Vec::new(),
            ployz_target_requests: Vec::new(),
            result: Err(failure),
            certificate_ready: Arc::new(AtomicBool::new(false)),
            ployz_wildcard_ready: Arc::new(AtomicBool::new(false)),
        }
    }

    pub(super) fn readiness(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.certificate_ready)
    }

    pub(super) fn ployz_readiness(&self) -> Arc<AtomicBool> {
        Arc::clone(&self.ployz_wildcard_ready)
    }
}

impl CertificateProvisioner for RecordingCertificates {
    async fn ensure(
        &mut self,
        owner_operation_id: &OperationId,
        _owner: ployz_core::ingress::CertificateOwner,
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

    async fn ensure_ployz_wildcard(
        &mut self,
        owner_operation_id: &OperationId,
        targets: &[GatewayCertificateTarget],
    ) -> Result<ActiveCertState, CertificateProvisionFailure> {
        self.ployz_wildcard_requests += 1;
        self.ployz_operation_ids.push(owner_operation_id.clone());
        self.ployz_target_requests.push(targets.to_vec());
        self.result.clone()?;
        self.ployz_wildcard_ready.store(true, Ordering::SeqCst);
        Ok(active_certificate(
            RouteHostname::try_new("wildcard.up.ployz.app").expect("hostname"),
        ))
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
                message: ployz_core::operation::FailureMessage::try_new("probe failed")
                    .expect("valid failure message"),
                log_hint: ployz_core::operation::OperatorHint::try_new(format!(
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
        containers: &[crate::control::operations::deploy::DeployContainer],
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
        _containers: &[crate::control::operations::deploy::DeployContainer],
    ) -> Result<(), DeployHealthCheckError> {
        tokio::time::sleep(Duration::from_secs(60)).await;
        Ok(())
    }
}

pub(super) struct NotifyingHangingHealth {
    reached: Arc<tokio::sync::Notify>,
}

impl NotifyingHangingHealth {
    pub(super) fn new(reached: Arc<tokio::sync::Notify>) -> Self {
        Self { reached }
    }
}

impl DeployHealthChecker for NotifyingHangingHealth {
    async fn wait_healthy(
        &mut self,
        _containers: &[crate::control::operations::deploy::DeployContainer],
    ) -> Result<(), DeployHealthCheckError> {
        self.reached.notify_one();
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
    pub(super) fn requiring_pin_commit(mut self, signal: Arc<AtomicBool>) -> Self {
        self.required_pin_commit = Some(signal);
        self
    }

    pub(super) fn with_volume_ensure_failure(
        mut self,
        failure: crate::control::operations::deploy::MachineVolumeEnsureError,
    ) -> Self {
        self.volume_ensure_failure = Some(failure);
        self
    }

    pub(super) fn with_containers<const N: usize>(containers: [&str; N]) -> Self {
        Self {
            actions: Vec::new(),
            resolutions: Vec::new(),
            requests: Vec::new(),
            hook_requests: Vec::new(),
            stops: Vec::new(),
            restarts: Vec::new(),
            removals: Vec::new(),
            image_removals: Vec::new(),
            image_ensures: Vec::new(),
            volume_ensures: Vec::new(),
            volume_ensure_failure: None,
            required_pin_commit: None,
            containers: containers.into_iter().map(container_id).rev().collect(),
            hook_outcomes: Vec::new(),
            fail_after_first: false,
            hang_after_first: None,
            reuse_existing: false,
            start_existing: false,
            fail_start: false,
            fail_start_after_first: false,
            fail_remove: false,
            fail_stop: false,
            fail_stop_for: Vec::new(),
            stop_outcomes: Vec::new(),
            hang_stop_for: Vec::new(),
            fail_restart: false,
            run_failure: None,
        }
    }

    pub(super) fn reusing_containers<const N: usize>(containers: [&str; N]) -> Self {
        Self {
            actions: Vec::new(),
            resolutions: Vec::new(),
            requests: Vec::new(),
            hook_requests: Vec::new(),
            stops: Vec::new(),
            restarts: Vec::new(),
            removals: Vec::new(),
            image_removals: Vec::new(),
            image_ensures: Vec::new(),
            volume_ensures: Vec::new(),
            volume_ensure_failure: None,
            required_pin_commit: None,
            containers: containers.into_iter().map(container_id).rev().collect(),
            hook_outcomes: Vec::new(),
            fail_after_first: false,
            hang_after_first: None,
            reuse_existing: true,
            start_existing: false,
            fail_start: false,
            fail_start_after_first: false,
            fail_remove: false,
            fail_stop: false,
            fail_stop_for: Vec::new(),
            stop_outcomes: Vec::new(),
            hang_stop_for: Vec::new(),
            fail_restart: false,
            run_failure: None,
        }
    }

    pub(super) fn starting_existing_containers<const N: usize>(containers: [&str; N]) -> Self {
        Self {
            actions: Vec::new(),
            resolutions: Vec::new(),
            requests: Vec::new(),
            hook_requests: Vec::new(),
            stops: Vec::new(),
            restarts: Vec::new(),
            removals: Vec::new(),
            image_removals: Vec::new(),
            image_ensures: Vec::new(),
            volume_ensures: Vec::new(),
            volume_ensure_failure: None,
            required_pin_commit: None,
            containers: containers.into_iter().map(container_id).rev().collect(),
            hook_outcomes: Vec::new(),
            fail_after_first: false,
            hang_after_first: None,
            reuse_existing: false,
            start_existing: true,
            fail_start: false,
            fail_start_after_first: false,
            fail_remove: false,
            fail_stop: false,
            fail_stop_for: Vec::new(),
            stop_outcomes: Vec::new(),
            hang_stop_for: Vec::new(),
            fail_restart: false,
            run_failure: None,
        }
    }

    pub(super) fn failing_after_first_container() -> Self {
        Self {
            actions: Vec::new(),
            resolutions: Vec::new(),
            requests: Vec::new(),
            hook_requests: Vec::new(),
            stops: Vec::new(),
            restarts: Vec::new(),
            removals: Vec::new(),
            image_removals: Vec::new(),
            image_ensures: Vec::new(),
            volume_ensures: Vec::new(),
            volume_ensure_failure: None,
            required_pin_commit: None,
            containers: vec![container_id("ctr_1")],
            hook_outcomes: Vec::new(),
            fail_after_first: true,
            hang_after_first: None,
            reuse_existing: false,
            start_existing: false,
            fail_start: false,
            fail_start_after_first: false,
            fail_remove: false,
            fail_stop: false,
            fail_stop_for: Vec::new(),
            stop_outcomes: Vec::new(),
            hang_stop_for: Vec::new(),
            fail_restart: false,
            run_failure: None,
        }
    }

    pub(super) fn hanging_after_first_container(
        container_id: &str,
        hang_reached: Arc<tokio::sync::Notify>,
    ) -> Self {
        Self {
            actions: Vec::new(),
            resolutions: Vec::new(),
            requests: Vec::new(),
            hook_requests: Vec::new(),
            stops: Vec::new(),
            restarts: Vec::new(),
            removals: Vec::new(),
            image_removals: Vec::new(),
            image_ensures: Vec::new(),
            volume_ensures: Vec::new(),
            volume_ensure_failure: None,
            required_pin_commit: None,
            containers: vec![self::container_id(container_id)],
            hook_outcomes: Vec::new(),
            fail_after_first: false,
            hang_after_first: Some(hang_reached),
            reuse_existing: false,
            start_existing: false,
            fail_start: false,
            fail_start_after_first: false,
            fail_remove: false,
            fail_stop: false,
            fail_stop_for: Vec::new(),
            stop_outcomes: Vec::new(),
            hang_stop_for: Vec::new(),
            fail_restart: false,
            run_failure: None,
        }
    }

    pub(super) fn failing_start(container_id: &str) -> Self {
        Self {
            actions: Vec::new(),
            resolutions: Vec::new(),
            requests: Vec::new(),
            hook_requests: Vec::new(),
            stops: Vec::new(),
            restarts: Vec::new(),
            removals: Vec::new(),
            image_removals: Vec::new(),
            image_ensures: Vec::new(),
            volume_ensures: Vec::new(),
            volume_ensure_failure: None,
            required_pin_commit: None,
            containers: vec![self::container_id(container_id)],
            hook_outcomes: Vec::new(),
            fail_after_first: false,
            hang_after_first: None,
            reuse_existing: false,
            start_existing: false,
            fail_start: true,
            fail_start_after_first: false,
            fail_remove: false,
            fail_stop: false,
            fail_stop_for: Vec::new(),
            stop_outcomes: Vec::new(),
            hang_stop_for: Vec::new(),
            fail_restart: false,
            run_failure: None,
        }
    }

    pub(super) fn with_remove_failure(mut self) -> Self {
        self.fail_remove = true;
        self
    }

    pub(super) fn failing_start_after_first<const N: usize>(containers: [&str; N]) -> Self {
        let mut runtime = Self::with_containers(containers);
        runtime.fail_start_after_first = true;
        runtime
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

    pub(super) fn with_stop_failure_for(mut self, container_id: &str) -> Self {
        self.fail_stop_for.push(self::container_id(container_id));
        self
    }

    pub(super) fn with_stop_outcome(
        mut self,
        container_id: &str,
        outcome: MachineContainerStopOutcome,
    ) -> Self {
        self.stop_outcomes.push((
            self::container_id(container_id),
            SyntheticStopResult::Outcome(outcome),
        ));
        self
    }

    pub(super) fn with_hanging_stop_for(mut self, container_id: &str) -> Self {
        self.hang_stop_for
            .push((self::container_id(container_id), None));
        self
    }

    pub(super) fn with_hanging_stop_completing_late(
        mut self,
        container_id: &str,
        late_completion: Arc<tokio::sync::Notify>,
    ) -> Self {
        self.hang_stop_for
            .push((self::container_id(container_id), Some(late_completion)));
        self
    }

    pub(super) fn with_stop_unavailable_after_delivery(
        mut self,
        container_id: &str,
        late_completion: Arc<tokio::sync::Notify>,
    ) -> Self {
        self.stop_outcomes.push((
            self::container_id(container_id),
            SyntheticStopResult::UnavailableAfterDelivery { late_completion },
        ));
        self
    }

    pub(super) fn with_restart_failure(mut self) -> Self {
        self.fail_restart = true;
        self
    }

    pub(super) fn with_run_ambiguity<const N: usize>(mut self, containers: [&str; N]) -> Self {
        self.run_failure = Some(SyntheticRunFailure::Ambiguous(
            containers.into_iter().map(container_id).collect(),
        ));
        self
    }

    pub(super) fn with_run_unavailable(mut self) -> Self {
        self.run_failure = Some(SyntheticRunFailure::Unavailable);
        self
    }

    pub(super) fn with_hanging_run(mut self) -> Self {
        self.run_failure = Some(SyntheticRunFailure::Hang);
        self
    }
}

impl crate::control::operations::deploy::MachineImageRemovalRuntime for RecordingRuntime {
    async fn remove_image(
        &mut self,
        machine_id: &MachineId,
        request: ployz_core::image::ImageRemoveRequest,
    ) -> Result<
        ployz_core::image::ImageRemoveOk,
        crate::control::role_client::machine::MachineImageRemoveError,
    > {
        self.image_removals.push((machine_id.clone(), request));
        Ok(ployz_core::image::ImageRemoveOk {
            machine_id: machine_id.clone(),
            outcome: ployz_core::image::ImageRemoveOutcome::RetainedInUse,
        })
    }
}

impl MachineContainerRuntime for RecordingRuntime {
    async fn ensure_volume(
        &mut self,
        machine_id: &MachineId,
        volume: &VolumePinState,
    ) -> Result<(), crate::control::operations::deploy::MachineVolumeEnsureError> {
        if let Some(signal) = &self.required_pin_commit {
            assert!(
                signal.load(Ordering::SeqCst),
                "volume pin must commit before its machine effect"
            );
        }
        self.volume_ensures
            .push((machine_id.clone(), volume.clone()));
        match &self.volume_ensure_failure {
            Some(failure) => Err(failure.clone()),
            None => Ok(()),
        }
    }

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
        request: ployz_core::image::ImageEnsureRequest,
    ) -> Result<
        ployz_core::image::ImageEnsureOk,
        crate::control::role_client::machine::MachineImageEnsureError,
    > {
        self.image_ensures
            .push((machine_id.clone(), request.clone()));
        Ok(ployz_core::image::ImageEnsureOk {
            machine_id: machine_id.clone(),
            platform: request.platform,
        })
    }

    async fn run_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRunRpcRequest,
    ) -> Result<MachineRunContainerOutcome, MachineContainerRuntimeError> {
        if let Some(hang_reached) = &self.hang_after_first
            && !self.requests.is_empty()
        {
            hang_reached.notify_one();
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        let identity = request.container.clone();
        self.requests.push((machine_id.clone(), request));
        match self.run_failure.clone() {
            Some(SyntheticRunFailure::Ambiguous(container_ids)) => {
                return Err(MachineContainerRuntimeError::OperationStepAmbiguous {
                    machine_id: machine_id.clone(),
                    operation_id: identity.operation_id,
                    step_id: identity.step_id,
                    container_ids,
                });
            }
            Some(SyntheticRunFailure::Unavailable) => {
                return Err(MachineContainerRuntimeError::Unavailable {
                    machine_id: machine_id.clone(),
                    reason: MachineRuntimeUnavailableReason::RequestFailed {
                        message: "synthetic runtime failure".to_owned(),
                    },
                });
            }
            Some(SyntheticRunFailure::Hang) => {
                tokio::time::sleep(Duration::from_secs(60)).await;
            }
            None => {}
        }
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
        self.actions.push(RuntimeAction::Run(container_id.clone()));

        if self.fail_start || (self.fail_start_after_first && self.requests.len() > 1) {
            return Err(MachineContainerRuntimeError::CreatedContainerStartFailed {
                machine_id: machine_id.clone(),
                inspect_hint: inspect_hint(container_id.as_str()),
                message: runtime_failure_message("container start failed: exec format error"),
                container_id,
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
        self.actions
            .push(RuntimeAction::Remove(container_id.clone()));
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
        self.actions
            .push(RuntimeAction::Remove(container_id.clone()));
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
    ) -> Result<
        crate::roles::machine::protocol::MachineContainerStopOutcome,
        MachineContainerRuntimeError,
    > {
        let container_id = request.container_id.clone();
        self.actions.push(RuntimeAction::Stop(container_id.clone()));
        self.stops.push((machine_id.clone(), request));
        if let Some((_, late_completion)) = self
            .hang_stop_for
            .iter()
            .find(|(expected, _)| expected == &container_id)
        {
            if let Some(late_completion) = late_completion {
                notify_late_stop(Arc::clone(late_completion));
            }
            tokio::time::sleep(Duration::from_secs(60)).await;
        }
        if self.fail_stop || self.fail_stop_for.contains(&container_id) {
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
        match self
            .stop_outcomes
            .iter()
            .find_map(|(expected, outcome)| (expected == &container_id).then_some(outcome.clone()))
            .unwrap_or(SyntheticStopResult::Outcome(
                MachineContainerStopOutcome::StoppedRunning,
            )) {
            SyntheticStopResult::Outcome(outcome) => Ok(outcome),
            SyntheticStopResult::UnavailableAfterDelivery { late_completion } => {
                notify_late_stop(late_completion);
                Err(MachineContainerRuntimeError::Unavailable {
                    machine_id: machine_id.clone(),
                    reason: MachineRuntimeUnavailableReason::RequestFailed {
                        message: "synthetic response lost after stop delivery".to_owned(),
                    },
                })
            }
        }
    }

    async fn restart_container(
        &mut self,
        machine_id: &MachineId,
        request: MachineContainerRestartRpcRequest,
    ) -> Result<(), MachineContainerRuntimeError> {
        let container_id = request.container_id.clone();
        self.actions
            .push(RuntimeAction::Restart(container_id.clone()));
        self.restarts.push((machine_id.clone(), request));
        if self.fail_restart {
            return Err(MachineContainerRuntimeError::RestartContainerFailed {
                machine_id: machine_id.clone(),
                container_id: container_id.clone(),
                message: runtime_failure_message("container restart failed: permission denied"),
                inspect_hint: inspect_hint(container_id.as_str()),
            });
        }
        Ok(())
    }
}

fn notify_late_stop(late_completion: Arc<tokio::sync::Notify>) {
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(10)).await;
        late_completion.notify_one();
    });
}

pub(super) fn deploy_command(replicas: u16) -> DeployExecutionInput {
    prepared_deploy_command(
        replicas,
        vec![machine_id("machine_a"), machine_id("machine_b")],
        Vec::new(),
    )
}

pub(super) fn global_deploy_command(
    eligible_machines: Vec<MachineId>,
    unusable_machines: Vec<UnusableMachine>,
    observed_machines: Vec<MachineContainerObservationSnapshot>,
    namespace_serving_entries: Vec<ServingTargetEntry>,
) -> DeployExecutionInput {
    let mut request = target_deploy_request(1);
    let [service] = request.services.as_mut_slice() else {
        panic!("one service")
    };
    service.mode = ServiceMode::Global;
    deploy_execution_input(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines,
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries,
            namespace_volume_pins: Vec::new(),
            dataplane_members: Vec::new(),
            eligible_machines,
            namespace_cleanup_candidates: namespace_cleanup_candidates(&observed_machines),
            observed_machines,
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn phased_deploy_command(service_ids: &[&str]) -> DeployExecutionInput {
    execution_input_for_request(phased_request(service_ids), Vec::new(), Vec::new())
}

pub(super) fn phased_deploy_with_routed_later_phase() -> DeployExecutionInput {
    let mut request = phased_request(&["svc_database", "svc_web"]);
    let [_, web] = request.services.as_mut_slice() else {
        panic!("fixture has two services");
    };
    web.routes = vec![DeployRoute {
        target: DeployRouteTarget::Hostname {
            hostname: RouteHostname::try_new("web.example.com").expect("valid route hostname"),
        },
        endpoint_port: route_port(8080),
    }];
    execution_input_for_request(request, Vec::new(), Vec::new())
}

fn phased_request(service_ids: &[&str]) -> DeployRequest {
    let mut request = target_deploy_request(1);
    let template = request.services.remove(0);
    request.services = service_ids
        .iter()
        .enumerate()
        .map(|(index, name)| {
            let mut service = template.clone();
            service.service_id = service_id(name);
            service.depends_on = index
                .checked_sub(1)
                .and_then(|dependency| service_ids.get(dependency))
                .map(|dependency| {
                    vec![ServiceDependency {
                        service_id: service_id(dependency),
                        condition: DependencyCondition::Started,
                    }]
                })
                .unwrap_or_default();
            service
        })
        .collect();
    request
}

pub(super) fn same_phase_deploy_command(service_ids: &[&str]) -> DeployExecutionInput {
    let mut request = phased_request(service_ids);
    for service in &mut request.services {
        service.depends_on.clear();
    }
    execution_input_for_request(request, Vec::new(), Vec::new())
}

pub(super) fn invalid_healthy_dependency_command() -> DeployExecutionInput {
    let mut request = phased_request(&["svc_database", "svc_web"]);
    let [_, web] = request.services.as_mut_slice() else {
        panic!("fixture has two services");
    };
    let [dependency] = web.depends_on.as_mut_slice() else {
        panic!("web has one dependency");
    };
    dependency.condition = DependencyCondition::Healthy;
    execution_input_for_request(request, Vec::new(), Vec::new())
}

pub(super) fn phased_deploy_with_reused_dependency() -> DeployExecutionInput {
    let mut request = target_deploy_request(1);
    let mut database = request.services.remove(0);
    database.service_id = service_id("svc_database");
    database.image = resolved_registry_image(database.image.as_str());
    database.runtime.healthcheck = Some(ContainerHealthcheck {
        test: ContainerHealthcheckTest::Shell(
            HealthcheckShellCommand::try_new("true").expect("valid healthcheck"),
        ),
        interval: None,
        timeout: None,
        retries: None,
        start_period: None,
    });
    let mut web = database.clone();
    web.service_id = service_id("svc_web");
    web.runtime.healthcheck = None;
    web.depends_on = vec![ServiceDependency {
        service_id: database.service_id.clone(),
        condition: DependencyCondition::Healthy,
    }];
    let database_entry =
        database.namespace_revision_entry_id(&request.namespace_id, &environment_revision_key());
    let mut promoted = serving_target_entry("svc_database", "unused");
    promoted.namespace_revision_entry_id = database_entry.clone();
    promoted.image = database.image.clone();
    promoted.mode = database.mode;
    promoted.volume_names = Vec::new();
    request.services = vec![database, web];
    let observation = containers::observation("machine_a", "ctr_database")
        .with(
            containers::identity("svc_database")
                .entry(database_entry.as_str())
                .operation("op_existing")
                .step("existing_ctr_database"),
        )
        .running_unroutable()
        .build();
    let snapshots = vec![
        MachineContainerObservationSnapshot::try_new(machine_id("machine_a"), [observation])
            .expect("valid observation"),
    ];
    execution_input_for_request(request, snapshots, vec![promoted])
}

fn execution_input_for_request(
    request: DeployRequest,
    observed_machines: Vec<MachineContainerObservationSnapshot>,
    namespace_serving_entries: Vec<ServingTargetEntry>,
) -> DeployExecutionInput {
    execution_input_for_request_with_routes(
        request,
        observed_machines,
        namespace_serving_entries,
        Vec::new(),
    )
}

fn execution_input_for_request_with_routes(
    request: DeployRequest,
    observed_machines: Vec<MachineContainerObservationSnapshot>,
    namespace_serving_entries: Vec<ServingTargetEntry>,
    namespace_route_bindings: Vec<RouteBindingState>,
) -> DeployExecutionInput {
    deploy_execution_input(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings,
            namespace_serving_entries,
            namespace_volume_pins: Vec::new(),
            dataplane_members: Vec::new(),
            eligible_machines: vec![machine_id("machine_a")],
            namespace_cleanup_candidates: namespace_cleanup_candidates(&observed_machines),
            observed_machines,
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
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
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            dataplane_members: Vec::new(),
            eligible_machines: vec![machine_id("machine_a")],
            namespace_cleanup_candidates: Vec::new(),
            observed_machines: Vec::new(),
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
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
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            dataplane_members: Vec::new(),
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            namespace_cleanup_candidates: Vec::new(),
            namespace_volume_pins: Vec::new(),
            observed_machines: Vec::new(),
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
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
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            dataplane_members: Vec::new(),
            eligible_machines: vec![machine_id("machine_a")],
            namespace_cleanup_candidates: Vec::new(),
            namespace_volume_pins: Vec::new(),
            observed_machines: Vec::new(),
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn routed_deploy_command(replicas: u16) -> DeployExecutionInput {
    routed_deploy_command_with_stored_routes(replicas, Vec::new())
}

pub(super) fn routed_deploy_replacing_route_command(replicas: u16) -> DeployExecutionInput {
    routed_deploy_command_with_stored_routes(replicas, vec![old_route_binding()])
}

pub(super) fn unrouted_deploy_removing_route_command(replicas: u16) -> DeployExecutionInput {
    execution_input_for_request_with_routes(
        target_deploy_request(replicas),
        Vec::new(),
        Vec::new(),
        vec![old_route_binding()],
    )
}

fn old_route_binding() -> RouteBindingState {
    RouteBindingState {
        id: RouteBindingId::try_new("route_old").expect("valid route binding id"),
        namespace_id: namespace_id("default"),
        target: RouteTarget::new(
            RouteHostname::try_new("old.example.com").expect("valid route hostname"),
        ),
        endpoint_port: route_port(8080),
        service_id: service_id("svc_api"),
        origin: RouteBindingOrigin::Declared,
    }
}

fn routed_deploy_command_with_stored_routes(
    replicas: u16,
    namespace_route_bindings: Vec<RouteBindingState>,
) -> DeployExecutionInput {
    deploy_execution_input(
        operation_id("op_123"),
        routed_deploy_request(replicas),
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings,
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            dataplane_members: Vec::new(),
            observed_machines: Vec::new(),
            namespace_cleanup_candidates: Vec::new(),
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: vec![GatewayCertificateTarget {
                machine_id: machine_id("gateway_a"),
                public_ips: vec![Ipv4Addr::new(203, 0, 113, 10).into()],
            }],
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

fn routed_deploy_request(replicas: u16) -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        origin: None,
        volumes: std::collections::BTreeMap::new(),
        services: vec![DeployServiceSpec {
            keep: None,
            service_id: service_id("svc_api"),
            image: image("registry.example/api:rev_2"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            mode: ployz_core::deploy::ServiceMode::Replicated {
                replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
            },
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: vec![DeployRoute {
                target: DeployRouteTarget::Hostname {
                    hostname: RouteHostname::try_new("api.example.com")
                        .expect("valid route hostname"),
                },
                endpoint_port: route_port(8080),
            }],
        }],
    }
}

pub(super) fn ployz_automatic_deploy_command() -> DeployExecutionInput {
    deploy_execution_input(
        operation_id("op_123"),
        DeployRequest {
            namespace_id: namespace_id("default"),
            origin: None,
            volumes: std::collections::BTreeMap::new(),
            services: vec![DeployServiceSpec {
                keep: None,
                service_id: service_id("svc_api"),
                image: image("registry.example/api:rev_2"),
                image_source: ployz_core::deploy::ImageSource::Registry,
                mode: ployz_core::deploy::ServiceMode::Replicated {
                    replicas: ReplicaCount::try_new(1).expect("valid replica count"),
                },
                runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
                pre_start: None,
                depends_on: Vec::new(),
                routes: vec![DeployRoute {
                    target: DeployRouteTarget::AutoHostname {
                        label: AutomaticHostnameLabel::try_new("api")
                            .expect("automatic hostname label"),
                    },
                    endpoint_port: route_port(8080),
                }],
            }],
        },
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            eligible_machines: vec![machine_id("machine_a")],
            dataplane_members: Vec::new(),
            observed_machines: Vec::new(),
            namespace_cleanup_candidates: Vec::new(),
            automatic_hostname_mode: AutomaticHostnameMode::Ployz {
                suffix: RouteHostname::try_new("cluster-one.up.ployz.app")
                    .expect("valid automatic hostname suffix"),
            },
            gateway_certificate_targets: vec![GatewayCertificateTarget {
                machine_id: machine_id("gateway_a"),
                public_ips: vec![Ipv4Addr::new(203, 0, 113, 10).into()],
            }],
            ployz_gateway_certificate_targets: vec![GatewayCertificateTarget {
                machine_id: machine_id("gateway_a"),
                public_ips: vec![Ipv4Addr::new(203, 0, 113, 10).into()],
            }],
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn route_less_pushed_deploy_command(replicas: u16) -> DeployExecutionInput {
    pushed_deploy_command(
        replicas,
        ployz_core::deploy::PushedImageReceipt::try_new([
            (platform("amd64"), platform_image("machine_seed", 'a', 'b')),
            (
                platform("arm64"),
                platform_image("machine_arm_seed", 'd', 'e'),
            ),
        ])
        .expect("pushed receipt"),
        [
            (machine_id("machine_a"), platform("amd64")),
            (machine_id("machine_b"), platform("arm64")),
        ]
        .into_iter()
        .collect(),
    )
}

pub(super) fn amd64_pushed_deploy_command(
    machine_platforms: impl IntoIterator<Item = (MachineId, ployz_core::image::OciPlatform)>,
) -> DeployExecutionInput {
    pushed_deploy_command(
        1,
        ployz_core::deploy::PushedImageReceipt::try_new([(
            platform("amd64"),
            platform_image("machine_seed", 'a', 'b'),
        )])
        .expect("pushed receipt"),
        machine_platforms.into_iter().collect(),
    )
}

fn pushed_deploy_command(
    replicas: u16,
    receipt: ployz_core::deploy::PushedImageReceipt,
    machine_platforms: std::collections::BTreeMap<MachineId, ployz_core::image::OciPlatform>,
) -> DeployExecutionInput {
    let image = image("local/api:rev_2")
        .with_digest(receipt.index_digest())
        .expect("pushed image pins receipt index");
    let request = DeployRequest {
        namespace_id: namespace_id("default"),
        origin: None,
        volumes: std::collections::BTreeMap::new(),
        services: vec![DeployServiceSpec {
            keep: None,
            service_id: service_id("svc_api"),
            image,
            image_source: ployz_core::deploy::ImageSource::PushedToSeed(receipt),
            mode: ployz_core::deploy::ServiceMode::Replicated {
                replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
            },
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    };
    let eligible_machines = machine_platforms.keys().cloned().collect();
    deploy_execution_input(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            machine_platforms,
            seed_clock_testimony: [
                (
                    machine_id("machine_seed"),
                    crate::control::role_client::machine::MachineClockTestimony {
                        control_request_started_at_unix_ms: 1_000_000,
                        machine_observed_at_unix_ms: 1_000_000,
                    },
                ),
                (
                    machine_id("machine_arm_seed"),
                    crate::control::role_client::machine::MachineClockTestimony {
                        control_request_started_at_unix_ms: 1_000_000,
                        machine_observed_at_unix_ms: 1_000_000,
                    },
                ),
            ]
            .into_iter()
            .collect(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            eligible_machines,
            dataplane_members: vec![
                ployz_core::network::DataplaneMember {
                    machine_id: machine_id("machine_seed"),
                    endpoint_subnet: ployz_core::network::MachineEndpointSubnet::try_new(
                        "10.198.99.0/24",
                    )
                    .expect("valid seed subnet"),
                },
                ployz_core::network::DataplaneMember {
                    machine_id: machine_id("machine_arm_seed"),
                    endpoint_subnet: ployz_core::network::MachineEndpointSubnet::try_new(
                        "10.198.98.0/24",
                    )
                    .expect("valid seed subnet"),
                },
            ],
            observed_machines: Vec::new(),
            namespace_cleanup_candidates: Vec::new(),
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn platform(architecture: &str) -> ployz_core::image::OciPlatform {
    ployz_core::image::OciPlatform::try_new("linux", architecture).expect("platform")
}

fn platform_image(seed: &str, manifest: char, image: char) -> ployz_core::deploy::PlatformImage {
    ployz_core::deploy::PlatformImage {
        seed: machine_id(seed),
        manifest_digest: ployz_core::image::OciDigest::try_new(format!(
            "sha256:{}",
            manifest.to_string().repeat(64)
        ))
        .expect("valid manifest digest"),
        image_id: ployz_core::image::OciDigest::try_new(format!(
            "sha256:{}",
            image.to_string().repeat(64)
        ))
        .expect("valid image id"),
        availability_expires_at: ployz_core::deploy::ImageAvailabilityExpiresAt::try_new(
            4_102_444_800,
        )
        .expect("expiry"),
    }
}

pub(super) fn deploy_command_without_eligible_machines(replicas: u16) -> DeployExecutionInput {
    prepared_deploy_command(replicas, Vec::new(), Vec::new())
}

pub(super) fn volume_backed_deploy_command(replicas: u16) -> DeployExecutionInput {
    volume_backed_deploy_command_with_pins(replicas, Vec::new())
}

pub(super) fn volume_backed_replacement_command(
    old_containers: &[(&str, bool)],
) -> DeployExecutionInput {
    volume_backed_replacement_command_with_options(old_containers, false)
}

pub(super) fn volume_backed_replacement_command_with_hook(
    old_containers: &[(&str, bool)],
) -> DeployExecutionInput {
    volume_backed_replacement_command_with_options(old_containers, true)
}

pub(super) fn two_service_volume_backed_replacement_command() -> DeployExecutionInput {
    let api_observation = containers::observation("machine_a", "ctr_old_api")
        .with(
            containers::identity("svc_api")
                .entry("entry_old_api")
                .operation("op_existing")
                .step("existing_api"),
        )
        .named_volume("api_data")
        .running_unroutable()
        .build();
    let worker_observation = containers::observation("machine_a", "ctr_old_worker")
        .with(
            containers::identity("svc_worker")
                .entry("entry_old_worker")
                .operation("op_existing")
                .step("existing_worker"),
        )
        .named_volume("worker_data")
        .running_unroutable()
        .build();
    let snapshot = MachineContainerObservationSnapshot::try_new(
        machine_id("machine_a"),
        [api_observation, worker_observation],
    )
    .expect("valid per-service volume-owner observations");
    let mut request = target_deploy_request(1);
    let [api] = request.services.as_mut_slice() else {
        panic!("deploy request fixture has one service");
    };
    api.runtime.volume_mounts = vec![ServiceVolumeMount {
        volume_name: volume_name("api_data"),
        target: ContainerMountPath::try_new("/data").expect("valid mount path"),
    }];
    let mut worker = api.clone();
    worker.service_id = service_id("svc_worker");
    worker.runtime.volume_mounts = vec![ServiceVolumeMount {
        volume_name: volume_name("worker_data"),
        target: ContainerMountPath::try_new("/data").expect("valid mount path"),
    }];
    request.services.push(worker);
    request
        .volumes
        .insert(volume_name("api_data"), VolumeSpec::Plain);
    request
        .volumes
        .insert(volume_name("worker_data"), VolumeSpec::Plain);
    deploy_execution_input(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: vec![
                VolumePinState::plain(
                    namespace_id("default"),
                    volume_name("api_data"),
                    machine_id("machine_a"),
                ),
                VolumePinState::plain(
                    namespace_id("default"),
                    volume_name("worker_data"),
                    machine_id("machine_a"),
                ),
            ],
            dataplane_members: Vec::new(),
            eligible_machines: vec![machine_id("machine_a")],
            namespace_cleanup_candidates: namespace_cleanup_candidates(std::slice::from_ref(
                &snapshot,
            )),
            observed_machines: vec![snapshot],
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn volume_and_ordinary_replacement_command() -> DeployExecutionInput {
    let old_owner = containers::observation("machine_a", "ctr_old_api")
        .with(
            containers::identity("svc_api")
                .entry("entry_old_api")
                .operation("op_existing")
                .step("existing_api"),
        )
        .named_volume("api_data")
        .running_unroutable()
        .build();
    let snapshot =
        MachineContainerObservationSnapshot::try_new(machine_id("machine_a"), [old_owner])
            .expect("valid volume owner observation");
    let mut request = target_deploy_request(1);
    let [api] = request.services.as_mut_slice() else {
        panic!("deploy request fixture has one service");
    };
    let mut worker = api.clone();
    worker.service_id = service_id("svc_worker");
    api.runtime.volume_mounts = vec![ServiceVolumeMount {
        volume_name: volume_name("api_data"),
        target: ContainerMountPath::try_new("/data").expect("valid mount path"),
    }];
    worker.runtime.volume_mounts.clear();
    request.services.push(worker);
    request
        .volumes
        .insert(volume_name("api_data"), VolumeSpec::Plain);
    deploy_execution_input(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: vec![VolumePinState::plain(
                namespace_id("default"),
                volume_name("api_data"),
                machine_id("machine_a"),
            )],
            dataplane_members: Vec::new(),
            eligible_machines: vec![machine_id("machine_a")],
            namespace_cleanup_candidates: namespace_cleanup_candidates(std::slice::from_ref(
                &snapshot,
            )),
            observed_machines: vec![snapshot],
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

fn volume_backed_replacement_command_with_options(
    old_containers: &[(&str, bool)],
    with_hook: bool,
) -> DeployExecutionInput {
    let observations = old_containers.iter().map(|(container, running)| {
        let observation = containers::observation("machine_a", container)
            .with(
                containers::identity("svc_api")
                    .entry("entry_old")
                    .operation("op_existing")
                    .step(&format!("existing_{container}")),
            )
            .named_volume("postgres_data");
        if *running {
            observation.running_unroutable().build()
        } else {
            observation.exited().build()
        }
    });
    let snapshot =
        MachineContainerObservationSnapshot::try_new(machine_id("machine_a"), observations)
            .expect("valid volume-owner observations");
    let mut request = target_deploy_request(1);
    request
        .volumes
        .insert(volume_name("postgres_data"), VolumeSpec::Plain);
    let [service] = request.services.as_mut_slice() else {
        panic!("deploy request fixture has one service");
    };
    service.runtime.volume_mounts = vec![ServiceVolumeMount {
        volume_name: volume_name("postgres_data"),
        target: ContainerMountPath::try_new("/var/lib/postgresql/data").expect("valid mount path"),
    }];
    if with_hook {
        service.pre_start = Some(PreStartHook {
            command: ContainerCommand::try_new(vec!["true".to_owned()])
                .expect("valid hook command"),
        });
    }
    deploy_execution_input(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: vec![VolumePinState::plain(
                namespace_id("default"),
                volume_name("postgres_data"),
                machine_id("machine_a"),
            )],
            dataplane_members: Vec::new(),
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            namespace_cleanup_candidates: namespace_cleanup_candidates(std::slice::from_ref(
                &snapshot,
            )),
            observed_machines: vec![snapshot],
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

fn volume_backed_deploy_command_with_pins(
    replicas: u16,
    namespace_volume_pins: Vec<VolumePinState>,
) -> DeployExecutionInput {
    let mut request = target_deploy_request(replicas);
    request.volumes.insert(
        volume_name("postgres_data"),
        ployz_core::deploy::VolumeSpec::Plain,
    );
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
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins,
            dataplane_members: Vec::new(),
            eligible_machines: vec![machine_id("machine_a"), machine_id("machine_b")],
            namespace_cleanup_candidates: Vec::new(),
            observed_machines: Vec::new(),
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn provisioned_volume_backed_deploy_command(
    existing_pin: bool,
) -> (DeployExecutionInput, VolumePinState) {
    let namespace_id = namespace_id("default");
    let volume_name = volume_name("postgres_data");
    let machine_id = machine_id("machine_a");
    let pool = ZfsPoolName::try_new("tank").expect("valid pool");
    let max_size_bytes = VolumeMaxSizeBytes::try_new(1024).expect("non-zero size");
    let pin = VolumePinState::try_new(
        namespace_id.clone(),
        volume_name.clone(),
        machine_id.clone(),
        VolumeKind::Provisioned {
            dataset: DatasetName::for_volume(&pool, &namespace_id, &volume_name)
                .expect("canonical dataset"),
            max_size_bytes,
        },
    )
    .expect("valid volume pin");
    let mut request = target_deploy_request(1);
    request.volumes.insert(
        volume_name.clone(),
        VolumeSpec::Provisioned { max_size_bytes },
    );
    let [service] = request.services.as_mut_slice() else {
        panic!("deploy request fixture has one service");
    };
    service.runtime.volume_mounts = vec![ServiceVolumeMount {
        volume_name,
        target: ContainerMountPath::try_new("/var/lib/postgresql/data").expect("valid mount path"),
    }];
    let input = deploy_execution_input(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::from([(
                machine_id.clone(),
                Some(ployz_core::machine::StorageCapability::Ready {
                    pool,
                    capacity: ployz_core::machine::PoolCapacityFacts {
                        total_bytes: 1024 * 1024,
                        provisioned_used_bytes: 0,
                        free_bytes: 1024 * 1024,
                        child_quotas: Vec::new(),
                    },
                }),
            )]),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: if existing_pin {
                vec![pin.clone()]
            } else {
                Vec::new()
            },
            dataplane_members: Vec::new(),
            eligible_machines: vec![machine_id],
            namespace_cleanup_candidates: Vec::new(),
            observed_machines: Vec::new(),
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    );
    (input, pin)
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
    let request = target_deploy_request(replicas);
    let mut promoted = serving_target_entry("svc_api", "unused");
    promoted.namespace_revision_entry_id = target_namespace_revision_entry_id();
    deploy_execution_input(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: vec![promoted],
            namespace_volume_pins: Vec::new(),
            dataplane_members: Vec::new(),
            eligible_machines: vec![self::machine_id("machine_a"), self::machine_id("machine_b")],
            namespace_cleanup_candidates: namespace_cleanup_candidates(std::slice::from_ref(
                &snapshot,
            )),
            observed_machines: vec![snapshot],
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
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

pub(super) fn deploy_command_replacing_old_container_with_keep(
    machine_id: &str,
    container_id: &str,
) -> DeployExecutionInput {
    let image_identity = ployz_core::image::OciDigest::sha256(b"old image");
    let snapshot = MachineContainerObservationSnapshot::try_new(
        self::machine_id(machine_id),
        [containers::observation(machine_id, container_id)
            .with(
                containers::identity("svc_api")
                    .entry("entry_old")
                    .operation("op_existing")
                    .step(&format!("existing_{container_id}")),
            )
            .running_unroutable()
            .resolved_image_identity(image_identity.as_str())
            .created_at_unix_seconds(10)
            .build()],
    )
    .expect("valid machine observation snapshot");
    let mut request = target_deploy_request(1);
    let [service] = request.services.as_mut_slice() else {
        panic!("target request has one service");
    };
    service.keep = Some(ployz_core::deploy::ContainerRetentionCount::new(0));
    deploy_execution_input(
        operation_id("op_123"),
        request,
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            dataplane_members: Vec::new(),
            eligible_machines: vec![self::machine_id("machine_a"), self::machine_id(machine_id)],
            namespace_cleanup_candidates: namespace_cleanup_candidates(std::slice::from_ref(
                &snapshot,
            )),
            observed_machines: vec![snapshot],
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

pub(super) fn target_deploy_request(replicas: u16) -> DeployRequest {
    DeployRequest {
        namespace_id: namespace_id("default"),
        origin: None,
        volumes: std::collections::BTreeMap::new(),
        services: vec![DeployServiceSpec {
            keep: None,
            service_id: service_id("svc_api"),
            image: image("registry.example/api:rev_2"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            mode: ployz_core::deploy::ServiceMode::Replicated {
                replicas: ReplicaCount::try_new(replicas).expect("valid replica count"),
            },
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
    mut facts: DeployExecutionFacts,
) -> DeployExecutionInput {
    if facts.machine_platforms.is_empty() {
        let platform = ployz_core::image::OciPlatform::try_new("linux", "amd64")
            .expect("valid default fixture platform");
        facts.machine_platforms = facts
            .eligible_machines
            .iter()
            .cloned()
            .map(|machine_id| (machine_id, platform.clone()))
            .collect();
    }
    DeployExecutionInput::new(
        operation_id,
        request,
        environment_revision_key(),
        facts,
        std::collections::BTreeMap::new(),
        std::collections::BTreeSet::new(),
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
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: Vec::new(),
            namespace_serving_entries: Vec::new(),
            namespace_volume_pins: Vec::new(),
            dataplane_members: Vec::new(),
            eligible_machines,
            namespace_cleanup_candidates: namespace_cleanup_candidates(&observed_machines),
            observed_machines,
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
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
            origin: None,
            volumes: std::collections::BTreeMap::new(),
            services: Vec::new(),
        },
        DeployExecutionFacts {
            machine_platforms: std::collections::BTreeMap::new(),
            seed_clock_testimony: std::collections::BTreeMap::new(),
            machine_storage_testimony: std::collections::BTreeMap::new(),
            unusable_machines: Vec::new(),
            namespace_route_bindings: vec![RouteBindingState {
                id: RouteBindingId::try_new("route_api").expect("valid route binding id"),
                namespace_id: namespace_id("default"),
                target: RouteTarget::new(
                    RouteHostname::try_new("api.example.com").expect("valid route hostname"),
                ),
                endpoint_port: route_port(8080),
                service_id: service_id("svc_api"),
                origin: RouteBindingOrigin::Declared,
            }],
            namespace_serving_entries: vec![serving_target_entry("svc_api", "entry_old")],
            namespace_volume_pins: Vec::new(),
            dataplane_members: Vec::new(),
            eligible_machines: vec![self::machine_id("machine_a")],
            namespace_cleanup_candidates,
            observed_machines: vec![snapshot],
            automatic_hostname_mode: AutomaticHostnameMode::Disabled,
            gateway_certificate_targets: Vec::new(),
            ployz_gateway_certificate_targets: Vec::new(),
            step_timeout: Duration::from_secs(5),
        },
    )
}

fn namespace_cleanup_candidates(
    observed_machines: &[MachineContainerObservationSnapshot],
) -> Vec<DeployCleanupContainer> {
    crate::control::operations::deploy::namespace_cleanup_candidates(
        &namespace_id("default"),
        observed_machines,
    )
}

pub(super) fn observed_service_container(
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

pub(super) fn observed_service_container_with_entry(
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
    request.namespace_revision_id(&environment_revision_key())
}

pub(super) fn routed_namespace_revision_id() -> NamespaceRevisionId {
    let mut request = routed_deploy_request(1);
    let [service] = request.services.as_mut_slice() else {
        panic!("routed deploy fixture has one service");
    };
    service.image = resolved_registry_image("registry.example/api:rev_2");
    request.namespace_revision_id(&environment_revision_key())
}

pub(super) fn target_namespace_revision_entry_id() -> NamespaceRevisionEntryId {
    ployz_core::deploy::namespace_revision_entry_id_for(
        &namespace_id("default"),
        &service_id("svc_api"),
        &resolved_registry_image("registry.example/api:rev_2"),
        &ployz_core::deploy::ImageSource::Registry,
        &ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
        &environment_revision_key(),
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

pub(super) fn route_target(hostname: &str, _port: u16) -> RouteTarget {
    RouteTarget::new(RouteHostname::try_new(hostname).expect("valid route hostname"))
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

pub(super) fn inspect_hint(container_id: &str) -> OperatorHint {
    OperatorHint::try_new(format!("ployz container inspect {container_id}"))
        .expect("valid inspect hint")
}

fn runtime_failure_message(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("valid failure message")
}
