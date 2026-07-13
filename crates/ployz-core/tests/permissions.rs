use ployz_core::permissions::{
    NatsPermissionProfile, ResponsePermission, inbox_prefix, inbox_subscribe_scope,
};
use ployz_core::security::NatsPrincipal;
use ployz_core::subjects::{
    CORE_RPC_QUERY_SCOPE, DATAPLANE_PROJECTION_GET, INTENT_CHANGED, INTENT_GET,
    JOIN_MACHINE_REDEEM, JOIN_MACHINE_REPORT, MACHINE_RPC_COMMAND_SCOPE, MACHINE_RPC_QUERY_SCOPE,
    MachineServiceEndpoint, OPERATION_PROGRESS_SCOPE, OPERATOR_MACHINE_IMAGE_COMMAND_SCOPE,
    OPERATOR_MACHINE_IMAGE_QUERY_SCOPE, OPERATOR_RPC_COMMAND_SCOPE, OPERATOR_RPC_QUERY_SCOPE,
    OPERATOR_RUNTIME_SNAPSHOT, PENDING_MACHINE_JOINS_CHANGED, gateway_status, gateway_status_scope,
    machine_container_facts, machine_facts, machine_facts_scope, machine_service,
    machine_service_command_scope, machine_service_query_scope,
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
        DATAPLANE_PROJECTION_GET.to_owned(),
        machine_facts(&machine_id),
        machine_container_facts(&machine_id),
        gateway_status(&machine_id),
    ];
    assert_eq!(profile.publish.allowed_subjects(), expected_publish);
    assert_eq!(profile.publish.denied_subjects(), &[] as &[String]);
    assert_eq!(
        profile.subscribe.allowed_subjects(),
        &[
            machine_service_query_scope(&machine_id),
            machine_service_command_scope(&machine_id),
            INTENT_CHANGED.to_owned(),
            PENDING_MACHINE_JOINS_CHANGED.to_owned(),
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
            MACHINE_RPC_QUERY_SCOPE.to_owned(),
            MACHINE_RPC_COMMAND_SCOPE.to_owned(),
            CORE_RPC_QUERY_SCOPE.to_owned(),
            OPERATION_PROGRESS_SCOPE.to_owned(),
            ployz_core::subjects::INTENT_CHANGED.to_owned(),
            PENDING_MACHINE_JOINS_CHANGED.to_owned(),
        ]
    );
    assert_eq!(
        profile.subscribe.allowed_subjects(),
        &[
            OPERATOR_RPC_QUERY_SCOPE.to_owned(),
            OPERATOR_RPC_COMMAND_SCOPE.to_owned(),
            JOIN_MACHINE_REDEEM.to_owned(),
            JOIN_MACHINE_REPORT.to_owned(),
            CORE_RPC_QUERY_SCOPE.to_owned(),
            MACHINE_RPC_QUERY_SCOPE.to_owned(),
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
fn operator_credential_renders_operator_rpc_scope_without_machine_or_join_scope() {
    let profile = NatsPermissionProfile::render(NatsPrincipal::Operator);

    assert_eq!(
        profile.publish.allowed_subjects(),
        &[
            OPERATOR_RPC_QUERY_SCOPE.to_owned(),
            OPERATOR_RPC_COMMAND_SCOPE.to_owned(),
            OPERATOR_MACHINE_IMAGE_QUERY_SCOPE.to_owned(),
            OPERATOR_MACHINE_IMAGE_COMMAND_SCOPE.to_owned(),
            INTENT_GET.to_owned(),
        ]
    );
    assert_eq!(
        profile.subscribe.allowed_subjects(),
        &[
            "_INBOX_operator.>".to_owned(),
            OPERATION_PROGRESS_SCOPE.to_owned(),
            machine_facts_scope(),
            gateway_status_scope(),
            INTENT_CHANGED.to_owned(),
        ]
    );
    assert!(
        !profile
            .publish
            .allowed_subjects()
            .contains(&MACHINE_RPC_QUERY_SCOPE.to_owned())
    );
    assert!(
        !profile
            .publish
            .allowed_subjects()
            .contains(&JOIN_MACHINE_REDEEM.to_owned())
    );
}

#[test]
fn image_ensure_is_controller_scoped_not_operator_image_scoped() {
    let subject = machine_service(
        &machine_id("machine_7"),
        MachineServiceEndpoint::ImageEnsure,
    );

    assert_eq!(
        subject,
        "plz.v1.rpc.machine.command.machine_7.container.ensure_image"
    );
    assert!(!subject.contains(".image."));
}

#[test]
fn runtime_snapshot_endpoint_is_inside_the_operator_query_scope() {
    assert!(OPERATOR_RUNTIME_SNAPSHOT.starts_with("plz.v1.rpc.operator.query."));
    let profile = NatsPermissionProfile::render(NatsPrincipal::Operator);

    assert_eq!(
        profile.publish.allowed_subjects(),
        &[
            OPERATOR_RPC_QUERY_SCOPE.to_owned(),
            OPERATOR_RPC_COMMAND_SCOPE.to_owned(),
            OPERATOR_MACHINE_IMAGE_QUERY_SCOPE.to_owned(),
            OPERATOR_MACHINE_IMAGE_COMMAND_SCOPE.to_owned(),
            INTENT_GET.to_owned(),
        ]
    );
}

#[test]
fn join_credential_can_only_redeem_and_report_with_its_own_inbox() {
    let profile = NatsPermissionProfile::render(NatsPrincipal::Join);

    assert_eq!(
        profile.publish.allowed_subjects(),
        &[
            JOIN_MACHINE_REDEEM.to_owned(),
            JOIN_MACHINE_REPORT.to_owned()
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
            .contains(&OPERATOR_RPC_QUERY_SCOPE.to_owned())
    );
}

#[test]
fn join_scope_does_not_include_operator_rpc() {
    let profile = NatsPermissionProfile::render(NatsPrincipal::Join);

    assert!(
        !profile
            .publish
            .allowed_subjects()
            .contains(&OPERATOR_RPC_COMMAND_SCOPE.to_owned())
    );
    assert!(
        !profile
            .publish
            .allowed_subjects()
            .contains(&OPERATOR_RPC_QUERY_SCOPE.to_owned())
    );
}

#[test]
fn machine_scope_is_bound_to_its_own_machine_id() {
    let machine_a = machine_id("machine_a");
    let machine_b = machine_id("machine_b");
    let profile = NatsPermissionProfile::render(NatsPrincipal::Machine {
        machine_id: machine_a.clone(),
    });

    assert!(
        profile
            .publish
            .allowed_subjects()
            .contains(&machine_facts(&machine_a))
    );
    assert!(
        !profile
            .publish
            .allowed_subjects()
            .contains(&machine_facts(&machine_b))
    );
    assert!(
        profile
            .subscribe
            .allowed_subjects()
            .contains(&machine_service_query_scope(&machine_a))
    );
    assert!(
        !profile
            .subscribe
            .allowed_subjects()
            .contains(&machine_service_query_scope(&machine_b))
    );
}

#[test]
fn no_profile_subscribes_the_shared_inbox_scope() {
    let principals = [
        NatsPrincipal::Machine {
            machine_id: machine_id("machine_7"),
        },
        NatsPrincipal::Controller,
        NatsPrincipal::Operator,
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
    assert_eq!(inbox_prefix(&NatsPrincipal::Operator), "_INBOX_operator");
    assert_eq!(inbox_prefix(&NatsPrincipal::Join), "_INBOX_join");
    assert_eq!(inbox_prefix(&NatsPrincipal::System), "_INBOX_sys");
    assert_eq!(
        inbox_prefix(&NatsPrincipal::Machine {
            machine_id: machine_id("machine_7")
        }),
        "_INBOX_machine_machine_7"
    );
}
