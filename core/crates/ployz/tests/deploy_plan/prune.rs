use std::collections::{BTreeMap, BTreeSet};

use super::support::*;
use ployz::deploy::{Step, plan_deploy, preview_deploy};
use ployz_core::{
    ContainerKind, ContainerRuntimeObservation, DependencyCondition, DeployWarning, DockerVolumeId,
    MachineFailure, PruneRefusal, PruneScope, QualifiedService, RpcError, RpcErrorCode,
    ServiceDependency, ServiceName, ServiceRemovals, VolumeObservationFailure,
};

#[test]
fn incomplete_snapshot_lists_obsolete_services_and_removes_nothing() {
    let (web, snapshot) = shop_with_obsolete_debug();
    let snapshot = DeploySnapshot {
        volume_snapshot: VolumeSnapshot::try_from_parts(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![machine_id('1')],
        )
        .expect("valid Volume Snapshot fixture"),
        ..snapshot
    };
    let plan = preview_deploy(
        &DeployIntent::apply_all(
            Namespace::parse("app").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &snapshot,
    )
    .unwrap();
    assert_eq!(
        plan.would_remove,
        [QualifiedService::parse("app/debug").unwrap()]
    );
    assert_eq!(plan.prune_refusal, Some(PruneRefusal::IncompleteSnapshot));
    assert!(!removes(&plan, 'd'));
}

#[test]
fn selected_services_list_obsolete_services_and_remove_nothing() {
    let (web, snapshot) = shop_with_obsolete_debug();
    let plan = preview_deploy(
        &DeployIntent::apply_one(
            Namespace::parse("app").unwrap(),
            web,
            PlanOptions::default(),
        ),
        &snapshot,
    )
    .unwrap();
    assert_eq!(
        plan.would_remove,
        [QualifiedService::parse("app/debug").unwrap()]
    );
    assert_eq!(plan.prune_refusal, Some(PruneRefusal::SelectedServices));
    assert!(!removes(&plan, 'd'));
}

#[test]
fn required_container_failure_makes_the_snapshot_incomplete() {
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        container_failures: vec![MachineFailure {
            machine_id: machine_id('1'),
            error: RpcError {
                code: RpcErrorCode::Unavailable,
                message: "container listing failed".into(),
                details: Default::default(),
                cause: Vec::new(),
            },
        }],
        ..Default::default()
    };
    assert!(!snapshot.is_observer_complete());
}

#[test]
fn required_named_volume_failure_makes_the_snapshot_incomplete() {
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        volume_snapshot: VolumeSnapshot::try_from_parts(
            Vec::new(),
            vec![VolumeObservationFailure {
                id: DockerVolumeId {
                    machine_id: machine_id('1'),
                    name: app_volume("data"),
                },
                error: RpcError {
                    code: RpcErrorCode::Unavailable,
                    message: "detail failed".into(),
                    details: Default::default(),
                    cause: Vec::new(),
                },
            }],
            Vec::new(),
            Vec::new(),
        )
        .expect("valid Volume Snapshot fixture"),
        ..Default::default()
    };

    assert!(!snapshot.is_observer_complete());
}

#[test]
fn down_machine_omissions_do_not_make_the_snapshot_incomplete() {
    let mut down = machine('2', "second");
    down.membership = MembershipObservation::Down;
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), down],
        container_omissions: vec![machine_id('2')],
        volume_snapshot: VolumeSnapshot::try_from_parts(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![machine_id('2')],
        )
        .expect("valid Volume Snapshot fixture"),
        ..Default::default()
    };
    assert!(snapshot.is_observer_complete());
}

