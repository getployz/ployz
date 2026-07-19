use ployz_sdk_types::operation_api::OperationApiEndpoint;
use ployz_sdk_types::{
    AcceptedOperation, AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue,
    AcmeHttp01Challenge, ActiveCertState, ActiveCertificateMetadata,
    AutomaticHostnameConfiguration, BuildCancelError, BuildCancelRequest, BuildSubmitError,
    BuildSubmitRequest, CertBundleRef, CertId, CertOperationState, CertRunningStage, CertTextError,
    CertValidAt, CertValidityWindow, CertificateOwner, CloudBootstrapAttemptId,
    CloudBootstrapCallbackAccepted, CloudBootstrapCallbackRequest, CloudBootstrapCallbackToken,
    CloudBootstrapDecision, CloudBootstrapEnvelope, CloudBootstrapIntent, CloudBootstrapOutcome,
    CloudBootstrapRedemptionId, CloudBootstrapSessionCreateRequest, CloudBootstrapSessionCreated,
    CloudBootstrapSessionPollRequest, CloudBootstrapToken, CloudBootstrapTokenRedeemRequest,
    CloudFounderBootstrapResult, ControlCertificateRenewalAttempt,
    ControlCertificateRenewalFailure, ControlCertificateRenewalHealth,
    ControlCertificateRenewalOutcome, ControlHealth, ControlIngressEndpointProjectionHealth,
    ControlNatsAuthorizationHealth, ControlPlaneEpoch, ControlRuntimeProjectionHealth,
    ControlRuntimeProjectionLoopHealth, ControlRuntimeProjectionServiceHealth,
    ControlTaskSupervisorFailure, ControlTaskSupervisorHealth, CoreReplaceError,
    CoreReplaceReportError, CoreReplaceReportRequest, CoreReplaceReported, CoreReplaceRequest,
    CredentialAddError, CredentialAddRequest, CredentialListError, CredentialListRequest,
    CredentialListResult, CredentialRemoveError, CredentialRemoveRequest, DependencyCondition,
    DeployOperationState, DeployPhaseNumber, DeployPhaseNumberError, DeployPreview,
    DeployPreviewError, DeployPreviewRequest, DeployRequest, DeployReservationId,
    DeployReserveError, DeployReserveRequest, DeployReserved, DeployRunningStage,
    DeployServiceSpec, DeploySubmitError, DeploySubmitRequest, DeploySubmitResponse, EventSequence,
    EventSequenceError, GatewayHttpFailure, GatewayProcessAttempt, GatewayProcessHealth,
    GatewayStatusPublishFailure, GatewayWatchFailure, GitSource, HostPortAssurance, ImageReference,
    ImageReferenceError, IngressConfiguration, IngressConfigureError, IngressConfigureRequest,
    IngressEndpointProjectionIdentity, InitFirstMachineActivateError,
    InitFirstMachineActivateRequest, InitFirstMachineActivated, InstallContractError,
    InstallRolePolicy, LogsTailError, LogsTailRequest, LogsTailResult,
    MAX_OPERATION_EVENT_REPLAY_LIMIT, MachineAddAccepted, MachineAddError, MachineAddRequest,
    MachineAddResponse, MachineBootstrapUrl, MachineInspectError, MachineInspectRequest,
    MachineJoinBundle, MachineJoinMaterial, MachineJoinRedeemError, MachineJoinRedeemRequest,
    MachineJoinRedeemResponse, MachineJoinRedeemResult, MachineJoinRedeemed,
    MachineJoinReportError, MachineJoinReportRequest, MachineJoinReported,
    MachineJoinRuntimeNatsUrl, MachineJoinSecretDelivery, MachineJoinTemplate, MachineJoinToken,
    MachineJoinTrustedNats, MachineListError, MachineListRequest, MachineListResult, MachineName,
    MachineSnapshot, MachineStoragePrepareError, MachineStoragePrepareRequest, MachineUpdateError,
    MachineUpdateRequest, MachineUpdateResponse, ManagedDnsReconcileSubject, ManagedLeaseName,
    NamespaceId, NamespaceRemoveError, NamespaceRemoveRequest, NatsCaCertificatePem, NatsUserSeed,
    NetworkRepairError, NetworkRepairRequest, NetworkResolveError, NetworkResolveRequest,
    NetworkResolveResult, NetworkStatusError, NetworkStatusRequest, NetworkStatusResult,
    NonEmptyTextError, OperationApiResponse, OperationEvent, OperationEventReplayCursor,
    OperationEventReplayLimit, OperationEventReplayLimitError, OperationEventReplayPage,
    OperationEventReplayRequest, OperationIdempotencyKey, OperationOutcome, OperationStatus,
    OperationStatusSnapshot, OperationSubject, OpsListError, OpsListRequest, OpsListResult,
    OpsStatusError, OpsStatusRequest, OpsStatusResponse, OpsWatchResponse, PloyzDnsTargetIntent,
    ReplicaCount, ReplicaCountError, RouteHostname, RouteHostnameError, RoutePort, RoutePortError,
    RuntimeSnapshotError, RuntimeSnapshotRequest, RuntimeSnapshotResult, ServiceDependency,
    ServiceId, ServiceInspectError, ServiceInspectRequest, ServiceListError, ServiceListRequest,
    ServiceListResult, ServiceMode, ServiceRestartError, ServiceRestartRequest, ServiceSnapshot,
    SubjectTokenError, VolumeCreateError, VolumeCreateRequest, VolumeKind, VolumeListError,
    VolumeListRequest, VolumeListResult, VolumeName, VolumeRemoveError, VolumeRemoveRequest,
    VolumeSnapshot, VolumeStatus, VolumeTestimony,
    operation_api::{
        BuildCancelApi, BuildSubmitApi, CoreReplaceApi, CoreReplaceReportApi, CredentialAddApi,
        CredentialListApi, CredentialRemoveApi, DeployPreviewApi, DeployReserveApi,
        DeploySubmitApi, IngressConfigureApi, InitFirstMachineActivateApi, LogsTailApi,
        MachineAddApi, MachineInspectApi, MachineJoinRedeemApi, MachineJoinReportApi,
        MachineListApi, MachineStoragePrepareApi, MachineUpdateApi, NamespaceRemoveApi,
        NetworkRepairApi, NetworkResolveApi, NetworkStatusApi, OperationApiContract, OpsListApi,
        OpsStatusApi, OpsWatchApi, RuntimeSnapshotApi, ServiceInspectApi, ServiceListApi,
        ServiceRestartApi, VolumeCreateApi, VolumeListApi, VolumeRemoveApi,
    },
};
use ts_rs::{Config, TS};

