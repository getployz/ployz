use ployz_core::ids::{NodeId, OperationId, SubjectTokenError};
use ployz_core::subjects::{
    NodeObservationEvent, NodeServiceEndpoint, node_observation, node_service, op_deploy_submitted,
    op_watch,
};

#[test]
fn operation_subjects_use_validated_operation_ids() {
    let op_id = OperationId::try_new("op_123").expect("valid operation id");

    assert_eq!(op_watch(&op_id), "plz.v1.op.op_123.>");
    assert_eq!(
        op_deploy_submitted(&op_id),
        "plz.v1.op.op_123.deploy.submitted"
    );
}

#[test]
fn node_subjects_use_known_endpoint_and_event_tokens() {
    let node_id = NodeId::try_new("node_7").expect("valid node id");

    assert_eq!(
        node_service(&node_id, NodeServiceEndpoint::ContainerRun),
        "plz.v1.svc.node.node_7.container.run"
    );
    assert_eq!(
        node_observation(&node_id, NodeObservationEvent::ContainerRunning),
        "plz.v1.obs.node.node_7.container.running"
    );
}

#[test]
fn ids_reject_wildcard_subject_tokens() {
    assert_eq!(
        OperationId::try_new("op.>"),
        Err(SubjectTokenError::InvalidCharacter {
            value: "op.>".to_owned()
        })
    );
}

#[test]
fn ids_use_positive_ascii_token_grammar() {
    assert_eq!(
        OperationId::try_new("op\u{7}123"),
        Err(SubjectTokenError::InvalidCharacter {
            value: "op\u{7}123".to_owned()
        })
    );
    assert_eq!(
        OperationId::try_new("op/123"),
        Err(SubjectTokenError::InvalidCharacter {
            value: "op/123".to_owned()
        })
    );
}
