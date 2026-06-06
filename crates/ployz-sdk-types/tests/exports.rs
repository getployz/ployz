use ployz_sdk_types::{
    AcceptedOperation, AcmeChallengeToken, AcmeChallengeTtlSeconds, AcmeChallengeValue,
    AcmeHttp01Challenge, ActiveCertState, CertBundleRef, CertId, CertOperationState,
    CertRunningStage, CertTextError, CertValidAt, CertValidityWindow, DeployOperationState,
    DeployRequest, DeployRunningStage, DeploySubmitRequest, DeploySubmitResponse, EventSequence,
    EventSequenceError, ImageReference, ImageReferenceError, MAX_OPERATION_EVENT_REPLAY_LIMIT,
    NonEmptyTextError, OperationApiResponse, OperationEvent, OperationEventReplayCursor,
    OperationEventReplayLimit, OperationEventReplayLimitError, OperationEventReplayPage,
    OperationEventReplayRequest, OperationIdempotencyKey, OperationLeaseExpiresAt,
    OperationOwnerId, OperationOwnerLease, OperationStatus, OperationStatusSnapshot,
    OperationSubject, OpsStatusResponse, OpsWatchResponse, ReplicaCount, ReplicaCountError,
    RevisionId, RouteHostname, RouteHostnameError, RoutePort, RoutePortError, ServiceId,
    SubjectTokenError,
};

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
}

#[test]
fn typescript_contract_fixture_matches_rust_wire_types() {
    let fixture: serde_json::Value = serde_json::from_str(include_str!(
        "../../../packages/ployz-sdk/test/fixtures/operation-contract.json"
    ))
    .expect("fixture is json");

    assert_fixture::<DeploySubmitRequest>(&fixture, "deploy_submit_request");
    assert_fixture::<OperationEventReplayRequest>(&fixture, "ops_watch_request");
    assert_fixture::<AcceptedOperation>(&fixture, "accepted_operation");
    assert_fixture::<DeploySubmitResponse>(&fixture, "deploy_submit_response");
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

fn operation_lease(operation_id: &str, owner_id: &str, expires_at: u64) -> OperationOwnerLease {
    OperationOwnerLease::new(
        ployz_sdk_types::OperationId::try_new(operation_id).expect("valid operation id"),
        OperationOwnerId::try_new(owner_id).expect("valid owner id"),
        OperationLeaseExpiresAt::try_new(expires_at).expect("valid lease expiry"),
    )
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
}

fn valid_at(value: u64) -> CertValidAt {
    CertValidAt::try_new(value).expect("valid cert timestamp")
}