#[test]
fn full_reconciliation_removes_obsolete_services_after_desired_work() {
    let web = spec("web");
    let mut db = spec("db");
    add_named_volume(&mut db, "data");
    let debug = spec("debug");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![container('d', '1', &debug, &service_id('b'))],
        ..Default::default()
    };
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&db, &web],
        PlanOptions::default(),
    )
    .with_dependencies(BTreeMap::from([(
        web.name.clone(),
        vec![ServiceDependency {
            service: db.name.clone(),
            condition: DependencyCondition::ServiceStarted,
        }],
    )]));
    let plan = preview_deploy(&intent, &snapshot).unwrap();
    assert_eq!(
        plan.would_remove,
        [QualifiedService::parse("app/debug").unwrap()]
    );
    assert_eq!(plan.prune_refusal, None);
    match operations(&plan).as_slice() {
        [
            DeployOperation::RunContainer { spec: first, .. },
            DeployOperation::RunContainer { spec: second, .. },
            DeployOperation::RemoveContainer {
                container_id: removed,
                purpose: ployz_core::RemoveContainerPurpose::Retire,
                ..
            },
        ] => {
            assert_eq!(first.name.as_str(), "db");
            assert_eq!(second.name.as_str(), "web");
            assert_eq!(*removed, container_id('d'));
        }
        other => panic!("expected db, web, then prune, got {other:?}"),
    }
    assert_eq!(plan.volumes_to_create.len(), 1);
    let planned = ployz::deploy::plan_deploy(&intent, &snapshot).unwrap();
    let Some(tail) = planned.groups().last() else {
        panic!("expected a removal group");
    };
    assert_eq!(tail.service, None);
    assert_eq!(
        tail.dependencies
            .iter()
            .collect::<std::collections::BTreeSet<_>>(),
        [&db.name, &web.name].into_iter().collect()
    );
}

#[test]
fn selecting_one_service_does_not_remove_the_rest_of_the_namespace() {
    let web = spec("web");
    let api = spec("api");
    let debug = spec("debug");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![
            container('e', '1', &web, &service_id('a')),
            container('a', '1', &api, &service_id('c')),
            container('d', '1', &debug, &service_id('b')),
        ],
        ..Default::default()
    };
    let plan = preview_deploy(
        &DeployIntent::new(
            Namespace::parse("app").unwrap(),
            vec![web, api],
            PlanOptions {
                selected: vec![ServiceAttempt {
                    name: ServiceName::parse("web").unwrap(),
                }],
                ..PlanOptions::default()
            },
        ),
        &snapshot,
    )
    .unwrap();
    assert_eq!(
        plan.would_remove,
        [QualifiedService::parse("app/debug").unwrap()]
    );
    assert_eq!(plan.prune_refusal, Some(PruneRefusal::SelectedServices));
    assert!(!removes(&plan, 'a'));
    assert!(!removes(&plan, 'd'));
    assert!(!removes(&plan, 'e'));
}

#[test]
fn partial_deploy_leaves_an_imperative_service_unless_it_is_selected() {
    let web = spec("web");
    let debug = spec("debug");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![
            container('e', '1', &web, &service_id('a')),
            container('d', '1', &debug, &service_id('b')),
        ],
        ..Default::default()
    };
    let partial = preview_deploy(
        &DeployIntent::apply_one(
            Namespace::parse("app").unwrap(),
            web,
            PlanOptions::default(),
        ),
        &snapshot,
    )
    .unwrap();
    assert!(!removes(&partial, 'd'));
    assert_eq!(partial.prune_refusal, Some(PruneRefusal::SelectedServices));

    let mut requested_debug = debug;
    requested_debug.container.image = "busybox".into();
    let selected_debug = preview_deploy(
        &DeployIntent::apply_one(
            Namespace::parse("app").unwrap(),
            requested_debug,
            PlanOptions::default(),
        ),
        &snapshot,
    )
    .unwrap();
    assert!(
        selected_debug.operations.iter().any(|row| {
            matches!(
                &row.operation,
                DeployOperation::ReplaceContainer(replacement)
                    if replacement.old_container_id == container_id('d')
            )
        }),
        "selecting the imperative Service updates it instead of leaving it as drift: {:?}",
        selected_debug.operations
    );
    assert!(!removes(&selected_debug, 'e'));
}

