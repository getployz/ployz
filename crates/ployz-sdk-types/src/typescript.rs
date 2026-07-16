//! TypeScript contract export owned by the Rust SDK type crate.

use crate::operation_api::OperationApiContract;
use crate::operation_api::OperationApiEndpoint;
use crate::{
    AbsoluteInstallPath, AcceptedOperation, AcmeChallengeToken, AcmeChallengeTtlSeconds,
    AcmeChallengeValue, AcmeHttp01Challenge, ActiveCertState, ActiveCertificateMetadata,
    ActiveMachineState, ArtifactUnavailableReason, AutomaticHostnameConfiguration,
    AutomaticHostnameLabel, AutomaticHostnameSuffix, CLOUD_BOOTSTRAP_PROTOCOL_VERSION,
    CancellationReason, CertBundleRef, CertId, CertInterruptionStage, CertOperationFailure,
    CertOperationState, CertRunningStage, CertValidAt, CertValidityWindow,
    CertificateInterruptionNextAction, CertificateOwner, CertificateProvisionFailure,
    CertificateProvisionWarning, CloudBootstrapAttemptId, CloudBootstrapCallbackAccepted,
    CloudBootstrapCallbackRequest, CloudBootstrapCallbackToken, CloudBootstrapClientInfo,
    CloudBootstrapDecision, CloudBootstrapDecisionFailure, CloudBootstrapEnvelope,
    CloudBootstrapFailure, CloudBootstrapIntent, CloudBootstrapMachineFacts, CloudBootstrapOutcome,
    CloudBootstrapRedemptionId, CloudBootstrapSessionCreateRequest, CloudBootstrapSessionCreated,
    CloudBootstrapSessionPollRequest, CloudBootstrapSessionSecret, CloudBootstrapToken,
    CloudBootstrapTokenRedeemRequest, CloudFounderBootstrap, CloudFounderBootstrapResult,
    CloudJoinerBootstrap, CloudJoinerBootstrapResult, ContainerCommand, ContainerEntrypoint,
    ContainerHealth, ContainerHealthcheck, ContainerHealthcheckTest, ContainerId,
    ContainerMountPath, ContainerResourceLimits, ContainerRestartPolicy, ContainerRetentionCount,
    ContainerRuntimeSpec, ContainerRuntimeState, ControlCertificateRenewalAttempt,
    ControlCertificateRenewalFailure, ControlCertificateRenewalHealth,
    ControlCertificateRenewalOutcome, ControlHealth, ControlIngressEndpointProjectionHealth,
    ControlPlaneCommitScope, ControlPlaneEpoch, ControlRuntimeProjectionHealth,
    ControlRuntimeProjectionLoopHealth, ControlRuntimeProjectionServiceHealth,
    ControlTaskSupervisorFailure, ControlTaskSupervisorHealth, CoreReplaceError,
    CoreReplaceFailure, CoreReplaceOperationState, CoreReplaceReportError,
    CoreReplaceReportOutcome, CoreReplaceReportRequest, CoreReplaceReported, CoreReplaceRequest,
    CredentialAddError, CredentialAddRequest, CredentialGrant, CredentialGrantAction,
    CredentialGrantFailure, CredentialGrantOperationState, CredentialListError,
    CredentialListRequest, CredentialListResult, CredentialName, CredentialRemoveError,
    CredentialRemoveRequest, CredentialRole, DataplaneAdmissionPeer, DataplaneMember,
    DataplaneProjectionAdmissionEvidence, DataplaneProjectionAdmissionFailure,
    DataplaneProjectionComponent, DataplaneProjectionFailure, DataplaneProjectionRevision,
    DataplaneProjectionRevisions, DataplaneProjectionTestimony, DataplaneUnavailableReason,
    DatasetName, DependencyCondition, DeployCleanupAction, DeployCleanupContainer,
    DeployCleanupFailure, DeployCompletionOutcome, DeployImageCleanup, DeployInterruptionStage,
    DeployOperationFailure, DeployOperationState, DeployOrigin, DeployPhaseNumber,
    DeployPhaseOutcome, DeployPhasePlan, DeployPlan, DeployPlanStep, DeployRequest,
    DeployReservationExpiresAt, DeployReservationId, DeployReserveError, DeployReserveRequest,
    DeployReserveResponse, DeployReserved, DeployRoute, DeployRouteTarget, DeployRunningStage,
    DeployServicePlan, DeployServiceResult, DeployServiceSpec, DeploySubmitError,
    DeploySubmitRequest, DeploySubmitResponse, EbpfAttachmentStatus, EbpfForwardingReady,
    EbpfForwardingReadyEvidence, EndpointBridgeStatus, EnvName, EnvValue, EventSequence,
    FailureMessage, FirstMachineInstallArtifacts, FirstMachineInstallSpec, GatewayHttpFailure,
    GatewayProcessAttempt, GatewayProcessHealth, GatewayRole, GatewayServingStatus,
    GatewayStatusObservation, GatewayStatusPublishFailure, GatewayWatchFailure, HealthCheckFailure,
    HealthcheckDurationNanos, HealthcheckRetries, HealthcheckShellCommand, HostPortAssurance,
    ImageReference, ImageSource, IngressConfiguration, IngressConfigureError,
    IngressConfigureFailure, IngressConfigureOperationState, IngressConfigureRequest,
    IngressEndpointProjection, IngressEndpointProjectionIdentity, IngressEndpointProjectionState,
    IngressEndpointSet, IngressEndpointUnavailableReason, InitFirstMachineActivateError,
    InitFirstMachineActivateRequest, InitFirstMachineActivateResponse, InitFirstMachineActivated,
    InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion, InstallRolePolicy,
    InstallSha256Digest, InternalDnsFactGeneration, InternalDnsFactWatermark,
    InternalDnsIntentHealth, InternalDnsIntentRefreshHealth, InternalDnsIntentWatchHealth,
    InternalDnsResolverCacheIncarnation, InternalDnsResolverStatus, InternalDnsStatus,
    InternalServiceName, IssuedJoinToken, JoinTokenExpiresAt, JoinTokenFingerprint,
    JoinTokenRedeemedAt, LinuxCapability, LogsTailError, LogsTailLines, LogsTailRequest,
    LogsTailResult, LogsTailResultTarget, LogsTailTarget, MAX_LOGS_TAIL_LINES,
    MAX_OPERATION_EVENT_REPLAY_LIMIT, MachineAddAccepted, MachineAddError, MachineAddFailure,
    MachineAddInterruptionEvidence, MachineAddInterruptionNextAction, MachineAddInterruptionStage,
    MachineAddInterruptionUncertainWork, MachineAddOperationState, MachineAddOperationStateName,
    MachineAddRequest, MachineAddResponse, MachineBootstrapUrl, MachineCredentialProvisioningStep,
    MachineDataplaneStatus, MachineDiskSpace, MachineEndpointObservation, MachineEndpointSubnet,
    MachineEndpointSupernet, MachineFactsRefreshConfirmation, MachineId, MachineInspectError,
    MachineInspectRequest, MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial,
    MachineJoinRedeemError, MachineJoinRedeemRequest, MachineJoinRedeemResponse,
    MachineJoinRedeemResult, MachineJoinRedeemed, MachineJoinReportError, MachineJoinReportFailure,
    MachineJoinReportOutcome, MachineJoinReportRequest, MachineJoinReported,
    MachineJoinReportedFailure, MachineJoinReportedOutcome, MachineJoinRuntimeNatsUrl,
    MachineJoinSecretDelivery, MachineJoinTemplate, MachineJoinToken, MachineJoinTrustedNats,
    MachineLifecycle, MachineLifecycleError, MachineLifecycleFailure,
    MachineLifecycleOperationState, MachineLifecycleRequest, MachineListError, MachineListRequest,
    MachineListResult, MachineName, MachineReadinessCheck, MachineReadinessEvidence,
    MachineSnapshot, MachineStoragePrepareError, MachineStoragePrepareFailure,
    MachineStoragePrepareOperationState, MachineStoragePrepareRequest, MachineSubstrateVersions,
    MachineTestimony, MachineUpdateError, MachineUpdateFailure, MachineUpdateOperationState,
    MachineUpdateRequest, MachineUpdateResponse, MachineUsabilityReason,
    ManagedContainerHealthStatus, ManagedContainerIdentity, ManagedContainerKind,
    ManagedContainerObservation, ManagedDnsReconcileFailure, ManagedDnsReconcileFailureClass,
    ManagedDnsReconcileOperationState, ManagedDnsReconcileSubject, ManagedDnsWithdrawAuthorization,
    ManagedLeaseName, MemoryBytes, NamespaceId, NamespaceRemoveError, NamespaceRemoveFailure,
    NamespaceRemoveOperationState, NamespaceRemoveRequest, NamespaceRemoveRunningStage,
    NamespaceRevisionEntryId, NamespaceRevisionId, NanoCpus, NativeDataplaneProjectionStatus,
    NatsAuthorizationGrant, NatsInternalAuthority, NatsPrincipal, NatsServerInstallSpec,
    NatsUserPublicKey, NetworkDataplaneTestimony, NetworkInternalDnsTestimony,
    NetworkRepairDnsRefreshProblem, NetworkRepairError, NetworkRepairFailure,
    NetworkRepairMachineFactsRefreshOutcome, NetworkRepairOperationState,
    NetworkRepairProgressPhase, NetworkRepairRequest, NetworkRepairRequestFailure,
    NetworkRepairRunningStage, NetworkResolveError, NetworkResolveMachineTestimony,
    NetworkResolveRequest, NetworkResolveResult, NetworkStatusError,
    NetworkStatusIntentFingerprint, NetworkStatusMachine, NetworkStatusMode, NetworkStatusRequest,
    NetworkStatusResult, OciDigest, OciPlatform, OperationApiResponse, OperationEvent,
    OperationEventRecordedAtUnixMs, OperationEventReplayCursor, OperationEventReplayLimit,
    OperationEventReplayPage, OperationEventReplayRequest, OperationId, OperationIdempotencyKey,
    OperationInterruptionCause, OperationInterruptionEvidence, OperationInterruptionNextAction,
    OperationInterruptionStage, OperationInterruptionUncertainWork, OperationKind, OperationStatus,
    OperationStatusSnapshot, OperationSubject, OperatorHint, OpsListError, OpsListRequest,
    OpsListResult, OpsStatusError, OpsStatusRequest, OpsStatusResponse, OpsWatchError,
    OpsWatchResponse, PidsLimit, PlatformImage, PloyzDnsTargetIntent, PloyzNativeMeshComponent,
    PloyzNativeMeshReady, PreStartHook, PreStartHookFailure, PreStartHookStep, PushedImageReceipt,
    RegistryCredential, RegistryCredentialSecret, RegistryCredentialUsername,
    ReplayedOperationEvent, ReplicaCount, ReplicaSlot, RetainedArtifact, RouteBindingId,
    RouteBindingOrigin, RouteBindingState, RouteCutoverFailureReason, RouteHostname, RoutePort,
    RouteTarget, RouteTlsAvailability, RouteTlsStatus, RuntimeDerivedCollectionSource,
    RuntimeDerivedCollectionStatus, RuntimePloyzDnsTarget, RuntimePloyzDnsTargetAllocation,
    RuntimePloyzDnsTargetPublication, RuntimeProjectionSource, RuntimeProjectionSources,
    RuntimeServiceInstance, RuntimeServiceRelease, RuntimeServiceRevision, RuntimeSnapshot,
    RuntimeSnapshotError, RuntimeSnapshotRequest, RuntimeSnapshotResult,
    ServiceContainerMembership, ServiceContainerTestimony, ServiceDependency, ServiceEnvironment,
    ServiceId, ServiceInspectError, ServiceInspectRequest, ServiceListError, ServiceListRequest,
    ServiceListResult, ServiceMachineTestimony, ServiceRestartError, ServiceRestartFailure,
    ServiceRestartOperationState, ServiceRestartRequest, ServiceRestartRunningStage,
    ServiceSnapshot, ServiceTestimony, ServiceVolumeMount, ServingTargetEntry, StepId,
    StopGracePeriod, StorageCapability, StorageEffectFailure, StorageUnavailableReason,
    StrandedVolumeAlarm, StrandedVolumeReason, UnusableMachine, VolumeKind, VolumeListError,
    VolumeListRequest, VolumeListResult, VolumeMaxSizeBytes, VolumeName, VolumePinState,
    VolumeRemoveError, VolumeRemoveFailure, VolumeRemoveOperationState, VolumeRemoveRequest,
    VolumeRemoveRunningStage, VolumeSnapshot, VolumeSpec, VolumeStatus, WireGuardConfiguredMtu,
    WireGuardDetectedMtu, WireGuardHandshakeStatus, WireGuardInterfaceMtu, WireGuardMtuProbe,
    WireGuardPeerEndpointSubnet, WireGuardPeerStatus, WireGuardPublicKey,
    WireGuardReadinessFailure, WireGuardReady, WireGuardReadyEvidence, WireGuardRttStatus,
    WireGuardStatus, WrappedCaKey, WrappedCoreSeeds, ZfsPoolName,
};
use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
use serde::Serialize;
use serde_json::{Value, json};
use ts_rs::{Config, TS};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NatsOperationApiEndpointMetadata {
    pub name: &'static str,
    pub subject: &'static str,
    pub execution: &'static str,
}

