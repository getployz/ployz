//! Deploy planning tests for managed and external Docker Volume identity.

use super::support::*;

#[test]
fn external_volume_keeps_its_identity_without_a_create_preview() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "shared");
    let mut volumes = requested.volume_graph.volumes().to_vec();
    let mounts = requested.volume_graph.mounts().to_vec();
    let volume = volumes.first_mut().expect("named volume was added");
    volume.source = VolumeSource::External {
        name: DockerVolumeName::parse("shared").unwrap(),
    };
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
    let plan_operations = operations(&plan);
    let [DeployOperation::RunContainer { spec, .. }] = plan_operations.as_slice() else {
        panic!("expected one run operation: {plan_operations:?}");
    };
    let operation_volume = spec
        .volume_graph
        .volumes()
        .first()
        .expect("run operation mounts the external Volume");
    assert!(matches!(
        &operation_volume.source,
        VolumeSource::External { name } if name.as_str() == "shared"
    ));
    assert!(plan.volumes_to_create.is_empty());
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
    let VolumeSource::Ordinary { name, labels, .. } = &mut volume.source else {
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
    let previewed = plan
        .volumes_to_create
        .first()
        .expect("missing managed Volume is previewed");
    let plan_operations = operations(&plan);
    let volume = plan_operations
        .iter()
        .filter_map(DeployOperation::spec)
        .flat_map(|spec| spec.volume_graph.volumes())
        .find(|volume| matches!(&volume.source, VolumeSource::Ordinary { .. }))
        .expect("run operation carries the managed Volume");
    assert!(matches!(
        operations(&plan).as_slice(),
        [DeployOperation::RunContainer { .. }]
            if matches!(
                &volume.source,
                VolumeSource::Ordinary { name, labels, .. }
                    if name.as_str() == "blog_data"
                        && labels.get(PROJECT_NAME_LABEL).map(String::as_str) == Some("blog")
            )
    ));
    assert_eq!(previewed.name.as_str(), "blog_data");
}
