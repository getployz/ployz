use ployz_core::ids::NodeId;
use ployz_core::security::NatsPrincipal;
use ployz_core::subjects::{
    API_SERVICE_SCOPE, AUDIT_STREAM_SUBJECT, DEPLOY_SUBMITTED_EVENTS_SUBJECT, JOBS_STREAM_SUBJECT,
    NODE_SERVICE_SCOPE, OPS_STREAM_SUBJECT, node_observation_scope, node_service_scope,
};
use ployz_nats::permissions::{
    NatsPermissionProfile, ResponsePermission, active_service_state_kv_write_scope,
};

#[test]
fn node_credential_renders_only_own_service_and_observation_scopes() {
    let node_id = node_id("node_7");
    let profile = NatsPermissionProfile::render(NatsPrincipal::Node {
        node_id: node_id.clone(),
    });

    assert_eq!(
        profile.publish.allowed_subjects(),
        &[node_observation_scope(&node_id)]
    );
    assert_eq!(
        profile.publish.denied_subjects(),
        &["$KV.KV_CORE.>".to_owned()]
    );
    assert_eq!(
        profile.subscribe.allowed_subjects(),
        &[node_service_scope(&node_id)]
    );
    assert_eq!(profile.subscribe.denied_subjects(), &[] as &[String]);
    assert_eq!(profile.allow_responses, ResponsePermission::Allowed);
}

#[test]
fn controller_credential_renders_worker_and_node_service_scopes() {
    let profile = NatsPermissionProfile::render(NatsPrincipal::Controller);

    assert_eq!(
        profile.publish.allowed_subjects(),
        &[
            NODE_SERVICE_SCOPE.to_owned(),
            OPS_STREAM_SUBJECT.to_owned(),
            JOBS_STREAM_SUBJECT.to_owned(),
            AUDIT_STREAM_SUBJECT.to_owned(),
            active_service_state_kv_write_scope(),
        ]
    );
    assert_eq!(
        profile.subscribe.allowed_subjects(),
        &[
            DEPLOY_SUBMITTED_EVENTS_SUBJECT.to_owned(),
            JOBS_STREAM_SUBJECT.to_owned(),
            "_INBOX.>".to_owned(),
        ]
    );
    assert_eq!(profile.publish.denied_subjects(), &[] as &[String]);
}

#[test]
fn user_credential_renders_api_service_scope_without_node_scope() {
    let profile = NatsPermissionProfile::render(NatsPrincipal::User);

    assert_eq!(
        profile.publish.allowed_subjects(),
        &[API_SERVICE_SCOPE.to_owned()]
    );
    assert_eq!(
        profile.subscribe.allowed_subjects(),
        &["_INBOX.>".to_owned(), OPS_STREAM_SUBJECT.to_owned()]
    );
    assert!(
        !profile
            .publish
            .allowed_subjects()
            .contains(&NODE_SERVICE_SCOPE.to_owned())
    );
}

#[test]
fn system_credential_renders_system_subjects_only() {
    let profile = NatsPermissionProfile::render(NatsPrincipal::System);

    assert_eq!(
        profile.publish.allowed_subjects(),
        &["$SYS.REQ.>".to_owned()]
    );
    assert_eq!(
        profile.subscribe.allowed_subjects(),
        &["$SYS.>".to_owned(), "_INBOX.>".to_owned()]
    );
    assert!(
        !profile
            .publish
            .allowed_subjects()
            .contains(&API_SERVICE_SCOPE.to_owned())
    );
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}