#[derive(Debug, Clone, Copy)]
pub struct NatsTypescriptMetadata {
    pub runtime_snapshot_seed: &'static str,
    pub runtime_snapshot_stream: &'static str,
    pub operation_api_endpoint: fn(OperationApiEndpoint) -> NatsOperationApiEndpointMetadata,
}

#[must_use]
pub fn generated_typescript(metadata: NatsTypescriptMetadata) -> String {
    let config = Config::new().with_large_int("number");
    let mut output = String::from("// Generated by ployz-sdk-types. Do not edit by hand.\n\n");

    output.push_str("export type Brand<T, B extends string> = T & { readonly __brand: B };\n\n");
    output.push_str(
        "export type SafeInteger<B extends string> = Brand<number, `safe_integer:${B}`>;\n\n",
    );
    output.push_str(&format!(
        "export const MAX_OPERATION_EVENT_REPLAY_LIMIT = {MAX_OPERATION_EVENT_REPLAY_LIMIT} as const;\n\n"
    ));
    output.push_str(&format!(
        "export const MAX_LOGS_TAIL_LINES = {MAX_LOGS_TAIL_LINES} as const;\n\n"
    ));
    output.push_str(&format!(
        "export const CLOUD_BOOTSTRAP_PROTOCOL_VERSION = {CLOUD_BOOTSTRAP_PROTOCOL_VERSION} as const;\n\n"
    ));
    output.push_str(&format!(
        "export const RUNTIME_SNAPSHOT_SEED = {:?} as const;\n\n",
        metadata.runtime_snapshot_seed
    ));
    output.push_str(&format!(
        "export const RUNTIME_SNAPSHOT_STREAM = {:?} as const;\n\n",
        metadata.runtime_snapshot_stream
    ));
    push_contract_decls(&mut output, &config);
    push_operation_api_contracts(&mut output, &config, metadata.operation_api_endpoint);

    strip_trailing_whitespace(&output)
}

