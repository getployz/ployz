//! Deploy planning tests for managed and external Docker Volume identity.

use super::support::*;

#[test]
fn external_volume_keeps_its_identity_without_a_create_preview() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "shared");
    let mut volumes = requested.volume_graph.volumes().to_vec();
    let mounts = requested.volume_graph.mounts().to_vec();
    let volume = volumes.first_mut().expect("named volume was added");
    volume.source = ployz_core::RawVolumeSource::External {
        name: DockerVolumeName::parse("shared").unwrap(),
    }
    .admit()
    .expect("valid volume declaration");
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
        operation_volume.source.kind(),
        ployz_core::RawVolumeSource::External { name } if name.as_str() == "shared"
    ));
    assert!(plan.volumes_to_create.is_empty());
}

#[test]
fn scale_import_preserves_foreign_observed_volume_identity() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut requested, "data");
    requested.volume_graph = requested
        .volume_graph
        .scope_to_project(&ProjectName::parse("blog").unwrap())
        .unwrap();
    let resolved = requested
        .to_resolved(
            ServiceId::random(),
            ployz_core::ResolvedUpdateConfig::default(),
        )
        .unwrap();
    let observed: ployz_core::ResolvedServiceSpec =
        serde_json::from_value(serde_json::to_value(&resolved).unwrap()).unwrap();
    let requested = observed.to_requested();
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();
    let plan: ployz_core::DeployPreview =
        serde_json::from_value(serde_json::to_value(&plan).unwrap()).unwrap();
    let previewed = plan
        .volumes_to_create
        .first()
        .expect("missing managed Volume is previewed");
    let plan_operations = operations(&plan);
    let volume = plan_operations
        .iter()
        .filter_map(DeployOperation::spec)
        .flat_map(|spec| spec.volume_graph.volumes())
        .find(|volume| {
            matches!(
                volume.source.kind(),
                ployz_core::RawVolumeSource::Ordinary { .. }
            )
        })
        .expect("run operation carries the managed Volume");
    assert!(matches!(
        operations(&plan).as_slice(),
        [DeployOperation::RunContainer { .. }]
            if matches!(
                volume.source.kind(),
                ployz_core::RawVolumeSource::Ordinary { name, .. }
                    if name.as_str() == "blog_data"
                        && volume.source.creation_labels().get(PROJECT_NAME_LABEL).map(String::as_str) == Some("blog")
            )
    ));
    assert_eq!(previewed.name.as_str(), "blog_data");
}