#[test]
fn git_source_typescript_subdir_is_optional() {
    let declaration = GitSource::decl(&Config::default());
    assert!(
        declaration.contains("subdir?: BuildContextPath"),
        "{declaration}"
    );
    assert!(!declaration.contains("subdir: BuildContextPath | null"));
}

#[test]
fn sdk_exports_core_wire_types() {
    let service_id = ServiceId::try_new("svc_api").expect("valid service id");
    let subject = OperationSubject::Deploy {
        service_id: service_id.clone(),
    };
    let state = DeployOperationState::Accepted;
    let running = DeployRunningStage::ServingTargetCommit;
    let deploy = DeployRequest {
        namespace_id: NamespaceId::try_new("default").expect("valid namespace id"),
        origin: None,
        volumes: std::collections::BTreeMap::new(),
        services: vec![DeployServiceSpec {
            keep: None,
            service_id: service_id.clone(),
            image: ImageReference::try_new("ghcr.io/acme/api:rev-1").expect("valid image"),
            image_source: ployz_core::deploy::ImageSource::Registry,
            mode: ServiceMode::Replicated {
                replicas: ReplicaCount::try_new(1).expect("valid replica count"),
            },
            runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
            pre_start: None,
            depends_on: Vec::new(),
            routes: Vec::new(),
        }],
    };
    let status = OperationStatus::deploy_accepted(
        ployz_sdk_types::OperationId::try_new("op_123").expect("valid operation id"),
        deploy.namespace_id.clone(),
        service_id,
        None,
        EventSequence::try_new(1).expect("valid event sequence"),
    );
    let replay_request = OperationEventReplayRequest {
        operation_id: ployz_sdk_types::OperationId::try_new("op_123").expect("valid operation id"),
        start_sequence: EventSequence::try_new(1).expect("valid event sequence"),
        limit: OperationEventReplayLimit::try_new(100).expect("valid replay limit"),
    };
    let replay_page = OperationEventReplayPage::caught_up(Vec::new());
    let terminal_replay_page =
        OperationEventReplayPage::terminal(Vec::new(), OperationOutcome::Failed);
    let dependency = ServiceDependency {
        service_id: ServiceId::try_new("svc_database").expect("valid service id"),
        condition: DependencyCondition::Healthy,
    };

    assert_eq!(
        serde_json::to_string(&subject).expect("subject serializes"),
        r#"{"kind":"deploy","service_id":"svc_api"}"#
    );
    assert_eq!(
        serde_json::to_string(&state).expect("state serializes"),
        r#"{"state":"accepted"}"#
    );
    assert_eq!(
        serde_json::to_string(&running).expect("running state serializes"),
        r#""serving_target_commit""#
    );
    assert_eq!(
        serde_json::to_string(&status).expect("status serializes"),
        r#"{"kind":"deploy","id":"op_123","namespace_id":"default","service_id":"svc_api","state":{"state":"accepted"},"last_event_sequence":"1"}"#
    );
    assert_eq!(replay_request.limit.get(), 100);
    assert_eq!(replay_page.cursor, OperationEventReplayCursor::CaughtUp);
    assert_eq!(
        serde_json::to_string(&terminal_replay_page).expect("terminal replay page serializes"),
        r#"{"events":[],"cursor":{"state":"terminal","outcome":"failed"}}"#
    );
    assert_eq!(
        serde_json::to_string(&dependency).expect("dependency serializes"),
        r#"{"service_id":"svc_database","condition":"healthy"}"#
    );
}

#[test]
fn sdk_exports_volume_testimony_wire_types() {
    let snapshot = VolumeSnapshot {
        namespace_id: NamespaceId::try_new("default").expect("valid namespace id"),
        volume_name: VolumeName::try_new("data").expect("valid volume name"),
        machine_id: ployz_sdk_types::MachineId::try_new("machine_1").expect("valid machine id"),
        kind: VolumeKind::Plain,
        referencing_services: vec![
            ServiceId::try_new("api").expect("valid service id"),
            ServiceId::try_new("worker").expect("valid service id"),
        ],
        testimony: VolumeTestimony::Available {
            used_bytes: 4_096,
            last_write_unix_seconds: 1_700_000_000,
        },
        status: VolumeStatus::InUse,
    };
    let encoded = serde_json::to_string(&snapshot).expect("volume snapshot serializes");
    assert_eq!(
        encoded,
        r#"{"namespace_id":"default","volume_name":"data","machine_id":"machine_1","kind":{"kind":"plain"},"referencing_services":["api","worker"],"testimony":{"status":"available","used_bytes":4096,"last_write_unix_seconds":1700000000},"status":"in_use"}"#
    );
    assert_eq!(
        serde_json::from_str::<VolumeSnapshot>(&encoded).expect("volume snapshot deserializes"),
        snapshot
    );
    assert!(
        serde_json::from_str::<VolumeTestimony>(
            r#"{"status":"available","used_bytes":4096,"last_write_unix_seconds":1700000000,"message":"private path"}"#
        )
        .is_err()
    );
    assert_eq!(
        serde_json::to_string(&VolumeTestimony::Unavailable)
            .expect("unavailable testimony serializes"),
        r#"{"status":"unavailable"}"#
    );
    assert_eq!(
        serde_json::to_string(&VolumeTestimony::NoAnswer).expect("no-answer testimony serializes"),
        r#"{"status":"no_answer"}"#
    );

    assert_wire_type::<VolumeTestimony>();
    assert_wire_type::<VolumeSnapshot>();
}

