use ployz_sdk_types::{
    DeployOperationState, DeployRequest, DeployRunningStage, EventSequence, EventSequenceError,
    ImageReference, ImageReferenceError, NonEmptyTextError, OperationStatus, OperationSubject,
    ReplicaCount, ReplicaCountError, RevisionId, RouteHostname, RouteHostnameError, RoutePort,
    RoutePortError, ServiceId, SubjectTokenError,
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
        r#"{"kind":"deploy","id":"op_123","service_id":"svc_api","state":{"state":"accepted"},"last_event_sequence":1}"#
    );
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
        ployz_sdk_types::CancellationReason::try_new(""),
        Err(NonEmptyTextError::Empty)
    ));
    assert!(matches!(
        RouteHostname::try_new("-api.example.com"),
        Err(RouteHostnameError::Invalid { .. })
    ));
    assert!(matches!(RoutePort::try_new(0), Err(RoutePortError::Zero)));
}
