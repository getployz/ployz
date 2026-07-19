use ployz_core::build::BUILD_RESPONSE_PERMISSION_EXPIRY;
use ployz_core::ids::{BuildExecutorId, BuildPoolId};
use ployz_core::security::NatsPrincipal;
use ployz_nats::permissions::{
    NatsPermissionProfile, ResponsePermission, inbox_prefix, inbox_subscribe_scope,
};
use ployz_nats::subjects::MachineServiceEndpoint;
use ployz_nats::subjects::{
    BUILD_EXECUTOR_RPC_BUILD_CANCEL_SCOPE, BUILD_EXECUTOR_RPC_BUILD_START_SCOPE,
    BUILD_EXECUTOR_RPC_READINESS_SCOPE, BUILD_EXECUTOR_SIGNAL_LOG_SCOPE,
    BuildExecutorServiceEndpoint, CORE_RPC_QUERY_SCOPE, INGRESS_ENDPOINT_CHANGED,
    INGRESS_ENDPOINT_GET, INTENT_CHANGED, INTENT_GET, JOIN_MACHINE_REDEEM, JOIN_MACHINE_REPORT,
    MACHINE_RPC_COMMAND_SCOPE, MACHINE_RPC_QUERY_SCOPE, OPERATION_PROGRESS_SCOPE,
    OPERATOR_INIT_FIRST_MACHINE_ACTIVATE, OPERATOR_MACHINE_IMAGE_COMMAND_SCOPE,
    OPERATOR_MACHINE_IMAGE_QUERY_SCOPE, OPERATOR_RPC_COMMAND_SCOPE, OPERATOR_RPC_QUERY_SCOPE,
    OPERATOR_RUNTIME_SNAPSHOT, PENDING_MACHINE_JOINS_CHANGED, RUNTIME_SNAPSHOT_SEED,
    RUNTIME_SNAPSHOT_STREAM, build_executor_log_publish_scope, build_executor_service,
    build_executor_service_discovery_subscriptions, gateway_status, gateway_status_scope,
    machine_build_log_publish_scope, machine_build_log_subscribe_scope, machine_container_facts,
    machine_facts, machine_facts_scope, machine_service, machine_service_command_scope,
    machine_service_query_scope,
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
        INGRESS_ENDPOINT_GET.to_owned(),
        machine_facts(&machine_id),
        machine_container_facts(&machine_id),
        gateway_status(&machine_id),
        machine_build_log_publish_scope(&machine_id),
    ];
    assert_eq!(profile.publish.allowed_subjects(), expected_publish);
    assert_eq!(profile.publish.denied_subjects(), &[] as &[String]);
    assert_eq!(
        profile.subscribe.allowed_subjects(),
        &[
            machine_service_query_scope(&machine_id),
            machine_service_command_scope(&machine_id),
            INTENT_CHANGED.to_owned(),
            INGRESS_ENDPOINT_CHANGED.to_owned(),
            PENDING_MACHINE_JOINS_CHANGED.to_owned(),
            machine_facts_scope(),
            gateway_status_scope(),
            "$SRV.>".to_owned(),
            "_INBOX_machine_machine_7.>".to_owned()
        ]
    );
    assert_eq!(profile.subscribe.denied_subjects(), &[] as &[String]);
    assert_eq!(
        profile.allow_responses,
        ResponsePermission::RequestScoped {
            expires: BUILD_RESPONSE_PERMISSION_EXPIRY
        }
    );
}