#[test]
fn reserved_namespace_and_system_workloads_are_excluded_before_removal_is_planned() {
    let web = spec("web");
    let mut system_ingress = spec("ingress");
    system_ingress.mode = ServiceMode::Global;
    let mut leftover = container('c', '1', &system_ingress, &service_id('a'));
    leftover
        .try_update(|parts| parts.namespace = Namespace::system())
        .unwrap();
    let shop = preview_deploy(
        &DeployIntent::apply_all(
            Namespace::parse("shop").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![leftover.clone()],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!removes(&shop, 'c'));
    assert!(shop.would_remove.is_empty());

    let metrics = spec("metrics");
    let mut extra = container('3', '1', &metrics, &service_id('b'));
    extra
        .try_update(|parts| parts.namespace = Namespace::system())
        .unwrap();
    leftover
        .try_update(|parts| parts.namespace = Namespace::system())
        .unwrap();
    let system = preview_deploy(
        &DeployIntent::apply_all(
            Namespace::system(),
            [&system_ingress],
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![leftover, extra],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!removes(&system, '3'));
    assert!(!removes(&system, 'c'));
    assert!(system.would_remove.is_empty());
}

#[test]
fn other_namespace_services_are_not_removed_by_a_user_namespace_reconcile() {
    let web = spec("web");
    let other_web = spec("web");
    let mut other = container('9', '1', &other_web, &service_id('c'));
    other
        .try_update(|parts| parts.namespace = Namespace::parse("other").unwrap())
        .unwrap();
    let plan = preview_deploy(
        &DeployIntent::apply_all(
            Namespace::parse("app").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![other],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(!removes(&plan, '9'));
    assert!(plan.would_remove.is_empty());
}

#[test]
fn prune_removes_hook_containers_of_an_obsolete_service() {
    let web = spec("web");
    let debug = spec("debug");
    let mut hook = container('8', '1', &debug, &service_id('b'));
    hook.try_update(|parts| parts.kind = ContainerKind::PreDeployHook)
        .unwrap();
    let plan = preview_deploy(
        &DeployIntent::apply_all(
            Namespace::parse("app").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container('d', '1', &debug, &service_id('b')), hook],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(removes(&plan, 'd'));
    assert!(removes(&plan, '8'));
}

#[test]
fn a_service_without_a_pre_deploy_command_removes_its_finished_hook_and_leaves_a_running_one() {
    let web = spec("web");
    let hook = |hex| {
        let mut hook = container(hex, '1', &web, &service_id('b'));
        hook.try_update(|parts| parts.kind = ContainerKind::PreDeployHook)
            .unwrap();
        hook
    };
    let mut finished = hook('8');
    finished
        .try_update(|parts| parts.runtime = ContainerRuntimeObservation::Exited { code: 3 })
        .unwrap();
    let plan = preview_deploy(
        &DeployIntent::apply_all(
            Namespace::parse("app").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![
                container('d', '1', &web, &service_id('b')),
                finished,
                hook('9'),
            ],
            ..Default::default()
        },
    )
    .unwrap();
    assert!(removes(&plan, '8'), "the finished hook is removed");
    assert!(
        !removes(&plan, '9'),
        "a running hook is left for a later deploy"
    );
    assert!(!removes(&plan, 'd'));
}

#[test]
fn failed_desired_change_leaves_prune_in_the_unexecuted_suffix() {
    let web = spec("web");
    let debug = spec("debug");
    let plan = preview_deploy(
        &DeployIntent::apply_all(
            Namespace::parse("app").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container('d', '1', &debug, &service_id('b'))],
            ..Default::default()
        },
    )
    .unwrap();
    let ops = operations(&plan);
    assert!(
        matches!(
            ops.first(),
            Some(DeployOperation::RunContainer { spec, .. }) if spec.name.as_str() == "web"
        ),
        "desired work comes first: {ops:?}"
    );
    assert!(
        ops.iter().skip(1).any(|operation| {
            matches!(
                operation,
                DeployOperation::RemoveContainer {
                    container_id: removed,
                    purpose: ployz_core::RemoveContainerPurpose::Retire,
                    ..
                } if *removed == container_id('d')
            )
        }),
        "prune is after desired work: {ops:?}"
    );
}

#[test]
fn a_removal_only_deploy_removes_exactly_the_named_service() {
    let (web, snapshot) = shop_with_obsolete_debug();
    let plan = preview_deploy(&retiring(&[web], &["debug"], &["debug"]), &snapshot).unwrap();
    assert!(removes(&plan, 'd'));
    assert!(!removes(&plan, 'c'));
    assert_eq!(operations(&plan).len(), 1);
    assert_eq!(plan.prune_refusal, Some(PruneRefusal::SelectedServices));
}

#[test]
fn a_mixed_deploy_updates_one_service_retires_another_and_leaves_drift() {
    let mut web = spec("web");
    let debug = spec("debug");
    let stray = spec("stray");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![
            container('c', '1', &web, &service_id('a')),
            container('d', '1', &debug, &service_id('b')),
            container('e', '1', &stray, &service_id('c')),
        ],
        ..Default::default()
    };
    web.container.image = "busybox".into();
    let plan = preview_deploy(&retiring(&[web], &["web", "debug"], &["debug"]), &snapshot).unwrap();
    assert!(removes(&plan, 'd'));
    assert!(
        !removes(&plan, 'e'),
        "unrelated drift stays under a targeted Deploy"
    );
    assert!(
        plan.operations.iter().any(|row| matches!(
            &row.operation,
            DeployOperation::ReplaceContainer(replacement)
                if replacement.old_container_id == container_id('c')
        )),
        "web is updated: {:?}",
        plan.operations
    );
    assert_eq!(
        plan.would_remove,
        [
            QualifiedService::parse("app/debug").unwrap(),
            QualifiedService::parse("app/stray").unwrap(),
        ]
    );
}

#[test]
fn an_incomplete_snapshot_retires_nothing() {
    let (web, snapshot) = shop_with_obsolete_debug();
    let snapshot = DeploySnapshot {
        volume_snapshot: VolumeSnapshot::try_from_parts(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![machine_id('1')],
        )
        .expect("valid Volume Snapshot fixture"),
        ..snapshot
    };
    let plan = preview_deploy(&retiring(&[web], &["debug"], &["debug"]), &snapshot).unwrap();
    assert!(operations(&plan).is_empty());
    assert_eq!(plan.prune_refusal, Some(PruneRefusal::IncompleteSnapshot));
}

#[test]
fn a_removal_on_an_incomplete_snapshot_is_blocked_naming_the_server_not_checked() {
    let (web, mut snapshot) = shop_with_obsolete_debug();
    snapshot.machines.push(machine('2', "second"));
    snapshot.container_failures.push(MachineFailure {
        machine_id: machine_id('2'),
        error: RpcError {
            code: RpcErrorCode::Unavailable,
            message: "no answer".into(),
            details: Default::default(),
            cause: Vec::new(),
        },
    });
    let plan = preview_deploy(&retiring(&[web], &["debug"], &["debug"]), &snapshot).unwrap();
    assert!(!removes(&plan, 'd'));
    let [blocked] = plan.blocked.as_slice() else {
        panic!("{:?}", plan.blocked);
    };
    assert_eq!(blocked.service, ServiceName::parse("debug").unwrap());
    assert!(
        blocked.error.message.contains("second"),
        "{}",
        blocked.error.message
    );
}

#[test]
fn retiring_an_already_absent_service_plans_nothing() {
    let web = spec("web");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![container('c', '1', &web, &service_id('a'))],
        ..Default::default()
    };
    let plan = preview_deploy(&retiring(&[web], &["debug"], &["debug"]), &snapshot).unwrap();
    assert!(operations(&plan).is_empty());
}

#[test]
fn retiring_a_service_the_deploy_also_runs_is_refused() {
    let (web, snapshot) = shop_with_obsolete_debug();
    let error = preview_deploy(&retiring(&[web], &["web"], &["web"]), &snapshot).unwrap_err();
    assert!(
        matches!(&error, PlanError::RemovalInTarget { service } if service.as_str() == "web"),
        "{error:?}"
    );
}

#[test]
fn retiring_a_service_the_deploy_does_not_select_is_refused() {
    let (web, snapshot) = shop_with_obsolete_debug();
    let error = preview_deploy(&retiring(&[web], &["web"], &["debug"]), &snapshot).unwrap_err();
    assert!(
        matches!(&error, PlanError::RemovalNotSelected { service } if service.as_str() == "debug"),
        "{error:?}"
    );
}

#[test]
fn retiring_in_the_reserved_namespace_is_refused() {
    let ingress = spec("ingress");
    let intent = DeployIntent::new(
        Namespace::system(),
        vec![ingress],
        PlanOptions {
            selected: vec![ServiceAttempt {
                name: ServiceName::parse("metrics").unwrap(),
            }],
            removals: ServiceRemovals::from_names([ServiceName::parse("metrics").unwrap()]),
            ..PlanOptions::default()
        },
    );
    let error = preview_deploy(&intent, &DeploySnapshot::default()).unwrap_err();
    assert!(
        matches!(error, PlanError::RemovalInReservedNamespace { .. }),
        "{error:?}"
    );
}

/// A targeted Deploy of `target`, applying `selected`, that retires `removals`.
#[test]
fn prune_limited_to_removals_retires_them_and_keeps_the_rest_with_a_warning() {
    let web = spec("web");
    let api = spec("api");
    let debug = spec("debug");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![
            container('e', '1', &web, &service_id('a')),
            container('a', '1', &api, &service_id('c')),
            container('d', '1', &debug, &service_id('b')),
        ],
        ..Default::default()
    };
    let intent = DeployIntent::new(
        Namespace::parse("app").unwrap(),
        vec![web],
        PlanOptions {
            removals: ServiceRemovals::from_names([ServiceName::parse("api").unwrap()]),
            prune: PruneScope::Removals,
            ..PlanOptions::default()
        },
    );
    let plan = preview_deploy(&intent, &snapshot).unwrap();
    assert_eq!(plan.prune_refusal, Some(PruneRefusal::RemovalsOnly));
    assert_eq!(
        plan.would_remove.iter().collect::<BTreeSet<_>>(),
        [
            &QualifiedService::parse("app/api").unwrap(),
            &QualifiedService::parse("app/debug").unwrap()
        ]
        .into_iter()
        .collect()
    );
    assert!(removes(&plan, 'a'), "the removal is retired");
    assert!(
        !removes(&plan, 'd'),
        "a Service neither the target nor the removals name is kept"
    );
    assert!(!removes(&plan, 'e'));
    let kept = DeployWarning::NotInStore {
        service: QualifiedService::parse("app/debug").unwrap(),
    };
    assert_eq!(
        plan.warnings
            .iter()
            .filter(|warning| matches!(warning, DeployWarning::NotInStore { .. }))
            .collect::<Vec<_>>(),
        [&kept]
    );
    assert_eq!(
        kept.to_string(),
        "app/debug: running, not in this environment; kept (--no-prune)."
    );
}

#[test]
fn prune_of_the_namespace_removes_a_service_the_removals_do_not_name() {
    let web = spec("web");
    let api = spec("api");
    let debug = spec("debug");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![
            container('e', '1', &web, &service_id('a')),
            container('a', '1', &api, &service_id('c')),
            container('d', '1', &debug, &service_id('b')),
        ],
        ..Default::default()
    };
    let intent = DeployIntent::new(
        Namespace::parse("app").unwrap(),
        vec![web],
        PlanOptions {
            removals: ServiceRemovals::from_names([ServiceName::parse("api").unwrap()]),
            prune: PruneScope::Namespace,
            ..PlanOptions::default()
        },
    );
    let plan = preview_deploy(&intent, &snapshot).unwrap();
    assert_eq!(plan.prune_refusal, None);
    assert!(removes(&plan, 'a'));
    assert!(removes(&plan, 'd'));
    assert!(!removes(&plan, 'e'));
    assert!(
        !plan
            .warnings
            .iter()
            .any(|warning| matches!(warning, DeployWarning::NotInStore { .. })),
        "{:?}",
        plan.warnings
    );
}

