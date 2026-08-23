use super::support::*;
use ployz_core::{
    PreservedVolume, ProvisionedVolume, ProvisionedVolumeMaximumBytes, ServiceAttempt, ServiceName,
};
use std::num::NonZeroU64;

fn maximum_bytes(bytes: u64) -> ProvisionedVolumeMaximumBytes {
    ProvisionedVolumeMaximumBytes::new(NonZeroU64::new(bytes).unwrap())
}

fn provisioned(service: &str, reference: &str, bytes: u64) -> ProvisionedVolume {
    ProvisionedVolume {
        service: ServiceName::parse(service).unwrap(),
        reference: ServiceVolumeReference::parse(reference).unwrap(),
        maximum_bytes: maximum_bytes(bytes),
    }
}

fn global_service(name: &str, machine_name: &str) -> RequestedServiceSpec {
    let mut service = requested(ServiceMode::Global);
    service.name = ServiceName::parse(name).unwrap();
    service.placement.machines = vec![MachineTarget::parse(machine_name).unwrap()];
    add_named_volume(&mut service, "data");
    service
}

#[test]
fn provisioned_volume_aliases_cannot_conflict_on_one_docker_volume() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut requested, "data");
    let mut volumes = requested.volume_graph.volumes().to_vec();
    let mounts = requested.volume_graph.mounts().to_vec();
    volumes.push(ServiceVolume {
        reference: ServiceVolumeReference::parse("data-alias").unwrap(),
        source: volumes.first().unwrap().source.clone(),
    });
    requested.volume_graph = ployz_core::ServiceVolumeGraph::parse(volumes, mounts).unwrap();
    let mut intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&requested],
        PlanOptions::default(),
    );
    intent.provisioned_volumes = vec![
        provisioned(requested.name.as_str(), "data", 1_073_741_824),
        provisioned(requested.name.as_str(), "data-alias", 2_147_483_648),
    ];

    assert_eq!(
        preview_deploy(
            &intent,
            &DeploySnapshot {
                machines: vec![machine('1', "first")],
                ..Default::default()
            },
            IngressContext::default(),
        ),
        Err(PlanError::ConflictingProvisionedVolumeBounds {
            name: app_volume("data"),
            existing_maximum_bytes: maximum_bytes(1_073_741_824),
            conflicting_maximum_bytes: maximum_bytes(2_147_483_648),
        })
    );
}

#[test]
fn disjoint_global_volumes_may_have_different_bounds() {
    let first = global_service("first", "first");
    let second = global_service("second", "second");
    let mut intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&first, &second],
        PlanOptions::default(),
    );
    intent.provisioned_volumes = vec![
        provisioned(first.name.as_str(), "data", 1_073_741_824),
        provisioned(second.name.as_str(), "data", 2_147_483_648),
    ];

    preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![machine('1', "first"), machine('2', "second")],
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap();
}

#[test]
fn partial_apply_rejects_different_bounds_for_colocated_global_volumes() {
    let first = global_service("first", "first");
    let second = global_service("second", "first");
    let mut intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&first, &second],
        PlanOptions {
            selected: vec![ServiceAttempt {
                name: first.name.clone(),
            }],
            ..PlanOptions::default()
        },
    );
    intent.provisioned_volumes = vec![
        provisioned("first", "data", 1_073_741_824),
        provisioned("second", "data", 2_147_483_648),
    ];

    let result = preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            ..Default::default()
        },
        IngressContext::default(),
    );
    assert!(
        matches!(
            &result,
            Err(PlanError::Service { source, .. })
                if matches!(source.as_ref(), PlanError::ConflictingProvisionedVolumeBounds { .. })
        ),
        "unexpected result: {result:?}"
    );
}

#[test]
fn unknown_provisioned_volume_reference_fails_planning() {
    let mut intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [],
        PlanOptions::default(),
    );
    intent
        .provisioned_volumes
        .push(provisioned("api", "data", 1_073_741_824));

    assert_eq!(
        preview_deploy(
            &intent,
            &DeploySnapshot::default(),
            IngressContext::default()
        ),
        Err(PlanError::UnknownProvisionedVolumeReference {
            service: ServiceName::parse("api").unwrap(),
            reference: ServiceVolumeReference::parse("data").unwrap(),
        })
    );
}

#[test]
fn duplicate_target_services_fail_before_volume_resolution() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let mut intent = DeployIntent::new(
        ProjectName::parse("app").unwrap(),
        vec![requested.clone(), requested],
        PlanOptions::default(),
    );
    intent
        .provisioned_volumes
        .push(provisioned("api", "data", 1_073_741_824));

    assert_eq!(
        preview_deploy(
            &intent,
            &DeploySnapshot::default(),
            IngressContext::default(),
        ),
        Err(PlanError::DuplicateTargetService {
            service: ServiceName::parse("api").unwrap(),
        })
    );
}

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