#[test]
fn controller_credential_renders_owner_machine_service_and_progress_scopes() {
    let profile = NatsPermissionProfile::render(NatsPrincipal::Controller);

    assert_eq!(
        profile.publish.allowed_subjects(),
        &[
            "_INBOX_ctl.>".to_owned(),
            OPERATOR_INIT_FIRST_MACHINE_ACTIVATE.to_owned(),
            MACHINE_RPC_QUERY_SCOPE.to_owned(),
            MACHINE_RPC_COMMAND_SCOPE.to_owned(),
            BUILD_EXECUTOR_RPC_READINESS_SCOPE.to_owned(),
            BUILD_EXECUTOR_RPC_BUILD_START_SCOPE.to_owned(),
            BUILD_EXECUTOR_RPC_BUILD_CANCEL_SCOPE.to_owned(),
            CORE_RPC_QUERY_SCOPE.to_owned(),
            OPERATION_PROGRESS_SCOPE.to_owned(),
            ployz_nats::subjects::INTENT_CHANGED.to_owned(),
            INGRESS_ENDPOINT_CHANGED.to_owned(),
            PENDING_MACHINE_JOINS_CHANGED.to_owned(),
            RUNTIME_SNAPSHOT_STREAM.to_owned(),
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
            ployz_nats::subjects::INTENT_GET.to_owned(),
            INTENT_CHANGED.to_owned(),
            INGRESS_ENDPOINT_CHANGED.to_owned(),
            machine_facts_scope(),
            machine_build_log_subscribe_scope(),
            BUILD_EXECUTOR_SIGNAL_LOG_SCOPE.to_owned(),
            gateway_status_scope(),
            "$SRV.>".to_owned(),
            "_INBOX_ctl.>".to_owned()
        ]
    );
    assert_eq!(profile.publish.denied_subjects(), &[] as &[String]);
}

#[test]
fn build_executor_credential_is_bound_to_one_external_executor() {
    let pool_id = BuildPoolId::try_new("pool-a").expect("pool");
    let executor_id = BuildExecutorId::try_new("executor-a").expect("executor");
    let principal = NatsPrincipal::BuildExecutor {
        pool_id: pool_id.clone(),
        executor_id: executor_id.clone(),
    };
    let profile = NatsPermissionProfile::render(principal.clone());

    assert_eq!(
        profile.publish.allowed_subjects(),
        &[
            "_INBOX_build_executor.pool-a.executor-a.>".to_owned(),
            build_executor_log_publish_scope(&pool_id, &executor_id),
            OPERATOR_MACHINE_IMAGE_QUERY_SCOPE.to_owned(),
            OPERATOR_MACHINE_IMAGE_COMMAND_SCOPE.to_owned(),
        ]
    );
    let mut expected_subscribe = vec![
        build_executor_service(
            &pool_id,
            &executor_id,
            BuildExecutorServiceEndpoint::ReadinessGet,
        ),
        build_executor_service(
            &pool_id,
            &executor_id,
            BuildExecutorServiceEndpoint::BuildStart,
        ),
        build_executor_service(
            &pool_id,
            &executor_id,
            BuildExecutorServiceEndpoint::BuildCancel,
        ),
    ];
    expected_subscribe.extend(build_executor_service_discovery_subscriptions());
    expected_subscribe.push("_INBOX_build_executor.pool-a.executor-a.>".to_owned());
    assert_eq!(profile.subscribe.allowed_subjects(), expected_subscribe);
    assert_eq!(profile.publish.denied_subjects(), &[] as &[String]);
    assert_eq!(profile.subscribe.denied_subjects(), &[] as &[String]);
    assert_eq!(
        profile.allow_responses,
        ResponsePermission::RequestScoped {
            expires: BUILD_RESPONSE_PERMISSION_EXPIRY,
        }
    );
    assert_eq!(
        inbox_prefix(&principal),
        "_INBOX_build_executor.pool-a.executor-a"
    );
}

