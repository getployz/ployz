//! TypeScript contract export owned by the Rust SDK type crate.

use crate::operation_api::OperationApiContract;
use crate::{
    AbsoluteInstallPath, AcceptedOperation, AcmeChallengeToken, AcmeChallengeTtlSeconds,
    AcmeChallengeValue, AcmeHttp01Challenge, ActiveCertState, ActiveMachineState,
    ActiveServiceCommitFailure, ActiveServiceCommitRequest, ActiveServiceState,
    ArtifactUnavailableReason, BackupArtifact, BackupArtifactKind, BackupArtifactLocation,
    BackupBundle, BackupCreateError, BackupCreateRequest, BackupItem, BackupManifest,
    BackupManifestVersion, BackupOperationFailure, BackupOperationState, BackupPolicy,
    BackupRestoreSource, BackupRunningStage, BackupScopeEntry, BackupTarget,
    BackupTargetValidationFailure, BackupTargetValidationField, BootstrapMaterialFailure,
    CLOUD_BOOTSTRAP_PROTOCOL_VERSION, CancellationReason, CertBundleRef, CertId,
    CertOperationFailure, CertOperationState, CertRunningStage, CertValidAt, CertValidityWindow,
    CloudBootstrapCallbackAccepted, CloudBootstrapCallbackRequest, CloudBootstrapCallbackToken,
    CloudBootstrapClientInfo, CloudBootstrapDecision, CloudBootstrapEnvelope,
    CloudBootstrapFailure, CloudBootstrapIntent, CloudBootstrapMachineFacts, CloudBootstrapOutcome,
    CloudBootstrapRedemptionId, CloudBootstrapRejection, CloudBootstrapReleaseSelection,
    CloudBootstrapSessionCreateRequest, CloudBootstrapSessionCreated,
    CloudBootstrapSessionPollRequest, CloudBootstrapSessionSecret, CloudBootstrapToken,
    CloudBootstrapTokenRedeemRequest, CloudFounderBootstrap, CloudFounderBootstrapResult,
    CloudJoinerBootstrap, CloudJoinerBootstrapResult, ContainerId, ControlPlaneKvSnapshot,
    DataplaneMember, DataplanePrepareProviderReport, DataplaneProviderKind, DeployCleanupContainer,
    DeployCleanupFailure, DeployCompletionOutcome, DeployOperationFailure, DeployOperationState,
    DeployPlan, DeployPlanStep, DeployRequest, DeployRoute, DeployRunningStage, DeploySubmitError,
    DeploySubmitRequest, DnsRole, EbpfForwardingReady, EbpfForwardingReadyEvidence,
    EventReplayFailure, EventSequence, ExpectedActiveService, FailureMessage,
    FirstMachineInstallArtifacts, FirstMachineInstallSpec, GatewayRole, GatewayServingStatus,
    GatewayStatusObservation, HealthCheckFailure, ImageReference, InitFirstMachineActivateError,
    InitFirstMachineActivateRequest, InitFirstMachineActivated, InstallArtifactSource,
    InstallArtifactSpec, InstallArtifactVersion, InstallRolePolicy, InstallSha256Digest,
    IssuedJoinToken, JoinTokenExpiresAt, JoinTokenFingerprint, JoinTokenRedeemedAt,
    KvBucketSnapshot, KvEntrySnapshot, LogsTailError, LogsTailLines, LogsTailRequest,
    LogsTailResult, LogsTailUnavailableSource, MAX_LOGS_TAIL_LINES,
    MAX_OPERATION_EVENT_REPLAY_LIMIT, MachineAddAccepted, MachineAddError, MachineAddFailure,
    MachineAddOperationState, MachineAddOperationStateName, MachineAddRequest,
    MachineAddUnavailableSource, MachineBootstrapUrl, MachineCredentialProvisioningStep,
    MachineEndpointSubnet, MachineId, MachineInspectError, MachineInspectRequest,
    MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial, MachineJoinRedeemError,
    MachineJoinRedeemRequest, MachineJoinRedeemResult, MachineJoinRedeemUnavailableSource,
    MachineJoinRedeemed, MachineJoinReportError, MachineJoinReportFailure,
    MachineJoinReportOutcome, MachineJoinReportRequest, MachineJoinReportUnavailableSource,
    MachineJoinReported, MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinTemplate,
    MachineJoinToken, MachineJoinTrustedNats, MachineListError, MachineListRequest,
    MachineListResult, MachineName, MachinePublicIpObservation, MachineQueryUnavailableSource,
    MachineReadinessCheck, MachineReadinessEvidence, MachineSnapshot, ManagedContainerKind,
    NatsServerInstallSpec, NatsUserPublicKey, OperationApiResponse, OperationEvent,
    OperationEventReplayCursor, OperationEventReplayLimit, OperationEventReplayPage,
    OperationEventReplayRequest, OperationId, OperationIdempotencyKey, OperationStatus,
    OperationStatusSnapshot, OperationSubject, OperationSubmitClockFailure,
    OperationSubmitEventFailure, OperationSubmitStatusFailure, OperationSubmitUnavailableSource,
    OperatorHint, OpsStatusError, OpsStatusRequest, OpsStatusUnavailableSource, OpsWatchError,
    OpsWatchUnavailableSource, ReplayedOperationEvent, ReplicaCount, ReplicaSlot, RestoreStep,
    RetainedArtifact, RevisionId, RouteCutoverFailureReason, RouteHostname, RoutePort, RouteTarget,
    S3AddressingStyle, S3BackupRestoreSource, S3BackupTarget, ServiceId, ServiceInspectError,
    ServiceInspectRequest, ServiceListError, ServiceListRequest, ServiceListResult,
    ServiceQueryUnavailableSource, ServiceSnapshot, StatusReadFailure, StepId,
    WireGuardEbpfComponent, WireGuardEbpfMachineReady, WireGuardEbpfPrepareReport,
    WireGuardEbpfReady, WireGuardPublicKey, WireGuardReady, WireGuardReadyEvidence,
};
use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
use ployz_core::subjects::OperationApiEndpointExecution;
use ts_rs::{Config, TS};

