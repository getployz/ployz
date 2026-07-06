use ployz_core::permissions::{
    NatsPermissionProfile, ResponsePermission, inbox_prefix, inbox_subscribe_scope,
};
use ployz_core::security::NatsPrincipal;
use ployz_core::subjects::{
    API_MACHINE_JOIN_REDEEM, API_MACHINE_JOIN_REPORT, API_RUNTIME_SNAPSHOT, API_SERVICE_SCOPE,
    INTENT_CHANGED, INTENT_GET, MACHINE_SERVICE_SCOPE, OPS_STREAM_SUBJECT, gateway_status,
    gateway_status_scope, machine_facts, machine_facts_scope, machine_service_scope,
};
use ployz_test_support::ids::machine_id;

#[test]
fn machine_credential_renders_own_scopes_and_intent_request() {
    let machine_id = machine_id("machine_7");
    let profile = NatsPermissionProfile::render(NatsPrincipal::Machine {
        machine_id: machine_id.clone(),
    });

    let expected_publish = vec![
        "_INBOX_machine_machine_7.>".to_owned(),
        INTENT_GET.to_owned(),
        machine_facts(&machine_id),
        gateway_status(&machine_id),
    ];
    assert_eq!(profile.publish.allowed_subjects(), expected_publish);
    assert_eq!(profile.publish.denied_subjects(), &[] as &[String]);
    assert_eq!(
        profile.subscribe.allowed_subjects(),
        &[
            machine_service_scope(&machine_id),
            INTENT_CHANGED.to_owned(),
            machine_facts_scope(),
            gateway_status_scope(),
            "$SRV.>".to_owned(),
            "_INBOX_machine_machine_7.>".to_owned()
        ]
    );
    assert_eq!(profile.subscribe.denied_subjects(), &[] as &[String]);
    assert_eq!(profile.allow_responses, ResponsePermission::Allowed);
}

#[test]
fn controller_credential_renders_owner_machine_service_and_progress_scopes() {
    let profile = NatsPermissionProfile::render(NatsPrincipal::Controller);

    assert_eq!(
        profile.publish.allowed_subjects(),
        &[
            "_INBOX_ctl.>".to_owned(),
            API_SERVICE_SCOPE.to_owned(),
            MACHINE_SERVICE_SCOPE.to_owned(),
            OPS_STREAM_SUBJECT.to_owned(),
            ployz_core::subjects::INTENT_GET.to_owned(),
            ployz_core::subjects::INTENT_CHANGED.to_owned(),
        ]
    );
    assert_eq!(
        profile.subscribe.allowed_subjects(),
        &[
            API_SERVICE_SCOPE.to_owned(),
            ployz_core::subjects::INTENT_CHANGED.to_owned(),
            ployz_core::subjects::INTENT_GET.to_owned(),
            machine_facts_scope(),
            gateway_status_scope(),
            "$SRV.>".to_owned(),
            "_INBOX_ctl.>".to_owned()
        ]
    );
    assert_eq!(profile.publish.denied_subjects(), &[] as &[String]);
}

#[test]
fn user_credential_renders_api_service_scope_without_machine_scope() {
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
            .contains(&MACHINE_SERVICE_SCOPE.to_owned())
    );
}

#[test]
fn runtime_snapshot_endpoint_is_inside_the_user_api_scope() {
    assert!(API_RUNTIME_SNAPSHOT.starts_with("plz.v1.svc.api."));
    let profile = NatsPermissionProfile::render(NatsPrincipal::User);

    assert_eq!(
        profile.publish.allowed_subjects(),
        &[API_SERVICE_SCOPE.to_owned()]
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
    assert_eq!(profile.allow_responses, ResponsePermission::Denied);
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
        NatsPrincipal::Machine {
            machine_id: machine_id("machine_7"),
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
        inbox_prefix(&NatsPrincipal::Machine {
            machine_id: machine_id("machine_7")
        }),
        "_INBOX_machine_machine_7"
    );
}
