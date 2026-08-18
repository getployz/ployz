use std::collections::BTreeMap;

use super::support::*;
use ployz::deploy::plan_deploy;
use ployz_core::{
    ComposePruneRefusal, MachineFailure, PruneRefusal, QualifiedService, RpcError, RpcErrorCode,
    ServiceName,
};

#[test]
fn incomplete_snapshot_lists_obsolete_services_and_removes_nothing() {
    let (web, snapshot) = shop_with_obsolete_debug();
    let snapshot = DeploySnapshot {
        volume_omissions: vec![machine_id('1')],
        ..snapshot
    };
    let plan = plan_deploy(
        &DeployIntent::apply_all(
            ProjectName::parse("app").unwrap(),
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
    let plan = plan_deploy(
        &DeployIntent::apply_one(
            ProjectName::parse("app").unwrap(),
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
fn filtered_profiles_list_obsolete_services_and_remove_nothing() {
    let (web, snapshot) = shop_with_obsolete_debug();
    let plan = plan_deploy(
        &DeployIntent::apply_all(
            ProjectName::parse("app").unwrap(),
            [&web],
            PlanOptions::default(),
        )
        .with_compose_refusal(Some(ComposePruneRefusal::FilteredProfiles)),
        &snapshot,
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
    let plan = plan_deploy(
        &DeployIntent::apply_all(
            ProjectName::parse("app").unwrap(),
            [&web],
            PlanOptions::default(),
        )
        .with_compose_refusal(Some(ComposePruneRefusal::GuessedProjectName)),
        &snapshot,
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
fn down_machine_omissions_do_not_make_the_snapshot_incomplete() {
    let mut down = machine('2', "second");
    down.membership = MembershipObservation::Down;
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), down],
        container_omissions: vec![machine_id('2')],
        volume_omissions: vec![machine_id('2')],
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
    let plan = plan_deploy(
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
    )
    .unwrap();
    assert!(plan.would_remove.is_empty());
    assert!(plan.prune_refusal.is_none());
    assert!(!removes(&plan, 'e'));
    assert!(!plan.operations.iter().any(|operation| {
        matches!(
            operation,
            DeployOperation::RunContainer { spec, .. }
                | DeployOperation::ReplaceContainer(ReplacementOperation { spec, .. })
                if spec.name.as_str() == "worker"
        )
    }));
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

fn removes(plan: &ployz::deploy::DeployPlan, hex: char) -> bool {
    let id = container_id(hex);
    plan.operations.iter().any(|operation| {
        matches!(
            operation,
            DeployOperation::RemoveContainer { container_id, .. } if *container_id == id
        )
    })
}