#[test]
fn sdk_exports_cert_wire_types() {
    let cert_id = CertId::try_new("cert_api").expect("valid cert id");
    let active_cert = ActiveCertState {
        cert_id: cert_id.clone(),
        hostname: RouteHostname::try_new("api.example.com").expect("valid hostname"),
        bundle_ref: CertBundleRef::try_new(
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef:/var/lib/ployz/certs/cert_api.pem",
        )
            .expect("valid bundle ref"),
        validity: CertValidityWindow::try_new(valid_at(1_700_000_000), valid_at(1_707_776_000))
            .expect("valid validity window"),
    };
    let status = OperationStatus::Cert {
        id: ployz_sdk_types::OperationId::try_new("op_cert").expect("valid operation id"),
        cert_id: cert_id.clone(),
        state: CertOperationState::Running {
            stage: CertRunningStage::ValidationStarted,
        },
        last_event_sequence: EventSequence::try_new(3).expect("valid event sequence"),
    };
    let event = OperationEvent::CertCompleted {
        operation_id: ployz_sdk_types::OperationId::try_new("op_cert").expect("valid operation id"),
        certificate: ActiveCertificateMetadata {
            owner: CertificateOwner::PloyzAutomaticNamespace,
            active: active_cert,
        },
    };
    let challenge = AcmeHttp01Challenge::try_new(
        RouteHostname::try_new("api.example.com").expect("valid hostname"),
        AcmeChallengeToken::try_new("token_123").expect("valid challenge token"),
        AcmeChallengeValue::try_new("token_123.thumbprint_456").expect("valid challenge value"),
        AcmeChallengeTtlSeconds::try_new(60).expect("valid challenge ttl"),
    )
    .expect("valid challenge");

    assert_eq!(
        serde_json::to_string(&OperationSubject::Cert { cert_id }).expect("subject serializes"),
        r#"{"kind":"cert","cert_id":"cert_api"}"#
    );
    assert_eq!(
        serde_json::to_string(&status).expect("status serializes"),
        r#"{"kind":"cert","id":"op_cert","cert_id":"cert_api","state":{"state":"running","stage":"validation_started"},"last_event_sequence":"3"}"#
    );
    assert_eq!(
        serde_json::to_string(&event).expect("event serializes"),
        r#"{"event":"cert_completed","operation_id":"op_cert","certificate":{"owner":{"owner":"ployz_automatic_namespace"},"active":{"cert_id":"cert_api","hostname":"api.example.com","bundle_ref":"sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef:/var/lib/ployz/certs/cert_api.pem","validity":{"not_before":"1700000000","not_after":"1707776000"}}}}"#
    );
    assert_eq!(challenge.ttl_seconds().get(), 60);
}

#[test]
fn sdk_exports_ingress_and_managed_dns_wire_types() {
    let configuration = IngressConfiguration::try_new(
        AutomaticHostnameConfiguration::Ployz,
        PloyzDnsTargetIntent::Enabled,
    )
    .expect("valid ingress configuration");
    let request = IngressConfigureRequest {
        operation_id: ployz_sdk_types::OperationId::try_new("op_ingress")
            .expect("valid operation id"),
        configuration,
    };
    let projection = IngressEndpointProjectionIdentity {
        control_plane_epoch: ControlPlaneEpoch::initial(),
        revision: 7,
    };
    let subject = ManagedDnsReconcileSubject::ProjectionApply {
        lease: ManagedLeaseName::try_new("brisk-river-x7f3").expect("valid lease name"),
        projection,
    };

    assert_eq!(
        serde_json::to_string(&request).expect("ingress request serializes"),
        r#"{"operation_id":"op_ingress","configuration":{"automatic_hostnames":{"mode":"ployz"},"ployz_dns_target":"enabled"}}"#
    );
    assert_eq!(
        serde_json::to_string(&subject).expect("managed DNS subject serializes"),
        r#"{"reason":"projection_apply","lease":"brisk-river-x7f3","projection":{"control_plane_epoch":1,"revision":7}}"#
    );
}

