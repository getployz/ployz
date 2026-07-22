//! Durable operation events: the flat stream shape, its subjects and
//! message ids, and the classification back into per-kind events.

use serde::{Deserialize, Serialize};

use crate::build::{
    BuildAdapter, BuildExecutorEvidence, BuildPlatforms, BuildSourceEvidence, BuildTarget,
    VerifiedBuildSource,
};
use crate::certificate::AcmeHttp01Challenge;
use crate::deploy::{
    DeployCleanupContainer, DeployPlan, DeployReservationId, DeployVolumeHandoffApplied, VolumeName,
};
use crate::ids::{CertId, ContainerId, MachineId, NamespaceId, OperationId, ServiceId};
use crate::ingress::{ActiveCertificateMetadata, IngressConfiguration};
use crate::install::{InstallArtifactVersion, MachineJoinRuntimeNatsUrl};
use crate::machine::{InstallRolePolicy, MachineLifecycle};
use crate::machine::{
    IssuedJoinToken, JoinTokenRedeemedAt, MachineAddFailure, MachineCredentialProvisioningStep,
    MachineName,
};

use super::build::{
    BuildCleanupEvidence, BuildEvent, BuildEvidence, BuildLogChunk, BuildOperationFailure,
    BuildPlatformFailure, BuildTimeoutFailure, BuildToolchainEvidence, BuildTransition,
};
use super::cert::{CertEvent, CertTransition, CertificateProvisionWarning};
use super::core_replace::{CoreReplaceEvent, CoreReplaceFailure, CoreReplaceTransition};
use super::credential_grant::{
    CredentialGrantAction, CredentialGrantEvent, CredentialGrantFailure, CredentialGrantTransition,
};
use super::deploy::{DeployEvent, DeployEvidence, DeployTransition};
use super::ingress_configure::{IngressConfigureEvent, IngressConfigureTransition};
use super::machine_add::MachineAddEvent;
use super::machine_build_cache_prune::{
    MachineBuildCachePruneEvent, MachineBuildCachePruneTransition,
};
use super::machine_lifecycle::{MachineLifecycleEvent, MachineLifecycleTransition};
use super::machine_storage_prepare::{MachineStoragePrepareEvent, MachineStoragePrepareTransition};
use super::machine_update::{MachineUpdateEvent, MachineUpdateTransition};
use super::managed_dns_reconcile::{ManagedDnsReconcileEvent, ManagedDnsReconcileTransition};
use super::namespace_remove::{NamespaceRemoveEvent, NamespaceRemoveTransition};
use super::network_repair::{NetworkRepairEvent, NetworkRepairEvidence, NetworkRepairTransition};
use super::service_restart::{ServiceRestartEvent, ServiceRestartTransition};
use super::text::CancellationReason;
use super::volume_create::{VolumeCreateEvent, VolumeCreateTransition};
use super::volume_remove::{VolumeRemoveEvent, VolumeRemoveTransition};
use super::{
    BuildCachePruneEvidence, CertOperationFailure, CertRunningStage, DeployCleanupFailure,
    DeployCompletionOutcome, DeployOperationFailure, DeployPhaseOutcome, DeployRunningStage,
    DeployServiceResult, DeployVolumeHandoffRollbackContainerOutcome, MachineAddOperationState,
    MachineBuildCachePruneFailure, MachineLifecycleFailure, MachineStoragePrepareFailure,
    MachineSubstrateVersions, MachineUpdateFailure, NamespaceRemoveFailure,
    NamespaceRemoveRunningStage, NetworkRepairFailure, NetworkRepairRunningStage, OperationKind,
    RouteTarget, ServiceRestartFailure, ServiceRestartRunningStage, VolumeCreateFailure,
    VolumeCreateRequest, VolumeCreateRunningStage, VolumeRemoveFailure, VolumeRemoveRunningStage,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationSubject {
    Build,
    Deploy {
        service_id: ServiceId,
    },
    Cert {
        cert_id: CertId,
    },
    MachineAdd {
        machine_id: MachineId,
    },
    MachineBuildCachePrune {
        machine_id: MachineId,
    },
    MachineUpdate {
        machine_id: MachineId,
    },
    MachineStoragePrepare {
        machine_id: MachineId,
    },
    CoreReplace {
        machine_id: MachineId,
    },
    CredentialGrant,
    NetworkRepair,
    ServiceRestart {
        service_id: ServiceId,
    },
    ManagedDnsReconcile {
        subject: super::ManagedDnsReconcileSubject,
    },
    IngressConfigure,
    NamespaceRemove {
        namespace_id: NamespaceId,
    },
    VolumeCreate {
        namespace_id: NamespaceId,
        volume_name: VolumeName,
    },
    VolumeRemove {
        namespace_id: NamespaceId,
        volume_name: VolumeName,
    },
}

/// Local operation evidence event and plain NATS progress payload.
///
/// Changing this shape intentionally breaks operation replay/history unless
/// paired with evidence cleanup or migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "event", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationEvent {
    BuildSubmitted {
        operation_id: OperationId,
        target: BuildTarget,
        source: BuildSourceEvidence,
        adapter: BuildAdapter,
        platforms: BuildPlatforms,
    },
    BuildPlacementStarted {
        operation_id: OperationId,
    },
    BuildPlatformPlaced {
        operation_id: OperationId,
        platform: crate::image::OciPlatform,
        #[serde(flatten)]
        #[cfg_attr(feature = "typescript", ts(flatten))]
        executor: BuildExecutorEvidence,
    },
    BuildSourceVerified {
        operation_id: OperationId,
        platform: crate::image::OciPlatform,
        #[serde(flatten)]
        #[cfg_attr(feature = "typescript", ts(flatten))]
        executor: BuildExecutorEvidence,
        source: VerifiedBuildSource,
    },
    BuildPlatformToolchainVerified {
        operation_id: OperationId,
        platform: crate::image::OciPlatform,
        #[serde(flatten)]
        #[cfg_attr(feature = "typescript", ts(flatten))]
        executor: BuildExecutorEvidence,
        toolchain: BuildToolchainEvidence,
    },
    BuildRunning {
        operation_id: OperationId,
    },
    BuildPlatformLog {
        operation_id: OperationId,
        platform: crate::image::OciPlatform,
        #[serde(flatten)]
        #[cfg_attr(feature = "typescript", ts(flatten))]
        executor: BuildExecutorEvidence,
        chunk: BuildLogChunk,
    },
    BuildPlatformLogTruncated {
        operation_id: OperationId,
        platform: crate::image::OciPlatform,
        #[serde(flatten)]
        #[cfg_attr(feature = "typescript", ts(flatten))]
        executor: BuildExecutorEvidence,
        omitted_bytes: u64,
    },
    BuildPlatformLogGap {
        operation_id: OperationId,
        platform: crate::image::OciPlatform,
        #[serde(flatten)]
        #[cfg_attr(feature = "typescript", ts(flatten))]
        executor: BuildExecutorEvidence,
        expected_sequence: u64,
        final_sequence: u64,
    },
    BuildPlatformCompleted {
        operation_id: OperationId,
        platform: crate::image::OciPlatform,
        #[serde(flatten)]
        #[cfg_attr(feature = "typescript", ts(flatten))]
        executor: BuildExecutorEvidence,
        image: crate::deploy::PlatformImage,
    },
    BuildPlatformFailed {
        operation_id: OperationId,
        platform: crate::image::OciPlatform,
        #[serde(flatten)]
        #[cfg_attr(feature = "typescript", ts(flatten))]
        executor: BuildExecutorEvidence,
        failure: BuildPlatformFailure,
    },
    BuildCompleted {
        operation_id: OperationId,
        receipt: crate::deploy::PushedImageReceipt,
    },
    BuildFailed {
        operation_id: OperationId,
        failure: BuildOperationFailure,
    },
    BuildCancelled {
        operation_id: OperationId,
        reason: CancellationReason,
        cleanup: BuildCleanupEvidence,
    },
    BuildTimedOut {
        operation_id: OperationId,
        failure: BuildTimeoutFailure,
        cleanup: BuildCleanupEvidence,
    },
    DeploySubmitted {
        operation_id: OperationId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reservation_id: Option<DeployReservationId>,
        target: crate::deploy::DeployRequestEvidence,
    },
    DeployPlanningStarted {
        operation_id: OperationId,
    },
    DeployImageResolved {
        operation_id: OperationId,
        service_id: ServiceId,
        machine_id: MachineId,
        requested: crate::deploy::ImageReference,
        resolved: crate::deploy::ImageReference,
        credential_supplied: bool,
    },
    DeployPlanCreated {
        operation_id: OperationId,
        plan: DeployPlan,
    },
    DeployRunning {
        operation_id: OperationId,
        stage: DeployRunningStage,
    },
    DeployImageAvailabilityVerified {
        operation_id: OperationId,
        service_id: ServiceId,
        machine_id: MachineId,
        image: crate::deploy::ImageReference,
        platform: crate::image::OciPlatform,
    },
    DeployContainerStarted {
        operation_id: OperationId,
        machine_id: MachineId,
        container_id: ContainerId,
    },
    DeployVolumeHandoffApplied {
        operation_id: OperationId,
        service_id: ServiceId,
        handoff: DeployVolumeHandoffApplied,
    },
    DeployVolumeHandoffRollbackFinished {
        operation_id: OperationId,
        service_id: ServiceId,
        machine_id: MachineId,
        outcomes: Vec<DeployVolumeHandoffRollbackContainerOutcome>,
    },
    DeployHealthCheckStarted {
        operation_id: OperationId,
    },
    DeployPhaseStarted {
        operation_id: OperationId,
        phase: super::DeployPhaseNumber,
        service_ids: Vec<ServiceId>,
    },
    DeployPhaseFinished {
        operation_id: OperationId,
        phase: super::DeployPhaseNumber,
        outcome: DeployPhaseOutcome,
        services: Vec<DeployServiceResult>,
    },
    DeployCleanupFinished {
        operation_id: OperationId,
        removed: Vec<DeployCleanupContainer>,
        failed: Vec<DeployCleanupFailure>,
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        images: Vec<super::deploy::DeployImageCleanup>,
    },
    DeployCompleted {
        operation_id: OperationId,
        outcome: DeployCompletionOutcome,
    },
    DeployFailed {
        operation_id: OperationId,
        failure: DeployOperationFailure,
    },
    CertProvisionSubmitted {
        operation_id: OperationId,
        cert_id: CertId,
    },
    CertChallengePublished {
        operation_id: OperationId,
        cert_id: CertId,
        challenge: AcmeHttp01Challenge,
    },
    CertValidationStarted {
        operation_id: OperationId,
        cert_id: CertId,
    },
    CertWarning {
        operation_id: OperationId,
        cert_id: CertId,
        warning: CertificateProvisionWarning,
    },
    CertCompleted {
        operation_id: OperationId,
        certificate: ActiveCertificateMetadata,
    },
    CertFailed {
        operation_id: OperationId,
        failure: CertOperationFailure,
    },
    MachineAddSubmitted {
        operation_id: OperationId,
        machine_id: MachineId,
        name: MachineName,
        roles: InstallRolePolicy,
        #[serde(default = "crate::install::HostPortAssurance::keeper")]
        host_port_assurance: crate::install::HostPortAssurance,
        join_token: IssuedJoinToken,
    },
    MachineAddJoined {
        operation_id: OperationId,
        machine_id: MachineId,
        joined_at: JoinTokenRedeemedAt,
    },
    MachineAddCredentialProvisioned {
        operation_id: OperationId,
        machine_id: MachineId,
        step: MachineCredentialProvisioningStep,
    },
    MachineAddCompleted {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    MachineAddFailed {
        operation_id: OperationId,
        machine_id: MachineId,
        failure: MachineAddFailure,
    },
    MachineUpdateSubmitted {
        operation_id: OperationId,
        machine_id: MachineId,
        target_version: InstallArtifactVersion,
    },
    MachineUpdateRunning {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    MachineUpdateCompleted {
        operation_id: OperationId,
        machine_id: MachineId,
        reported: MachineSubstrateVersions,
    },
    MachineUpdateFailed {
        operation_id: OperationId,
        machine_id: MachineId,
        failure: MachineUpdateFailure,
    },
    MachineStoragePrepareSubmitted {
        operation_id: OperationId,
        machine_id: MachineId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        requested_pool: Option<crate::deploy::ZfsPoolName>,
    },
    MachineStoragePreparePreparing {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    MachineStoragePrepareCompleted {
        operation_id: OperationId,
        machine_id: MachineId,
        pool: crate::deploy::ZfsPoolName,
    },
    MachineStoragePrepareFailed {
        operation_id: OperationId,
        machine_id: MachineId,
        failure: MachineStoragePrepareFailure,
    },
    MachineBuildCachePruneSubmitted {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    MachineBuildCachePrunePruning {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    MachineBuildCachePruneCompleted {
        operation_id: OperationId,
        machine_id: MachineId,
        evidence: BuildCachePruneEvidence,
    },
    MachineBuildCachePruneFailed {
        operation_id: OperationId,
        machine_id: MachineId,
        failure: MachineBuildCachePruneFailure,
    },
    MachineLifecycleSubmitted {
        operation_id: OperationId,
        machine_id: MachineId,
        target: MachineLifecycle,
    },
    MachineLifecycleCompleted {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    MachineLifecycleFailed {
        operation_id: OperationId,
        machine_id: MachineId,
        failure: MachineLifecycleFailure,
    },
    CoreReplaceSubmitted {
        operation_id: OperationId,
        machine_id: MachineId,
        successor_nats_url: MachineJoinRuntimeNatsUrl,
    },
    CoreReplaceCompleted {
        operation_id: OperationId,
        machine_id: MachineId,
    },
    CoreReplaceFailed {
        operation_id: OperationId,
        machine_id: MachineId,
        failure: CoreReplaceFailure,
    },
    CredentialGrantSubmitted {
        operation_id: OperationId,
        action: CredentialGrantAction,
    },
    CredentialGrantCompleted {
        operation_id: OperationId,
    },
    CredentialGrantFailed {
        operation_id: OperationId,
        failure: CredentialGrantFailure,
    },
    NetworkRepairSubmitted {
        operation_id: OperationId,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target_machine_id: Option<MachineId>,
    },
    NetworkRepairRunning {
        operation_id: OperationId,
        stage: NetworkRepairRunningStage,
    },
    NetworkRepairDataplaneConverged {
        operation_id: OperationId,
        revision: crate::network::DataplaneProjectionRevision,
        machine_ids: Vec<MachineId>,
    },
    NetworkRepairMachineFactsRefreshed {
        operation_id: OperationId,
        refreshes: Vec<crate::machine::runtime::MachineFactsRefreshConfirmation>,
    },
    NetworkRepairDnsRefreshConfirmed {
        operation_id: OperationId,
        machine_ids: Vec<MachineId>,
    },
    NetworkRepairCompleted {
        operation_id: OperationId,
    },
    NetworkRepairFailed {
        operation_id: OperationId,
        failure: NetworkRepairFailure,
    },
    ServiceRestartSubmitted {
        operation_id: OperationId,
        namespace_id: NamespaceId,
        service_id: ServiceId,
    },
    ServiceRestartRunning {
        operation_id: OperationId,
        stage: ServiceRestartRunningStage,
    },
    ServiceRestartContainerRestarted {
        operation_id: OperationId,
        machine_id: MachineId,
        container_id: ContainerId,
    },
    ServiceRestartCompleted {
        operation_id: OperationId,
    },
    ServiceRestartFailed {
        operation_id: OperationId,
        failure: ServiceRestartFailure,
    },
    ManagedDnsReconcileSubmitted {
        operation_id: OperationId,
        subject: super::ManagedDnsReconcileSubject,
    },
    ManagedDnsReconcileCompleted {
        operation_id: OperationId,
        subject: super::ManagedDnsReconcileSubject,
    },
    ManagedDnsReconcileFailed {
        operation_id: OperationId,
        subject: super::ManagedDnsReconcileSubject,
        failure: super::ManagedDnsReconcileFailure,
    },
    IngressConfigureSubmitted {
        operation_id: OperationId,
        configuration: IngressConfiguration,
    },
    IngressConfigureCompleted {
        operation_id: OperationId,
    },
    IngressConfigureFailed {
        operation_id: OperationId,
        failure: super::IngressConfigureFailure,
    },
    NamespaceRemoveSubmitted {
        operation_id: OperationId,
        namespace_id: NamespaceId,
    },
    NamespaceRemoveRunning {
        operation_id: OperationId,
        stage: NamespaceRemoveRunningStage,
    },
    NamespaceRemoveRouteBindingRemoved {
        operation_id: OperationId,
        target: RouteTarget,
    },
    NamespaceRemoveContainerRemoved {
        operation_id: OperationId,
        machine_id: MachineId,
        container_id: ContainerId,
    },
    NamespaceRemoveCompleted {
        operation_id: OperationId,
    },
    NamespaceRemoveFailed {
        operation_id: OperationId,
        failure: NamespaceRemoveFailure,
    },
    VolumeCreateSubmitted {
        request: VolumeCreateRequest,
    },
    VolumeCreatePlanningStarted {
        operation_id: OperationId,
    },
    VolumeCreateRunning {
        operation_id: OperationId,
        stage: VolumeCreateRunningStage,
    },
    VolumeCreateCompleted {
        operation_id: OperationId,
    },
    VolumeCreateFailed {
        operation_id: OperationId,
        failure: VolumeCreateFailure,
    },
    VolumeRemoveSubmitted {
        operation_id: OperationId,
        namespace_id: NamespaceId,
        volume_name: VolumeName,
    },
    VolumeRemoveRunning {
        operation_id: OperationId,
        stage: VolumeRemoveRunningStage,
    },
    VolumeRemoveCompleted {
        operation_id: OperationId,
    },
    VolumeRemoveFailed {
        operation_id: OperationId,
        failure: VolumeRemoveFailure,
    },
    OperationInterrupted {
        operation_id: OperationId,
        evidence: super::OperationInterruptionEvidence,
    },
    Cancelled {
        operation_id: OperationId,
        kind: OperationKind,
        reason: CancellationReason,
    },
}

impl OperationEvent {
    #[must_use]
    pub fn operation_id(&self) -> &OperationId {
        match self {
            Self::BuildSubmitted { operation_id, .. }
            | Self::BuildPlacementStarted { operation_id }
            | Self::BuildPlatformPlaced { operation_id, .. }
            | Self::BuildSourceVerified { operation_id, .. }
            | Self::BuildPlatformToolchainVerified { operation_id, .. }
            | Self::BuildRunning { operation_id }
            | Self::BuildPlatformLog { operation_id, .. }
            | Self::BuildPlatformLogTruncated { operation_id, .. }
            | Self::BuildPlatformLogGap { operation_id, .. }
            | Self::BuildPlatformCompleted { operation_id, .. }
            | Self::BuildPlatformFailed { operation_id, .. }
            | Self::BuildCompleted { operation_id, .. }
            | Self::BuildFailed { operation_id, .. }
            | Self::BuildCancelled { operation_id, .. }
            | Self::BuildTimedOut { operation_id, .. }
            | Self::DeploySubmitted { operation_id, .. }
            | Self::DeployPlanningStarted { operation_id }
            | Self::DeployImageResolved { operation_id, .. }
            | Self::DeployPlanCreated { operation_id, .. }
            | Self::DeployRunning { operation_id, .. }
            | Self::DeployImageAvailabilityVerified { operation_id, .. }
            | Self::DeployContainerStarted { operation_id, .. }
            | Self::DeployVolumeHandoffApplied { operation_id, .. }
            | Self::DeployVolumeHandoffRollbackFinished { operation_id, .. }
            | Self::DeployHealthCheckStarted { operation_id }
            | Self::DeployPhaseStarted { operation_id, .. }
            | Self::DeployPhaseFinished { operation_id, .. }
            | Self::DeployCleanupFinished { operation_id, .. }
            | Self::DeployCompleted { operation_id, .. }
            | Self::DeployFailed { operation_id, .. }
            | Self::CertProvisionSubmitted { operation_id, .. }
            | Self::CertChallengePublished { operation_id, .. }
            | Self::CertValidationStarted { operation_id, .. }
            | Self::CertWarning { operation_id, .. }
            | Self::CertCompleted { operation_id, .. }
            | Self::CertFailed { operation_id, .. }
            | Self::MachineAddSubmitted { operation_id, .. }
            | Self::MachineAddJoined { operation_id, .. }
            | Self::MachineAddCredentialProvisioned { operation_id, .. }
            | Self::MachineAddCompleted { operation_id, .. }
            | Self::MachineAddFailed { operation_id, .. }
            | Self::MachineUpdateSubmitted { operation_id, .. }
            | Self::MachineUpdateRunning { operation_id, .. }
            | Self::MachineUpdateCompleted { operation_id, .. }
            | Self::MachineUpdateFailed { operation_id, .. }
            | Self::MachineStoragePrepareSubmitted { operation_id, .. }
            | Self::MachineStoragePreparePreparing { operation_id, .. }
            | Self::MachineStoragePrepareCompleted { operation_id, .. }
            | Self::MachineStoragePrepareFailed { operation_id, .. }
            | Self::MachineBuildCachePruneSubmitted { operation_id, .. }
            | Self::MachineBuildCachePrunePruning { operation_id, .. }
            | Self::MachineBuildCachePruneCompleted { operation_id, .. }
            | Self::MachineBuildCachePruneFailed { operation_id, .. }
            | Self::MachineLifecycleSubmitted { operation_id, .. }
            | Self::MachineLifecycleCompleted { operation_id, .. }
            | Self::MachineLifecycleFailed { operation_id, .. }
            | Self::CoreReplaceSubmitted { operation_id, .. }
            | Self::CoreReplaceCompleted { operation_id, .. }
            | Self::CoreReplaceFailed { operation_id, .. }
            | Self::CredentialGrantSubmitted { operation_id, .. }
            | Self::CredentialGrantCompleted { operation_id }
            | Self::CredentialGrantFailed { operation_id, .. }
            | Self::NetworkRepairSubmitted { operation_id, .. }
            | Self::NetworkRepairRunning { operation_id, .. }
            | Self::NetworkRepairDataplaneConverged { operation_id, .. }
            | Self::NetworkRepairMachineFactsRefreshed { operation_id, .. }
            | Self::NetworkRepairDnsRefreshConfirmed { operation_id, .. }
            | Self::NetworkRepairCompleted { operation_id }
            | Self::NetworkRepairFailed { operation_id, .. }
            | Self::ServiceRestartSubmitted { operation_id, .. }
            | Self::ServiceRestartRunning { operation_id, .. }
            | Self::ServiceRestartContainerRestarted { operation_id, .. }
            | Self::ServiceRestartCompleted { operation_id }
            | Self::ServiceRestartFailed { operation_id, .. }
            | Self::ManagedDnsReconcileSubmitted { operation_id, .. }
            | Self::ManagedDnsReconcileCompleted { operation_id, .. }
            | Self::ManagedDnsReconcileFailed { operation_id, .. }
            | Self::IngressConfigureSubmitted { operation_id, .. }
            | Self::IngressConfigureCompleted { operation_id }
            | Self::IngressConfigureFailed { operation_id, .. }
            | Self::NamespaceRemoveSubmitted { operation_id, .. }
            | Self::NamespaceRemoveRunning { operation_id, .. }
            | Self::NamespaceRemoveRouteBindingRemoved { operation_id, .. }
            | Self::NamespaceRemoveContainerRemoved { operation_id, .. }
            | Self::NamespaceRemoveCompleted { operation_id }
            | Self::NamespaceRemoveFailed { operation_id, .. }
            | Self::VolumeCreatePlanningStarted { operation_id }
            | Self::VolumeCreateRunning { operation_id, .. }
            | Self::VolumeCreateCompleted { operation_id }
            | Self::VolumeCreateFailed { operation_id, .. }
            | Self::VolumeRemoveSubmitted { operation_id, .. }
            | Self::VolumeRemoveRunning { operation_id, .. }
            | Self::VolumeRemoveCompleted { operation_id }
            | Self::VolumeRemoveFailed { operation_id, .. }
            | Self::OperationInterrupted { operation_id, .. }
            | Self::Cancelled { operation_id, .. } => operation_id,
            Self::VolumeCreateSubmitted { request } => &request.operation_id,
        }
    }

    /// The key of operation evidence recorded once per operation
    /// phase. `None` for multi-instance evidence (per-container starts) and
    /// every non-evidence event. The store keys idempotent singleton evidence
    /// on this.
    #[must_use]
    pub fn singleton_evidence_key(&self) -> Option<&'static str> {
        match self {
            Self::DeployPlanCreated { .. } => Some("deploy.plan.created"),
            Self::NetworkRepairDataplaneConverged { .. } => {
                Some("network.repair.dataplane.converged")
            }
            Self::NetworkRepairMachineFactsRefreshed { .. } => {
                Some("network.repair.machine_facts.refreshed")
            }
            Self::NetworkRepairDnsRefreshConfirmed { .. } => {
                Some("network.repair.dns_refresh.confirmed")
            }
            Self::DeployHealthCheckStarted { .. } => Some("deploy.health_check.started"),
            Self::DeployCleanupFinished { .. } => Some("deploy.cleanup.finished"),
            Self::BuildSubmitted { .. }
            | Self::BuildPlacementStarted { .. }
            | Self::BuildPlatformPlaced { .. }
            | Self::BuildSourceVerified { .. }
            | Self::BuildPlatformToolchainVerified { .. }
            | Self::BuildRunning { .. }
            | Self::BuildPlatformLog { .. }
            | Self::BuildPlatformLogTruncated { .. }
            | Self::BuildPlatformLogGap { .. }
            | Self::BuildPlatformCompleted { .. }
            | Self::BuildPlatformFailed { .. }
            | Self::BuildCompleted { .. }
            | Self::BuildFailed { .. }
            | Self::BuildCancelled { .. }
            | Self::BuildTimedOut { .. }
            | Self::DeploySubmitted { .. }
            | Self::DeployPlanningStarted { .. }
            | Self::DeployImageResolved { .. }
            | Self::DeployImageAvailabilityVerified { .. }
            | Self::DeployContainerStarted { .. }
            | Self::DeployVolumeHandoffApplied { .. }
            | Self::DeployVolumeHandoffRollbackFinished { .. }
            | Self::DeployPhaseStarted { .. }
            | Self::DeployPhaseFinished { .. }
            | Self::DeployRunning { .. }
            | Self::DeployCompleted { .. }
            | Self::DeployFailed { .. }
            | Self::CertProvisionSubmitted { .. }
            | Self::CertChallengePublished { .. }
            | Self::CertValidationStarted { .. }
            | Self::CertWarning { .. }
            | Self::CertCompleted { .. }
            | Self::CertFailed { .. }
            | Self::MachineAddSubmitted { .. }
            | Self::MachineAddJoined { .. }
            | Self::MachineAddCredentialProvisioned { .. }
            | Self::MachineAddCompleted { .. }
            | Self::MachineAddFailed { .. }
            | Self::MachineUpdateSubmitted { .. }
            | Self::MachineUpdateRunning { .. }
            | Self::MachineUpdateCompleted { .. }
            | Self::MachineUpdateFailed { .. }
            | Self::MachineStoragePrepareSubmitted { .. }
            | Self::MachineStoragePreparePreparing { .. }
            | Self::MachineStoragePrepareCompleted { .. }
            | Self::MachineStoragePrepareFailed { .. }
            | Self::MachineBuildCachePruneSubmitted { .. }
            | Self::MachineBuildCachePrunePruning { .. }
            | Self::MachineBuildCachePruneCompleted { .. }
            | Self::MachineBuildCachePruneFailed { .. }
            | Self::MachineLifecycleSubmitted { .. }
            | Self::MachineLifecycleCompleted { .. }
            | Self::MachineLifecycleFailed { .. }
            | Self::CoreReplaceSubmitted { .. }
            | Self::CoreReplaceCompleted { .. }
            | Self::CoreReplaceFailed { .. }
            | Self::CredentialGrantSubmitted { .. }
            | Self::CredentialGrantCompleted { .. }
            | Self::CredentialGrantFailed { .. }
            | Self::NetworkRepairSubmitted { .. }
            | Self::NetworkRepairRunning { .. }
            | Self::NetworkRepairCompleted { .. }
            | Self::NetworkRepairFailed { .. }
            | Self::ServiceRestartSubmitted { .. }
            | Self::ServiceRestartRunning { .. }
            | Self::ServiceRestartContainerRestarted { .. }
            | Self::ServiceRestartCompleted { .. }
            | Self::ServiceRestartFailed { .. }
            | Self::ManagedDnsReconcileSubmitted { .. }
            | Self::ManagedDnsReconcileCompleted { .. }
            | Self::ManagedDnsReconcileFailed { .. }
            | Self::IngressConfigureSubmitted { .. }
            | Self::IngressConfigureCompleted { .. }
            | Self::IngressConfigureFailed { .. }
            | Self::NamespaceRemoveSubmitted { .. }
            | Self::NamespaceRemoveRunning { .. }
            | Self::NamespaceRemoveRouteBindingRemoved { .. }
            | Self::NamespaceRemoveContainerRemoved { .. }
            | Self::NamespaceRemoveCompleted { .. }
            | Self::NamespaceRemoveFailed { .. }
            | Self::VolumeCreateSubmitted { .. }
            | Self::VolumeCreatePlanningStarted { .. }
            | Self::VolumeCreateRunning { .. }
            | Self::VolumeCreateCompleted { .. }
            | Self::VolumeCreateFailed { .. }
            | Self::VolumeRemoveSubmitted { .. }
            | Self::VolumeRemoveRunning { .. }
            | Self::VolumeRemoveCompleted { .. }
            | Self::VolumeRemoveFailed { .. }
            | Self::OperationInterrupted { .. }
            | Self::Cancelled { .. } => None,
        }
    }

    /// The deploy evidence this event carries, if any. `None` for non-evidence
    /// deploy transitions and every other operation kind.
    #[must_use]
    pub fn deploy_evidence(&self) -> Option<DeployEvidence> {
        match self {
            Self::DeployImageResolved {
                service_id,
                machine_id,
                requested,
                resolved,
                credential_supplied,
                ..
            } => Some(DeployEvidence::ImageResolved {
                service_id: service_id.clone(),
                machine_id: machine_id.clone(),
                requested: requested.clone(),
                resolved: resolved.clone(),
                credential_supplied: *credential_supplied,
            }),
            Self::DeployPlanCreated { plan, .. } => {
                Some(DeployEvidence::PlanCreated { plan: plan.clone() })
            }
            Self::DeployImageAvailabilityVerified {
                service_id,
                machine_id,
                image,
                platform,
                ..
            } => Some(DeployEvidence::ImageAvailabilityVerified {
                service_id: service_id.clone(),
                machine_id: machine_id.clone(),
                image: image.clone(),
                platform: platform.clone(),
            }),
            Self::DeployContainerStarted {
                machine_id,
                container_id,
                ..
            } => Some(DeployEvidence::ContainerStarted {
                machine_id: machine_id.clone(),
                container_id: container_id.clone(),
            }),
            Self::DeployVolumeHandoffApplied {
                service_id,
                handoff,
                ..
            } => Some(DeployEvidence::VolumeHandoffApplied {
                service_id: service_id.clone(),
                handoff: handoff.clone(),
            }),
            Self::DeployVolumeHandoffRollbackFinished {
                service_id,
                machine_id,
                outcomes,
                ..
            } => Some(DeployEvidence::VolumeHandoffRollbackFinished {
                service_id: service_id.clone(),
                machine_id: machine_id.clone(),
                outcomes: outcomes.clone(),
            }),
            Self::DeployHealthCheckStarted { .. } => Some(DeployEvidence::HealthCheckStarted),
            Self::DeployPhaseStarted {
                phase, service_ids, ..
            } => Some(DeployEvidence::PhaseStarted {
                phase: *phase,
                service_ids: service_ids.clone(),
            }),
            Self::DeployPhaseFinished {
                phase,
                outcome,
                services,
                ..
            } => Some(DeployEvidence::PhaseFinished {
                phase: *phase,
                outcome: *outcome,
                services: services.clone(),
            }),
            Self::DeployCleanupFinished {
                removed,
                failed,
                images,
                ..
            } => Some(DeployEvidence::CleanupFinished {
                removed: removed.clone(),
                failed: failed.clone(),
                images: images.clone(),
            }),
            Self::BuildSubmitted { .. }
            | Self::BuildPlacementStarted { .. }
            | Self::BuildPlatformPlaced { .. }
            | Self::BuildSourceVerified { .. }
            | Self::BuildPlatformToolchainVerified { .. }
            | Self::BuildRunning { .. }
            | Self::BuildPlatformLog { .. }
            | Self::BuildPlatformLogTruncated { .. }
            | Self::BuildPlatformLogGap { .. }
            | Self::BuildPlatformCompleted { .. }
            | Self::BuildPlatformFailed { .. }
            | Self::BuildCompleted { .. }
            | Self::BuildFailed { .. }
            | Self::BuildCancelled { .. }
            | Self::BuildTimedOut { .. }
            | Self::DeploySubmitted { .. }
            | Self::DeployPlanningStarted { .. }
            | Self::DeployRunning { .. }
            | Self::DeployCompleted { .. }
            | Self::DeployFailed { .. }
            | Self::CertProvisionSubmitted { .. }
            | Self::CertChallengePublished { .. }
            | Self::CertValidationStarted { .. }
            | Self::CertWarning { .. }
            | Self::CertCompleted { .. }
            | Self::CertFailed { .. }
            | Self::MachineAddSubmitted { .. }
            | Self::MachineAddJoined { .. }
            | Self::MachineAddCredentialProvisioned { .. }
            | Self::MachineAddCompleted { .. }
            | Self::MachineAddFailed { .. }
            | Self::MachineUpdateSubmitted { .. }
            | Self::MachineUpdateRunning { .. }
            | Self::MachineUpdateCompleted { .. }
            | Self::MachineUpdateFailed { .. }
            | Self::MachineStoragePrepareSubmitted { .. }
            | Self::MachineStoragePreparePreparing { .. }
            | Self::MachineStoragePrepareCompleted { .. }
            | Self::MachineStoragePrepareFailed { .. }
            | Self::MachineBuildCachePruneSubmitted { .. }
            | Self::MachineBuildCachePrunePruning { .. }
            | Self::MachineBuildCachePruneCompleted { .. }
            | Self::MachineBuildCachePruneFailed { .. }
            | Self::MachineLifecycleSubmitted { .. }
            | Self::MachineLifecycleCompleted { .. }
            | Self::MachineLifecycleFailed { .. }
            | Self::CoreReplaceSubmitted { .. }
            | Self::CoreReplaceCompleted { .. }
            | Self::CoreReplaceFailed { .. }
            | Self::CredentialGrantSubmitted { .. }
            | Self::CredentialGrantCompleted { .. }
            | Self::CredentialGrantFailed { .. }
            | Self::NetworkRepairSubmitted { .. }
            | Self::NetworkRepairRunning { .. }
            | Self::NetworkRepairDataplaneConverged { .. }
            | Self::NetworkRepairMachineFactsRefreshed { .. }
            | Self::NetworkRepairDnsRefreshConfirmed { .. }
            | Self::NetworkRepairCompleted { .. }
            | Self::NetworkRepairFailed { .. }
            | Self::ServiceRestartSubmitted { .. }
            | Self::ServiceRestartRunning { .. }
            | Self::ServiceRestartContainerRestarted { .. }
            | Self::ServiceRestartCompleted { .. }
            | Self::ServiceRestartFailed { .. }
            | Self::ManagedDnsReconcileSubmitted { .. }
            | Self::ManagedDnsReconcileCompleted { .. }
            | Self::ManagedDnsReconcileFailed { .. }
            | Self::IngressConfigureSubmitted { .. }
            | Self::IngressConfigureCompleted { .. }
            | Self::IngressConfigureFailed { .. }
            | Self::NamespaceRemoveSubmitted { .. }
            | Self::NamespaceRemoveRunning { .. }
            | Self::NamespaceRemoveRouteBindingRemoved { .. }
            | Self::NamespaceRemoveContainerRemoved { .. }
            | Self::NamespaceRemoveCompleted { .. }
            | Self::NamespaceRemoveFailed { .. }
            | Self::VolumeCreateSubmitted { .. }
            | Self::VolumeCreatePlanningStarted { .. }
            | Self::VolumeCreateRunning { .. }
            | Self::VolumeCreateCompleted { .. }
            | Self::VolumeCreateFailed { .. }
            | Self::VolumeRemoveSubmitted { .. }
            | Self::VolumeRemoveRunning { .. }
            | Self::VolumeRemoveCompleted { .. }
            | Self::VolumeRemoveFailed { .. }
            | Self::OperationInterrupted { .. }
            | Self::Cancelled { .. } => None,
        }
    }
}

/// The subject an event claims to be about, checked against the status
/// record during projection so a misrouted event surfaces as typed
/// evidence instead of silently mutating the wrong operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationSubjectRef {
    Build,
    Cert(CertId),
    MachineAdd(MachineId),
    MachineUpdate(MachineId),
    MachineStoragePrepare(MachineId),
    MachineBuildCachePrune(MachineId),
    MachineLifecycle(MachineId),
    CoreReplace(MachineId),
    CredentialGrant,
    ManagedDnsReconcile(super::ManagedDnsReconcileSubject),
    IngressConfigure,
}

pub(super) enum ClassifiedOperationEvent {
    Build {
        operation_id: OperationId,
        event: BuildEvent,
    },
    Deploy {
        operation_id: OperationId,
        event: DeployEvent,
    },
    Cert {
        operation_id: OperationId,
        event: CertEvent,
    },
    MachineAdd {
        operation_id: OperationId,
        event: MachineAddEvent,
    },
    MachineUpdate {
        operation_id: OperationId,
        event: MachineUpdateEvent,
    },
    MachineStoragePrepare {
        operation_id: OperationId,
        event: MachineStoragePrepareEvent,
    },
    MachineBuildCachePrune {
        operation_id: OperationId,
        event: MachineBuildCachePruneEvent,
    },
    MachineLifecycle {
        operation_id: OperationId,
        event: MachineLifecycleEvent,
    },
    CoreReplace {
        operation_id: OperationId,
        event: CoreReplaceEvent,
    },
    CredentialGrant {
        operation_id: OperationId,
        event: CredentialGrantEvent,
    },
    NetworkRepair {
        operation_id: OperationId,
        event: NetworkRepairEvent,
    },
    ServiceRestart {
        operation_id: OperationId,
        event: ServiceRestartEvent,
    },
    ManagedDnsReconcile {
        operation_id: OperationId,
        event: ManagedDnsReconcileEvent,
    },
    IngressConfigure {
        operation_id: OperationId,
        event: IngressConfigureEvent,
    },
    NamespaceRemove {
        operation_id: OperationId,
        event: NamespaceRemoveEvent,
    },
    VolumeCreate {
        operation_id: OperationId,
        event: VolumeCreateEvent,
    },
    VolumeRemove {
        operation_id: OperationId,
        event: VolumeRemoveEvent,
    },
    OperationInterrupted {
        operation_id: OperationId,
        evidence: super::OperationInterruptionEvidence,
    },
}

impl ClassifiedOperationEvent {
    pub(super) fn operation_id(&self) -> &OperationId {
        match self {
            Self::Build { operation_id, .. }
            | Self::Deploy { operation_id, .. }
            | Self::Cert { operation_id, .. }
            | Self::MachineAdd { operation_id, .. }
            | Self::MachineUpdate { operation_id, .. }
            | Self::MachineStoragePrepare { operation_id, .. }
            | Self::MachineBuildCachePrune { operation_id, .. }
            | Self::MachineLifecycle { operation_id, .. }
            | Self::CoreReplace { operation_id, .. }
            | Self::CredentialGrant { operation_id, .. }
            | Self::NetworkRepair { operation_id, .. }
            | Self::ServiceRestart { operation_id, .. }
            | Self::ManagedDnsReconcile { operation_id, .. }
            | Self::IngressConfigure { operation_id, .. }
            | Self::NamespaceRemove { operation_id, .. } => operation_id,
            Self::VolumeCreate { operation_id, .. }
            | Self::VolumeRemove { operation_id, .. }
            | Self::OperationInterrupted { operation_id, .. } => operation_id,
        }
    }
}

impl From<OperationEvent> for ClassifiedOperationEvent {
    fn from(event: OperationEvent) -> Self {
        match event {
            OperationEvent::BuildSubmitted {
                operation_id,
                target,
                source,
                adapter,
                platforms,
            } => Self::Build {
                operation_id,
                event: BuildEvent::Submitted {
                    target,
                    source,
                    adapter,
                    platforms,
                },
            },
            OperationEvent::BuildPlacementStarted { operation_id } => Self::Build {
                operation_id,
                event: BuildEvent::Transition(BuildTransition::Placing),
            },
            OperationEvent::BuildPlatformPlaced {
                operation_id,
                platform,
                executor,
            } => Self::Build {
                operation_id,
                event: BuildEvent::Evidence(BuildEvidence::PlatformPlaced { platform, executor }),
            },
            OperationEvent::BuildSourceVerified {
                operation_id,
                platform,
                executor,
                source,
            } => Self::Build {
                operation_id,
                event: BuildEvent::Evidence(BuildEvidence::VerifiedSource {
                    platform,
                    executor,
                    source,
                }),
            },
            OperationEvent::BuildPlatformToolchainVerified {
                operation_id,
                platform,
                executor,
                toolchain,
            } => Self::Build {
                operation_id,
                event: BuildEvent::Evidence(BuildEvidence::ToolchainVerified {
                    platform,
                    executor,
                    toolchain,
                }),
            },
            OperationEvent::BuildRunning { operation_id } => Self::Build {
                operation_id,
                event: BuildEvent::Transition(BuildTransition::Building),
            },
            OperationEvent::BuildPlatformLog {
                operation_id,
                platform,
                executor,
                chunk,
            } => Self::Build {
                operation_id,
                event: BuildEvent::Evidence(BuildEvidence::PlatformLog {
                    platform,
                    executor,
                    chunk,
                }),
            },
            OperationEvent::BuildPlatformLogTruncated {
                operation_id,
                platform,
                executor,
                omitted_bytes,
            } => Self::Build {
                operation_id,
                event: BuildEvent::Evidence(BuildEvidence::PlatformLogTruncated {
                    platform,
                    executor,
                    omitted_bytes,
                }),
            },
            OperationEvent::BuildPlatformLogGap {
                operation_id,
                platform,
                executor,
                expected_sequence,
                final_sequence,
            } => Self::Build {
                operation_id,
                event: BuildEvent::Evidence(BuildEvidence::PlatformLogGap {
                    platform,
                    executor,
                    expected_sequence,
                    final_sequence,
                }),
            },
            OperationEvent::BuildPlatformCompleted {
                operation_id,
                platform,
                executor,
                image,
            } => Self::Build {
                operation_id,
                event: BuildEvent::Evidence(BuildEvidence::PlatformCompleted {
                    platform,
                    executor,
                    image,
                }),
            },
            OperationEvent::BuildPlatformFailed {
                operation_id,
                platform,
                executor,
                failure,
            } => Self::Build {
                operation_id,
                event: BuildEvent::Evidence(BuildEvidence::PlatformFailed {
                    platform,
                    executor,
                    failure,
                }),
            },
            OperationEvent::BuildCompleted {
                operation_id,
                receipt,
            } => Self::Build {
                operation_id,
                event: BuildEvent::Transition(BuildTransition::Completed { receipt }),
            },
            OperationEvent::BuildFailed {
                operation_id,
                failure,
            } => Self::Build {
                operation_id,
                event: BuildEvent::Transition(BuildTransition::Failed { failure }),
            },
            OperationEvent::BuildCancelled {
                operation_id,
                reason,
                cleanup,
            } => Self::Build {
                operation_id,
                event: BuildEvent::Transition(BuildTransition::Cancelled { reason, cleanup }),
            },
            OperationEvent::BuildTimedOut {
                operation_id,
                failure,
                cleanup,
            } => Self::Build {
                operation_id,
                event: BuildEvent::Transition(BuildTransition::TimedOut { failure, cleanup }),
            },
            OperationEvent::DeploySubmitted { operation_id, .. } => Self::Deploy {
                operation_id,
                event: DeployEvent::Submitted,
            },
            OperationEvent::DeployPlanningStarted { operation_id } => Self::Deploy {
                operation_id,
                event: DeployEvent::Transition(DeployTransition::Planning),
            },
            OperationEvent::DeployImageResolved {
                operation_id,
                service_id,
                machine_id,
                requested,
                resolved,
                credential_supplied,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::ImageResolved {
                    service_id,
                    machine_id,
                    requested,
                    resolved,
                    credential_supplied,
                }),
            },
            OperationEvent::DeployPlanCreated {
                operation_id, plan, ..
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::PlanCreated { plan }),
            },
            OperationEvent::DeployRunning {
                operation_id,
                stage,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Transition(DeployTransition::Running { stage }),
            },
            OperationEvent::DeployImageAvailabilityVerified {
                operation_id,
                service_id,
                machine_id,
                image,
                platform,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::ImageAvailabilityVerified {
                    service_id,
                    machine_id,
                    image,
                    platform,
                }),
            },
            OperationEvent::DeployContainerStarted {
                operation_id,
                machine_id,
                container_id,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::ContainerStarted {
                    machine_id,
                    container_id,
                }),
            },
            OperationEvent::DeployVolumeHandoffApplied {
                operation_id,
                service_id,
                handoff,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::VolumeHandoffApplied {
                    service_id,
                    handoff,
                }),
            },
            OperationEvent::DeployVolumeHandoffRollbackFinished {
                operation_id,
                service_id,
                machine_id,
                outcomes,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::VolumeHandoffRollbackFinished {
                    service_id,
                    machine_id,
                    outcomes,
                }),
            },
            OperationEvent::DeployHealthCheckStarted { operation_id } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::HealthCheckStarted),
            },
            OperationEvent::DeployPhaseStarted {
                operation_id,
                phase,
                service_ids,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::PhaseStarted { phase, service_ids }),
            },
            OperationEvent::DeployPhaseFinished {
                operation_id,
                phase,
                outcome,
                services,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::PhaseFinished {
                    phase,
                    outcome,
                    services,
                }),
            },
            OperationEvent::DeployCleanupFinished {
                operation_id,
                removed,
                failed,
                images,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Evidence(DeployEvidence::CleanupFinished {
                    removed,
                    failed,
                    images,
                }),
            },
            OperationEvent::DeployCompleted {
                operation_id,
                outcome,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Transition(DeployTransition::Completed { outcome }),
            },
            OperationEvent::DeployFailed {
                operation_id,
                failure,
            } => Self::Deploy {
                operation_id,
                event: DeployEvent::Transition(DeployTransition::Failed { failure }),
            },
            OperationEvent::CertProvisionSubmitted {
                operation_id,
                cert_id,
            } => Self::Cert {
                operation_id,
                event: CertEvent::Submitted { cert_id },
            },
            OperationEvent::CertChallengePublished {
                operation_id,
                cert_id,
                ..
            } => Self::Cert {
                operation_id,
                event: CertEvent::Transition {
                    cert_id,
                    transition: Box::new(CertTransition::Running {
                        stage: CertRunningStage::ChallengePublished,
                    }),
                },
            },
            OperationEvent::CertValidationStarted {
                operation_id,
                cert_id,
            } => Self::Cert {
                operation_id,
                event: CertEvent::Transition {
                    cert_id,
                    transition: Box::new(CertTransition::Running {
                        stage: CertRunningStage::ValidationStarted,
                    }),
                },
            },
            OperationEvent::CertWarning {
                operation_id,
                cert_id,
                warning,
            } => Self::Cert {
                operation_id,
                event: CertEvent::Warning { cert_id, warning },
            },
            OperationEvent::CertCompleted {
                operation_id,
                certificate,
            } => Self::Cert {
                operation_id,
                event: CertEvent::Transition {
                    cert_id: certificate.active.cert_id.clone(),
                    transition: Box::new(CertTransition::Completed),
                },
            },
            OperationEvent::CertFailed {
                operation_id,
                failure,
            } => Self::Cert {
                operation_id,
                event: CertEvent::Transition {
                    cert_id: failure.cert_id().clone(),
                    transition: Box::new(CertTransition::Failed { failure }),
                },
            },
            OperationEvent::MachineAddSubmitted {
                operation_id,
                machine_id,
                ..
            } => Self::MachineAdd {
                operation_id,
                event: MachineAddEvent::Submitted { machine_id },
            },
            OperationEvent::MachineAddJoined {
                operation_id,
                machine_id,
                joined_at,
            } => Self::MachineAdd {
                operation_id,
                event: MachineAddEvent::Transition {
                    machine_id,
                    state: MachineAddOperationState::Joining { joined_at },
                },
            },
            OperationEvent::MachineAddCredentialProvisioned {
                operation_id,
                machine_id,
                ..
            } => Self::MachineAdd {
                operation_id,
                event: MachineAddEvent::CredentialProvisioned { machine_id },
            },
            OperationEvent::MachineAddCompleted {
                operation_id,
                machine_id,
            } => Self::MachineAdd {
                operation_id,
                event: MachineAddEvent::Transition {
                    machine_id,
                    state: MachineAddOperationState::Completed,
                },
            },
            OperationEvent::MachineAddFailed {
                operation_id,
                machine_id,
                failure,
            } => Self::MachineAdd {
                operation_id,
                event: MachineAddEvent::Transition {
                    machine_id,
                    state: MachineAddOperationState::Failed { failure },
                },
            },
            OperationEvent::MachineUpdateSubmitted {
                operation_id,
                machine_id,
                ..
            } => Self::MachineUpdate {
                operation_id,
                event: MachineUpdateEvent::Submitted { machine_id },
            },
            OperationEvent::MachineUpdateRunning {
                operation_id,
                machine_id,
            } => Self::MachineUpdate {
                operation_id,
                event: MachineUpdateEvent::Transition {
                    machine_id,
                    transition: MachineUpdateTransition::Running,
                },
            },
            OperationEvent::MachineUpdateCompleted {
                operation_id,
                machine_id,
                reported,
            } => Self::MachineUpdate {
                operation_id,
                event: MachineUpdateEvent::Transition {
                    machine_id,
                    transition: MachineUpdateTransition::Completed { reported },
                },
            },
            OperationEvent::MachineUpdateFailed {
                operation_id,
                machine_id,
                failure,
            } => Self::MachineUpdate {
                operation_id,
                event: MachineUpdateEvent::Transition {
                    machine_id,
                    transition: MachineUpdateTransition::Failed { failure },
                },
            },
            OperationEvent::MachineStoragePrepareSubmitted {
                operation_id,
                machine_id,
                ..
            } => Self::MachineStoragePrepare {
                operation_id,
                event: MachineStoragePrepareEvent::Submitted { machine_id },
            },
            OperationEvent::MachineStoragePreparePreparing {
                operation_id,
                machine_id,
            } => Self::MachineStoragePrepare {
                operation_id,
                event: MachineStoragePrepareEvent::Transition {
                    machine_id,
                    transition: MachineStoragePrepareTransition::Preparing,
                },
            },
            OperationEvent::MachineStoragePrepareCompleted {
                operation_id,
                machine_id,
                pool,
            } => Self::MachineStoragePrepare {
                operation_id,
                event: MachineStoragePrepareEvent::Transition {
                    machine_id,
                    transition: MachineStoragePrepareTransition::Completed { pool },
                },
            },
            OperationEvent::MachineStoragePrepareFailed {
                operation_id,
                machine_id,
                failure,
            } => Self::MachineStoragePrepare {
                operation_id,
                event: MachineStoragePrepareEvent::Transition {
                    machine_id,
                    transition: MachineStoragePrepareTransition::Failed { failure },
                },
            },
            OperationEvent::MachineBuildCachePruneSubmitted {
                operation_id,
                machine_id,
            } => Self::MachineBuildCachePrune {
                operation_id,
                event: MachineBuildCachePruneEvent::Submitted { machine_id },
            },
            OperationEvent::MachineBuildCachePrunePruning {
                operation_id,
                machine_id,
            } => Self::MachineBuildCachePrune {
                operation_id,
                event: MachineBuildCachePruneEvent::Transition {
                    machine_id,
                    transition: MachineBuildCachePruneTransition::Pruning,
                },
            },
            OperationEvent::MachineBuildCachePruneCompleted {
                operation_id,
                machine_id,
                evidence,
            } => Self::MachineBuildCachePrune {
                operation_id,
                event: MachineBuildCachePruneEvent::Transition {
                    machine_id,
                    transition: MachineBuildCachePruneTransition::Completed { evidence },
                },
            },
            OperationEvent::MachineBuildCachePruneFailed {
                operation_id,
                machine_id,
                failure,
            } => Self::MachineBuildCachePrune {
                operation_id,
                event: MachineBuildCachePruneEvent::Transition {
                    machine_id,
                    transition: MachineBuildCachePruneTransition::Failed { failure },
                },
            },
            OperationEvent::MachineLifecycleSubmitted {
                operation_id,
                machine_id,
                ..
            } => Self::MachineLifecycle {
                operation_id,
                event: MachineLifecycleEvent::Submitted { machine_id },
            },
            OperationEvent::MachineLifecycleCompleted {
                operation_id,
                machine_id,
            } => Self::MachineLifecycle {
                operation_id,
                event: MachineLifecycleEvent::Transition {
                    machine_id,
                    transition: MachineLifecycleTransition::Completed,
                },
            },
            OperationEvent::MachineLifecycleFailed {
                operation_id,
                machine_id,
                failure,
            } => Self::MachineLifecycle {
                operation_id,
                event: MachineLifecycleEvent::Transition {
                    machine_id,
                    transition: MachineLifecycleTransition::Failed { failure },
                },
            },
            OperationEvent::CoreReplaceSubmitted {
                operation_id,
                machine_id,
                ..
            } => Self::CoreReplace {
                operation_id,
                event: CoreReplaceEvent::Submitted { machine_id },
            },
            OperationEvent::CoreReplaceCompleted {
                operation_id,
                machine_id,
            } => Self::CoreReplace {
                operation_id,
                event: CoreReplaceEvent::Transition {
                    machine_id,
                    transition: CoreReplaceTransition::Completed,
                },
            },
            OperationEvent::CoreReplaceFailed {
                operation_id,
                machine_id,
                failure,
            } => Self::CoreReplace {
                operation_id,
                event: CoreReplaceEvent::Transition {
                    machine_id,
                    transition: CoreReplaceTransition::Failed { failure },
                },
            },
            OperationEvent::CredentialGrantSubmitted {
                operation_id,
                action,
            } => Self::CredentialGrant {
                operation_id,
                event: CredentialGrantEvent::Submitted { action },
            },
            OperationEvent::CredentialGrantCompleted { operation_id } => Self::CredentialGrant {
                operation_id,
                event: CredentialGrantEvent::Transition(CredentialGrantTransition::Completed),
            },
            OperationEvent::CredentialGrantFailed {
                operation_id,
                failure,
            } => Self::CredentialGrant {
                operation_id,
                event: CredentialGrantEvent::Transition(CredentialGrantTransition::Failed {
                    failure,
                }),
            },
            OperationEvent::NetworkRepairSubmitted {
                operation_id,
                target_machine_id: _,
            } => Self::NetworkRepair {
                operation_id,
                event: NetworkRepairEvent::Submitted,
            },
            OperationEvent::NetworkRepairRunning {
                operation_id,
                stage,
            } => Self::NetworkRepair {
                operation_id,
                event: NetworkRepairEvent::Transition(NetworkRepairTransition::Running { stage }),
            },
            OperationEvent::NetworkRepairDataplaneConverged {
                operation_id,
                revision,
                machine_ids,
            } => Self::NetworkRepair {
                operation_id,
                event: NetworkRepairEvent::Evidence(NetworkRepairEvidence::DataplaneConverged {
                    revision,
                    machine_ids,
                }),
            },
            OperationEvent::NetworkRepairMachineFactsRefreshed {
                operation_id,
                refreshes,
            } => Self::NetworkRepair {
                operation_id,
                event: NetworkRepairEvent::Evidence(NetworkRepairEvidence::MachineFactsRefreshed {
                    refreshes,
                }),
            },
            OperationEvent::NetworkRepairDnsRefreshConfirmed {
                operation_id,
                machine_ids,
            } => Self::NetworkRepair {
                operation_id,
                event: NetworkRepairEvent::Evidence(NetworkRepairEvidence::DnsRefreshConfirmed {
                    machine_ids,
                }),
            },
            OperationEvent::NetworkRepairCompleted { operation_id } => Self::NetworkRepair {
                operation_id,
                event: NetworkRepairEvent::Transition(NetworkRepairTransition::Completed),
            },
            OperationEvent::NetworkRepairFailed {
                operation_id,
                failure,
            } => Self::NetworkRepair {
                operation_id,
                event: NetworkRepairEvent::Transition(NetworkRepairTransition::Failed { failure }),
            },
            OperationEvent::ServiceRestartSubmitted {
                operation_id,
                namespace_id,
                service_id,
            } => Self::ServiceRestart {
                operation_id,
                event: ServiceRestartEvent::Submitted {
                    namespace_id,
                    service_id,
                },
            },
            OperationEvent::ServiceRestartRunning {
                operation_id,
                stage,
            } => Self::ServiceRestart {
                operation_id,
                event: ServiceRestartEvent::Transition(ServiceRestartTransition::Running { stage }),
            },
            OperationEvent::ServiceRestartContainerRestarted {
                operation_id,
                machine_id,
                container_id,
            } => Self::ServiceRestart {
                operation_id,
                event: ServiceRestartEvent::ContainerRestarted {
                    machine_id,
                    container_id,
                },
            },
            OperationEvent::ServiceRestartCompleted { operation_id } => Self::ServiceRestart {
                operation_id,
                event: ServiceRestartEvent::Transition(ServiceRestartTransition::Completed),
            },
            OperationEvent::ServiceRestartFailed {
                operation_id,
                failure,
            } => Self::ServiceRestart {
                operation_id,
                event: ServiceRestartEvent::Transition(ServiceRestartTransition::Failed {
                    failure,
                }),
            },
            OperationEvent::ManagedDnsReconcileSubmitted {
                operation_id,
                subject,
            } => Self::ManagedDnsReconcile {
                operation_id,
                event: ManagedDnsReconcileEvent::Submitted { subject },
            },
            OperationEvent::ManagedDnsReconcileCompleted {
                operation_id,
                subject,
            } => Self::ManagedDnsReconcile {
                operation_id,
                event: ManagedDnsReconcileEvent::Transition {
                    subject,
                    transition: ManagedDnsReconcileTransition::Completed,
                },
            },
            OperationEvent::ManagedDnsReconcileFailed {
                operation_id,
                subject,
                failure,
            } => Self::ManagedDnsReconcile {
                operation_id,
                event: ManagedDnsReconcileEvent::Transition {
                    subject,
                    transition: ManagedDnsReconcileTransition::Failed { failure },
                },
            },
            OperationEvent::IngressConfigureSubmitted {
                operation_id,
                configuration,
            } => Self::IngressConfigure {
                operation_id,
                event: IngressConfigureEvent::Submitted { configuration },
            },
            OperationEvent::IngressConfigureCompleted { operation_id } => Self::IngressConfigure {
                operation_id,
                event: IngressConfigureEvent::Transition(IngressConfigureTransition::Completed),
            },
            OperationEvent::IngressConfigureFailed {
                operation_id,
                failure,
            } => Self::IngressConfigure {
                operation_id,
                event: IngressConfigureEvent::Transition(IngressConfigureTransition::Failed {
                    failure,
                }),
            },
            OperationEvent::NamespaceRemoveSubmitted {
                operation_id,
                namespace_id,
            } => Self::NamespaceRemove {
                operation_id,
                event: NamespaceRemoveEvent::Submitted { namespace_id },
            },
            OperationEvent::NamespaceRemoveRunning {
                operation_id,
                stage,
            } => Self::NamespaceRemove {
                operation_id,
                event: NamespaceRemoveEvent::Transition(NamespaceRemoveTransition::Running {
                    stage,
                }),
            },
            OperationEvent::NamespaceRemoveRouteBindingRemoved {
                operation_id,
                target,
            } => Self::NamespaceRemove {
                operation_id,
                event: NamespaceRemoveEvent::RouteBindingRemoved { target },
            },
            OperationEvent::NamespaceRemoveContainerRemoved {
                operation_id,
                machine_id,
                container_id,
            } => Self::NamespaceRemove {
                operation_id,
                event: NamespaceRemoveEvent::ContainerRemoved {
                    machine_id,
                    container_id,
                },
            },
            OperationEvent::NamespaceRemoveCompleted { operation_id } => Self::NamespaceRemove {
                operation_id,
                event: NamespaceRemoveEvent::Transition(NamespaceRemoveTransition::Completed),
            },
            OperationEvent::NamespaceRemoveFailed {
                operation_id,
                failure,
            } => Self::NamespaceRemove {
                operation_id,
                event: NamespaceRemoveEvent::Transition(NamespaceRemoveTransition::Failed {
                    failure,
                }),
            },
            OperationEvent::VolumeRemoveSubmitted {
                operation_id,
                namespace_id,
                volume_name,
            } => Self::VolumeRemove {
                operation_id,
                event: VolumeRemoveEvent::Submitted {
                    namespace_id,
                    volume_name,
                },
            },
            OperationEvent::VolumeCreateSubmitted { request } => Self::VolumeCreate {
                operation_id: request.operation_id.clone(),
                event: VolumeCreateEvent::Submitted { request },
            },
            OperationEvent::VolumeCreatePlanningStarted { operation_id } => Self::VolumeCreate {
                operation_id,
                event: VolumeCreateEvent::Transition(VolumeCreateTransition::Planning),
            },
            OperationEvent::VolumeCreateRunning {
                operation_id,
                stage,
            } => Self::VolumeCreate {
                operation_id,
                event: VolumeCreateEvent::Transition(VolumeCreateTransition::Running { stage }),
            },
            OperationEvent::VolumeCreateCompleted { operation_id } => Self::VolumeCreate {
                operation_id,
                event: VolumeCreateEvent::Transition(VolumeCreateTransition::Completed),
            },
            OperationEvent::VolumeCreateFailed {
                operation_id,
                failure,
            } => Self::VolumeCreate {
                operation_id,
                event: VolumeCreateEvent::Transition(VolumeCreateTransition::Failed { failure }),
            },
            OperationEvent::VolumeRemoveRunning {
                operation_id,
                stage,
            } => Self::VolumeRemove {
                operation_id,
                event: VolumeRemoveEvent::Transition(VolumeRemoveTransition::Running { stage }),
            },
            OperationEvent::VolumeRemoveCompleted { operation_id } => Self::VolumeRemove {
                operation_id,
                event: VolumeRemoveEvent::Transition(VolumeRemoveTransition::Completed),
            },
            OperationEvent::VolumeRemoveFailed {
                operation_id,
                failure,
            } => Self::VolumeRemove {
                operation_id,
                event: VolumeRemoveEvent::Transition(VolumeRemoveTransition::Failed { failure }),
            },
            OperationEvent::OperationInterrupted {
                operation_id,
                evidence,
            } => Self::OperationInterrupted {
                operation_id,
                evidence,
            },
            OperationEvent::Cancelled {
                operation_id,
                kind,
                reason,
            } => match kind {
                OperationKind::Build => Self::Build {
                    operation_id,
                    event: BuildEvent::Transition(BuildTransition::Cancelled {
                        reason,
                        cleanup: BuildCleanupEvidence::NotRequired,
                    }),
                },
                OperationKind::Deploy => Self::Deploy {
                    operation_id,
                    event: DeployEvent::Transition(DeployTransition::Cancelled { reason }),
                },
                OperationKind::Cert => Self::Cert {
                    operation_id,
                    event: CertEvent::Cancelled(reason),
                },
                OperationKind::MachineAdd => Self::MachineAdd {
                    operation_id,
                    event: MachineAddEvent::Cancelled(reason),
                },
                OperationKind::MachineUpdate => Self::MachineUpdate {
                    operation_id,
                    event: MachineUpdateEvent::Cancelled(reason),
                },
                OperationKind::MachineStoragePrepare => Self::MachineStoragePrepare {
                    operation_id,
                    event: MachineStoragePrepareEvent::Cancelled(reason),
                },
                OperationKind::MachineBuildCachePrune => Self::MachineBuildCachePrune {
                    operation_id,
                    event: MachineBuildCachePruneEvent::Cancelled(reason),
                },
                OperationKind::MachineLifecycle => Self::MachineLifecycle {
                    operation_id,
                    event: MachineLifecycleEvent::Cancelled(reason),
                },
                OperationKind::CoreReplace => Self::CoreReplace {
                    operation_id,
                    event: CoreReplaceEvent::Cancelled(reason),
                },
                OperationKind::CredentialGrant => Self::CredentialGrant {
                    operation_id,
                    event: CredentialGrantEvent::Cancelled(reason),
                },
                OperationKind::NetworkRepair => Self::NetworkRepair {
                    operation_id,
                    event: NetworkRepairEvent::Transition(NetworkRepairTransition::Cancelled {
                        reason,
                    }),
                },
                OperationKind::ServiceRestart => Self::ServiceRestart {
                    operation_id,
                    event: ServiceRestartEvent::Transition(ServiceRestartTransition::Cancelled {
                        reason,
                    }),
                },
                OperationKind::ManagedDnsReconcile => Self::ManagedDnsReconcile {
                    operation_id,
                    event: ManagedDnsReconcileEvent::UnsupportedCancellation,
                },
                OperationKind::IngressConfigure => Self::IngressConfigure {
                    operation_id,
                    event: IngressConfigureEvent::UnsupportedCancellation,
                },
                OperationKind::NamespaceRemove => Self::NamespaceRemove {
                    operation_id,
                    event: NamespaceRemoveEvent::Transition(NamespaceRemoveTransition::Cancelled {
                        reason,
                    }),
                },
                OperationKind::VolumeRemove => Self::VolumeRemove {
                    operation_id,
                    event: VolumeRemoveEvent::Cancelled(reason),
                },
                OperationKind::VolumeCreate => Self::VolumeCreate {
                    operation_id,
                    event: VolumeCreateEvent::Cancelled,
                },
            },
        }
    }
}