fn strip_trailing_whitespace(value: &str) -> String {
    let mut stripped = String::with_capacity(value.len());
    for line in value.lines() {
        stripped.push_str(line.trim_end());
        stripped.push('\n');
    }
    stripped
}

macro_rules! exported_types {
    ($macro:ident) => {
        $macro!(
            OperationId,
            OperationIdempotencyKey,
            EventSequence,
            DeployReservationId,
            DeployReservationExpiresAt,
            ServiceId,
            MachineId,
            ContainerId,
            StepId,
            CertId,
            MachineName,
            JoinTokenFingerprint,
            JoinTokenExpiresAt,
            JoinTokenRedeemedAt,
            IssuedJoinToken,
            MachineBootstrapUrl,
            MachineJoinToken,
            CloudBootstrapToken,
            CloudBootstrapSessionSecret,
            CloudBootstrapCallbackToken,
            CloudBootstrapRedemptionId,
            CloudBootstrapAttemptId,
            CloudBootstrapClientInfo,
            CloudBootstrapMachineFacts,
            CloudBootstrapSessionCreateRequest,
            CloudBootstrapTokenRedeemRequest,
            CloudBootstrapSessionCreated,
            CloudBootstrapSessionPollRequest,
            CloudBootstrapDecision,
            CloudBootstrapDecisionFailure,
            CloudBootstrapEnvelope,
            CloudBootstrapIntent,
            CloudFounderBootstrap,
            CloudJoinerBootstrap,
            CloudBootstrapCallbackRequest,
            CloudBootstrapOutcome,
            CloudFounderBootstrapResult,
            CloudJoinerBootstrapResult,
            CloudBootstrapFailure,
            CloudBootstrapCallbackAccepted,
            NamespaceId,
            NamespaceRevisionId,
            NamespaceRevisionEntryId,
            ImageReference,
            ImageSource,
            RegistryCredentialSecret,
            RegistryCredentialUsername,
            RegistryCredential,
            OciDigest,
            OciPlatform,
            PlatformImage,
            PushedImageReceipt,
            ReplicaCount,
            ReplicaSlot,
            EnvName,
            EnvValue,
            ServiceEnvironment,
            ContainerCommand,
            ContainerEntrypoint,
            StopGracePeriod,
            VolumeName,
            ZfsPoolName,
            DatasetName,
            VolumeMaxSizeBytes,
            VolumeSpec,
            ContainerMountPath,
            ServiceVolumeMount,
            VolumeKind,
            VolumePinState,
            VolumeStatus,
            VolumeSnapshot,
            HealthcheckShellCommand,
            HealthcheckDurationNanos,
            HealthcheckRetries,
            ContainerHealthcheckTest,
            ContainerHealthcheck,
            ContainerRestartPolicy,
            LinuxCapability,
            NanoCpus,
            MemoryBytes,
            PidsLimit,
            ContainerResourceLimits,
            ContainerRuntimeSpec,
            DependencyCondition,
            ServiceDependency,
            DeployOrigin,
            DeployRequest,
            DeployServiceSpec,
            ContainerRetentionCount,
            PreStartHook,
            DeployRoute,
            DeployRouteTarget,
            DeployPlan,
            DeployPhasePlan,
            DeployServicePlan,
            DeployPhaseNumber,
            DeployPhaseOutcome,
            DeployServiceResult,
            PreStartHookStep,
            DeployCleanupContainer,
            DeployCleanupAction,
            DeployCleanupFailure,
            DeployImageCleanup,
            ManagedContainerKind,
            ContainerRuntimeState,
            ContainerHealth,
            ManagedContainerHealthStatus,
            ManagedContainerIdentity,
            ManagedContainerObservation,
            DeployPlanStep,
            OperationEventRecordedAtUnixMs,
            OperationEventReplayLimit,
            OperationEventReplayRequest,
            OperationEventReplayPage,
            OperationEventReplayCursor,
            ReplayedOperationEvent,
            OperationStatus,
            OperationStatusSnapshot,
            OperationSubject,
            CredentialGrantAction,
            CredentialGrantOperationState,
            CredentialGrantFailure,
            MachineAddOperationState,
            MachineAddOperationStateName,
            MachineAddFailure,
            MachineAddInterruptionEvidence,
            MachineAddInterruptionStage,
            MachineAddInterruptionUncertainWork,
            MachineAddInterruptionNextAction,
            DataplaneAdmissionPeer,
            DataplaneProjectionAdmissionEvidence,
            DataplaneProjectionAdmissionFailure,
            WireGuardReadinessFailure,
            MachineCredentialProvisioningStep,
            MachineReadinessEvidence,
            MachineReadinessCheck,
            GatewayRole,
            HostPortAssurance,
            InstallRolePolicy,
            DeployOperationState,
            DeployRunningStage,
            DeployCompletionOutcome,
            ServiceRestartOperationState,
            ServiceRestartRunningStage,
            ServiceRestartFailure,
            ManagedDnsReconcileOperationState,
            ManagedDnsReconcileFailure,
            ManagedDnsReconcileFailureClass,
            ManagedDnsReconcileSubject,
            ManagedDnsWithdrawAuthorization,
            IngressConfigureOperationState,
            IngressConfigureFailure,
            NamespaceRemoveOperationState,
            NamespaceRemoveRunningStage,
            NamespaceRemoveFailure,
            NetworkRepairOperationState,
            NetworkRepairRunningStage,
            NetworkRepairFailure,
            NetworkRepairMachineFactsRefreshOutcome,
            NetworkRepairDnsRefreshProblem,
            NetworkRepairProgressPhase,
            NetworkRepairRequestFailure,
            VolumeRemoveOperationState,
            VolumeRemoveRunningStage,
            VolumeRemoveFailure,
            CertOperationState,
            CertRunningStage,
            CertInterruptionStage,
            CertificateInterruptionNextAction,
            OperationKind,
            OperationInterruptionCause,
            OperationInterruptionEvidence,
            OperationInterruptionStage,
            DeployInterruptionStage,
            OperationInterruptionUncertainWork,
            OperationInterruptionNextAction,
            MachineLifecycle,
            MachineLifecycleOperationState,
            MachineLifecycleFailure,
            CoreReplaceOperationState,
            CoreReplaceFailure,
            MachineUsabilityReason,
            DataplaneUnavailableReason,
            UnusableMachine,
            OperationEvent,
            FailureMessage,
            CancellationReason,
            RouteHostname,
            RoutePort,
            RouteTarget,
            RetainedArtifact,
            HealthCheckFailure,
            MachineEndpointSubnet,
            MachineEndpointSupernet,
            DataplaneMember,
            DataplaneProjectionRevision,
            DataplaneProjectionRevisions,
            DataplaneProjectionTestimony,
            DataplaneProjectionFailure,
            DataplaneProjectionComponent,
            EndpointBridgeStatus,
            NativeDataplaneProjectionStatus,
            PloyzNativeMeshComponent,
            PloyzNativeMeshReady,
            MachineDataplaneStatus,
            NetworkStatusMode,
            WireGuardStatus,
            WireGuardConfiguredMtu,
            WireGuardDetectedMtu,
            WireGuardInterfaceMtu,
            WireGuardPeerEndpointSubnet,
            WireGuardPeerStatus,
            WireGuardHandshakeStatus,
            WireGuardRttStatus,
            WireGuardMtuProbe,
            EbpfAttachmentStatus,
            InternalDnsStatus,
            InternalDnsIntentHealth,
            InternalDnsIntentRefreshHealth,
            InternalDnsIntentWatchHealth,
            InternalDnsResolverStatus,
            InternalDnsResolverCacheIncarnation,
            InternalDnsFactGeneration,
            InternalDnsFactWatermark,
            MachineFactsRefreshConfirmation,
            WireGuardPublicKey,
            WireGuardReady,
            WireGuardReadyEvidence,
            EbpfForwardingReady,
            EbpfForwardingReadyEvidence,
            ArtifactUnavailableReason,
            RouteCutoverFailureReason,
            ControlPlaneCommitScope,
            CertificateProvisionFailure,
            CertificateProvisionWarning,
            DeployOperationFailure,
            PreStartHookFailure,
            CertOperationFailure,
            OperatorHint,
            CertValidAt,
            CertValidityWindow,
            CertBundleRef,
            ManagedLeaseName,
            AutomaticHostnameConfiguration,
            AutomaticHostnameLabel,
            AutomaticHostnameSuffix,
            IngressConfiguration,
            PloyzDnsTargetIntent,
            RouteBindingId,
            RouteBindingOrigin,
            CertificateOwner,
            ActiveCertificateMetadata,
            IngressEndpointSet,
            IngressEndpointUnavailableReason,
            IngressEndpointProjectionState,
            IngressEndpointProjectionIdentity,
            IngressEndpointProjection,
            AcmeChallengeToken,
            AcmeChallengeValue,
            AcmeChallengeTtlSeconds,
            AcmeHttp01Challenge,
            ActiveCertState,
            ActiveMachineState,
            ControlPlaneEpoch,
            RouteBindingState,
            ServingTargetEntry,
            MachineEndpointObservation,
            GatewayServingStatus,
            GatewayProcessHealth,
            GatewayProcessAttempt,
            GatewayHttpFailure,
            GatewayWatchFailure,
            GatewayStatusPublishFailure,
            GatewayStatusObservation,
            MachineDiskSpace,
            MachineSnapshot,
            MachineTestimony,
            StorageCapability,
            StorageUnavailableReason,
            StrandedVolumeAlarm,
            StrandedVolumeReason,
            InitFirstMachineActivateRequest,
            InitFirstMachineActivated,
            InitFirstMachineActivateError,
            DeployReserveRequest,
            DeployReserved,
            DeploySubmitRequest,
            MachineAddRequest,
            MachineAddAccepted,
            MachineListRequest,
            MachineListResult,
            MachineListError,
            MachineInspectRequest,
            MachineInspectError,
            InternalServiceName,
            NetworkStatusIntentFingerprint,
            NetworkStatusRequest,
            NetworkStatusResult,
            NetworkStatusMachine,
            NetworkDataplaneTestimony,
            NetworkInternalDnsTestimony,
            NetworkStatusError,
            NetworkResolveRequest,
            NetworkResolveResult,
            NetworkResolveMachineTestimony,
            NetworkResolveError,
            ServiceListRequest,
            ServiceListResult,
            ServiceSnapshot,
            ServiceTestimony,
            ServiceContainerTestimony,
            ServiceContainerMembership,
            ServiceMachineTestimony,
            ServiceListError,
            ServiceInspectRequest,
            ServiceInspectError,
            ServiceRestartRequest,
            ServiceRestartError,
            NamespaceRemoveRequest,
            NamespaceRemoveError,
            NetworkRepairRequest,
            NetworkRepairError,
            VolumeListRequest,
            VolumeListResult,
            VolumeListError,
            VolumeRemoveRequest,
            VolumeRemoveError,
            RuntimeSnapshotRequest,
            RuntimeSnapshotResult,
            ControlHealth,
            ControlTaskSupervisorHealth,
            ControlTaskSupervisorFailure,
            ControlIngressEndpointProjectionHealth,
            ControlRuntimeProjectionHealth,
            ControlRuntimeProjectionLoopHealth,
            ControlRuntimeProjectionServiceHealth,
            ControlCertificateRenewalHealth,
            ControlCertificateRenewalAttempt,
            ControlCertificateRenewalFailure,
            ControlCertificateRenewalOutcome,
            RuntimeSnapshot,
            RuntimePloyzDnsTarget,
            RuntimePloyzDnsTargetAllocation,
            RuntimePloyzDnsTargetPublication,
            RouteTlsStatus,
            RouteTlsAvailability,
            RuntimeServiceRevision,
            RuntimeServiceRelease,
            RuntimeServiceInstance,
            RuntimeProjectionSources,
            RuntimeProjectionSource,
            RuntimeDerivedCollectionSource,
            RuntimeDerivedCollectionStatus,
            RuntimeSnapshotError,
            LogsTailLines,
            LogsTailTarget,
            LogsTailRequest,
            LogsTailResultTarget,
            LogsTailResult,
            LogsTailError,
            MachineJoinClusterName,
            MachineJoinRuntimeNatsUrl,
            MachineJoinMaterial,
            MachineJoinSecretDelivery,
            MachineJoinTemplate,
            FirstMachineInstallSpec,
            FirstMachineInstallArtifacts,
            NatsServerInstallSpec,
            NatsUserSeed,
            NatsUserPublicKey,
            NatsPrincipal,
            CredentialName,
            CredentialRole,
            CredentialGrant,
            NatsInternalAuthority,
            NatsAuthorizationGrant,
            NatsCaCertificatePem,
            MachineJoinTrustedNats,
            WrappedCaKey,
            WrappedCoreSeeds,
            InstallArtifactVersion,
            InstallArtifactSource,
            InstallSha256Digest,
            AbsoluteInstallPath,
            InstallArtifactSpec,
            MachineJoinBundle,
            MachineJoinRedeemRequest,
            MachineJoinRedeemed,
            MachineJoinRedeemResult,
            MachineJoinRedeemError,
            MachineJoinReportRequest,
            MachineJoinReportOutcome,
            MachineJoinReportFailure,
            MachineJoinReportedOutcome,
            MachineJoinReportedFailure,
            MachineJoinReported,
            MachineJoinReportError,
            OpsListRequest,
            OpsListResult,
            OpsListError,
            OpsStatusRequest,
            AcceptedOperation,
            DeployReserveError,
            OperationApiResponse<AcceptedOperation, DeploySubmitError>,
            DeploySubmitError,
            MachineAddError,
            MachineUpdateOperationState,
            MachineSubstrateVersions,
            MachineUpdateFailure,
            MachineUpdateRequest,
            MachineStoragePrepareOperationState,
            StorageEffectFailure,
            MachineStoragePrepareFailure,
            MachineStoragePrepareRequest,
            MachineStoragePrepareError,
            MachineLifecycleRequest,
            MachineLifecycleError,
            CoreReplaceRequest,
            CoreReplaceError,
            CoreReplaceReportRequest,
            CoreReplaceReportOutcome,
            CoreReplaceReported,
            CoreReplaceReportError,
            CredentialAddRequest,
            CredentialAddError,
            CredentialListRequest,
            CredentialListResult,
            CredentialListError,
            CredentialRemoveRequest,
            CredentialRemoveError,
            IngressConfigureRequest,
            IngressConfigureError,
            MachineUpdateError,
            OpsStatusError,
            OpsWatchError
        );
    };
}