#[test]
fn build_executor_permission_matrix_excludes_other_authority() {
    let pool_id = BuildPoolId::try_new("pool-a").expect("pool");
    let executor_id = BuildExecutorId::try_new("executor-a").expect("executor");
    let profile = NatsPermissionProfile::render(NatsPrincipal::BuildExecutor {
        pool_id: pool_id.clone(),
        executor_id: executor_id.clone(),
    });
    let other_pool = BuildPoolId::try_new("pool-b").expect("pool");
    let other_executor = BuildExecutorId::try_new("executor-b").expect("executor");

    for allowed in [
        build_executor_service(
            &pool_id,
            &executor_id,
            BuildExecutorServiceEndpoint::ReadinessGet,
        ),
        build_executor_service(
            &pool_id,
            &executor_id,
            BuildExecutorServiceEndpoint::BuildStart,
        ),
        build_executor_service(
            &pool_id,
            &executor_id,
            BuildExecutorServiceEndpoint::BuildCancel,
        ),
        "$SRV.PING.plz-build-executor.runtime-id".to_owned(),
    ] {
        assert!(
            profile
                .subscribe
                .allowed_subjects()
                .iter()
                .any(|pattern| nats_subject_matches(pattern, &allowed)),
            "expected subscription authority for {allowed}"
        );
    }

    for denied in [
        build_executor_service(
            &other_pool,
            &executor_id,
            BuildExecutorServiceEndpoint::BuildStart,
        ),
        build_executor_service(
            &pool_id,
            &other_executor,
            BuildExecutorServiceEndpoint::BuildCancel,
        ),
        "$SRV.PING.other-service.runtime-id".to_owned(),
        "$SRV.PING.plz-build-executor.runtime.extra".to_owned(),
        "plz.v1.rpc.operator.command.build.submit".to_owned(),
        "plz.v1.rpc.machine.command.machine-a.build.start".to_owned(),
        "plz.v1.rpc.machine.query.machine-a.facts.get".to_owned(),
    ] {
        assert!(
            profile
                .subscribe
                .allowed_subjects()
                .iter()
                .all(|pattern| !nats_subject_matches(pattern, &denied)),
            "unexpected subscription authority for {denied}"
        );
    }

    for allowed in [
        "plz.v1.signal.build_executor.pool-a.executor-a.build.operation.op-1.log",
        "plz.v1.rpc.machine.query.machine-a.image.blob.check",
        "plz.v1.rpc.machine.command.machine-a.image.blob.push",
    ] {
        assert!(
            profile
                .publish
                .allowed_subjects()
                .iter()
                .any(|pattern| nats_subject_matches(pattern, allowed)),
            "expected publication authority for {allowed}"
        );
    }

    for denied in [
        "plz.v1.signal.build_executor.pool-b.executor-a.build.operation.op-1.log",
        "plz.v1.signal.build_executor.pool-a.executor-b.build.operation.op-1.log",
        "plz.v1.signal.machine.machine-a.build.operation.op-1.log",
        "plz.v1.testimony.machine.machine-a.snapshot",
        "plz.v1.rpc.operator.query.ops.list",
        "plz.v1.rpc.machine.command.machine-a.container.run",
    ] {
        assert!(
            profile
                .publish
                .allowed_subjects()
                .iter()
                .all(|pattern| !nats_subject_matches(pattern, denied)),
            "unexpected publication authority for {denied}"
        );
    }
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
            INGRESS_ENDPOINT_GET.to_owned(),
        ]
    );
    assert_eq!(
        profile.subscribe.allowed_subjects(),
        &[
            "_INBOX_operator.>".to_owned(),
            INGRESS_ENDPOINT_CHANGED.to_owned(),
            OPERATION_PROGRESS_SCOPE.to_owned(),
            RUNTIME_SNAPSHOT_STREAM.to_owned(),
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
fn image_remove_is_controller_scoped_and_operator_image_wildcards_do_not_match() {
    let subject = machine_service(
        &machine_id("machine_7"),
        MachineServiceEndpoint::ImageRemove,
    );
    let controller = NatsPermissionProfile::render(NatsPrincipal::Controller);
    let operator = NatsPermissionProfile::render(NatsPrincipal::Operator);
    let machine = NatsPermissionProfile::render(NatsPrincipal::Machine {
        machine_id: machine_id("machine_7"),
    });
    let other_machine_subject = machine_service(
        &machine_id("machine_8"),
        MachineServiceEndpoint::ImageRemove,
    );

    assert!(
        controller
            .publish
            .allowed_subjects()
            .iter()
            .any(|pattern| nats_subject_matches(pattern, &subject))
    );
    assert!(
        operator
            .publish
            .allowed_subjects()
            .iter()
            .all(|pattern| !nats_subject_matches(pattern, &subject))
    );
    assert!(
        machine
            .subscribe
            .allowed_subjects()
            .iter()
            .any(|pattern| nats_subject_matches(pattern, &subject))
    );
    assert!(
        machine
            .subscribe
            .allowed_subjects()
            .iter()
            .all(|pattern| !nats_subject_matches(pattern, &other_machine_subject))
    );
    assert_eq!(
        subject,
        "plz.v1.rpc.machine.command.machine_7.container.remove_image"
    );
}

fn nats_subject_matches(pattern: &str, subject: &str) -> bool {
    let mut pattern = pattern.split('.');
    let mut subject = subject.split('.');
    loop {
        match (pattern.next(), subject.next()) {
            (Some(">"), _) => return true,
            (Some("*"), Some(_)) => {}
            (Some(expected), Some(actual)) if expected == actual => {}
            (None, None) => return true,
            _ => return false,
        }
    }
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
            INGRESS_ENDPOINT_GET.to_owned(),
        ]
    );
}

