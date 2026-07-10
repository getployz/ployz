//! TypeScript contract export owned by the Rust SDK type crate.

use crate::operation_api::OperationApiContract;
use crate::{
    AbsoluteInstallPath, AcceptedOperation, AcmeChallengeToken, AcmeChallengeTtlSeconds,
    AcmeChallengeValue, AcmeHttp01Challenge, ActiveCertState, ActiveMachineState,
    ArtifactUnavailableReason, CLOUD_BOOTSTRAP_PROTOCOL_VERSION, CancellationReason, CertBundleRef,
    CertId, CertOperationFailure, CertOperationState, CertRunningStage, CertValidAt,
    CertValidityWindow, CloudBootstrapAttemptId, CloudBootstrapCallbackAccepted,
    CloudBootstrapCallbackRequest, CloudBootstrapCallbackToken, CloudBootstrapClientInfo,
    CloudBootstrapDecision, CloudBootstrapDecisionFailure, CloudBootstrapEnvelope,
    CloudBootstrapFailure, CloudBootstrapIntent, CloudBootstrapMachineFacts, CloudBootstrapOutcome,
    CloudBootstrapRedemptionId, CloudBootstrapSessionCreateRequest, CloudBootstrapSessionCreated,
    CloudBootstrapSessionPollRequest, CloudBootstrapSessionSecret, CloudFounderBootstrap,
    CloudFounderBootstrapResult, CloudJoinerBootstrap, CloudJoinerBootstrapResult,
    ContainerCommand, ContainerEntrypoint, ContainerHealth, ContainerHealthcheck,
    ContainerHealthcheckTest, ContainerId, ContainerMountPath, ContainerResourceLimits,
    ContainerRestartPolicy, ContainerRuntimeSpec, ContainerRuntimeState, ControlPlaneCommitScope,
    CoreReplaceError, CoreReplaceFailure, CoreReplaceOperationState, CoreReplaceReportError,
    CoreReplaceReportOutcome, CoreReplaceReportRequest, CoreReplaceReported, CoreReplaceRequest,
    DEFAULT_MANAGED_LEASE_TTL_SECONDS, DataplaneMember, DataplaneProviderFailure,
    DeployCleanupContainer, DeployCleanupFailure, DeployCompletionOutcome, DeployOperationFailure,
    DeployOperationState, DeployPlan, DeployPlanStep, DeployRequest, DeployRoute,
    DeployRouteTarget, DeployRunningStage, DeployServicePlan, DeployServiceSpec, DeploySubmitError,
    DeploySubmitRequest, DeploySubmitResponse, DnsRole, EbpfForwardingReady,
    EbpfForwardingReadyEvidence, EnvName, EnvValue, EventSequence, FailureMessage,
    FirstMachineInstallArtifacts, FirstMachineInstallSpec, GatewayRole, GatewayServingStatus,
    GatewayStatusObservation, HealthCheckFailure, HealthcheckDurationNanos, HealthcheckRetries,
    HealthcheckShellCommand, ImageReference, InitFirstMachineActivateError,
    InitFirstMachineActivateRequest, InitFirstMachineActivateResponse, InitFirstMachineActivated,
    InstallArtifactSource, InstallArtifactSpec, InstallArtifactVersion, InstallRolePolicy,
    InstallSha256Digest, InternalServiceName, IssuedJoinToken, JoinTokenExpiresAt,
    JoinTokenFingerprint, JoinTokenRedeemedAt, LeaseBearerToken, LeaseExpiresAt, LeaseIssuedAt,
    LinuxCapability, LogsTailError, LogsTailLines, LogsTailRequest, LogsTailResult,
    LogsTailResultTarget, LogsTailTarget, MAX_LOGS_TAIL_LINES, MAX_OPERATION_EVENT_REPLAY_LIMIT,
    MachineAddAccepted, MachineAddError, MachineAddFailure, MachineAddOperationState,
    MachineAddOperationStateName, MachineAddRequest, MachineAddResponse, MachineBootstrapUrl,
    MachineCredentialProvisioningStep, MachineDiskSpace, MachineEndpointObservation,
    MachineEndpointSubnet, MachineId, MachineInspectError, MachineInspectRequest,
    MachineJoinBundle, MachineJoinClusterName, MachineJoinMaterial, MachineJoinRedeemError,
    MachineJoinRedeemRequest, MachineJoinRedeemResponse, MachineJoinRedeemResult,
    MachineJoinRedeemed, MachineJoinReportError, MachineJoinReportFailure,
    MachineJoinReportOutcome, MachineJoinReportRequest, MachineJoinReported,
    MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinTemplate, MachineJoinToken,
    MachineJoinTrustedNats, MachineLifecycle, MachineLifecycleError, MachineLifecycleFailure,
    MachineLifecycleOperationState, MachineLifecycleRequest, MachineListError, MachineListRequest,
    MachineListResult, MachineName, MachineReadinessCheck, MachineReadinessEvidence,
    MachineSnapshot, MachineSubstrateVersions, MachineTestimony, MachineUpdateError,
    MachineUpdateFailure, MachineUpdateOperationState, MachineUpdateRequest, MachineUpdateResponse,
    MachineUsabilityReason, ManagedCertBundle, ManagedContainerHealthStatus,
    ManagedContainerIdentity, ManagedContainerKind, ManagedContainerObservation,
    ManagedLeaseAcquireRequest, ManagedLeaseAcquired, ManagedLeaseFailureClass, ManagedLeaseName,
    ManagedLeaseOperationFailure, ManagedLeaseOperationState, ManagedLeaseRecord,
    ManagedLeaseRenewed, ManagedLeaseSubject, MemoryBytes, NamespaceId, NamespaceRemoveError,
    NamespaceRemoveFailure, NamespaceRemoveOperationState, NamespaceRemoveRequest,
    NamespaceRemoveRunningStage, NamespaceRevisionEntryId, NamespaceRevisionId, NanoCpus,
    NatsAuthorizedUser, NatsPrincipal, NatsServerInstallSpec, NatsUserPublicKey,
    NetworkRepairError, NetworkRepairFailure, NetworkRepairOperationState, NetworkRepairRequest,
    NetworkRepairRunningStage, NetworkResolveError, NetworkResolveMachineTestimony,
    NetworkResolveRequest, NetworkResolveResult, OperationApiResponse, OperationEvent,
    OperationEventReplayCursor, OperationEventReplayLimit, OperationEventReplayPage,
    OperationEventReplayRequest, OperationId, OperationIdempotencyKey, OperationKind,
    OperationStatus, OperationStatusSnapshot, OperationSubject, OperatorHint, OpsListError,
    OpsListRequest, OpsListResult, OpsStatusError, OpsStatusRequest, OpsStatusResponse,
    OpsWatchError, OpsWatchResponse, PidsLimit, PloyzNativeMeshComponent,
    PloyzNativeMeshMachineReady, PloyzNativeMeshPrepareReport, PloyzNativeMeshReady, PublicUrlMode,
    ReplayedOperationEvent, ReplicaCount, ReplicaSlot, RetainedArtifact, RouteBindingState,
    RouteCutoverFailureReason, RouteHostname, RoutePort, RouteTarget,
    RuntimeDerivedCollectionSource, RuntimeDerivedCollectionStatus, RuntimeProjectionSource,
    RuntimeProjectionSources, RuntimeServiceInstance, RuntimeServiceRelease,
    RuntimeServiceRevision, RuntimeSnapshot, RuntimeSnapshotError, RuntimeSnapshotRequest,
    RuntimeSnapshotResult, ServiceContainerMembership, ServiceContainerTestimony,
    ServiceEnvironment, ServiceId, ServiceInspectError, ServiceInspectRequest, ServiceListError,
    ServiceListRequest, ServiceListResult, ServiceMachineTestimony, ServiceRestartError,
    ServiceRestartFailure, ServiceRestartOperationState, ServiceRestartRequest,
    ServiceRestartRunningStage, ServiceSnapshot, ServiceTestimony, ServiceVolumeMount,
    ServingTargetEntry, StepId, StopGracePeriod, UnusableMachine, VolumeListError,
    VolumeListRequest, VolumeListResult, VolumeName, VolumePinState, VolumeRemoveError,
    VolumeRemoveFailure, VolumeRemoveOperationState, VolumeRemoveRequest, VolumeRemoveRunningStage,
    VolumeSnapshot, VolumeStatus, WireGuardPublicKey, WireGuardReady, WireGuardReadyEvidence,
    WrappedCaKey, WrappedCoreSeeds,
};
use ployz_core::nats_config::{NatsCaCertificatePem, NatsUserSeed};
use ployz_core::subjects::OperationApiEndpointExecution;
use serde::Serialize;
use serde_json::{Value, json};
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
    output.push_str(&format!(
        "export const DEFAULT_MANAGED_LEASE_TTL_SECONDS = {DEFAULT_MANAGED_LEASE_TTL_SECONDS} as const;\n\n"
    ));

    push_contract_decls(&mut output, &config);
    push_operation_api_contracts(&mut output, &config);

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
            CloudBootstrapSessionSecret,
            CloudBootstrapCallbackToken,
            CloudBootstrapRedemptionId,
            CloudBootstrapAttemptId,
            CloudBootstrapClientInfo,
            CloudBootstrapMachineFacts,
            CloudBootstrapSessionCreateRequest,
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
            ReplicaCount,
            ReplicaSlot,
            EnvName,
            EnvValue,
            ServiceEnvironment,
            ContainerCommand,
            ContainerEntrypoint,
            StopGracePeriod,
            VolumeName,
            ContainerMountPath,
            ServiceVolumeMount,
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
            DeployRequest,
            DeployServiceSpec,
            DeployRoute,
            DeployRouteTarget,
            DeployPlan,
            DeployServicePlan,
            DeployCleanupContainer,
            DeployCleanupFailure,
            ManagedContainerKind,
            ContainerRuntimeState,
            ContainerHealth,
            ManagedContainerHealthStatus,
            ManagedContainerIdentity,
            ManagedContainerObservation,
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
            ServiceRestartOperationState,
            ServiceRestartRunningStage,
            ServiceRestartFailure,
            ManagedLeaseOperationState,
            ManagedLeaseOperationFailure,
            ManagedLeaseFailureClass,
            ManagedLeaseSubject,
            NamespaceRemoveOperationState,
            NamespaceRemoveRunningStage,
            NamespaceRemoveFailure,
            NetworkRepairOperationState,
            NetworkRepairRunningStage,
            NetworkRepairFailure,
            VolumeRemoveOperationState,
            VolumeRemoveRunningStage,
            VolumeRemoveFailure,
            CertOperationState,
            CertRunningStage,
            OperationKind,
            MachineLifecycle,
            MachineLifecycleOperationState,
            MachineLifecycleFailure,
            CoreReplaceOperationState,
            CoreReplaceFailure,
            MachineUsabilityReason,
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
            DataplaneMember,
            DataplaneProviderFailure,
            PloyzNativeMeshComponent,
            PloyzNativeMeshPrepareReport,
            PloyzNativeMeshMachineReady,
            PloyzNativeMeshReady,
            WireGuardPublicKey,
            WireGuardReady,
            WireGuardReadyEvidence,
            EbpfForwardingReady,
            EbpfForwardingReadyEvidence,
            ArtifactUnavailableReason,
            RouteCutoverFailureReason,
            ControlPlaneCommitScope,
            DeployOperationFailure,
            CertOperationFailure,
            OperatorHint,
            CertValidAt,
            CertValidityWindow,
            CertBundleRef,
            ManagedLeaseName,
            LeaseBearerToken,
            LeaseIssuedAt,
            LeaseExpiresAt,
            ManagedLeaseRecord,
            ManagedCertBundle,
            ManagedLeaseAcquireRequest,
            ManagedLeaseAcquired,
            ManagedLeaseRenewed,
            PublicUrlMode,
            AcmeChallengeToken,
            AcmeChallengeValue,
            AcmeChallengeTtlSeconds,
            AcmeHttp01Challenge,
            ActiveCertState,
            ActiveMachineState,
            RouteBindingState,
            ServingTargetEntry,
            MachineEndpointObservation,
            GatewayServingStatus,
            GatewayStatusObservation,
            MachineDiskSpace,
            MachineSnapshot,
            MachineTestimony,
            InitFirstMachineActivateRequest,
            InitFirstMachineActivated,
            InitFirstMachineActivateError,
            DeploySubmitRequest,
            MachineAddRequest,
            MachineAddAccepted,
            MachineListRequest,
            MachineListResult,
            MachineListError,
            MachineInspectRequest,
            MachineInspectError,
            InternalServiceName,
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
            RuntimeSnapshot,
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
            NatsAuthorizedUser,
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
            MachineJoinReported,
            MachineJoinReportError,
            OpsListRequest,
            OpsListResult,
            OpsListError,
            OpsStatusRequest,
            AcceptedOperation,
            OperationApiResponse<AcceptedOperation, DeploySubmitError>,
            DeploySubmitError,
            MachineAddError,
            MachineUpdateOperationState,
            MachineSubstrateVersions,
            MachineUpdateFailure,
            MachineUpdateRequest,
            MachineLifecycleRequest,
            MachineLifecycleError,
            CoreReplaceRequest,
            CoreReplaceError,
            CoreReplaceReportRequest,
            CoreReplaceReportOutcome,
            CoreReplaceReported,
            CoreReplaceReportError,
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
    output.push('\n');
    output.push_str(
        "export type PloyzApiEndpoint = (typeof OPERATION_API_CONTRACTS)[number][\"name\"];\n\n",
    );
    output.push_str("export type OperationApiRequestByEndpoint = {\n");
    macro_rules! push_request_map {
        ($($contract:ty),+ $(,)?) => {
            $(push_operation_api_request_map_row_for::<$contract>(output, config);)+
        };
    }
    crate::operation_api_contracts!(push_request_map);
    output.push_str("};\n\n");
    output.push_str("export type OperationApiResponseByEndpoint = {\n");
    macro_rules! push_response_map {
        ($($contract:ty),+ $(,)?) => {
            $(push_operation_api_response_map_row_for::<$contract>(output);)+
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

fn push_operation_api_request_map_row_for<C>(output: &mut String, config: &Config)
where
    C: OperationApiContract,
    C::Request: TS,
{
    output.push_str(&format!(
        "  \"{}\": {};\n",
        C::ENDPOINT.name(),
        operation_api_request_name_for::<C>(config),
    ));
}

fn push_operation_api_response_map_row_for<C>(output: &mut String)
where
    C: OperationApiContract,
{
    output.push_str(&format!(
        "  \"{}\": {};\n",
        C::ENDPOINT.name(),
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

#[must_use]
pub fn operation_contract_fixture() -> Value {
    let deploy_target = DeployRequest {
        namespace_id: NamespaceId::try_new("default").expect("valid namespace id"),
        services: vec![DeployServiceSpec {
            service_id: service_id("svc_api"),
            image: ImageReference::try_new("ghcr.io/acme/api:rev-2").expect("valid image"),
            replicas: ReplicaCount::try_new(2).expect("valid replica count"),
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            routes: Vec::new(),
        }],
    };
    let accepted = accepted_namespace_operation("op_123", "default", 1);
    let machine_accepted = accepted_machine_operation("op_machine", "machine_2", 7);
    let status = OperationStatusSnapshot::new(OperationStatus::deploy_accepted(
        operation_id("op_123"),
        deploy_target.namespace_id.clone(),
        service_id("svc_api"),
        event_sequence(1),
    ));
    let replay_page = OperationEventReplayPage {
        events: vec![ReplayedOperationEvent {
            sequence: event_sequence(1),
            event: OperationEvent::DeploySubmitted {
                operation_id: operation_id("op_123"),
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
            idempotency_key: OperationIdempotencyKey::try_new("idem_deploy_123")
                .expect("valid idempotency key"),
            target: deploy_target,
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
            public_url_mode: PublicUrlMode::Auto,
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
