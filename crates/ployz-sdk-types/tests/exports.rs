use ployz_core::subjects::{OPERATION_API_ENDPOINTS, OperationApiEndpointExecution};
use ployz_sdk_types::{
    AcceptedOperation, AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue,
    AcmeHttp01Challenge, ActiveCertState, CertBundleRef, CertId, CertOperationState,
    CertRunningStage, CertTextError, CertValidAt, CertValidityWindow, DeployOperationState,
    DeployRequest, DeployRunningStage, DeploySubmitError, DeploySubmitRequest,
    DeploySubmitResponse, EventSequence, EventSequenceError, ImageReference, ImageReferenceError,
    MAX_OPERATION_EVENT_REPLAY_LIMIT, MachineAddAccepted, MachineAddError, MachineAddGateway,
    MachineAddRequest, MachineAddResponse, MachineBootstrapUrl, MachineJoinBundle,
    MachineJoinPloyzdArtifact, MachineJoinRedeemError, MachineJoinRedeemRequest,
    MachineJoinRedeemResponse, MachineJoinRedeemResult, MachineJoinRedeemed,
    MachineJoinReportError, MachineJoinReportRequest, MachineJoinReported, MachineJoinToken,
    MachineName, NonEmptyTextError, OperationApiResponse, OperationEvent,
    OperationEventReplayCursor, OperationEventReplayLimit, OperationEventReplayLimitError,
    OperationEventReplayPage, OperationEventReplayRequest, OperationIdempotencyKey,
    OperationLeaseExpiresAt, OperationOwnerId, OperationOwnerLease, OperationStatus,
    OperationStatusSnapshot, OperationSubject, OpsStatusError, OpsStatusRequest, OpsStatusResponse,
    OpsWatchResponse, ReplicaCount, ReplicaCountError, RevisionId, RouteHostname,
    RouteHostnameError, RoutePort, RoutePortError, ServiceId, SubjectTokenError,
    operation_api::{
        DeploySubmitApi, MachineAddApi, MachineJoinRedeemApi, MachineJoinReportApi,
        OperationApiContract, OpsStatusApi, OpsWatchApi,
    },
};
use ts_rs::{Config, TS};

#[test]
fn sdk_exports_core_wire_types() {
    let service_id = ServiceId::try_new("svc_api").expect("valid service id");
    let subject = OperationSubject::Deploy {
        service_id: service_id.clone(),
    };
    let state = DeployOperationState::Accepted;
    let running = DeployRunningStage::ActiveServiceCommit;
    let _deploy = DeployRequest {
        service_id: service_id.clone(),
        target_revision: RevisionId::try_new("rev_1").expect("valid revision id"),
        image: ImageReference::try_new("ghcr.io/acme/api:rev-1").expect("valid image"),
        replicas: ReplicaCount::try_new(1).expect("valid replica count"),
    };
    let status = OperationStatus::deploy_accepted(
        ployz_sdk_types::OperationId::try_new("op_123").expect("valid operation id"),
        service_id,
        EventSequence::try_new(1).expect("valid event sequence"),
    );
    let replay_request = OperationEventReplayRequest {
        operation_id: ployz_sdk_types::OperationId::try_new("op_123").expect("valid operation id"),
        start_sequence: EventSequence::try_new(1).expect("valid event sequence"),
        limit: OperationEventReplayLimit::try_new(100).expect("valid replay limit"),
    };
    let replay_page = OperationEventReplayPage::caught_up(Vec::new());

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
        r#""active_service_commit""#
    );
    assert_eq!(
        serde_json::to_string(&status).expect("status serializes"),
        r#"{"kind":"deploy","id":"op_123","service_id":"svc_api","state":{"state":"accepted"},"last_event_sequence":"1"}"#
    );
    assert_eq!(replay_request.limit.get(), 100);
    assert_eq!(replay_page.cursor, OperationEventReplayCursor::CaughtUp);
}

#[test]
fn sdk_exports_cert_wire_types() {
    let cert_id = CertId::try_new("cert_api").expect("valid cert id");
    let active_cert = ActiveCertState {
        cert_id: cert_id.clone(),
        hostname: RouteHostname::try_new("api.example.com").expect("valid hostname"),
        bundle_ref: CertBundleRef::try_new("obj://PLZ_CERTS/cert_api/rev_1")
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
        active_cert,
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
        r#"{"event":"cert_completed","operation_id":"op_cert","active_cert":{"cert_id":"cert_api","hostname":"api.example.com","bundle_ref":"obj://PLZ_CERTS/cert_api/rev_1","validity":{"not_before":"1700000000","not_after":"1707776000"}}}"#
    );
    assert_eq!(challenge.ttl_seconds().get(), 60);
}