#[test]
fn runtime_snapshot_stream_is_controller_published_and_operator_subscribed() {
    let controller = NatsPermissionProfile::render(NatsPrincipal::Controller);
    let operator = NatsPermissionProfile::render(NatsPrincipal::Operator);

    assert_eq!(
        RUNTIME_SNAPSHOT_STREAM,
        "plz.v1.projection.runtime.snapshot"
    );
    assert!(
        controller
            .publish
            .allowed_subjects()
            .contains(&RUNTIME_SNAPSHOT_STREAM.to_owned())
    );
    assert!(
        operator
            .subscribe
            .allowed_subjects()
            .contains(&RUNTIME_SNAPSHOT_STREAM.to_owned())
    );
    assert!(
        !operator
            .subscribe
            .allowed_subjects()
            .contains(&machine_facts_scope())
    );
    assert!(
        !operator
            .subscribe
            .allowed_subjects()
            .contains(&gateway_status_scope())
    );
    assert!(
        !operator
            .subscribe
            .allowed_subjects()
            .contains(&INTENT_CHANGED.to_owned())
    );
}

#[test]
fn runtime_snapshot_seed_is_an_operator_query_served_by_controller() {
    assert_eq!(
        RUNTIME_SNAPSHOT_SEED,
        "plz.v1.rpc.operator.query.runtime.snapshot.seed"
    );
    let operator = NatsPermissionProfile::render(NatsPrincipal::Operator);
    let controller = NatsPermissionProfile::render(NatsPrincipal::Controller);

    assert!(RUNTIME_SNAPSHOT_SEED.starts_with(OPERATOR_RPC_QUERY_SCOPE.trim_end_matches('>')));
    assert!(
        operator
            .publish
            .allowed_subjects()
            .contains(&OPERATOR_RPC_QUERY_SCOPE.to_owned())
    );
    assert!(
        controller
            .subscribe
            .allowed_subjects()
            .contains(&OPERATOR_RPC_QUERY_SCOPE.to_owned())
    );
    assert_eq!(
        controller.allow_responses,
        ResponsePermission::RequestScoped {
            expires: std::time::Duration::from_secs(2 * 60)
        }
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
        NatsPrincipal::BuildExecutor {
            pool_id: BuildPoolId::try_new("pool-a").expect("pool"),
            executor_id: BuildExecutorId::try_new("executor-a").expect("executor"),
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
        inbox_prefix(&NatsPrincipal::BuildExecutor {
            pool_id: BuildPoolId::try_new("pool-a").expect("pool"),
            executor_id: BuildExecutorId::try_new("executor-a").expect("executor"),
        }),
        "_INBOX_build_executor.pool-a.executor-a"
    );
    assert_ne!(
        inbox_prefix(&NatsPrincipal::BuildExecutor {
            pool_id: BuildPoolId::try_new("pool_a").expect("pool"),
            executor_id: BuildExecutorId::try_new("executor").expect("executor"),
        }),
        inbox_prefix(&NatsPrincipal::BuildExecutor {
            pool_id: BuildPoolId::try_new("pool").expect("pool"),
            executor_id: BuildExecutorId::try_new("a_executor").expect("executor"),
        })
    );
    assert_eq!(
        inbox_prefix(&NatsPrincipal::Machine {
            machine_id: machine_id("machine_7")
        }),
        "_INBOX_machine_machine_7"
    );
}