fn push_contract_decls(output: &mut String, config: &Config) {
    macro_rules! push_all {
        ($($type:ty),+ $(,)?) => {
            $(push_decl::<$type>(output, config);)+
        };
    }

    exported_types!(push_all);
}

fn push_decl<T: TS>(output: &mut String, config: &Config) {
    output.push_str("export ");
    output.push_str(&T::decl(config));
    output.push_str("\n\n");
}

fn push_operation_api_contracts(
    output: &mut String,
    config: &Config,
    endpoint_metadata: fn(OperationApiEndpoint) -> NatsOperationApiEndpointMetadata,
) {
    macro_rules! push_aliases {
        ($($contract:ty),+ $(,)?) => {
            $(push_operation_api_aliases_for::<$contract>(output, config);)+
        };
    }
    crate::operation_api_contracts!(push_aliases);

    output.push_str("export const OPERATION_API_CONTRACTS = [\n");
    macro_rules! push_rows {
        ($($contract:ty),+ $(,)?) => {
            $(push_operation_api_contract_row_for::<$contract>(output, config, endpoint_metadata);)+
        };
    }
    crate::operation_api_contracts!(push_rows);
    output.push_str("] as const;\n");
    output.push('\n');
    output.push_str(
        "export type PloyzApiEndpoint = (typeof OPERATION_API_CONTRACTS)[number][\"name\"];\n\n",
    );
    output.push_str("export type OperationApiRequestByEndpoint = {\n");
    macro_rules! push_request_map {
        ($($contract:ty),+ $(,)?) => {
            $(push_operation_api_request_map_row_for::<$contract>(output, config, endpoint_metadata);)+
        };
    }
    crate::operation_api_contracts!(push_request_map);
    output.push_str("};\n\n");
    output.push_str("export type OperationApiResponseByEndpoint = {\n");
    macro_rules! push_response_map {
        ($($contract:ty),+ $(,)?) => {
            $(push_operation_api_response_map_row_for::<$contract>(output, endpoint_metadata);)+
        };
    }
    crate::operation_api_contracts!(push_response_map);
    output.push_str("};\n");
}