#[must_use]
pub fn generated_typescript() -> String {
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

    push_contract_decls(&mut output, &config);
    push_operation_api_contracts(&mut output, &config);

    output
}

macro_rules! exported_types {
    ($macro:ident) => {
        $macro!(
            OperationId,
            OperationIdempotencyKey,
            EventSequence,
            ServiceId,
            RevisionId,
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
            CloudBootstrapClientInfo,
            CloudBootstrapMachineFacts,
            CloudBootstrapSessionCreateRequest,
            CloudBootstrapSessionCreated,
            CloudBootstrapSessionPollRequest,
            CloudBootstrapTokenRedeemRequest,
            CloudBootstrapDecision,
            CloudBootstrapRejection,
            CloudBootstrapEnvelope,
            CloudBootstrapReleaseSelection,
            CloudBootstrapIntent,
            CloudFounderBootstrap,
            CloudJoinerBootstrap,
            CloudBootstrapCallbackRequest,
            CloudBootstrapOutcome,
            CloudFounderBootstrapResult,
            CloudJoinerBootstrapResult,
            CloudBootstrapFailure,
            CloudBootstrapCallbackAccepted,
            ImageReference,
            ReplicaCount,
            ReplicaSlot,
            DeployRequest,
            DeployRoute,
            DeployPlan,
            DeployCleanupContainer,
            DeployCleanupFailure,
            ManagedContainerKind,
            DeployPlanStep,
            OperationEventReplayLimit,
            OperationEventReplayRequest,
            OperationEventReplayPage,
            OperationEventReplayCursor,
            ReplayedOperationEvent,
            OperationStatus,
            OperationStatusSnapshot,
            OperationSubject,
            MachineAddOperationState,
            MachineAddOperationStateName,
            MachineAddFailure,
            MachineCredentialProvisioningStep,
            MachineReadinessEvidence,
            MachineReadinessCheck,
            GatewayRole,
            DnsRole,
            InstallRolePolicy,
            DeployOperationState,
            DeployRunningStage,
            DeployCompletionOutcome,
            CertOperationState,
            CertRunningStage,
            BackupOperationState,
            BackupRunningStage,
            OperationEvent,
            FailureMessage,
            CancellationReason,
            RouteHostname,
            RoutePort,
            RouteTarget,
            RetainedArtifact,
            HealthCheckFailure,
            MachineEndpointSubnet,
            DataplaneMember,
            DataplaneProviderKind,
            DataplanePrepareProviderReport,
            WireGuardEbpfComponent,
            WireGuardEbpfPrepareReport,
            WireGuardEbpfMachineReady,
            WireGuardEbpfReady,
            WireGuardPublicKey,
            WireGuardReady,
            WireGuardReadyEvidence,
            EbpfForwardingReady,
            EbpfForwardingReadyEvidence,
            ActiveServiceCommitFailure,
            ArtifactUnavailableReason,
            RouteCutoverFailureReason,
            DeployOperationFailure,
            CertOperationFailure,
            BackupOperationFailure,
            BackupItem,
            BackupPolicy,
            BackupScopeEntry,
            BackupTargetValidationField,
            BackupTargetValidationFailure,
            S3AddressingStyle,
            S3BackupTarget,
            BackupTarget,
            S3BackupRestoreSource,
            BackupRestoreSource,
            BackupArtifactKind,
            BackupArtifactLocation,
            BackupArtifact,
            BackupManifestVersion,
            KvEntrySnapshot,
            KvBucketSnapshot,
            ControlPlaneKvSnapshot,
            BackupBundle,
            BackupManifest,
            RestoreStep,
            OperatorHint,
            CertValidAt,
            CertValidityWindow,
            CertBundleRef,
            AcmeChallengeToken,
            AcmeChallengeValue,
            AcmeChallengeTtlSeconds,
            AcmeHttp01Challenge,
            ActiveCertState,
            ExpectedActiveService,
            ActiveMachineState,
            ActiveServiceState,
            ActiveServiceCommitRequest,
            MachinePublicIpObservation,
            GatewayServingStatus,
            GatewayStatusObservation,
            MachineSnapshot,
            InitFirstMachineActivateRequest,
            InitFirstMachineActivated,
            InitFirstMachineActivateError,
            DeploySubmitRequest,
            BackupCreateRequest,
            MachineAddRequest,
            MachineAddAccepted,
            MachineListRequest,
            MachineListResult,
            MachineListError,
            MachineInspectRequest,
            MachineInspectError,
            MachineQueryUnavailableSource,
            ServiceListRequest,
            ServiceListResult,
            ServiceSnapshot,
            ServiceListError,
            ServiceInspectRequest,
            ServiceInspectError,
            ServiceQueryUnavailableSource,
            LogsTailLines,
            LogsTailRequest,
            LogsTailResult,
            LogsTailError,
            LogsTailUnavailableSource,
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
            NatsCaCertificatePem,
            MachineJoinTrustedNats,
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
            MachineJoinRedeemUnavailableSource,
            MachineJoinReportRequest,
            MachineJoinReportOutcome,
            MachineJoinReportFailure,
            MachineJoinReported,
            MachineJoinReportError,
            MachineJoinReportUnavailableSource,
            OpsStatusRequest,
            AcceptedOperation,
            OperationApiResponse<AcceptedOperation, DeploySubmitError>,
            DeploySubmitError,
            BackupCreateError,
            OperationSubmitUnavailableSource,
            OperationSubmitStatusFailure,
            OperationSubmitEventFailure,
            OperationSubmitClockFailure,
            MachineAddError,
            MachineAddUnavailableSource,
            BootstrapMaterialFailure,
            OpsStatusError,
            OpsStatusUnavailableSource,
            StatusReadFailure,
            OpsWatchError,
            OpsWatchUnavailableSource,
            EventReplayFailure
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

fn push_operation_api_contracts(output: &mut String, config: &Config) {
    macro_rules! push_aliases {
        ($($contract:ty),+ $(,)?) => {
            $(push_operation_api_aliases_for::<$contract>(output, config);)+
        };
    }
    crate::operation_api_contracts!(push_aliases);

    output.push_str("export const OPERATION_API_CONTRACTS = [\n");
    macro_rules! push_rows {
        ($($contract:ty),+ $(,)?) => {
            $(push_operation_api_contract_row_for::<$contract>(output, config);)+
        };
    }
    crate::operation_api_contracts!(push_rows);
    output.push_str("] as const;\n");
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

fn push_operation_api_contract_row_for<C>(output: &mut String, config: &Config)
where
    C: OperationApiContract,
    C::Request: TS,
    C::Success: TS,
    C::Error: TS,
{
    output.push_str(&format!(
        "  {{ name: \"{}\", subject: \"{}\", execution: \"{}\", request: \"{}\", success: \"{}\", error: \"{}\", response: \"{}\" }},\n",
        C::ENDPOINT.name(),
        C::ENDPOINT.subject(),
        operation_api_execution_name(C::ENDPOINT.execution()),
        operation_api_request_name_for::<C>(config),
        C::Success::name(config),
        C::Error::name(config),
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

const fn operation_api_execution_name(execution: OperationApiEndpointExecution) -> &'static str {
    match execution {
        OperationApiEndpointExecution::AcceptsOperation => "accepts_operation",
        OperationApiEndpointExecution::MutatesOperation => "mutates_operation",
        OperationApiEndpointExecution::Query => "query",
    }
}
