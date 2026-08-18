use super::support::*;
use ployz::deploy::{VolumeFate, data_loss_from_plan, plan_project_removal};
use ployz_core::{DataLoss, PruneRefusal, QualifiedService};

fn project() -> ProjectName {
    ProjectName::parse("app").unwrap()
}

#[test]
fn project_removal_deletes_visible_services_and_preserves_volumes() {
    let spec = requested(ServiceMode::Global);
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![container('b', '1', &spec, &service_id('a'))],
        volumes: vec![
            owned_volume(machine_id('1'), "data"),
            observed_volume(machine_id('1'), "orphan"),
        ],
        ..Default::default()
    };
    let plan = plan_project_removal(&project(), &snapshot, VolumeFate::Preserve).unwrap();
    assert_eq!(plan.prune_refusal, None);
    assert_eq!(
        plan.would_remove,
        [QualifiedService::parse("app/api").unwrap()]
    );
    assert!(
        plan.operations
            .iter()
            .any(|operation| matches!(operation, DeployOperation::RemoveContainer { .. }))
    );
    assert!(
        plan.operations
            .iter()
            .all(|operation| !matches!(operation, DeployOperation::RemoveVolume { .. }))
    );
    assert_eq!(plan.preserved_volumes.len(), 1);
    assert_eq!(
        plan.preserved_volumes
            .first()
            .expect("owned volume is preserved")
            .id
            .name
            .as_str(),
        "app_data"
    );
}

#[test]
fn incomplete_snapshot_refuses_removal_and_does_not_prune() {
    let spec = requested(ServiceMode::Global);
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![container('b', '1', &spec, &service_id('a'))],
        volumes: vec![owned_volume(machine_id('1'), "data")],
        volume_omissions: vec![machine_id('1')],
        ..Default::default()
    };
    let plan = plan_project_removal(&project(), &snapshot, VolumeFate::Destroy).unwrap();
    assert_eq!(plan.prune_refusal, Some(PruneRefusal::IncompleteSnapshot));
    assert!(plan.operations.is_empty());
    assert_eq!(
        plan.would_remove,
        [QualifiedService::parse("app/api").unwrap()]
    );
    assert_eq!(plan.preserved_volumes.len(), 1);
}

#[test]
fn incomplete_empty_view_still_refuses() {
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        volume_omissions: vec![machine_id('1')],
        ..Default::default()
    };
    let plan = plan_project_removal(&project(), &snapshot, VolumeFate::Preserve).unwrap();
    assert_eq!(plan.prune_refusal, Some(PruneRefusal::IncompleteSnapshot));
    assert!(plan.operations.is_empty());
    assert!(plan.would_remove.is_empty());
    assert!(plan.preserved_volumes.is_empty());
}

#[test]
fn destroying_volumes_emits_remove_volume_only_when_complete() {
    let volume = owned_volume(machine_id('1'), "data");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        volumes: vec![volume.clone()],
        ..Default::default()
    };
    let plan = plan_project_removal(&project(), &snapshot, VolumeFate::Destroy).unwrap();
    assert_eq!(plan.prune_refusal, None);
    assert_eq!(
        plan.operations,
        [DeployOperation::RemoveVolume { id: volume.id }]
    );
    assert!(plan.preserved_volumes.is_empty());
}

#[test]
fn unlabeled_volumes_are_never_assigned_or_removed() {
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        volumes: vec![observed_volume(machine_id('1'), "orphan")],
        ..Default::default()
    };
    for fate in [VolumeFate::Preserve, VolumeFate::Destroy] {
        let plan = plan_project_removal(&project(), &snapshot, fate).unwrap();
        assert!(
            plan.operations.is_empty(),
            "{fate:?}: {:?}",
            plan.operations
        );
        assert!(
            plan.preserved_volumes.is_empty(),
            "{fate:?}: {:?}",
            plan.preserved_volumes
        );
    }
}