fn push_operation_api_aliases_for<C>(output: &mut String, config: &Config)
where
    C: OperationApiContract,
    C::Request: TS,
    C::Success: TS,
    C::Error: TS,
{
    if let Some(alias) = C::REQUEST_ALIAS {
        output.push_str(&format!(
            "export type {} = {};\n\n",
            alias,
            C::Request::name(config)
        ));
    }
    output.push_str(&format!(
        "export type {} = OperationApiResponse<{}, {}>;\n\n",
        C::RESPONSE_ALIAS,
        C::Success::name(config),
        C::Error::name(config)
    ));
}

fn push_operation_api_contract_row_for<C>(
    output: &mut String,
    config: &Config,
    endpoint_metadata: fn(OperationApiEndpoint) -> NatsOperationApiEndpointMetadata,
) where
    C: OperationApiContract,
    C::Request: TS,
    C::Success: TS,
    C::Error: TS,
{
    let endpoint = endpoint_metadata(C::ENDPOINT);
    output.push_str(&format!(
        "  {{ name: \"{}\", subject: \"{}\", execution: \"{}\", request: \"{}\", success: \"{}\", error: \"{}\", response: \"{}\" }},\n",
        endpoint.name,
        endpoint.subject,
        endpoint.execution,
        operation_api_request_name_for::<C>(config),
        C::Success::name(config),
        C::Error::name(config),
        C::RESPONSE_ALIAS,
    ));
}