fn retiring(target: &[RequestedServiceSpec], selected: &[&str], removals: &[&str]) -> DeployIntent {
    let name = |name: &&str| ServiceName::parse(*name).unwrap();
    DeployIntent::new(
        Namespace::parse("app").unwrap(),
        target.to_vec(),
        PlanOptions {
            selected: selected
                .iter()
                .map(|service| ServiceAttempt {
                    name: name(service),
                })
                .collect(),
            removals: ServiceRemovals::from_names(removals.iter().map(name)),
            ..PlanOptions::default()
        },
    )
}

fn shop_with_obsolete_debug() -> (RequestedServiceSpec, DeploySnapshot) {
    let web = spec("web");
    let debug = spec("debug");
    (
        web.clone(),
        DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![
                container('c', '1', &web, &service_id('a')),
                container('d', '1', &debug, &service_id('b')),
            ],
            ..Default::default()
        },
    )
}

fn removes(plan: &ployz::deploy::DeployPreview, hex: char) -> bool {
    let id = container_id(hex);
    plan.operations.iter().any(|row| {
        matches!(
            row.operation,
            DeployOperation::RemoveContainer { container_id, .. } if container_id == id
        )
    })
}

#[test]
fn a_removal_retires_a_stray_pending_copy_before_any_routed_one() {
    let debug = spec("debug");
    let service = service_id('b');
    let rotated = |hex, machine_hex, rotation| {
        let mut copy = container(hex, machine_hex, &debug, &service);
        copy.try_update(|parts| parts.rotation = Some(rotation))
            .unwrap();
        copy
    };
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), machine('2', "second")],
        containers: vec![
            rotated('a', '1', ployz_core::CopyRotation::In),
            rotated('c', '2', ployz_core::CopyRotation::Pending),
        ],
        ..Default::default()
    };
    let intent =
        DeployIntent::apply_all(Namespace::parse("app").unwrap(), [], PlanOptions::default());

    let plan = preview_deploy(&intent, &snapshot).unwrap();

    // While a Pending sibling remains, proxies keep the last routed copy, so
    // its Out could never be confirmed unlisted.
    let retired: Vec<ContainerId> = operations(&plan)
        .iter()
        .map(|operation| {
            let DeployOperation::RemoveContainer { container_id, .. } = operation else {
                panic!("only removals are planned: {operation:?}");
            };
            *container_id
        })
        .collect();
    assert_eq!(retired, [container_id('c'), container_id('a')]);
}

