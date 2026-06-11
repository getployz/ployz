use ployz_core::ids::NodeId;
use ployz_core::permissions::{
    NatsPermissionProfile, ResponsePermission, active_machine_state_kv_write_scope,
    active_route_state_kv_write_scope, active_service_state_kv_write_scope, inbox_prefix,
    inbox_subscribe_scope, kv_read_js_api_subjects, lock_kv_write_scope,
    nats_authorized_user_kv_write_scope, observation_kv_write_scope,
    operation_status_kv_write_scope,
};
use ployz_core::security::NatsPrincipal;
use ployz_core::subjects::{
    API_MACHINE_JOIN_REDEEM, API_MACHINE_JOIN_REPORT, API_SERVICE_SCOPE, AUDIT_STREAM_SUBJECT,
    JOBS_STREAM_SUBJECT, NODE_SERVICE_SCOPE, OPS_STREAM_SUBJECT, node_observation_scope,
    node_service_scope,
};

#[test]
fn node_credential_renders_own_scopes_and_route_state_reads() {
    let node_id = node_id("node_7");
    let profile = NatsPermissionProfile::render(NatsPrincipal::Node {
        node_id: node_id.clone(),
    });

    let mut expected_publish = vec![
        node_observation_scope(&node_id),
        observation_kv_write_scope(),
    ];
    expected_publish.extend(kv_read_js_api_subjects("KV_OBS"));
    expected_publish.extend(kv_read_js_api_subjects("KV_CORE"));
    assert_eq!(profile.publish.allowed_subjects(), expected_publish);
    assert_eq!(
        profile.publish.denied_subjects(),
        &["$KV.KV_CORE.>".to_owned()]
    );
    assert_eq!(
        profile.subscribe.allowed_subjects(),
        &[
            node_service_scope(&node_id),
            "_INBOX_node_node_7.>".to_owned()
        ]
    );
    assert_eq!(profile.subscribe.denied_subjects(), &[] as &[String]);
    assert_eq!(profile.allow_responses, ResponsePermission::Allowed);
}

#[test]
fn controller_credential_renders_owner_node_service_and_jetstream_scopes() {
    let profile = NatsPermissionProfile::render(NatsPrincipal::Controller);

    assert_eq!(
        profile.publish.allowed_subjects(),
        &[
            NODE_SERVICE_SCOPE.to_owned(),
            OPS_STREAM_SUBJECT.to_owned(),
            JOBS_STREAM_SUBJECT.to_owned(),
            AUDIT_STREAM_SUBJECT.to_owned(),
            "$JS.API.>".to_owned(),
            "$JS.ACK.>".to_owned(),
            "$O.PLZ_BACKUPS.>".to_owned(),
            active_service_state_kv_write_scope(),
            active_route_state_kv_write_scope(),
            active_machine_state_kv_write_scope(),
            nats_authorized_user_kv_write_scope(),
            operation_status_kv_write_scope(),
            lock_kv_write_scope(),
        ]
    );
    assert_eq!(
        profile.subscribe.allowed_subjects(),
        &[
            JOBS_STREAM_SUBJECT.to_owned(),
            API_SERVICE_SCOPE.to_owned(),
            "_INBOX_ctl.>".to_owned()
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
        &["_INBOX_user.>".to_owned(), OPS_STREAM_SUBJECT.to_owned()]
    );
    assert!(
        !profile
            .publish
            .allowed_subjects()
            .contains(&NODE_SERVICE_SCOPE.to_owned())
    );
}

#[test]
fn join_credential_can_only_redeem_and_report_with_its_own_inbox() {
    let profile = NatsPermissionProfile::render(NatsPrincipal::Join);

    assert_eq!(
        profile.publish.allowed_subjects(),
        &[
            API_MACHINE_JOIN_REDEEM.to_owned(),
            API_MACHINE_JOIN_REPORT.to_owned()
        ]
    );
    assert_eq!(
        profile.subscribe.allowed_subjects(),
        &["_INBOX_join.>".to_owned()]
    );
    assert_eq!(profile.allow_responses, ResponsePermission::Allowed);
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
        &["$SYS.>".to_owned(), "_INBOX_sys.>".to_owned()]
    );
    assert!(
        !profile
            .publish
            .allowed_subjects()
            .contains(&API_SERVICE_SCOPE.to_owned())
    );
}

#[test]
fn no_profile_subscribes_the_shared_inbox_scope() {
    let principals = [
        NatsPrincipal::Node {
            node_id: node_id("node_7"),
        },
        NatsPrincipal::Controller,
        NatsPrincipal::User,
        NatsPrincipal::Join,
        NatsPrincipal::System,
    ];

    for principal in principals {
        let profile = NatsPermissionProfile::render(principal.clone());
        assert!(
            !profile
                .subscribe
                .allowed_subjects()
                .contains(&"_INBOX.>".to_owned()),
            "{principal:?} must not subscribe the shared inbox scope"
        );
        assert!(
            profile
                .subscribe
                .allowed_subjects()
                .contains(&inbox_subscribe_scope(&principal)),
            "{principal:?} must subscribe its own inbox prefix"
        );
    }
}

#[test]
fn inbox_prefixes_are_distinct_per_principal() {
    assert_eq!(inbox_prefix(&NatsPrincipal::Controller), "_INBOX_ctl");
    assert_eq!(inbox_prefix(&NatsPrincipal::User), "_INBOX_user");
    assert_eq!(inbox_prefix(&NatsPrincipal::Join), "_INBOX_join");
    assert_eq!(inbox_prefix(&NatsPrincipal::System), "_INBOX_sys");
    assert_eq!(
        inbox_prefix(&NatsPrincipal::Node {
            node_id: node_id("node_7")
        }),
        "_INBOX_node_node_7"
    );
}

fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}