#[test]
fn sdk_exports_operation_api_wire_types() {
    let operation_id = ployz_sdk_types::OperationId::try_new("op_123").expect("valid operation id");
    let request = DeploySubmitRequest {
        operation_id: operation_id.clone(),
        idempotency_key: OperationIdempotencyKey::try_new("idem_1").expect("valid idempotency key"),
        target: DeployRequest {
            service_id: ServiceId::try_new("svc_api").expect("valid service id"),
            target_revision: RevisionId::try_new("rev_1").expect("valid revision id"),
            image: ImageReference::try_new("ghcr.io/acme/api:rev-1").expect("valid image"),
            replicas: ReplicaCount::try_new(1).expect("valid replica count"),
        },
    };
    let response: DeploySubmitResponse = OperationApiResponse::Ok {
        value: AcceptedOperation {
            operation_id: operation_id.clone(),
            watch_subject: "plz.v1.op.op_123.>".to_owned(),
            start_sequence: EventSequence::try_new(1).expect("valid event sequence"),
            owner_lease: operation_lease("op_123", "control", 120),
        },
    };

    assert_eq!(
        serde_json::to_string(&request).expect("request serializes"),
        r#"{"operation_id":"op_123","idempotency_key":"idem_1","target":{"service_id":"svc_api","target_revision":"rev_1","image":"ghcr.io/acme/api:rev-1","replicas":1}}"#
    );
    assert_eq!(
        serde_json::to_string(&response).expect("response serializes"),
        r#"{"status":"ok","value":{"operation_id":"op_123","watch_subject":"plz.v1.op.op_123.>","start_sequence":"1","owner_lease":{"operation_id":"op_123","owner_id":"control","expires_at":"120"}}}"#
    );

    let OperationApiResponse::Ok { value } = response else {
        panic!("response should be ok");
    };
    assert_eq!(value.operation_id, operation_id);
    assert_eq!(value.watch_subject, "plz.v1.op.op_123.>".to_owned());
    assert_eq!(
        value.start_sequence,
        EventSequence::try_new(1).expect("valid event sequence")
    );
    assert_eq!(value.owner_lease, operation_lease("op_123", "control", 120));

    let machine_add = MachineAddRequest {
        operation_id: ployz_sdk_types::OperationId::try_new("op_machine")
            .expect("valid operation id"),
        idempotency_key: OperationIdempotencyKey::try_new("idem_machine")
            .expect("valid idempotency key"),
        node_id: ployz_sdk_types::NodeId::try_new("node_2").expect("valid node id"),
        name: MachineName::try_new("edge_2").expect("valid machine name"),
        gateway: MachineAddGateway::Skip,
        join_bundle: machine_join_bundle(),
    };
    let machine_response: MachineAddResponse = OperationApiResponse::Ok {
        value: MachineAddAccepted {
            accepted: AcceptedOperation {
                operation_id: ployz_sdk_types::OperationId::try_new("op_machine")
                    .expect("valid operation id"),
                watch_subject: "plz.v1.op.op_machine.>".to_owned(),
                start_sequence: EventSequence::try_new(7).expect("valid event sequence"),
                owner_lease: operation_lease("op_machine", "control", 120),
            },
            node_id: ployz_sdk_types::NodeId::try_new("node_2").expect("valid node id"),
            bootstrap_url: MachineBootstrapUrl::try_new("https://get.ployz.sh")
                .expect("valid bootstrap url"),
            join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
        },
    };

    assert_eq!(
        serde_json::to_string(&machine_add).expect("request serializes"),
        r#"{"operation_id":"op_machine","idempotency_key":"idem_machine","node_id":"node_2","name":"edge_2","gateway":"skip","join_bundle":{"cluster_name":"prod","ployzd":{"version":"0.1.0","source":"/tmp/ployzd","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","install_path":"/usr/local/bin/ployzd"}}}"#
    );
    assert_eq!(
        serde_json::to_string(&machine_response).expect("response serializes"),
        r#"{"status":"ok","value":{"accepted":{"operation_id":"op_machine","watch_subject":"plz.v1.op.op_machine.>","start_sequence":"7","owner_lease":{"operation_id":"op_machine","owner_id":"control","expires_at":"120"}},"node_id":"node_2","bootstrap_url":"https://get.ployz.sh","join_token":"join_once_123"}}"#
    );

    let redeem_request = MachineJoinRedeemRequest {
        join_token: MachineJoinToken::try_new("join_once_123").expect("valid join token"),
    };
    let redeem_response: MachineJoinRedeemResponse = OperationApiResponse::Ok {
        value: MachineJoinRedeemed {
            operation_id: ployz_sdk_types::OperationId::try_new("op_machine")
                .expect("valid operation id"),
            node_id: ployz_sdk_types::NodeId::try_new("node_2").expect("valid node id"),
            name: MachineName::try_new("edge_2").expect("valid machine name"),
            gateway: ployz_sdk_types::FirstNodeGateway::Skip,
            join_bundle: machine_join_bundle(),
            joined_at: ployz_sdk_types::JoinTokenRedeemedAt::try_new(60)
                .expect("valid redeemed timestamp"),
            last_event_sequence: EventSequence::try_new(8).expect("valid event sequence"),
            result: MachineJoinRedeemResult::Joined,
        },
    };

    assert_eq!(
        serde_json::to_string(&redeem_request).expect("request serializes"),
        r#"{"join_token":"join_once_123"}"#
    );
    assert_eq!(
        serde_json::to_string(&redeem_response).expect("response serializes"),
        r#"{"status":"ok","value":{"operation_id":"op_machine","node_id":"node_2","name":"edge_2","gateway":"skip","join_bundle":{"cluster_name":"prod","ployzd":{"version":"0.1.0","source":"/tmp/ployzd","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","install_path":"/usr/local/bin/ployzd"}},"joined_at":"60","last_event_sequence":"8","result":"joined"}}"#
    );
}

