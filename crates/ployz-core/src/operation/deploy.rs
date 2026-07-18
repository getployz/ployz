//! The Deploy operation: a namespace manifest converges onto the cluster
//! through planning, staged execution, route cutover, and cleanup. States,
//! failures, transitions, evidence, and status projection live together
//! here.

use serde::{Deserialize, Serialize};
use std::num::NonZeroU16;

use crate::certificate::CertificateProvisionFailure;
use crate::deploy::{DeployCleanupContainer, DeployPlan, ImageReference};
use crate::deploy::{VolumeAdmissionFailure, VolumeName};
use crate::ids::{
    ContainerId, MachineId, NamespaceId, NamespaceRevisionEntryId, NamespaceRevisionId,
    OperationId, RouteBindingId, ServiceId,
};
use crate::image::OciDigest;
use crate::machine::MachineUsabilityReason;

use super::events::OperationEvent;
use super::projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, kind_mismatch,
};
use super::routes::{RouteHostname, RouteTarget};
use super::text::{CancellationReason, FailureMessage, OperatorHint};
use super::{EventSequence, OperationKind, OperationStatus};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DeployRunningStage {
    EnsuringImages,
    EnsuringVolumes,
    StartingContainers,
    WaitingForHealth,
    EnsuringCertificates,
    RouteCutover,
    ServingTargetCommit,
    RemovingSupersededContainers,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DeployCompletionOutcome {
    Completed,
    CompletedWithWarnings,
    PartiallyCompleted,
    PartiallyCompletedWithWarnings,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "result", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployServiceResult {
    Completed {
        service_id: ServiceId,
    },
    Failed {
        service_id: ServiceId,
        failure: DeployOperationFailure,
    },
    Skipped {
        service_id: ServiceId,
    },
    Unchanged {
        service_id: ServiceId,
    },
    Removed {
        service_id: ServiceId,
    },
}

impl DeployServiceResult {
    #[must_use]
    pub fn service_id(&self) -> &ServiceId {
        match self {
            Self::Completed { service_id }
            | Self::Failed { service_id, .. }
            | Self::Skipped { service_id }
            | Self::Unchanged { service_id }
            | Self::Removed { service_id } => service_id,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DeployPhaseOutcome {
    Promoted,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployOperationState {
    Accepted,
    Planning,
    Running {
        stage: DeployRunningStage,
    },
    Completed {
        outcome: DeployCompletionOutcome,
    },
    Failed {
        failure: DeployOperationFailure,
    },
    Cancelled {
        reason: CancellationReason,
    },
    Interrupted {
        evidence: super::OperationInterruptionEvidence,
    },
}

impl DeployOperationState {
    #[must_use]
    pub const fn completed() -> Self {
        Self::Completed {
            outcome: DeployCompletionOutcome::Completed,
        }
    }

    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Completed { .. }
            | Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::Interrupted { .. } => true,
            Self::Accepted | Self::Planning | Self::Running { .. } => false,
        }
    }

    pub(super) fn interruption_evidence(
        &self,
        cause: super::OperationInterruptionCause,
    ) -> Option<super::OperationInterruptionEvidence> {
        let stage = match self {
            Self::Accepted => super::DeployInterruptionStage::Accepted,
            Self::Planning => super::DeployInterruptionStage::Planning,
            Self::Running { stage } => super::DeployInterruptionStage::Running { stage: *stage },
            Self::Completed { .. }
            | Self::Failed { .. }
            | Self::Cancelled { .. }
            | Self::Interrupted { .. } => return None,
        };
        Some(super::OperationInterruptionEvidence::new(
            cause,
            super::OperationInterruptionStage::Deploy { stage },
        ))
    }

    pub(super) const fn interrupted(evidence: super::OperationInterruptionEvidence) -> Self {
        Self::Interrupted { evidence }
    }
}

/// One machine's placement rejection, carried on NoUsableMachines evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct UnusableMachine {
    pub machine_id: MachineId,
    pub reason: crate::machine::MachineUsabilityReason,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "SafeInteger<\"DeployPhaseNumber\">")
)]
#[serde(try_from = "u16", into = "u16")]
pub struct DeployPhaseNumber(NonZeroU16);

impl DeployPhaseNumber {
    pub fn try_new(value: u16) -> Result<Self, DeployPhaseNumberError> {
        let Some(value) = NonZeroU16::new(value) else {
            return Err(DeployPhaseNumberError::Zero);
        };
        Ok(Self(value))
    }

    #[must_use]
    pub const fn get(self) -> u16 {
        self.0.get()
    }
}

impl TryFrom<u16> for DeployPhaseNumber {
    type Error = DeployPhaseNumberError;

    fn try_from(value: u16) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<DeployPhaseNumber> for u16 {
    fn from(value: DeployPhaseNumber) -> Self {
        value.get()
    }
}

impl std::fmt::Display for DeployPhaseNumber {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        self.get().fmt(formatter)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DeployPhaseNumberError {
    #[error("deploy phase number must be greater than zero")]
    Zero,
}

/// What a failed control-plane commit was writing. Empty-manifest deploys
/// commit no service entry, so namespace-level record failures carry the
/// namespace revision id instead of a counterfeit entry digest.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "scope", rename_all = "snake_case", deny_unknown_fields)]
pub enum ControlPlaneCommitScope {
    DeployPhase {
        namespace_revision_id: NamespaceRevisionId,
        phase: DeployPhaseNumber,
    },
    ServiceEntry {
        service_id: ServiceId,
        namespace_revision_entry_id: NamespaceRevisionEntryId,
    },
    Namespace {
        namespace_revision_id: NamespaceRevisionId,
    },
    VolumePin {
        namespace_id: NamespaceId,
        volume_name: VolumeName,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeployFailureClass {
    PreconditionRejected,
    ImageResolvePullFailed,
    PreStartHookFailed,
    ContainerStartFailed,
    HealthGateFailed,
    CertificateProvisionFailed,
    MachineNoAnswer,
    Timeout,
    RuntimeUnavailable,
    VolumeEnsureFailed,
    ControlPlaneCommitFailed,
    RouteCutoverFailed,
}

impl DeployFailureClass {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PreconditionRejected => "precondition-rejected",
            Self::ImageResolvePullFailed => "image-resolve-pull-failed",
            Self::PreStartHookFailed => "pre-start-hook-failed",
            Self::ContainerStartFailed => "container-start-failed",
            Self::HealthGateFailed => "health-gate-failed",
            Self::CertificateProvisionFailed => "certificate-provision-failed",
            Self::MachineNoAnswer => "machine-no-answer",
            Self::Timeout => "timeout",
            Self::RuntimeUnavailable => "runtime-unavailable",
            Self::VolumeEnsureFailed => "volume-ensure-failed",
            Self::ControlPlaneCommitFailed => "control-plane-commit-failed",
            Self::RouteCutoverFailed => "route-cutover-failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployOperationFailure {
    NoUsableMachines {
        /// Why each known machine was rejected for placement.
        reasons: Vec<UnusableMachine>,
    },
    PlanningFailed {
        service_id: ServiceId,
        namespace_revision_id: NamespaceRevisionId,
        message: FailureMessage,
    },
    VolumeAdmissionFailed {
        service_id: ServiceId,
        machine_id: MachineId,
        failure: Box<VolumeAdmissionFailure>,
    },
    AutomaticHostnameCollision {
        hostname: RouteHostname,
        route_binding_id: RouteBindingId,
    },
    ImageResolutionFailed {
        service_id: ServiceId,
        machine_id: MachineId,
        image: ImageReference,
        message: FailureMessage,
    },
    ArtifactUnavailable {
        service_id: ServiceId,
        namespace_revision_entry_id: NamespaceRevisionEntryId,
        reason: ArtifactUnavailableReason,
    },
    ImageMissingOnSeed {
        service_id: ServiceId,
        seed: MachineId,
        manifest_digest: OciDigest,
    },
    ImageDigestMismatch {
        service_id: ServiceId,
        seed: MachineId,
        expected: OciDigest,
        actual: OciDigest,
    },
    SeedUnavailable {
        service_id: ServiceId,
        seed: MachineId,
        message: FailureMessage,
    },
    PlatformImageUnavailable {
        service_id: ServiceId,
        machine_id: MachineId,
        target_platform: crate::image::OciPlatform,
    },
    PlatformImageExpired {
        service_id: ServiceId,
        seed: MachineId,
        target_platform: crate::image::OciPlatform,
        expired_at: crate::deploy::ImageAvailabilityExpiresAt,
    },
    UnsupportedTargetPlatform {
        service_id: ServiceId,
        machine_id: MachineId,
        image_platform: crate::image::OciPlatform,
        target_platform: crate::image::OciPlatform,
    },
    RuntimeUnavailable {
        machine_id: MachineId,
        message: FailureMessage,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    VolumeEnsureFailed {
        machine_id: MachineId,
        volume_name: VolumeName,
        failure: crate::machine::VolumeEnsureFailure,
    },
    ContainerStartFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    PreStartHookFailed {
        machine_id: MachineId,
        failure: PreStartHookFailure,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    HealthCheckFailed {
        health_check: HealthCheckFailure,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    CertificateProvisionFailed {
        hostname: RouteHostname,
        namespace_revision_id: NamespaceRevisionId,
        failure: CertificateProvisionFailure,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    CertificateProvisionTimedOut {
        hostname: RouteHostname,
        namespace_revision_id: NamespaceRevisionId,
        timeout_seconds: u32,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    ControlPlaneCommitFailed {
        scope: ControlPlaneCommitScope,
        message: FailureMessage,
        retained_artifacts: Vec<RetainedArtifact>,
    },
    RouteCutoverFailed {
        route: RouteTarget,
        reason: RouteCutoverFailureReason,
        retained_artifacts: Vec<RetainedArtifact>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum PreStartHookFailure {
    RuntimeUnavailable {
        message: FailureMessage,
    },
    OperationStepAmbiguous {
        operation_id: OperationId,
        step_id: crate::ids::StepId,
        container_ids: Vec<ContainerId>,
    },
    CreateFailed {
        message: FailureMessage,
    },
    StartFailed {
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
    WaitFailed {
        container_id: ContainerId,
        message: FailureMessage,
        log_hint: OperatorHint,
    },
    TimedOut {
        container_id: ContainerId,
        timeout_millis: u64,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
    Exited {
        container_id: ContainerId,
        exit_code: i64,
        message: FailureMessage,
        log_hint: OperatorHint,
    },
    CleanupFailed {
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
}

impl DeployOperationFailure {
    #[must_use]
    pub fn failure_class(&self) -> DeployFailureClass {
        match self {
            Self::NoUsableMachines { reasons } => {
                if reasons
                    .iter()
                    .any(|reason| matches!(reason.reason, MachineUsabilityReason::FactsUnavailable))
                {
                    DeployFailureClass::MachineNoAnswer
                } else {
                    DeployFailureClass::PreconditionRejected
                }
            }
            Self::PlanningFailed { .. }
            | Self::VolumeAdmissionFailed { .. }
            | Self::AutomaticHostnameCollision { .. } => DeployFailureClass::PreconditionRejected,
            Self::ArtifactUnavailable { .. }
            | Self::ImageResolutionFailed { .. }
            | Self::ImageMissingOnSeed { .. }
            | Self::ImageDigestMismatch { .. }
            | Self::SeedUnavailable { .. }
            | Self::PlatformImageUnavailable { .. }
            | Self::PlatformImageExpired { .. }
            | Self::UnsupportedTargetPlatform { .. } => DeployFailureClass::ImageResolvePullFailed,
            Self::RuntimeUnavailable { .. } => DeployFailureClass::RuntimeUnavailable,
            Self::VolumeEnsureFailed { .. } => DeployFailureClass::VolumeEnsureFailed,
            Self::ContainerStartFailed { .. } => DeployFailureClass::ContainerStartFailed,
            Self::PreStartHookFailed { .. } => DeployFailureClass::PreStartHookFailed,
            Self::HealthCheckFailed { health_check, .. } => match health_check {
                HealthCheckFailure::ProbeFailed { .. } => DeployFailureClass::HealthGateFailed,
                HealthCheckFailure::TimedOut { .. } => DeployFailureClass::Timeout,
            },
            Self::CertificateProvisionFailed { .. } => {
                DeployFailureClass::CertificateProvisionFailed
            }
            Self::CertificateProvisionTimedOut { .. } => DeployFailureClass::Timeout,
            Self::ControlPlaneCommitFailed { .. } => DeployFailureClass::ControlPlaneCommitFailed,
            Self::RouteCutoverFailed { reason, .. } => match reason {
                RouteCutoverFailureReason::GatewayUnavailable { .. } => {
                    DeployFailureClass::MachineNoAnswer
                }
                RouteCutoverFailureReason::RouteRejected { .. }
                | RouteCutoverFailureReason::StateStoreFailed { .. } => {
                    DeployFailureClass::RouteCutoverFailed
                }
                RouteCutoverFailureReason::TimedOut { .. } => DeployFailureClass::Timeout,
            },
        }
    }

    /// The artifacts this failure retained for inspection; empty for failure
    /// classes that reject before any container work starts.
    #[must_use]
    pub fn retained_artifacts(&self) -> &[RetainedArtifact] {
        match self {
            Self::RuntimeUnavailable {
                retained_artifacts, ..
            }
            | Self::ContainerStartFailed {
                retained_artifacts, ..
            }
            | Self::PreStartHookFailed {
                retained_artifacts, ..
            }
            | Self::HealthCheckFailed {
                retained_artifacts, ..
            }
            | Self::CertificateProvisionFailed {
                retained_artifacts, ..
            }
            | Self::CertificateProvisionTimedOut {
                retained_artifacts, ..
            }
            | Self::ControlPlaneCommitFailed {
                retained_artifacts, ..
            }
            | Self::RouteCutoverFailed {
                retained_artifacts, ..
            } => retained_artifacts,
            Self::NoUsableMachines { .. }
            | Self::PlanningFailed { .. }
            | Self::VolumeAdmissionFailed { .. }
            | Self::AutomaticHostnameCollision { .. }
            | Self::VolumeEnsureFailed { .. }
            | Self::ImageResolutionFailed { .. }
            | Self::ArtifactUnavailable { .. }
            | Self::ImageMissingOnSeed { .. }
            | Self::ImageDigestMismatch { .. }
            | Self::SeedUnavailable { .. }
            | Self::PlatformImageUnavailable { .. }
            | Self::PlatformImageExpired { .. }
            | Self::UnsupportedTargetPlatform { .. } => &[],
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum ArtifactUnavailableReason {
    BundleMissing,
    BundleUnreadable {
        message: FailureMessage,
    },
    ImagePullFailed {
        machine_id: MachineId,
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum HealthCheckFailure {
    ProbeFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        log_hint: OperatorHint,
    },
    TimedOut {
        timeout_seconds: u32,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "reason", rename_all = "snake_case", deny_unknown_fields)]
pub enum RouteCutoverFailureReason {
    GatewayUnavailable { machine_id: MachineId },
    RouteRejected { message: FailureMessage },
    StateStoreFailed { message: FailureMessage },
    TimedOut { timeout_seconds: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum RetainedArtifact {
    CreatedContainer {
        machine_id: MachineId,
        container_id: ContainerId,
        inspect_hint: OperatorHint,
    },
    StartedContainer {
        machine_id: MachineId,
        container_id: ContainerId,
        log_hint: OperatorHint,
    },
    ContainerStopFailed {
        machine_id: MachineId,
        container_id: ContainerId,
        message: FailureMessage,
        inspect_hint: OperatorHint,
    },
}

impl RetainedArtifact {
    /// Whether this artifact is a container the failed attempt left behind
    /// for inspection; false for records of a removal that failed on a
    /// pre-existing container.
    #[must_use]
    pub fn is_container(&self) -> bool {
        match self {
            RetainedArtifact::CreatedContainer {
                machine_id: _,
                container_id: _,
                inspect_hint: _,
            }
            | RetainedArtifact::StartedContainer {
                machine_id: _,
                container_id: _,
                log_hint: _,
            } => true,
            RetainedArtifact::ContainerStopFailed {
                machine_id: _,
                container_id: _,
                message: _,
                inspect_hint: _,
            } => false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DeployCleanupFailure {
    pub target: DeployCleanupContainer,
    pub message: FailureMessage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "outcome", rename_all = "snake_case", deny_unknown_fields)]
pub enum DeployImageCleanup {
    Removed {
        machine_id: MachineId,
        service_id: ServiceId,
        image_identity: OciDigest,
    },
    AlreadyAbsent {
        machine_id: MachineId,
        service_id: ServiceId,
        image_identity: OciDigest,
    },
    RetainedInUse {
        machine_id: MachineId,
        service_id: ServiceId,
        image_identity: OciDigest,
    },
    MissingIdentity {
        machine_id: MachineId,
        service_id: ServiceId,
        container_id: ContainerId,
        observed_identity: Option<String>,
    },
    Failed {
        machine_id: MachineId,
        service_id: ServiceId,
        image_identity: OciDigest,
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployTransition {
    Planning,
    Running { stage: DeployRunningStage },
    Completed { outcome: DeployCompletionOutcome },
    Failed { failure: DeployOperationFailure },
    Cancelled { reason: CancellationReason },
}

impl DeployTransition {
    #[must_use]
    pub const fn completed() -> Self {
        Self::Completed {
            outcome: DeployCompletionOutcome::Completed,
        }
    }

    /// Renders this transition as the operation event it records.
    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::Planning => OperationEvent::DeployPlanningStarted {
                operation_id: operation_id.clone(),
            },
            Self::Running { stage } => OperationEvent::DeployRunning {
                operation_id: operation_id.clone(),
                stage: *stage,
            },
            Self::Completed { outcome } => OperationEvent::DeployCompleted {
                operation_id: operation_id.clone(),
                outcome: *outcome,
            },
            Self::Failed { failure } => OperationEvent::DeployFailed {
                operation_id: operation_id.clone(),
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => OperationEvent::Cancelled {
                operation_id: operation_id.clone(),
                kind: OperationKind::Deploy,
                reason: reason.clone(),
            },
        }
    }

    #[must_use]
    pub fn state(&self) -> DeployOperationState {
        match self {
            Self::Planning => DeployOperationState::Planning,
            Self::Running { stage } => DeployOperationState::Running { stage: *stage },
            Self::Completed { outcome } => DeployOperationState::Completed { outcome: *outcome },
            Self::Failed { failure } => DeployOperationState::Failed {
                failure: failure.clone(),
            },
            Self::Cancelled { reason } => DeployOperationState::Cancelled {
                reason: reason.clone(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DeployEvidence {
    ImageResolved {
        service_id: ServiceId,
        machine_id: MachineId,
        requested: ImageReference,
        resolved: ImageReference,
        credential_supplied: bool,
    },
    PlanCreated {
        plan: DeployPlan,
    },
    ImageAvailabilityVerified {
        service_id: ServiceId,
        seed: MachineId,
        platform: crate::image::OciPlatform,
        manifest_digest: OciDigest,
    },
    ContainerStarted {
        machine_id: MachineId,
        container_id: ContainerId,
    },
    HealthCheckStarted,
    PhaseStarted {
        phase: DeployPhaseNumber,
        service_ids: Vec<ServiceId>,
    },
    PhaseFinished {
        phase: DeployPhaseNumber,
        outcome: DeployPhaseOutcome,
        services: Vec<DeployServiceResult>,
    },
    CleanupFinished {
        removed: Vec<DeployCleanupContainer>,
        failed: Vec<DeployCleanupFailure>,
        images: Vec<DeployImageCleanup>,
    },
}

impl DeployEvidence {
    #[must_use]
    pub fn event(&self, operation_id: &OperationId) -> OperationEvent {
        match self {
            Self::ImageResolved {
                service_id,
                machine_id,
                requested,
                resolved,
                credential_supplied,
            } => OperationEvent::DeployImageResolved {
                operation_id: operation_id.clone(),
                service_id: service_id.clone(),
                machine_id: machine_id.clone(),
                requested: requested.clone(),
                resolved: resolved.clone(),
                credential_supplied: *credential_supplied,
            },
            Self::PlanCreated { plan } => OperationEvent::DeployPlanCreated {
                operation_id: operation_id.clone(),
                plan: plan.clone(),
            },
            Self::ImageAvailabilityVerified {
                service_id,
                seed,
                platform,
                manifest_digest,
            } => OperationEvent::DeployImageAvailabilityVerified {
                operation_id: operation_id.clone(),
                service_id: service_id.clone(),
                seed: seed.clone(),
                platform: platform.clone(),
                manifest_digest: manifest_digest.clone(),
            },
            Self::ContainerStarted {
                machine_id,
                container_id,
            } => OperationEvent::DeployContainerStarted {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                container_id: container_id.clone(),
            },
            Self::HealthCheckStarted => OperationEvent::DeployHealthCheckStarted {
                operation_id: operation_id.clone(),
            },
            Self::PhaseStarted { phase, service_ids } => OperationEvent::DeployPhaseStarted {
                operation_id: operation_id.clone(),
                phase: *phase,
                service_ids: service_ids.clone(),
            },
            Self::PhaseFinished {
                phase,
                outcome,
                services,
            } => OperationEvent::DeployPhaseFinished {
                operation_id: operation_id.clone(),
                phase: *phase,
                outcome: *outcome,
                services: services.clone(),
            },
            Self::CleanupFinished {
                removed,
                failed,
                images,
            } => OperationEvent::DeployCleanupFinished {
                operation_id: operation_id.clone(),
                removed: removed.clone(),
                failed: failed.clone(),
                images: images.clone(),
            },
        }
    }
}

/// Deploy events after classification from the flat [`OperationEvent`]
/// stream shape.
pub(super) enum DeployEvent {
    Submitted,
    Evidence(DeployEvidence),
    Transition(DeployTransition),
}

/// What a piece of deploy evidence requires of the operation state to count
/// as fresh. This is the single source of the evidence→stage mapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EvidenceRequirement {
    Planning,
    Running,
    RunningStage(DeployRunningStage),
    Cleanup,
}

const fn evidence_requirement(evidence: &DeployEvidence) -> EvidenceRequirement {
    match evidence {
        DeployEvidence::ImageResolved { .. } | DeployEvidence::PlanCreated { .. } => {
            EvidenceRequirement::Planning
        }
        DeployEvidence::ImageAvailabilityVerified { .. } => {
            EvidenceRequirement::RunningStage(DeployRunningStage::EnsuringImages)
        }
        DeployEvidence::ContainerStarted { .. } => {
            EvidenceRequirement::RunningStage(DeployRunningStage::StartingContainers)
        }
        DeployEvidence::HealthCheckStarted => {
            EvidenceRequirement::RunningStage(DeployRunningStage::WaitingForHealth)
        }
        DeployEvidence::PhaseStarted { .. } => EvidenceRequirement::Running,
        DeployEvidence::PhaseFinished { .. } => EvidenceRequirement::Running,
        DeployEvidence::CleanupFinished { .. } => EvidenceRequirement::Cleanup,
    }
}

pub fn validate_fresh_deploy_evidence(
    current: &OperationStatus,
    evidence: &DeployEvidence,
) -> Result<(), StatusProjectionError> {
    let OperationStatus::Deploy { id, state, .. } = current else {
        return Err(kind_mismatch(current, OperationKind::Deploy));
    };
    let valid = match evidence_requirement(evidence) {
        EvidenceRequirement::Planning => matches!(state, DeployOperationState::Planning),
        EvidenceRequirement::Running => matches!(state, DeployOperationState::Running { .. }),
        EvidenceRequirement::RunningStage(stage) => {
            evidence_is_current_or_past_running_stage(state, stage)
        }
        EvidenceRequirement::Cleanup => cleanup_evidence_is_valid(state),
    };
    if valid {
        return Ok(());
    }

    Err(StatusProjectionError::InvalidTransition {
        operation_id: id.clone(),
        current: Box::new(ProjectionOperationState::Deploy(state.clone())),
        attempted: Box::new(ProjectionOperationState::Deploy(evidence_required_state(
            evidence,
        ))),
    })
}

fn evidence_is_current_or_past_running_stage(
    state: &DeployOperationState,
    evidence_stage: DeployRunningStage,
) -> bool {
    let DeployOperationState::Running { stage } = state else {
        return false;
    };

    stage_rank(*stage) >= stage_rank(evidence_stage)
}

fn cleanup_evidence_is_valid(state: &DeployOperationState) -> bool {
    matches!(state, DeployOperationState::Running { .. })
}

fn evidence_required_state(evidence: &DeployEvidence) -> DeployOperationState {
    match evidence_requirement(evidence) {
        EvidenceRequirement::Planning => DeployOperationState::Planning,
        EvidenceRequirement::Running => DeployOperationState::Running {
            stage: DeployRunningStage::StartingContainers,
        },
        EvidenceRequirement::RunningStage(stage) => DeployOperationState::Running { stage },
        EvidenceRequirement::Cleanup => DeployOperationState::Running {
            stage: DeployRunningStage::RemovingSupersededContainers,
        },
    }
}

pub fn project_deploy_transition(
    current: &OperationStatus,
    transition: DeployTransition,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    let OperationStatus::Deploy {
        id,
        namespace_id,
        service_id,
        origin,
        state: current_state,
        ..
    } = current
    else {
        return Err(kind_mismatch(current, OperationKind::Deploy));
    };

    project_state(
        id,
        namespace_id,
        service_id,
        origin,
        current_state,
        transition.state(),
        event_sequence,
    )
}

pub(super) fn project_state(
    id: &OperationId,
    namespace_id: &NamespaceId,
    service_id: &ServiceId,
    origin: &Option<crate::deploy::DeployOrigin>,
    current_state: &DeployOperationState,
    attempted: DeployOperationState,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    if transition_satisfied(current_state, &attempted) {
        return Ok(OperationProjection::AlreadySatisfied);
    }

    validate_transition(id, current_state, &attempted)?;

    Ok(OperationProjection::StatusChanged {
        status: Box::new(OperationStatus::Deploy {
            id: id.clone(),
            namespace_id: namespace_id.clone(),
            service_id: service_id.clone(),
            origin: origin.clone(),
            state: attempted,
            last_event_sequence: event_sequence,
        }),
    })
}

pub(super) fn project_event(
    id: &OperationId,
    namespace_id: &NamespaceId,
    service_id: &ServiceId,
    origin: &Option<crate::deploy::DeployOrigin>,
    state: &DeployOperationState,
    event: DeployEvent,
    event_sequence: EventSequence,
) -> Result<OperationProjection, StatusProjectionError> {
    match event {
        DeployEvent::Evidence(evidence) => {
            // Evidence records (advancing the status cursor without changing
            // state) once the operation has reached the phase that produces
            // it — including late arrivals after later stages or completion.
            // Evidence from a phase not yet reached is a stale duplicate and
            // is already satisfied.
            let records = match evidence_requirement(&evidence) {
                EvidenceRequirement::Planning => !matches!(state, DeployOperationState::Accepted),
                EvidenceRequirement::Running => matches!(
                    state,
                    DeployOperationState::Running { .. } | DeployOperationState::Completed { .. }
                ),
                EvidenceRequirement::RunningStage(required) => {
                    evidence_is_current_or_past_running_stage(state, required)
                        || matches!(state, DeployOperationState::Completed { .. })
                }
                EvidenceRequirement::Cleanup => {
                    cleanup_evidence_is_valid(state)
                        || matches!(state, DeployOperationState::Completed { .. })
                }
            };
            if !records {
                return Ok(OperationProjection::AlreadySatisfied);
            }

            Ok(OperationProjection::StatusChanged {
                status: Box::new(evidence_status(
                    id,
                    namespace_id,
                    service_id,
                    origin,
                    state,
                    event_sequence,
                )),
            })
        }
        DeployEvent::Transition(transition) => project_state(
            id,
            namespace_id,
            service_id,
            origin,
            state,
            transition.state(),
            event_sequence,
        ),
        DeployEvent::Submitted => Ok(OperationProjection::AlreadySatisfied),
    }
}

fn evidence_status(
    id: &OperationId,
    namespace_id: &NamespaceId,
    service_id: &ServiceId,
    origin: &Option<crate::deploy::DeployOrigin>,
    state: &DeployOperationState,
    event_sequence: EventSequence,
) -> OperationStatus {
    OperationStatus::Deploy {
        id: id.clone(),
        namespace_id: namespace_id.clone(),
        service_id: service_id.clone(),
        origin: origin.clone(),
        state: state.clone(),
        last_event_sequence: event_sequence,
    }
}

fn transition_satisfied(current: &DeployOperationState, attempted: &DeployOperationState) -> bool {
    match attempted {
        DeployOperationState::Accepted => matches!(current, DeployOperationState::Accepted),
        DeployOperationState::Planning => !matches!(current, DeployOperationState::Accepted),
        DeployOperationState::Running { stage: attempted } => match current {
            DeployOperationState::Running { stage: current } => {
                let current_rank = stage_rank(*current);
                let attempted_rank = stage_rank(*attempted);
                current_rank > attempted_rank
                    && !matches!(
                        (current, attempted),
                        (
                            DeployRunningStage::ServingTargetCommit,
                            DeployRunningStage::RouteCutover
                        )
                    )
                    || current_rank == attempted_rank && current == attempted
            }
            DeployOperationState::Accepted
            | DeployOperationState::Planning
            | DeployOperationState::Completed { .. }
            | DeployOperationState::Failed { .. }
            | DeployOperationState::Cancelled { .. }
            | DeployOperationState::Interrupted { .. } => false,
        },
        DeployOperationState::Completed { outcome: attempted } => {
            matches!(current, DeployOperationState::Completed { outcome } if outcome == attempted)
        }
        DeployOperationState::Failed { failure: attempted } => {
            matches!(current, DeployOperationState::Failed { failure } if failure == attempted)
        }
        DeployOperationState::Cancelled { reason: attempted } => {
            matches!(current, DeployOperationState::Cancelled { reason } if reason == attempted)
        }
        DeployOperationState::Interrupted {
            evidence: attempted,
        } => {
            matches!(current, DeployOperationState::Interrupted { evidence } if evidence == attempted)
        }
    }
}

fn validate_transition(
    operation_id: &OperationId,
    current: &DeployOperationState,
    attempted: &DeployOperationState,
) -> Result<(), StatusProjectionError> {
    if current.is_terminal() {
        return Err(StatusProjectionError::TerminalState {
            operation_id: operation_id.clone(),
            current: Box::new(ProjectionOperationState::Deploy(current.clone())),
            attempted: Box::new(ProjectionOperationState::Deploy(attempted.clone())),
        });
    }

    if transition_allowed(current, attempted) {
        return Ok(());
    }

    Err(StatusProjectionError::InvalidTransition {
        operation_id: operation_id.clone(),
        current: Box::new(ProjectionOperationState::Deploy(current.clone())),
        attempted: Box::new(ProjectionOperationState::Deploy(attempted.clone())),
    })
}

fn transition_allowed(current: &DeployOperationState, attempted: &DeployOperationState) -> bool {
    match (current, attempted) {
        (DeployOperationState::Accepted, DeployOperationState::Planning)
        | (DeployOperationState::Accepted, DeployOperationState::Cancelled { .. })
        | (DeployOperationState::Accepted, DeployOperationState::Failed { .. })
        | (DeployOperationState::Planning, DeployOperationState::Cancelled { .. })
        | (DeployOperationState::Planning, DeployOperationState::Failed { .. })
        | (DeployOperationState::Running { .. }, DeployOperationState::Cancelled { .. })
        | (DeployOperationState::Running { .. }, DeployOperationState::Failed { .. }) => true,
        (
            DeployOperationState::Running {
                stage: DeployRunningStage::ServingTargetCommit,
            },
            DeployOperationState::Completed { .. },
        )
        | (
            DeployOperationState::Running {
                stage: DeployRunningStage::RemovingSupersededContainers,
            },
            DeployOperationState::Completed { .. },
        ) => true,
        (
            DeployOperationState::Planning,
            DeployOperationState::Running {
                stage: DeployRunningStage::EnsuringImages,
            },
        )
        | (
            DeployOperationState::Planning,
            DeployOperationState::Running {
                stage: DeployRunningStage::EnsuringVolumes,
            },
        )
        | (
            DeployOperationState::Planning,
            DeployOperationState::Running {
                stage: DeployRunningStage::StartingContainers,
            },
        ) => true,
        (
            DeployOperationState::Running { stage: current },
            DeployOperationState::Running { stage: attempted },
        ) => stage_is_next(*current, *attempted),
        (_, DeployOperationState::Interrupted { .. })
        | (DeployOperationState::Accepted, _)
        | (DeployOperationState::Completed { .. }, _)
        | (DeployOperationState::Failed { .. }, _)
        | (DeployOperationState::Cancelled { .. }, _)
        | (DeployOperationState::Interrupted { .. }, _)
        | (DeployOperationState::Planning, _)
        | (DeployOperationState::Running { .. }, _) => false,
    }
}

fn stage_rank(stage: DeployRunningStage) -> u8 {
    match stage {
        DeployRunningStage::EnsuringImages => 0,
        DeployRunningStage::EnsuringVolumes => 1,
        DeployRunningStage::StartingContainers => 2,
        DeployRunningStage::WaitingForHealth => 3,
        DeployRunningStage::EnsuringCertificates => 4,
        DeployRunningStage::RouteCutover => 5,
        DeployRunningStage::ServingTargetCommit => 6,
        DeployRunningStage::RemovingSupersededContainers => 7,
    }
}

fn stage_is_next(current: DeployRunningStage, attempted: DeployRunningStage) -> bool {
    matches!(
        (current, attempted),
        (
            DeployRunningStage::EnsuringImages,
            DeployRunningStage::EnsuringVolumes
        ) | (
            DeployRunningStage::EnsuringImages,
            DeployRunningStage::StartingContainers
        ) | (
            DeployRunningStage::EnsuringVolumes,
            DeployRunningStage::StartingContainers
        ) | (
            DeployRunningStage::StartingContainers,
            DeployRunningStage::WaitingForHealth
        ) | (
            DeployRunningStage::WaitingForHealth,
            DeployRunningStage::EnsuringCertificates
        ) | (
            DeployRunningStage::EnsuringCertificates,
            DeployRunningStage::RouteCutover
        ) | (
            DeployRunningStage::RouteCutover,
            DeployRunningStage::ServingTargetCommit
        ) | (
            DeployRunningStage::ServingTargetCommit,
            DeployRunningStage::RouteCutover
        ) | (
            DeployRunningStage::ServingTargetCommit,
            DeployRunningStage::RemovingSupersededContainers
        ) | (
            DeployRunningStage::WaitingForHealth,
            DeployRunningStage::RouteCutover
        ) | (
            DeployRunningStage::WaitingForHealth,
            DeployRunningStage::ServingTargetCommit
        )
    )
}

#[cfg(test)]
mod volume_stage_tests {
    use super::*;

    #[test]
    fn ensuring_volumes_is_between_images_and_containers() {
        assert!(stage_is_next(
            DeployRunningStage::EnsuringImages,
            DeployRunningStage::EnsuringVolumes,
        ));
        assert!(stage_is_next(
            DeployRunningStage::EnsuringVolumes,
            DeployRunningStage::StartingContainers,
        ));
        assert!(!stage_is_next(
            DeployRunningStage::EnsuringVolumes,
            DeployRunningStage::WaitingForHealth,
        ));
    }

    #[test]
    fn image_stage_may_skip_volume_stage_for_an_empty_ensure_list() {
        assert!(stage_is_next(
            DeployRunningStage::EnsuringImages,
            DeployRunningStage::StartingContainers,
        ));
    }
}