#[test]
fn sdk_exports_operation_api_wire_types() {
    let operation_id = ployz_sdk_types::OperationId::try_new("op_123").expect("valid operation id");
    let request = DeploySubmitRequest {
        registry_credentials: std::collections::BTreeMap::new(),
        idempotency_key: OperationIdempotencyKey::try_new("idem_deploy_123")
            .expect("valid idempotency key"),
        reservation_id: DeployReservationId::first(),
        target: DeployRequest {
            namespace_id: NamespaceId::try_new("default").expect("valid namespace id"),
            origin: None,
            volumes: std::collections::BTreeMap::new(),
            services: vec![DeployServiceSpec {
                keep: None,
                service_id: ServiceId::try_new("svc_api").expect("valid service id"),
                image: ImageReference::try_new("ghcr.io/acme/api:rev-1").expect("valid image"),
                image_source: ployz_core::deploy::ImageSource::Registry,
                mode: ServiceMode::Replicated {
                    replicas: ReplicaCount::try_new(1).expect("valid replica count"),
                },
                runtime: ployz_core::deploy::ContainerRuntimeSpec::image_defaults(),
                pre_start: None,
                depends_on: Vec::new(),
                routes: Vec::new(),
            }],
        },
    };
    let response: DeploySubmitResponse = OperationApiResponse::Ok {
        value: AcceptedOperation {
            operation_id: operation_id.clone(),
            watch_subject: "plz.v1.progress.namespace.default.operation.op_123.>".to_owned(),
            start_sequence: EventSequence::try_new(1).expect("valid event sequence"),
        },
    };
    assert_eq!(
        serde_json::to_string(&request).expect("request serializes"),
        r#"{"idempotency_key":"idem_deploy_123","reservation_id":"1","target":{"namespace_id":"default","services":[{"service_id":"svc_api","image":"ghcr.io/acme/api:rev-1","mode":{"kind":"replicated","replicas":1},"runtime":{"command":null,"entrypoint":null,"environment":{},"stop_grace_period":10}}]}}"#
    );
    assert_eq!(
        serde_json::to_string(&response).expect("response serializes"),
        r#"{"status":"ok","value":{"operation_id":"op_123","watch_subject":"plz.v1.progress.namespace.default.operation.op_123.>","start_sequence":"1"}}"#
    );
    let OperationApiResponse::Ok { value } = response else {
        panic!("response should be ok");
    };
    assert_eq!(value.operation_id, operation_id);
    assert_eq!(
        value.watch_subject,
        "plz.v1.progress.namespace.default.operation.op_123.>".to_owned()
    );
    assert_eq!(
        value.start_sequence,
        EventSequence::try_new(1).expect("valid event sequence")
    );

    let machine_add = MachineAddRequest {
        operation_id: ployz_sdk_types::OperationId::try_new("op_machine")
            .expect("valid operation id"),
        idempotency_key: OperationIdempotencyKey::try_new("idem_machine")
            .expect("valid idempotency key"),
        machine_id: ployz_sdk_types::MachineId::try_new("machine_2").expect("valid machine id"),
        name: MachineName::try_new("edge_2").expect("valid machine name"),
        roles: InstallRolePolicy::install_all().without_gateway(),
        host_port_assurance: HostPortAssurance::External,
    };
    let machine_response: MachineAddResponse = OperationApiResponse::Ok {
        value: MachineAddAccepted {
            accepted: AcceptedOperation {
                operation_id: ployz_sdk_types::OperationId::try_new("op_machine")
                    .expect("valid operation id"),
                watch_subject: "plz.v1.progress.machine.machine_2.operation.op_machine.>"
                    .to_owned(),
                start_sequence: EventSequence::try_new(7).expect("valid event sequence"),
            },
            machine_id: ployz_sdk_types::MachineId::try_new("machine_2").expect("valid machine id"),
            bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
                .expect("valid bootstrap url"),
            join_bundle: machine_join_bundle(),
            join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
            join_secret_delivery: machine_join_secret_delivery(),
        },
    };

    assert_eq!(
        serde_json::to_string(&machine_add).expect("request serializes"),
        r#"{"operation_id":"op_machine","idempotency_key":"idem_machine","machine_id":"machine_2","name":"edge_2","roles":{"gateway":"skip"},"host_port_assurance":"external"}"#
    );
    assert_eq!(
        serde_json::to_string(&machine_response).expect("response serializes"),
        r#"{"status":"ok","value":{"accepted":{"operation_id":"op_machine","watch_subject":"plz.v1.progress.machine.machine_2.operation.op_machine.>","start_sequence":"7"},"machine_id":"machine_2","bootstrap_url":"https://get.ployz.sh","join_bundle":{"material":{"cluster_name":"prod","dataplane_endpoint_supernet":"10.198.0.0/16","runtime_nats_url":"nats://127.0.0.1:7422","trusted_nats":{"ca_pem":"-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n"},"recovery_key_wrapped":[1,2,3],"core_seeds_wrapped":[4,5,6],"ployzd":{"version":"0.1.0","source":"/tmp/ployzd","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","install_path":"/usr/local/bin/ployzd"},"ebpf_bytecode":{"version":"0.1.0","source":"/tmp/ployz-ebpf-tc","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","install_path":"/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"},"ebpf_ctl":{"version":"0.1.0","source":"/tmp/ployz-ebpf-ctl","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","install_path":"/usr/local/bin/ployz-ebpf-ctl"},"railpack":{"version":"0.1.0","source":"/tmp/ployz-railpack","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","install_path":"/usr/local/lib/ployz/railpack/v0.31.0/railpack"}}},"join_token":"join_once_123","join_secret_delivery":{"nats_credentials":"SUAIZ5LKGG2Y4WC7ZPKS46LSLLJQIFTO6KMSWSU2VN3TC7YRRIKH5WRXJQ"}}}"#
    );

    let redeem_request = MachineJoinRedeemRequest {
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
    };
    let redeem_response: MachineJoinRedeemResponse = OperationApiResponse::Ok {
        value: MachineJoinRedeemed {
            operation_id: ployz_sdk_types::OperationId::try_new("op_machine")
                .expect("valid operation id"),
            machine_id: ployz_sdk_types::MachineId::try_new("machine_2").expect("valid machine id"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            roles: InstallRolePolicy::install_all().without_gateway(),
            host_port_assurance: HostPortAssurance::External,
            endpoint_subnet: ployz_core::network::MachineEndpointSubnet::try_new("10.198.2.0/24")
                .expect("valid subnet"),
            join_bundle: machine_join_bundle(),
            secret_delivery: machine_join_secret_delivery(),
            joined_at: ployz_sdk_types::JoinTokenRedeemedAt::try_new(60)
                .expect("valid redeemed timestamp"),
            last_event_sequence: EventSequence::try_new(8).expect("valid event sequence"),
            result: MachineJoinRedeemResult::Joined,
        },
    };
    let join_template = MachineJoinTemplate {
        join_bundle: machine_join_bundle(),
    };

    assert_eq!(
        serde_json::to_string(&redeem_request).expect("request serializes"),
        r#"{"join_token":"join_once_123"}"#
    );
    let redeem_response_value =
        serde_json::to_value(&redeem_response).expect("response serializes");
    assert_eq!(
        redeem_response_value.pointer("/value/endpoint_subnet"),
        Some(&serde_json::Value::String("10.198.2.0/24".to_owned()))
    );
    assert_eq!(
        serde_json::to_string(&join_template).expect("join template serializes"),
        r#"{"join_bundle":{"material":{"cluster_name":"prod","dataplane_endpoint_supernet":"10.198.0.0/16","runtime_nats_url":"nats://127.0.0.1:7422","trusted_nats":{"ca_pem":"-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n"},"recovery_key_wrapped":[1,2,3],"core_seeds_wrapped":[4,5,6],"ployzd":{"version":"0.1.0","source":"/tmp/ployzd","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","install_path":"/usr/local/bin/ployzd"},"ebpf_bytecode":{"version":"0.1.0","source":"/tmp/ployz-ebpf-tc","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","install_path":"/usr/local/lib/ployz/ebpf/ployz-ebpf-tc"},"ebpf_ctl":{"version":"0.1.0","source":"/tmp/ployz-ebpf-ctl","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","install_path":"/usr/local/bin/ployz-ebpf-ctl"},"railpack":{"version":"0.1.0","source":"/tmp/ployz-railpack","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","install_path":"/usr/local/lib/ployz/railpack/v0.31.0/railpack"}}}}"#
    );
}

#[test]
fn sdk_exports_cloud_bootstrap_wire_types() {
    let attempt_id = CloudBootstrapAttemptId::try_new("pcba_123").expect("valid attempt id");
    let token = CloudBootstrapToken::try_new("pcbt_abc123").expect("valid bootstrap token");
    let redeem_request = CloudBootstrapTokenRedeemRequest {
        attempt_id: attempt_id.clone(),
        client: ployz_sdk_types::CloudBootstrapClientInfo::current("0.1.0"),
        machine: ployz_sdk_types::CloudBootstrapMachineFacts {
            hostname: Some("edge-1".to_owned()),
            os: "linux".to_owned(),
            arch: "x86_64".to_owned(),
            candidate_runtime_nats_url: None,
        },
    };
    let decision = CloudBootstrapDecision::Ready {
        envelope: Box::new(CloudBootstrapEnvelope {
            attempt_id: attempt_id.clone(),
            redemption_id: CloudBootstrapRedemptionId::try_new("pcbr_123")
                .expect("valid redemption id"),
            callback_url: "https://cloud.ployz.com/api/bootstrap/callback".to_owned(),
            callback_token: CloudBootstrapCallbackToken::try_new("pcbc_abc123")
                .expect("valid callback token"),
            intent: CloudBootstrapIntent::Joiner {
                joiner: Box::new(ployz_sdk_types::CloudJoinerBootstrap {
                    runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("tls://203.0.113.10:4222")
                        .expect("valid nats url"),
                    trusted_nats: MachineJoinTrustedNats {
                        ca_pem: NatsCaCertificatePem::try_new(
                            "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
                        )
                        .expect("valid ca pem"),
                    },
                    join_token: MachineJoinToken::try_new("join_once_123")
                        .expect("valid join token"),
                    join_secret_delivery: machine_join_secret_delivery(),
                }),
            },
        }),
    };
    let callback = CloudBootstrapCallbackRequest {
        attempt_id,
        redemption_id: CloudBootstrapRedemptionId::try_new("pcbr_123")
            .expect("valid redemption id"),
        outcome: CloudBootstrapOutcome::FounderSucceeded {
            result: CloudFounderBootstrapResult {
                machine_id: ployz_sdk_types::MachineId::try_new("core_1")
                    .expect("valid machine id"),
                runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("tls://203.0.113.10:4222")
                    .expect("valid nats url"),
                trusted_nats: MachineJoinTrustedNats {
                    ca_pem: NatsCaCertificatePem::try_new(
                        "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
                    )
                    .expect("valid ca pem"),
                },
            },
        },
    };

    assert_eq!(
        serde_json::to_string(&redeem_request).expect("redeem request serializes"),
        r#"{"attempt_id":"pcba_123","client":{"protocol_version":1,"host_runner_version":"0.1.0"},"machine":{"hostname":"edge-1","os":"linux","arch":"x86_64","candidate_runtime_nats_url":null}}"#
    );
    assert_eq!(token.secret(), "pcbt_abc123");
    assert_eq!(format!("{token:?}"), "CloudBootstrapToken([redacted])");

    assert_eq!(
        serde_json::to_string(&callback).expect("callback serializes"),
        r#"{"attempt_id":"pcba_123","redemption_id":"pcbr_123","outcome":{"outcome":"founder_succeeded","result":{"machine_id":"core_1","runtime_nats_url":"tls://203.0.113.10:4222","trusted_nats":{"ca_pem":"-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n"}}}}"#
    );

    let decision_debug = format!("{decision:?}");
    assert!(!decision_debug.contains("pcbc_abc123"));
    assert!(decision_debug.contains("CloudBootstrapCallbackToken([redacted])"));
}

#[test]
fn typescript_contract_fixture_matches_rust_wire_types() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../packages/ployz-sdk/test/fixtures/operation-contract.json"
    ))
    .expect("fixture is json");

    assert_fixture::<DeploySubmitRequest>(&fixture, "deploy_submit_request");
    assert_fixture::<DeployPreviewRequest>(&fixture, "deploy_preview_request");
    assert_fixture::<OperationApiResponse<DeployPreview, DeployPreviewError>>(
        &fixture,
        "deploy_preview_response",
    );
    assert_fixture::<DeployReserveRequest>(&fixture, "deploy_reserve_request");
    assert_fixture::<InitFirstMachineActivateRequest>(
        &fixture,
        "init_first_machine_activate_request",
    );
    assert_fixture::<MachineAddRequest>(&fixture, "machine_add_request");
    assert_fixture::<MachineUpdateRequest>(&fixture, "machine_update_request");
    assert_fixture::<MachineJoinRedeemRequest>(&fixture, "machine_join_redeem_request");
    assert_fixture::<CloudBootstrapSessionCreateRequest>(
        &fixture,
        "cloud_bootstrap_session_create_request",
    );
    assert_fixture::<CloudBootstrapSessionCreated>(&fixture, "cloud_bootstrap_session_created");
    assert_fixture::<CloudBootstrapSessionPollRequest>(
        &fixture,
        "cloud_bootstrap_session_poll_request",
    );
    assert_fixture::<CloudBootstrapDecision>(&fixture, "cloud_bootstrap_decision");
    assert_fixture::<CloudBootstrapCallbackRequest>(&fixture, "cloud_bootstrap_callback_request");
    assert_fixture::<CloudBootstrapCallbackAccepted>(&fixture, "cloud_bootstrap_callback_accepted");
    assert_fixture::<OperationEventReplayRequest>(&fixture, "ops_watch_request");
    assert_fixture::<AcceptedOperation>(&fixture, "accepted_operation");
    assert_fixture::<DeploySubmitResponse>(&fixture, "deploy_submit_response");
    assert_fixture::<OperationApiResponse<DeployReserved, DeployReserveError>>(
        &fixture,
        "deploy_reserve_response",
    );
    assert_fixture::<OperationApiResponse<InitFirstMachineActivated, InitFirstMachineActivateError>>(
        &fixture,
        "init_first_machine_activate_response",
    );
    assert_fixture::<MachineAddResponse>(&fixture, "machine_add_response");
    assert_fixture::<MachineUpdateResponse>(&fixture, "machine_update_response");
    assert_fixture::<MachineJoinRedeemResponse>(&fixture, "machine_join_redeem_response");
    assert_fixture::<OperationStatusSnapshot>(&fixture, "operation_status_snapshot");
    assert_fixture::<OpsStatusResponse>(&fixture, "ops_status_response");
    assert_fixture::<OperationEventReplayPage>(&fixture, "operation_event_replay_page");
    assert_fixture::<OpsWatchResponse>(&fixture, "ops_watch_response");
    assert_fixture::<OpsStatusResponse>(&fixture, "ops_status_error_response");
}

