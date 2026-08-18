use super::support::*;
use ployz_core::{PreservedVolume, ServiceAttempt, ServiceName};

#[test]
fn already_owned_volume_names_are_not_prefixed_again() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut requested, "data");
    let requested = scoped_spec(&requested);
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
            .all(|row| !matches!(row.operation, DeployOperation::CreateVolume { .. })),
        "already-owned physical names must not be prefixed again: {:?}",
        plan.operations
    );
    assert!(plan.preserved_volumes.is_empty());
}

#[test]
fn sibling_target_volume_is_not_listed_as_preserved_on_a_partial_deploy() {
    let mut web = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    web.name = ServiceName::parse("web").unwrap();
    add_named_volume(&mut web, "web-data");
    let mut worker = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    worker.name = ServiceName::parse("worker").unwrap();
    add_named_volume(&mut worker, "worker-data");
    let plan = preview_deploy(
        &DeployIntent::new(
            ProjectName::parse("app").unwrap(),
            vec![web, worker],
            PlanOptions {
                selected: vec![ServiceAttempt {
                    name: ServiceName::parse("web").unwrap(),
                }],
                ..PlanOptions::default()
            },
        ),
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            volumes: vec![
                owned_volume(machine_id('1'), "web-data"),
                owned_volume(machine_id('1'), "worker-data"),
            ],
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap();
    assert!(
        plan.preserved_volumes.is_empty(),
        "{:?}",
        plan.preserved_volumes
    );
}

#[test]
fn omitted_owned_volume_is_preserved_in_plan_order() {
    let requested = requested(ServiceMode::Global);
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            volumes: vec![
                owned_volume(machine_id('1'), "keep-b"),
                owned_volume(machine_id('1'), "keep-a"),
            ],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();
    assert_eq!(
        plan.preserved_volumes,
        [
            PreservedVolume {
                id: owned_volume(machine_id('1'), "keep-a").id,
                machine_name: Some(MachineName::parse("first").unwrap()),
            },
            PreservedVolume {
                id: owned_volume(machine_id('1'), "keep-b").id,
                machine_name: Some(MachineName::parse("first").unwrap()),
            },
        ]
    );
}

#[test]
fn external_named_volume_keeps_its_declared_identity() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "shared");
    let mut volumes = requested.volume_graph.volumes().to_vec();
    let mounts = requested.volume_graph.mounts().to_vec();
    let volume = volumes.first_mut().expect("named volume was added");
    let VolumeSource::Named {
        external, labels, ..
    } = &mut volume.source
    else {
        panic!("named volume");
    };
    *external = true;
    labels.insert("keep".into(), "me".into());
    requested.volume_graph = ployz_core::ServiceVolumeGraph::parse(volumes, mounts).unwrap();
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();
    assert!(matches!(
        operations(&plan).as_slice(),
        [DeployOperation::CreateVolume { volume, .. }, DeployOperation::RunContainer { .. }]
            if matches!(
                &volume.source,
                VolumeSource::Named { name, external: true, labels, .. }
                    if name.as_str() == "shared"
                        && !labels.contains_key(MANAGED_LABEL)
                        && !labels.contains_key(PROJECT_NAME_LABEL)
                        && labels.get("keep").map(String::as_str) == Some("me")
            )
    ));
}

#[test]
fn foreign_project_volume_label_is_not_rewritten() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut requested, "data");
    let mut volumes = requested.volume_graph.volumes().to_vec();
    let mounts = requested.volume_graph.mounts().to_vec();
    let volume = volumes.first_mut().expect("named volume was added");
    let VolumeSource::Named { name, labels, .. } = &mut volume.source else {
        panic!("named volume");
    };
    *name = DockerVolumeName::parse("blog_data").unwrap();
    labels.insert(PROJECT_NAME_LABEL.into(), "blog".into());
    requested.volume_graph = ployz_core::ServiceVolumeGraph::parse(volumes, mounts).unwrap();
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();
    assert!(matches!(
        operations(&plan).as_slice(),
        [DeployOperation::CreateVolume { volume, .. }, DeployOperation::RunContainer { .. }]
            if matches!(
                &volume.source,
                VolumeSource::Named { name, labels, .. }
                    if name.as_str() == "blog_data"
                        && labels.get(PROJECT_NAME_LABEL).map(String::as_str) == Some("blog")
            )
    ));
}
