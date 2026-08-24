use super::support::*;
use ployz_core::{
    DockerVolume, DockerVolumeStorageObservation, MachineStorageObservation, PreservedVolume,
    ProvisionedVolume, ProvisionedVolumeMaximumBytes, ServiceAttempt, ServiceName,
};
use std::{collections::BTreeMap, num::NonZeroU64};

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

fn automatic_provisioned_intent() -> DeployIntent {
    let mut service = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut service, "data");
    let mut intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&service],
        PlanOptions::default(),
    );
    intent.provisioned_volumes = vec![provisioned("api", "data", 1_073_741_824)];
    intent
}

fn explicitly_targeted_provisioned_deploy(
    storage: Option<MachineStorageObservation>,
    volumes: Vec<DockerVolume>,
) -> Result<DeployPreview, PlanError> {
    let mut service = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    service.placement.machines = vec![MachineTarget::parse("first").unwrap()];
    add_named_volume(&mut service, "data");
    add_named_volume(&mut service, "cache");
    service.pre_deploy = Some(PreDeployHook {
        command: vec!["prepare".into()],
        environment: Default::default(),
        privileged: None,
        timeout_millis: None,
        user: None,
    });
    let mut intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&service],
        PlanOptions::default(),
    );
    intent.provisioned_volumes = vec![provisioned("api", "data", 1_073_741_824)];
    let mut target = machine('1', "first");
    target.storage = storage;
    preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![target],
            volumes,
            ..Default::default()
        },
        IngressContext::default(),
    )
}

#[test]
fn explicitly_targeted_provisioned_volume_precedes_hook_service_and_plain_volume() {
    let preview =
        explicitly_targeted_provisioned_deploy(Some(MachineStorageObservation::Ready), Vec::new())
            .unwrap();

    assert!(matches!(
        operations(&preview).as_slice(),
        [
            DeployOperation::CreateVolume { volume: cache, .. },
            DeployOperation::CreateProvisionedVolume {
                volume: data,
                ..
            },
            DeployOperation::RunHook { .. },
            DeployOperation::RunContainer { .. },
        ] if data.reference.as_str() == "data" && cache.reference.as_str() == "cache"
    ));
}

#[test]
fn stateless_explicit_target_requires_storage_preparation() {
    let error = explicitly_targeted_provisioned_deploy(
        Some(MachineStorageObservation::Stateless),
        Vec::new(),
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("first"), "{error}");
    assert!(error.contains("storage preparation"), "{error}");
    assert!(error.contains("--storage zfs"), "{error}");
}

#[test]
fn missing_storage_evidence_requires_storage_preparation() {
    let error = explicitly_targeted_provisioned_deploy(None, Vec::new())
        .unwrap_err()
        .to_string();

    assert!(error.contains("first"), "{error}");
    assert!(error.contains("storage preparation"), "{error}");
    assert!(error.contains("--storage zfs"), "{error}");
}

#[test]
fn existing_plain_volume_is_not_adopted_as_provisioned() {
    let error = explicitly_targeted_provisioned_deploy(
        Some(MachineStorageObservation::Pool {
            size_bytes: NonZeroU64::new(10 * 1024_u64.pow(3)).unwrap(),
            used_bytes: 0,
            free_bytes: 10 * 1024_u64.pow(3),
        }),
        vec![observed_volume(machine_id('1'), "data")],
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("app_data"), "{error}");
    assert!(error.contains("Plain Docker Volume"), "{error}");
    assert!(
        error.contains("outside the Provisioned Volume MVP"),
        "{error}"
    );
}

#[test]
fn existing_matching_provisioned_volume_is_reused_without_creation() {
    let mut existing = observed_volume(machine_id('1'), "data");
    existing.options = BTreeMap::from([("size".into(), "2g".into())]);
    existing.storage = DockerVolumeStorageObservation::Provisioned {
        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/app_data").unwrap(),
        bound_bytes: NonZeroU64::new(1_073_741_824).unwrap(),
        used_bytes: 0,
    };

    let preview = explicitly_targeted_provisioned_deploy(
        Some(MachineStorageObservation::Pool {
            size_bytes: NonZeroU64::new(10 * 1024_u64.pow(3)).unwrap(),
            used_bytes: 0,
            free_bytes: 10 * 1024_u64.pow(3),
        }),
        vec![existing],
    )
    .unwrap();

    assert!(
        operations(&preview)
            .iter()
            .all(|operation| !matches!(operation, DeployOperation::CreateProvisionedVolume { .. }))
    );
}

#[test]
fn existing_provisioned_volume_is_not_implicitly_resized() {
    let mut existing = observed_volume(machine_id('1'), "data");
    existing.options = BTreeMap::from([("size".into(), "1g".into())]);
    existing.storage = DockerVolumeStorageObservation::Provisioned {
        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/app_data").unwrap(),
        bound_bytes: NonZeroU64::new(2_147_483_648).unwrap(),
        used_bytes: 0,
    };

    let error = explicitly_targeted_provisioned_deploy(
        Some(MachineStorageObservation::Pool {
            size_bytes: NonZeroU64::new(10 * 1024_u64.pow(3)).unwrap(),
            used_bytes: 0,
            free_bytes: 10 * 1024_u64.pow(3),
        }),
        vec![existing],
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("will not be resized or replaced"), "{error}");
}