#[test]
fn operation_api_contract_registry_owns_endpoint_shapes() {
    assert_contract::<BuildSubmitApi, BuildSubmitRequest, AcceptedOperation, BuildSubmitError>();
    assert_contract::<BuildCancelApi, BuildCancelRequest, AcceptedOperation, BuildCancelError>();
    assert_contract::<DeployReserveApi, DeployReserveRequest, DeployReserved, DeployReserveError>();
    assert_contract::<DeployPreviewApi, DeployPreviewRequest, DeployPreview, DeployPreviewError>();
    assert_contract::<DeploySubmitApi, DeploySubmitRequest, AcceptedOperation, DeploySubmitError>();
    assert_contract::<
        InitFirstMachineActivateApi,
        InitFirstMachineActivateRequest,
        InitFirstMachineActivated,
        InitFirstMachineActivateError,
    >();
    assert_contract::<MachineAddApi, MachineAddRequest, MachineAddAccepted, MachineAddError>();
    assert_contract::<
        ployz_sdk_types::operation_api::MachineBuildCachePruneApi,
        ployz_sdk_types::MachineBuildCachePruneRequest,
        AcceptedOperation,
        ployz_sdk_types::MachineBuildCachePruneError,
    >();
    assert_contract::<MachineUpdateApi, MachineUpdateRequest, AcceptedOperation, MachineUpdateError>(
    );
    assert_contract::<
        MachineStoragePrepareApi,
        MachineStoragePrepareRequest,
        AcceptedOperation,
        MachineStoragePrepareError,
    >();
    assert_contract::<
        ServiceRestartApi,
        ServiceRestartRequest,
        AcceptedOperation,
        ServiceRestartError,
    >();
    assert_contract::<
        NamespaceRemoveApi,
        NamespaceRemoveRequest,
        AcceptedOperation,
        NamespaceRemoveError,
    >();
    assert_contract::<VolumeRemoveApi, VolumeRemoveRequest, AcceptedOperation, VolumeRemoveError>();
    assert_contract::<VolumeCreateApi, VolumeCreateRequest, AcceptedOperation, VolumeCreateError>();
    assert_contract::<MachineListApi, MachineListRequest, MachineListResult, MachineListError>();
    assert_contract::<MachineInspectApi, MachineInspectRequest, MachineSnapshot, MachineInspectError>(
    );
    assert_contract::<
        NetworkStatusApi,
        NetworkStatusRequest,
        NetworkStatusResult,
        NetworkStatusError,
    >();
    assert_contract::<
        NetworkResolveApi,
        NetworkResolveRequest,
        NetworkResolveResult,
        NetworkResolveError,
    >();
    assert_contract::<NetworkRepairApi, NetworkRepairRequest, AcceptedOperation, NetworkRepairError>(
    );
    assert_contract::<ServiceListApi, ServiceListRequest, ServiceListResult, ServiceListError>();
    assert_contract::<VolumeListApi, VolumeListRequest, VolumeListResult, VolumeListError>();
    assert_contract::<ServiceInspectApi, ServiceInspectRequest, ServiceSnapshot, ServiceInspectError>(
    );
    assert_contract::<
        RuntimeSnapshotApi,
        RuntimeSnapshotRequest,
        RuntimeSnapshotResult,
        RuntimeSnapshotError,
    >();
    assert_contract::<LogsTailApi, LogsTailRequest, LogsTailResult, LogsTailError>();
    assert_contract::<
        MachineJoinRedeemApi,
        MachineJoinRedeemRequest,
        MachineJoinRedeemed,
        MachineJoinRedeemError,
    >();
    assert_contract::<
        MachineJoinReportApi,
        MachineJoinReportRequest,
        MachineJoinReported,
        MachineJoinReportError,
    >();
    assert_contract::<
        ployz_sdk_types::operation_api::MachineDrainApi,
        ployz_sdk_types::MachineLifecycleRequest,
        AcceptedOperation,
        ployz_sdk_types::MachineLifecycleError,
    >();
    assert_contract::<
        ployz_sdk_types::operation_api::MachineResumeApi,
        ployz_sdk_types::MachineLifecycleRequest,
        AcceptedOperation,
        ployz_sdk_types::MachineLifecycleError,
    >();
    assert_contract::<CoreReplaceApi, CoreReplaceRequest, AcceptedOperation, CoreReplaceError>();
    assert_contract::<
        CoreReplaceReportApi,
        CoreReplaceReportRequest,
        CoreReplaceReported,
        CoreReplaceReportError,
    >();
    assert_contract::<CredentialAddApi, CredentialAddRequest, AcceptedOperation, CredentialAddError>(
    );
    assert_contract::<
        CredentialListApi,
        CredentialListRequest,
        CredentialListResult,
        CredentialListError,
    >();
    assert_contract::<
        CredentialRemoveApi,
        CredentialRemoveRequest,
        AcceptedOperation,
        CredentialRemoveError,
    >();
    assert_contract::<
        IngressConfigureApi,
        IngressConfigureRequest,
        AcceptedOperation,
        IngressConfigureError,
    >();
    assert_contract::<OpsListApi, OpsListRequest, OpsListResult, OpsListError>();
    assert_contract::<OpsStatusApi, OpsStatusRequest, OperationStatusSnapshot, OpsStatusError>();
    assert_contract::<
        OpsWatchApi,
        OperationEventReplayRequest,
        OperationEventReplayPage,
        ployz_sdk_types::OpsWatchError,
    >();
    assert_eq!(
        operation_api_contract_endpoints(),
        [
            OperationApiEndpoint::BuildSubmit,
            OperationApiEndpoint::BuildCancel,
            OperationApiEndpoint::DeployReserve,
            OperationApiEndpoint::DeployPreview,
            OperationApiEndpoint::DeploySubmit,
            OperationApiEndpoint::SystemDeploy,
            OperationApiEndpoint::InitFirstMachineActivate,
            OperationApiEndpoint::MachineAdd,
            OperationApiEndpoint::MachineBuildCachePrune,
            OperationApiEndpoint::MachineUpdate,
            OperationApiEndpoint::MachineStoragePrepare,
            OperationApiEndpoint::MachineDrain,
            OperationApiEndpoint::MachineResume,
            OperationApiEndpoint::ServiceRestart,
            OperationApiEndpoint::NamespaceRemove,
            OperationApiEndpoint::VolumeCreate,
            OperationApiEndpoint::VolumeRemove,
            OperationApiEndpoint::CoreReplace,
            OperationApiEndpoint::CoreReplaceReport,
            OperationApiEndpoint::CredentialAdd,
            OperationApiEndpoint::CredentialList,
            OperationApiEndpoint::CredentialRemove,
            OperationApiEndpoint::IngressConfigure,
            OperationApiEndpoint::MachineList,
            OperationApiEndpoint::MachineInspect,
            OperationApiEndpoint::NetworkStatus,
            OperationApiEndpoint::NetworkResolve,
            OperationApiEndpoint::NetworkRepair,
            OperationApiEndpoint::MachineJoinRedeem,
            OperationApiEndpoint::MachineJoinReport,
            OperationApiEndpoint::ServiceList,
            OperationApiEndpoint::VolumeList,
            OperationApiEndpoint::ServiceInspect,
            OperationApiEndpoint::RuntimeSnapshot,
            OperationApiEndpoint::LogsTail,
            OperationApiEndpoint::OpsList,
            OperationApiEndpoint::OpsStatus,
            OperationApiEndpoint::OpsWatch,
        ]
    );
}