#[test]
fn a_removal_retires_every_copy_of_a_service_in_one_step() {
    let debug = spec("debug");
    let service = service_id('b');
    let routed = |hex, machine_hex, runtime| {
        let mut copy = container(hex, machine_hex, &debug, &service);
        copy.try_update(|parts| {
            parts.rotation = Some(ployz_core::CopyRotation::In);
            parts.runtime = runtime;
        })
        .unwrap();
        copy
    };
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), machine('2', "second")],
        containers: vec![
            routed('a', '1', ContainerRuntimeObservation::Restarting),
            routed('c', '2', ContainerRuntimeObservation::Exited { code: 1 }),
        ],
        ..Default::default()
    };
    let intent =
        DeployIntent::apply_all(Namespace::parse("app").unwrap(), [], PlanOptions::default());

    let plan = plan_deploy(&intent, &snapshot).unwrap();

    let Some(tail) = plan.groups().last() else {
        panic!("expected a removal group");
    };
    assert_eq!(tail.steps, [Step::Parallel(vec![0, 1])]);
}

#[test]
fn a_removal_removes_pending_copies_one_at_a_time_before_the_rest_go_out_together() {
    let debug = spec("debug");
    let service = service_id('b');
    let copy = |hex, machine_hex, rotation| {
        let mut copy = container(hex, machine_hex, &debug, &service);
        copy.try_update(|parts| parts.rotation = Some(rotation))
            .unwrap();
        copy
    };
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), machine('2', "second")],
        containers: vec![
            copy('a', '1', ployz_core::CopyRotation::In),
            copy('b', '2', ployz_core::CopyRotation::Pending),
            copy('c', '2', ployz_core::CopyRotation::In),
            copy('d', '1', ployz_core::CopyRotation::Pending),
            copy('e', '1', ployz_core::CopyRotation::In),
        ],
        ..Default::default()
    };
    let intent =
        DeployIntent::apply_all(Namespace::parse("app").unwrap(), [], PlanOptions::default());

    let plan = plan_deploy(&intent, &snapshot).unwrap();

    let Some(tail) = plan.groups().last() else {
        panic!("expected a removal group");
    };
    assert_eq!(
        tail.steps,
        [
            Step::Serial(0),
            Step::Serial(1),
            Step::Parallel(vec![2, 3, 4])
        ]
    );
}