#[test]
fn automatic_provisioned_volume_uses_a_storage_ready_machine() {
    let intent = automatic_provisioned_intent();
    let mut stateless = machine('1', "stateless");
    stateless.storage = Some(MachineStorageObservation::Stateless);
    let mut ready = machine('2', "ready");
    ready.storage = Some(MachineStorageObservation::Ready);

    let preview = preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![stateless, ready],
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap();

    assert!(
        operations(&preview)
            .iter()
            .all(|operation| operation.machine_id() == machine_id('2'))
    );
    assert!(
        operations(&preview)
            .iter()
            .any(|operation| matches!(operation, DeployOperation::CreateProvisionedVolume { .. }))
    );
}

#[test]
fn automatic_provisioned_volume_reports_observer_relative_storage_guidance() {
    let intent = automatic_provisioned_intent();
    let mut stateless = machine('1', "stateless");
    stateless.storage = Some(MachineStorageObservation::Stateless);

    let error = preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![stateless, machine('2', "unobserved")],
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap_err();

    assert_eq!(error, PlanError::ProvisionedVolumeStorageUnavailable);
    let error = error.to_string();
    assert!(error.contains("no observed eligible Machine"), "{error}");
    assert!(error.contains("--storage zfs"), "{error}");
}

#[test]
fn automatic_provisioned_volume_does_not_move_an_existing_plain_volume() {
    let intent = automatic_provisioned_intent();
    let mut pinned = machine('1', "pinned");
    pinned.storage = Some(MachineStorageObservation::Ready);
    let mut other = machine('2', "other");
    other.storage = Some(MachineStorageObservation::Ready);

    let error = preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![pinned, other],
            volumes: vec![observed_volume(machine_id('1'), "data")],
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("app_data"), "{error}");
    assert!(error.contains("pinned"), "{error}");
    assert!(
        error.contains("outside the Provisioned Volume MVP"),
        "{error}"
    );
}

#[test]
fn automatic_provisioned_volume_keeps_its_existing_machine_pin() {
    let intent = automatic_provisioned_intent();
    let mut pinned = machine('1', "pinned");
    pinned.storage = Some(MachineStorageObservation::Stateless);
    let mut other = machine('2', "other");
    other.storage = Some(MachineStorageObservation::Ready);
    let mut existing = observed_volume(machine_id('1'), "data");
    existing.options = BTreeMap::from([("size".into(), "1g".into())]);
    existing.storage = DockerVolumeStorageObservation::Provisioned {
        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/app_data").unwrap(),
        bound_bytes: NonZeroU64::new(1_073_741_824).unwrap(),
        used_bytes: 0,
    };

    assert_eq!(
        preview_deploy(
            &intent,
            &DeploySnapshot {
                machines: vec![pinned, other],
                volumes: vec![existing],
                ..Default::default()
            },
            IngressContext::default(),
        ),
        Err(PlanError::ProvisionedVolumeStorageUnavailable)
    );
}

#[test]
fn unselected_provisioned_service_leaves_stateless_machine_unchanged() {
    let applied = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let mut unchanged = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    unchanged.name = ServiceName::parse("storage").unwrap();
    unchanged.placement.machines = vec![MachineTarget::parse("stateless").unwrap()];
    add_named_volume(&mut unchanged, "data");
    let mut intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&applied, &unchanged],
        PlanOptions {
            selected: vec![ServiceAttempt {
                name: applied.name.clone(),
            }],
            ..PlanOptions::default()
        },
    );
    intent.provisioned_volumes = vec![provisioned("storage", "data", 1_073_741_824)];
    let mut stateless = machine('1', "stateless");
    stateless.storage = Some(MachineStorageObservation::Stateless);

    let preview = preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![stateless],
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap();

    assert!(operations(&preview).iter().all(|operation| {
        operation.service_name() != Some(&unchanged.name)
            && !matches!(operation, DeployOperation::CreateProvisionedVolume { .. })
    }));
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

    let mut first_machine = machine('1', "first");
    first_machine.storage = Some(MachineStorageObservation::Ready);
    let mut second_machine = machine('2', "second");
    second_machine.storage = Some(MachineStorageObservation::Ready);
    preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![first_machine, second_machine],
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
fn profile_filtered_service_still_contributes_to_bound_conflicts() {
    let first = global_service("first", "first");
    let second = global_service("second", "first");
    let mut intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&first, &second],
        PlanOptions::default(),
    )
    .with_service_profiles(BTreeMap::from([(
        second.name.clone(),
        vec!["tools".into()],
    )]));
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
fn preview_distinguishes_provisioned_and_ordinary_volume_creates() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "data");
    add_named_volume(&mut requested, "cache");
    let mut intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&requested],
        PlanOptions::default(),
    );
    intent.provisioned_volumes = vec![provisioned("api", "data", 1_073_741_824)];

    let mut ready = machine('1', "first");
    ready.storage = Some(MachineStorageObservation::Ready);
    let preview = preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![ready],
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap();

    let operations = operations(&preview);
    assert!(operations.iter().any(|operation| matches!(
        operation,
        DeployOperation::CreateProvisionedVolume {
            volume,
            maximum_bytes: requested_maximum_bytes,
            ..
        } if volume.reference.as_str() == "data"
            && *requested_maximum_bytes == maximum_bytes(1_073_741_824)
    )));
    assert!(operations.iter().any(|operation| matches!(
        operation,
        DeployOperation::CreateVolume { volume, .. } if volume.reference.as_str() == "cache"
    )));
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