#[test]
fn typescript_contract_fixture_matches_rust_wire_types() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../packages/ployz-sdk/test/fixtures/operation-contract.json"
    ))
    .expect("fixture is json");

    assert_fixture::<DeploySubmitRequest>(&fixture, "deploy_submit_request");
    assert_fixture::<MachineAddRequest>(&fixture, "machine_add_request");
    assert_fixture::<MachineJoinRedeemRequest>(&fixture, "machine_join_redeem_request");
    assert_fixture::<OperationEventReplayRequest>(&fixture, "ops_watch_request");
    assert_fixture::<AcceptedOperation>(&fixture, "accepted_operation");
    assert_fixture::<DeploySubmitResponse>(&fixture, "deploy_submit_response");
    assert_fixture::<MachineAddResponse>(&fixture, "machine_add_response");
    assert_fixture::<MachineJoinRedeemResponse>(&fixture, "machine_join_redeem_response");
    assert_fixture::<OperationStatusSnapshot>(&fixture, "operation_status_snapshot");
    assert_fixture::<OpsStatusResponse>(&fixture, "ops_status_response");
    assert_fixture::<OperationEventReplayPage>(&fixture, "operation_event_replay_page");
    assert_fixture::<OpsWatchResponse>(&fixture, "ops_watch_response");
    assert_fixture::<OpsStatusResponse>(&fixture, "ops_status_error_response");
}

#[test]
fn package_typescript_contract_is_generated_from_rust_crate() {
    assert_eq!(
        include_str!("../../../packages/ployz-sdk/src/generated.ts"),
        ployz_sdk_types::typescript::generated_typescript()
    );
}

