use std::collections::BTreeMap;

use super::support::*;
use ployz::deploy::{IngressContext, preview_deploy};
use ployz_core::{
    ComposePruneRefusal, ContainerKind, DependencyCondition, DockerVolumeId, MachineFailure,
    PruneRefusal, QualifiedService, RpcError, RpcErrorCode, ServiceDependency, ServiceName,
    VolumeObservationFailure,
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
            ProjectName::parse("app").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &snapshot,
        IngressContext::default(),
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
            ProjectName::parse("app").unwrap(),
            web,
            PlanOptions::default(),
        ),
        &snapshot,
        IngressContext::default(),
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
fn filtered_profiles_list_obsolete_services_and_remove_nothing() {
    let (web, snapshot) = shop_with_obsolete_debug();
    let plan = preview_deploy(
        &DeployIntent::apply_all(
            ProjectName::parse("app").unwrap(),
            [&web],
            PlanOptions::default(),
        )
        .with_compose_refusal(Some(ComposePruneRefusal::FilteredProfiles)),
        &snapshot,
        IngressContext::default(),
    )
    .unwrap();
    assert_eq!(
        plan.would_remove,
        [QualifiedService::parse("app/debug").unwrap()]
    );
    assert_eq!(plan.prune_refusal, Some(PruneRefusal::FilteredProfiles));
    assert!(!removes(&plan, 'd'));
}

#[test]
fn guessed_project_name_lists_obsolete_services_and_removes_nothing() {
    let (web, snapshot) = shop_with_obsolete_debug();
    let plan = preview_deploy(
        &DeployIntent::apply_all(
            ProjectName::parse("app").unwrap(),
            [&web],
            PlanOptions::default(),
        )
        .with_compose_refusal(Some(ComposePruneRefusal::GuessedProjectName)),
        &snapshot,
        IngressContext::default(),
    )
    .unwrap();
    assert_eq!(
        plan.would_remove,
        [QualifiedService::parse("app/debug").unwrap()]
    );
    assert_eq!(plan.prune_refusal, Some(PruneRefusal::GuessedProjectName));
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
fn full_reconciliation_keeps_profiled_services_in_the_target_without_starting_them() {
    let web = spec("web");
    let worker = spec("worker");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![
            container('c', '1', &web, &service_id('a')),
            container('e', '1', &worker, &service_id('b')),
        ],
        ..Default::default()
    };
    let plan = preview_deploy(
        &DeployIntent::apply_all(
            ProjectName::parse("app").unwrap(),
            [&web, &worker],
            PlanOptions::default(),
        )
        .with_service_profiles(BTreeMap::from([(
            ServiceName::parse("worker").unwrap(),
            vec!["tools".into()],
        )])),
        &snapshot,
        IngressContext::default(),
    )
    .unwrap();
    assert!(plan.would_remove.is_empty());
    assert!(plan.prune_refusal.is_none());
    assert!(!removes(&plan, 'e'));
    assert!(!plan.operations.iter().any(|row| {
        matches!(
            &row.operation,
            DeployOperation::RunContainer { spec, .. }
                | DeployOperation::ReplaceContainer(ReplacementOperation { spec, .. })
                if spec.name.as_str() == "worker"
        )
    }));
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
        ProjectName::parse("app").unwrap(),
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
    let plan = preview_deploy(&intent, &snapshot, IngressContext::default()).unwrap();
    assert_eq!(
        plan.would_remove,
        [QualifiedService::parse("app/debug").unwrap()]
    );
    assert_eq!(plan.prune_refusal, None);
    match operations(&plan).as_slice() {
        [
            DeployOperation::CreateVolume { .. },
            DeployOperation::RunContainer { spec: first, .. },
            DeployOperation::RunContainer { spec: second, .. },
            DeployOperation::RemoveContainer {
                container_id: removed,
                ..
            },
        ] => {
            assert_eq!(first.name.as_str(), "db");
            assert_eq!(second.name.as_str(), "web");
            assert_eq!(*removed, container_id('d'));
        }
        other => panic!("expected volume, db, web, then prune, got {other:?}"),
    }
}

#[test]
fn selecting_one_service_does_not_remove_the_rest_of_the_project() {
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
            ProjectName::parse("app").unwrap(),
            vec![web, api],
            PlanOptions {
                selected: vec![ServiceAttempt {
                    name: ServiceName::parse("web").unwrap(),
                }],
                ..PlanOptions::default()
            },
        ),
        &snapshot,
        IngressContext::default(),
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
            ProjectName::parse("app").unwrap(),
            web.clone(),
            PlanOptions::default(),
        ),
        &snapshot,
        IngressContext::default(),
    )
    .unwrap();
    assert!(!removes(&partial, 'd'));
    assert_eq!(partial.prune_refusal, Some(PruneRefusal::SelectedServices));

    let mut requested_debug = debug;
    requested_debug.container.image = "busybox".into();
    let selected_debug = preview_deploy(
        &DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
            requested_debug,
            PlanOptions::default(),
        ),
        &snapshot,
        IngressContext::default(),
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
fn reserved_project_and_system_workloads_are_excluded_before_removal_is_planned() {
    let web = spec("web");
    let mut system_caddy = spec("caddy");
    system_caddy.mode = ServiceMode::Global;
    let mut leftover = container('c', '1', &system_caddy, &service_id('a'));
    leftover.project_name = ProjectName::system();
    let shop = preview_deploy(
        &DeployIntent::apply_all(
            ProjectName::parse("shop").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![leftover.clone()],
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap();
    assert!(!removes(&shop, 'c'));
    assert!(shop.would_remove.is_empty());

    let metrics = spec("metrics");
    let mut extra = container('3', '1', &metrics, &service_id('b'));
    extra.project_name = ProjectName::system();
    leftover.project_name = ProjectName::system();
    let system = preview_deploy(
        &DeployIntent::apply_all(
            ProjectName::system(),
            [&system_caddy],
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![leftover, extra],
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap();
    assert!(!removes(&system, '3'));
    assert!(!removes(&system, 'c'));
    assert!(system.would_remove.is_empty());
}

#[test]
fn other_project_services_are_not_removed_by_a_user_project_reconcile() {
    let web = spec("web");
    let other_web = spec("web");
    let mut other = container('9', '1', &other_web, &service_id('c'));
    other.project_name = ProjectName::parse("other").unwrap();
    let plan = preview_deploy(
        &DeployIntent::apply_all(
            ProjectName::parse("app").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![other],
            ..Default::default()
        },
        IngressContext::default(),
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
    hook.kind = ContainerKind::PreDeployHook;
    let plan = preview_deploy(
        &DeployIntent::apply_all(
            ProjectName::parse("app").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container('d', '1', &debug, &service_id('b')), hook],
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap();
    assert!(removes(&plan, 'd'));
    assert!(removes(&plan, '8'));
}

#[test]
fn failed_desired_change_leaves_prune_in_the_unexecuted_suffix() {
    let web = spec("web");
    let debug = spec("debug");
    let plan = preview_deploy(
        &DeployIntent::apply_all(
            ProjectName::parse("app").unwrap(),
            [&web],
            PlanOptions::default(),
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container('d', '1', &debug, &service_id('b'))],
            ..Default::default()
        },
        IngressContext::default(),
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
                    ..
                } if *removed == container_id('d')
            )
        }),
        "prune is after desired work: {ops:?}"
    );
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

fn spec(name: &str) -> RequestedServiceSpec {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    requested.name = ServiceName::parse(name).unwrap();
    requested
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