fn push_operation_api_request_map_row_for<C>(
    output: &mut String,
    config: &Config,
    endpoint_metadata: fn(OperationApiEndpoint) -> NatsOperationApiEndpointMetadata,
) where
    C: OperationApiContract,
    C::Request: TS,
{
    let endpoint = endpoint_metadata(C::ENDPOINT);
    output.push_str(&format!(
        "  \"{}\": {};\n",
        endpoint.name,
        operation_api_request_name_for::<C>(config),
    ));
}

fn push_operation_api_response_map_row_for<C>(
    output: &mut String,
    endpoint_metadata: fn(OperationApiEndpoint) -> NatsOperationApiEndpointMetadata,
) where
    C: OperationApiContract,
{
    let endpoint = endpoint_metadata(C::ENDPOINT);
    output.push_str(&format!(
        "  \"{}\": {};\n",
        endpoint.name,
        C::RESPONSE_ALIAS,
    ));
}

fn operation_api_request_name_for<C>(config: &Config) -> String
where
    C: OperationApiContract,
    C::Request: TS,
{
    C::REQUEST_ALIAS.map_or_else(|| C::Request::name(config), str::to_owned)
}

#[must_use]
pub fn operation_contract_fixture() -> Value {
    let deploy_target = DeployRequest {
        namespace_id: NamespaceId::try_new("default").expect("valid namespace id"),
        origin: None,
        volumes: std::collections::BTreeMap::new(),
        services: vec![DeployServiceSpec {
            keep: None,
            service_id: service_id("svc_api"),
            image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            replicas: ReplicaCount::try_new(2).expect("valid replica count"),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    };
    let accepted = accepted_namespace_operation("op_123", "default", 1);
    let machine_accepted = accepted_machine_operation("op_machine", "machine_2", 7);
    let status = OperationStatusSnapshot::new(OperationStatus::deploy_accepted(
        operation_id("op_123"),
        deploy_target.namespace_id.clone(),
        service_id("svc_api"),
        None,
        event_sequence(1),
    ));
    let replay_page = OperationEventReplayPage {
        events: vec![ReplayedOperationEvent {
            sequence: event_sequence(1),
            recorded_at_unix_ms: OperationEventRecordedAtUnixMs::try_new(1_784_116_800_123)
                .expect("valid recorded-at timestamp"),
            event: OperationEvent::DeploySubmitted {
                operation_id: operation_id("op_123"),
                reservation_id: Some(DeployReservationId::first()),
                target: deploy_target.clone(),
            },
        }],
        cursor: OperationEventReplayCursor::More {
            next_start_sequence: event_sequence(2),
        },
    };
    let attempt_id = CloudBootstrapAttemptId::try_new("pcba_123").expect("valid attempt id");
    let machine = cloud_machine_facts();
    let trusted_nats = trusted_nats();
    let join_secret_delivery = machine_join_secret_delivery();

    json!({
        "deploy_submit_request": value(DeploySubmitRequest {
            registry_credentials: std::collections::BTreeMap::new(),
            idempotency_key: OperationIdempotencyKey::try_new("idem_deploy_123")
                .expect("valid idempotency key"),
            reservation_id: DeployReservationId::first(),
            target: deploy_target,
        }),
        "deploy_reserve_request": value(DeployReserveRequest {
            namespace_id: NamespaceId::try_new("default").expect("valid namespace id"),
        }),
        "deploy_reserve_response": value(DeployReserveResponse::Ok {
            value: DeployReserved {
                reservation_id: DeployReservationId::first(),
                expires_at: DeployReservationExpiresAt::try_new(4_102_444_800)
                    .expect("valid expiration"),
            },
        }),
        "ops_watch_request": value(OperationEventReplayRequest {
            operation_id: operation_id("op_123"),
            start_sequence: event_sequence(1),
            limit: OperationEventReplayLimit::try_new(100).expect("valid replay limit"),
        }),
        "accepted_operation": value(accepted.clone()),
        "deploy_submit_response": value(DeploySubmitResponse::Ok { value: accepted }),
        "init_first_machine_activate_request": value(InitFirstMachineActivateRequest {
            machine_id: machine_id("core_1"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            automatic_hostname_configuration: AutomaticHostnameConfiguration::Ployz,
            ployz_dns_target: PloyzDnsTargetIntent::Enabled,
        }),
        "init_first_machine_activate_response": value(InitFirstMachineActivateResponse::Ok {
            value: InitFirstMachineActivated {
                operation_id: operation_id("op_init_core_1"),
                machine_id: machine_id("core_1"),
            },
        }),
        "machine_add_request": value(MachineAddRequest {
            operation_id: operation_id("op_machine"),
            idempotency_key: OperationIdempotencyKey::try_new("idem_machine")
                .expect("valid idempotency key"),
            machine_id: machine_id("machine_2"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            host_port_assurance: HostPortAssurance::Keeper,
        }),
        "machine_add_response": value(MachineAddResponse::Ok {
            value: MachineAddAccepted {
                accepted: machine_accepted,
                machine_id: machine_id("machine_2"),
                bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
                    .expect("valid bootstrap url"),
                join_bundle: machine_join_bundle(),
                join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
                join_secret_delivery: machine_join_secret_delivery(),
            },
        }),
        "machine_update_request": value(MachineUpdateRequest {
            operation_id: operation_id("op_machine_update"),
            machine_id: machine_id("machine_2"),
            target_version: InstallArtifactVersion::try_new("0.2.0").expect("valid version"),
        }),
        "machine_update_response": value(MachineUpdateResponse::Ok {
            value: accepted_machine_operation("op_machine_update", "machine_2", 9),
        }),
        "machine_join_redeem_request": value(MachineJoinRedeemRequest {
            join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
        }),
        "machine_join_redeem_response": value(MachineJoinRedeemResponse::Ok {
            value: MachineJoinRedeemed {
                operation_id: operation_id("op_machine"),
                machine_id: machine_id("machine_2"),
                name: MachineName::try_new("edge_2").expect("valid machine name"),
                roles: InstallRolePolicy::install_all().without_gateway(),
                host_port_assurance: HostPortAssurance::External,
                endpoint_subnet: ployz_core::network::MachineEndpointSubnet::try_new("10.198.2.0/24").expect("valid subnet"),
                join_bundle: machine_join_bundle(),
                secret_delivery: machine_join_secret_delivery(),
                joined_at: JoinTokenRedeemedAt::try_new(60).expect("valid redeemed timestamp"),
                last_event_sequence: event_sequence(8),
                result: MachineJoinRedeemResult::Joined,
            },
        }),
        "cloud_bootstrap_session_create_request": value(CloudBootstrapSessionCreateRequest {
            attempt_id: attempt_id.clone(),
            client: CloudBootstrapClientInfo::current("0.1.0"),
            machine: machine.clone(),
        }),
        "cloud_bootstrap_token_redeem_request": value(CloudBootstrapTokenRedeemRequest {
            attempt_id: attempt_id.clone(),
            client: CloudBootstrapClientInfo::current("0.1.0"),
            machine: machine.clone(),
        }),
        "cloud_bootstrap_session_created": value(CloudBootstrapSessionCreated {
            browser_url: "https://cloud.ployz.com/bootstrap/pcbsess_123".to_owned(),
            user_code: "PLOZ-1234".to_owned(),
            session_secret: CloudBootstrapSessionSecret::try_new("pcbsess_secret_123")
                .expect("valid session secret"),
            poll_after_seconds: 2,
            expires_at_unix_seconds: 1_893_456_000,
        }),
        "cloud_bootstrap_session_poll_request": value(CloudBootstrapSessionPollRequest {
            attempt_id: attempt_id.clone(),
            session_secret: CloudBootstrapSessionSecret::try_new("pcbsess_secret_123")
                .expect("valid session secret"),
            machine: machine.clone(),
        }),
        "cloud_bootstrap_decision": value(CloudBootstrapDecision::Ready {
            envelope: Box::new(CloudBootstrapEnvelope {
                attempt_id: attempt_id.clone(),
                redemption_id: CloudBootstrapRedemptionId::try_new("pcbr_123")
                    .expect("valid redemption id"),
                callback_url: "https://cloud.ployz.com/api/bootstrap/callback".to_owned(),
                callback_token: CloudBootstrapCallbackToken::try_new("pcbc_abc123")
                    .expect("valid callback token"),
                intent: CloudBootstrapIntent::Joiner {
                    joiner: Box::new(CloudJoinerBootstrap {
                        runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new(
                            "tls://203.0.113.10:4222",
                        )
                        .expect("valid nats url"),
                        trusted_nats: trusted_nats.clone(),
                        join_token: MachineJoinToken::try_new("join_once_123")
                            .expect("valid join token"),
                        join_secret_delivery,
                    }),
                },
            }),
        }),
        "cloud_bootstrap_callback_request": value(CloudBootstrapCallbackRequest {
            attempt_id,
            redemption_id: CloudBootstrapRedemptionId::try_new("pcbr_123")
                .expect("valid redemption id"),
            outcome: CloudBootstrapOutcome::FounderSucceeded {
                result: CloudFounderBootstrapResult {
                    machine_id: machine_id("core_1"),
                    runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new(
                        "tls://203.0.113.10:4222",
                    )
                    .expect("valid nats url"),
                    trusted_nats,
                },
            },
        }),
        "cloud_bootstrap_callback_accepted": value(CloudBootstrapCallbackAccepted {
            accepted_at_unix_seconds: 1_893_456_060,
        }),
        "operation_status_snapshot": value(status.clone()),
        "ops_status_response": value(OpsStatusResponse::Ok { value: status }),
        "operation_event_replay_page": value(replay_page.clone()),
        "ops_watch_response": value(OpsWatchResponse::Ok { value: replay_page }),
        "ops_status_error_response": value(OpsStatusResponse::DomainError {
            error: OpsStatusError::NoSuchOperation {
                operation_id: operation_id("op_missing"),
            },
        }),
    })
}

fn value<T: Serialize>(value: T) -> Value {
    serde_json::to_value(value).expect("operation contract fixture value serializes")
}

fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

fn machine_id(value: &str) -> MachineId {
    MachineId::try_new(value).expect("valid machine id")
}

fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

fn accepted_namespace_operation(
    id: &str,
    namespace_id: &str,
    start_sequence: u64,
) -> AcceptedOperation {
    AcceptedOperation {
        operation_id: operation_id(id),
        watch_subject: format!("plz.v1.progress.namespace.{namespace_id}.operation.{id}.>"),
        start_sequence: event_sequence(start_sequence),
    }
}

fn accepted_machine_operation(
    id: &str,
    machine_id: &str,
    start_sequence: u64,
) -> AcceptedOperation {
    AcceptedOperation {
        operation_id: operation_id(id),
        watch_subject: format!("plz.v1.progress.machine.{machine_id}.operation.{id}.>"),
        start_sequence: event_sequence(start_sequence),
    }
}

fn trusted_nats() -> MachineJoinTrustedNats {
    MachineJoinTrustedNats {
        ca_pem: NatsCaCertificatePem::try_new(
            "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
        )
        .expect("valid ca pem"),
    }
}

fn cloud_machine_facts() -> CloudBootstrapMachineFacts {
    CloudBootstrapMachineFacts {
        hostname: Some("web-01".to_owned()),
        os: "linux".to_owned(),
        arch: "x86_64".to_owned(),
        candidate_runtime_nats_url: Some(
            MachineJoinRuntimeNatsUrl::try_new("tls://203.0.113.10:4222").expect("valid nats url"),
        ),
    }
}

fn machine_join_bundle() -> MachineJoinBundle {
    MachineJoinBundle {
        material: MachineJoinMaterial {
            cluster_name: MachineJoinClusterName::try_new("prod").expect("valid cluster name"),
            dataplane_endpoint_supernet: ployz_core::network::MachineEndpointSupernet::default_v1(),
            runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
                .expect("valid runtime nats url"),
            trusted_nats: trusted_nats(),
            recovery_key_wrapped: WrappedCaKey::new(vec![1, 2, 3]),
            core_seeds_wrapped: WrappedCoreSeeds::new(vec![4, 5, 6]),
            ployzd: machine_join_artifact("/tmp/ployzd", "/usr/local/bin/ployzd"),
            ebpf_bytecode: machine_join_artifact(
                "/tmp/ployz-ebpf-tc",
                "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
            ),
            ebpf_ctl: machine_join_artifact("/tmp/ployz-ebpf-ctl", "/usr/local/bin/ployz-ebpf-ctl"),
        },
    }
}

fn machine_join_artifact(source: &str, install_path: &str) -> InstallArtifactSpec {
    InstallArtifactSpec {
        version: InstallArtifactVersion::try_new("0.1.0").expect("valid artifact version"),
        source: InstallArtifactSource::try_new(source).expect("valid artifact source"),
        sha256: InstallSha256Digest::try_new(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("valid artifact digest"),
        install_path: AbsoluteInstallPath::try_new(install_path)
            .expect("valid artifact install path"),
    }
}

fn machine_join_secret_delivery() -> MachineJoinSecretDelivery {
    MachineJoinSecretDelivery {
        nats_credentials: NatsUserSeed::try_new(
            "SUAIZ5LKGG2Y4WC7ZPKS46LSLLJQIFTO6KMSWSU2VN3TC7YRRIKH5WRXJQ",
        )
        .expect("valid nats credentials"),
    }
}