fn assert_fixture<T>(fixture: &serde_json::Value, key: &'static str)
where
    T: serde::de::DeserializeOwned + serde::Serialize,
{
    let value = fixture.get(key).expect("fixture key exists").clone();
    let decoded: T = serde_json::from_value(value.clone()).expect("fixture matches rust type");

    assert_eq!(
        serde_json::to_value(decoded).expect("fixture serializes through rust type"),
        value
    );
}

fn assert_contract<C, Request, Success, Error>()
where
    C: OperationApiContract<Request = Request, Success = Success, Error = Error>,
    Request: TS,
    Success: TS,
    Error: TS,
{
}

fn operation_api_contract_endpoints() -> Vec<OperationApiEndpoint> {
    let mut endpoints = Vec::new();
    macro_rules! push_endpoints {
        ($($contract:ty),+ $(,)?) => {
            $(endpoints.push(<$contract as OperationApiContract>::ENDPOINT);)+
        };
    }
    ployz_sdk_types::operation_api_contracts!(push_endpoints);
    endpoints
}

fn machine_join_bundle() -> MachineJoinBundle {
    MachineJoinBundle {
        material: MachineJoinMaterial {
            dataplane_endpoint_supernet: ployz_core::network::MachineEndpointSupernet::default_v1(),
            cluster_name: ployz_core::install::MachineJoinClusterName::try_new("prod")
                .expect("valid cluster name"),
            runtime_nats_url: MachineJoinRuntimeNatsUrl::try_new("nats://127.0.0.1:7422")
                .expect("valid runtime nats url"),
            trusted_nats: MachineJoinTrustedNats {
                ca_pem: NatsCaCertificatePem::try_new(
                    "-----BEGIN CERTIFICATE-----\nTUlJQg==\n-----END CERTIFICATE-----\n",
                )
                .expect("valid ca pem"),
            },
            recovery_key_wrapped: ployz_core::install::WrappedCaKey::new(vec![1, 2, 3]),
            core_seeds_wrapped: ployz_core::install::WrappedCoreSeeds::new(vec![4, 5, 6]),
            ployzd: machine_join_artifact("/tmp/ployzd", "/usr/local/bin/ployzd"),
            railpack: machine_join_artifact(
                "/tmp/ployz-railpack",
                "/usr/local/lib/ployz/railpack/v0.31.0/railpack",
            ),
            ebpf_bytecode: machine_join_artifact(
                "/tmp/ployz-ebpf-tc",
                "/usr/local/lib/ployz/ebpf/ployz-ebpf-tc",
            ),
            ebpf_ctl: machine_join_artifact("/tmp/ployz-ebpf-ctl", "/usr/local/bin/ployz-ebpf-ctl"),
        },
    }
}

fn machine_join_artifact(
    source: &str,
    install_path: &str,
) -> ployz_core::install::InstallArtifactSpec {
    ployz_core::install::InstallArtifactSpec {
        version: ployz_core::install::InstallArtifactVersion::try_new("0.1.0")
            .expect("valid artifact version"),
        source: ployz_core::install::InstallArtifactSource::try_new(source)
            .expect("valid artifact source"),
        sha256: ployz_core::install::InstallSha256Digest::try_new(
            "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
        )
        .expect("valid artifact digest"),
        install_path: ployz_core::install::AbsoluteInstallPath::try_new(install_path)
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

#[test]
fn sdk_exports_constructor_error_types() {
    assert!(matches!(
        ployz_sdk_types::PushedImageReceipt::try_new([]),
        Err(ployz_sdk_types::PushedImageReceiptError::Empty)
    ));
    assert!(matches!(
        ployz_sdk_types::OciPlatform::try_new("linux", "amd 64"),
        Err(ployz_sdk_types::OciPlatformError::InvalidCharacter { .. })
    ));
    assert!(matches!(
        ImageReference::try_new(""),
        Err(ImageReferenceError::Empty)
    ));
    assert!(matches!(
        ReplicaCount::try_new(0),
        Err(ReplicaCountError::Zero)
    ));
    assert!(matches!(
        DeployPhaseNumber::try_new(0),
        Err(DeployPhaseNumberError::Zero)
    ));
    assert!(matches!(
        ServiceId::try_new("svc.api"),
        Err(SubjectTokenError::InvalidCharacter { .. })
    ));
    assert!(matches!(
        EventSequence::try_new(0),
        Err(EventSequenceError::Zero)
    ));
    assert!(matches!(
        OperationEventReplayLimit::try_new(0),
        Err(OperationEventReplayLimitError::Zero)
    ));
    assert!(matches!(
        OperationEventReplayLimit::try_new(MAX_OPERATION_EVENT_REPLAY_LIMIT + 1),
        Err(OperationEventReplayLimitError::TooLarge { .. })
    ));
    assert!(matches!(
        ployz_sdk_types::CancellationReason::try_new(""),
        Err(NonEmptyTextError::Empty)
    ));
    assert!(matches!(
        AcmeChallengeValue::try_new("missing_thumbprint"),
        Err(CertTextError::InvalidAcmeChallengeValue { .. })
    ));
    assert!(matches!(
        CertBundleRef::try_new("obj://PLZ_CERTS/cert_api/rev_1"),
        Err(CertTextError::InvalidBundleRef { .. })
    ));
    assert!(matches!(
        RouteHostname::try_new("-api.example.com"),
        Err(RouteHostnameError::Invalid { .. })
    ));
    assert!(matches!(RoutePort::try_new(0), Err(RoutePortError::Zero)));
    assert!(matches!(
        MachineBootstrapUrl::try_new("http://get.ployz.sh"),
        Err(InstallContractError::InvalidBootstrapUrl { .. })
    ));
    assert!(matches!(
        MachineJoinToken::try_new("join token"),
        Err(ployz_sdk_types::BootstrapCommandError::InvalidJoinToken)
    ));
}

#[test]
fn sdk_exports_operational_health_wire_types() {
    assert_wire_type::<GatewayProcessHealth>();
    assert_wire_type::<GatewayProcessAttempt>();
    assert_wire_type::<GatewayHttpFailure>();
    assert_wire_type::<GatewayWatchFailure>();
    assert_wire_type::<GatewayStatusPublishFailure>();
    assert_wire_type::<ControlHealth>();
    assert_wire_type::<ControlNatsAuthorizationHealth>();
    assert_wire_type::<ControlTaskSupervisorHealth>();
    assert_wire_type::<ControlTaskSupervisorFailure>();
    assert_wire_type::<ControlRuntimeProjectionHealth>();
    assert_wire_type::<ControlRuntimeProjectionLoopHealth>();
    assert_wire_type::<ControlRuntimeProjectionServiceHealth>();
    assert_wire_type::<ControlCertificateRenewalHealth>();
    assert_wire_type::<ControlIngressEndpointProjectionHealth>();
    assert_wire_type::<ControlCertificateRenewalAttempt>();
    assert_wire_type::<ControlCertificateRenewalFailure>();
    assert_wire_type::<ControlCertificateRenewalOutcome>();
}

#[test]
fn sdk_exports_interruption_wire_types() {
    assert_wire_type::<ployz_sdk_types::OperationInterruptionCause>();
    assert_wire_type::<ployz_sdk_types::OperationInterruptionEvidence>();
    assert_wire_type::<ployz_sdk_types::OperationInterruptionStage>();
    assert_wire_type::<ployz_sdk_types::DeployInterruptionStage>();
    assert_wire_type::<ployz_sdk_types::OperationInterruptionUncertainWork>();
    assert_wire_type::<ployz_sdk_types::OperationInterruptionNextAction>();
    assert_wire_type::<ployz_sdk_types::CertInterruptionStage>();
    assert_wire_type::<ployz_sdk_types::CertificateInterruptionNextAction>();
}

fn assert_wire_type<T>()
where
    T: serde::Serialize + for<'de> serde::Deserialize<'de> + TS,
{
}

fn valid_at(value: u64) -> CertValidAt {
    CertValidAt::try_new(value).expect("valid cert timestamp")
}