#[test]
fn volume_only_project_still_plans_preservation() {
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        volumes: vec![owned_volume(machine_id('1'), "data")],
        ..Default::default()
    };
    let plan = plan_project_removal(&project(), &snapshot, VolumeFate::Preserve).unwrap();
    assert!(plan.operations.is_empty());
    assert_eq!(plan.preserved_volumes.len(), 1);
}

#[test]
fn other_project_resources_are_left_alone() {
    let spec = requested(ServiceMode::Global);
    let mut other = container('c', '1', &spec, &service_id('c'));
    other.project_name = ProjectName::parse("other").unwrap();
    other.service_name = ServiceName::parse("web").unwrap();
    let mut other_volume = owned_volume(machine_id('1'), "data");
    other_volume
        .labels
        .insert(PROJECT_NAME_LABEL.to_owned(), "other".to_owned());
    other_volume.id.name = ProjectName::parse("other")
        .unwrap()
        .volume_name(&DockerVolumeName::parse("data").unwrap());
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![container('b', '1', &spec, &service_id('a')), other],
        volumes: vec![owned_volume(machine_id('1'), "keep"), other_volume],
        ..Default::default()
    };
    let plan = plan_project_removal(&project(), &snapshot, VolumeFate::Destroy).unwrap();
    assert_eq!(
        plan.would_remove,
        [QualifiedService::parse("app/api").unwrap()]
    );
    assert!(plan.operations.iter().all(|operation| match operation {
        DeployOperation::RemoveContainer { container_id, .. } => {
            *container_id == super::support::container_id('b')
        }
        DeployOperation::RemoveVolume { id } => id.name.as_str() == "app_keep",
        other @ (DeployOperation::CreateVolume { .. }
        | DeployOperation::RunContainer { .. }
        | DeployOperation::StopContainer { .. }
        | DeployOperation::ReplaceContainer(_)
        | DeployOperation::StopHook { .. }
        | DeployOperation::RunHook { .. }) => panic!("unexpected operation: {other:?}"),
    }));
}

#[test]
fn planner_does_not_refuse_reserved_names() {
    let spec = requested(ServiceMode::Global);
    let mut caddy = container('b', '1', &spec, &service_id('a'));
    caddy.project_name = ProjectName::system();
    caddy.service_name = ServiceName::parse("caddy").unwrap();
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![caddy],
        ..Default::default()
    };
    let plan =
        plan_project_removal(&ProjectName::system(), &snapshot, VolumeFate::Preserve).unwrap();
    assert_eq!(plan.prune_refusal, None);
    assert!(plan.would_remove.is_empty());
    assert!(plan.operations.is_empty());
}

#[test]
fn preserving_volumes_is_empty_data_loss() {
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        volumes: vec![owned_volume(machine_id('1'), "data")],
        ..Default::default()
    };
    assert_eq!(
        data_loss_from_plan(
            &plan_project_removal(&project(), &snapshot, VolumeFate::Preserve).unwrap()
        )
        .data_loss,
        Vec::<DataLoss>::new()
    );
}

#[test]
fn destroying_volumes_names_owned_docker_volumes_only() {
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        volumes: vec![
            owned_volume(machine_id('1'), "data"),
            observed_volume(machine_id('1'), "orphan"),
        ],
        ..Default::default()
    };
    assert_eq!(
        data_loss_from_plan(
            &plan_project_removal(&project(), &snapshot, VolumeFate::Destroy).unwrap()
        )
        .data_loss,
        [DataLoss::DockerVolume(
            owned_volume(machine_id('1'), "data").id
        )]
    );
}

#[test]
fn plan_deploy_never_emits_remove_volume() {
    let requested = requested(ServiceMode::Global);
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            volumes: vec![owned_volume(machine_id('1'), "data")],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();
    assert!(
        plan.operations
            .iter()
            .all(|operation| !matches!(operation, DeployOperation::RemoveVolume { .. }))
    );
    assert_eq!(plan.preserved_volumes.len(), 1);
}