#[test]
fn operation_api_contract_registry_owns_endpoint_shapes() {
    assert_contract::<DeploySubmitApi, DeploySubmitRequest, AcceptedOperation, DeploySubmitError>();
    assert_contract::<MachineAddApi, MachineAddRequest, MachineAddAccepted, MachineAddError>();
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
    assert_contract::<OpsStatusApi, OpsStatusRequest, OperationStatusSnapshot, OpsStatusError>();
    assert_contract::<
        OpsWatchApi,
        OperationEventReplayRequest,
        OperationEventReplayPage,
        ployz_sdk_types::OpsWatchError,
    >();

    assert_eq!(operation_api_contract_endpoints(), OPERATION_API_ENDPOINTS);
    assert_eq!(
        operation_api_contract_rows(),
        vec![
            (
                "deploy.submit",
                "plz.v1.svc.api.deploy.submit",
                OperationApiEndpointExecution::AcceptsOperation,
                "DeploySubmitRequest".to_owned(),
                "AcceptedOperation".to_owned(),
                "DeploySubmitError".to_owned(),
                "DeploySubmitResponse",
            ),
            (
                "machine.add",
                "plz.v1.svc.api.machine.add",
                OperationApiEndpointExecution::AcceptsOperation,
                "MachineAddRequest".to_owned(),
                "MachineAddAccepted".to_owned(),
                "MachineAddError".to_owned(),
                "MachineAddResponse",
            ),
            (
                "machine.join.redeem",
                "plz.v1.svc.api.machine.join.redeem",
                OperationApiEndpointExecution::MutatesOperation,
                "MachineJoinRedeemRequest".to_owned(),
                "MachineJoinRedeemed".to_owned(),
                "MachineJoinRedeemError".to_owned(),
                "MachineJoinRedeemResponse",
            ),
            (
                "machine.join.report",
                "plz.v1.svc.api.machine.join.report",
                OperationApiEndpointExecution::MutatesOperation,
                "MachineJoinReportRequest".to_owned(),
                "MachineJoinReported".to_owned(),
                "MachineJoinReportError".to_owned(),
                "MachineJoinReportResponse",
            ),
            (
                "ops.status",
                "plz.v1.svc.api.ops.status",
                OperationApiEndpointExecution::Query,
                "OpsStatusRequest".to_owned(),
                "OperationStatusSnapshot".to_owned(),
                "OpsStatusError".to_owned(),
                "OpsStatusResponse",
            ),
            (
                "ops.watch",
                "plz.v1.svc.api.ops.watch",
                OperationApiEndpointExecution::Query,
                "OpsWatchRequest".to_owned(),
                "OperationEventReplayPage".to_owned(),
                "OpsWatchError".to_owned(),
                "OpsWatchResponse",
            ),
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

fn operation_api_contract_endpoints() -> Vec<ployz_core::subjects::OperationApiEndpoint> {
    let mut endpoints = Vec::new();
    macro_rules! push_endpoints {
        ($($contract:ty),+ $(,)?) => {
            $(endpoints.push(<$contract as OperationApiContract>::ENDPOINT);)+
        };
    }
    ployz_sdk_types::operation_api_contracts!(push_endpoints);
    endpoints
}

fn operation_api_contract_rows() -> Vec<(
    &'static str,
    &'static str,
    OperationApiEndpointExecution,
    String,
    String,
    String,
    &'static str,
)> {
    let config = Config::new().with_large_int("number");
    let mut rows = Vec::new();
    macro_rules! push_rows {
        ($($contract:ty),+ $(,)?) => {
            $(rows.push(contract_row_for::<$contract>(&config));)+
        };
    }
    ployz_sdk_types::operation_api_contracts!(push_rows);
    rows
}

fn contract_row_for<C>(
    config: &Config,
) -> (
    &'static str,
    &'static str,
    OperationApiEndpointExecution,
    String,
    String,
    String,
    &'static str,
)
where
    C: OperationApiContract,
    C::Request: TS,
    C::Success: TS,
    C::Error: TS,
{
    (
        C::ENDPOINT.name(),
        C::ENDPOINT.subject(),
        C::ENDPOINT.execution(),
        C::REQUEST_ALIAS.map_or_else(|| C::Request::name(config), str::to_owned),
        C::Success::name(config),
        C::Error::name(config),
        C::RESPONSE_ALIAS,
    )
}

fn operation_lease(operation_id: &str, owner_id: &str, expires_at: u64) -> OperationOwnerLease {
    OperationOwnerLease::new(
        ployz_sdk_types::OperationId::try_new(operation_id).expect("valid operation id"),
        OperationOwnerId::try_new(owner_id).expect("valid owner id"),
        OperationLeaseExpiresAt::try_new(expires_at).expect("valid lease expiry"),
    )
}

fn machine_join_bundle() -> MachineJoinBundle {
    MachineJoinBundle {
        cluster_name: ployz_core::install::MachineJoinClusterName::try_new("prod")
            .expect("valid cluster name"),
        ployzd: MachineJoinPloyzdArtifact {
            version: ployz_core::install::InstallArtifactVersion::try_new("0.1.0")
                .expect("valid version"),
            source: ployz_core::install::InstallArtifactSource::try_new("/tmp/ployzd")
                .expect("valid source"),
            sha256: ployz_core::install::InstallSha256Digest::try_new(
                "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
            )
            .expect("valid digest"),
            install_path: ployz_core::install::AbsoluteInstallPath::try_new(
                "/usr/local/bin/ployzd",
            )
            .expect("valid install path"),
        },
    }
}

#[test]
fn sdk_exports_constructor_error_types() {
    assert!(matches!(
        ImageReference::try_new(""),
        Err(ImageReferenceError::Empty)
    ));
    assert!(matches!(
        ReplicaCount::try_new(0),
        Err(ReplicaCountError::Zero)
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
        CertBundleRef::try_new("file://PLZ_CERTS/cert_api/rev_1"),
        Err(CertTextError::InvalidBundleRef { .. })
    ));
    assert!(matches!(
        RouteHostname::try_new("-api.example.com"),
        Err(RouteHostnameError::Invalid { .. })
    ));
    assert!(matches!(RoutePort::try_new(0), Err(RoutePortError::Zero)));
    assert!(matches!(
        MachineBootstrapUrl::try_new("http://get.ployz.sh"),
        Err(ployz_sdk_types::BootstrapCommandError::InvalidBootstrapUrl)
    ));
    assert!(matches!(
        MachineJoinToken::try_new("join token"),
        Err(ployz_sdk_types::BootstrapCommandError::InvalidJoinToken)
    ));
}

fn valid_at(value: u64) -> CertValidAt {
    CertValidAt::try_new(value).expect("valid cert timestamp")
}
