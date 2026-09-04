use super::support::*;
use ployz_core::{
    DockerVolume, DockerVolumeStorageObservation, MachineFailure, MachineStorageObservation,
    PreservedVolume, ProvisionedVolumeMaximumBytes, PruneRefusal, RpcError, RpcErrorCode,
    ServiceAttempt, ServiceName, VolumeObservationFailure,
};
use std::{collections::BTreeMap, num::NonZeroU64};

fn maximum_bytes(bytes: u64) -> ProvisionedVolumeMaximumBytes {
    ProvisionedVolumeMaximumBytes::new(NonZeroU64::new(bytes).unwrap())
}

fn automatic_provisioned_intent() -> DeployIntent {
    let mut service = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut service, "data");
    make_provisioned(&mut service, "data", 1_073_741_824);
    DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&service],
        PlanOptions::default(),
    )
}

#[test]
fn unavailable_named_volume_blocks_only_a_dependent_service() {
    let mut dependent = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut dependent, "data");
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
                    message: "volume detail failed".into(),
                    details: Default::default(),
                },
            }],
            Vec::new(),
            Vec::new(),
        )
        .expect("valid Volume Snapshot fixture"),
        ..Default::default()
    };

    let error = plan_deploy([&dependent], &snapshot, PlanOptions::default()).unwrap_err();
    assert!(error.to_string().contains("app_data"), "{error}");
    assert!(
        error.to_string().contains("volume detail failed"),
        "{error}"
    );

    let unrelated = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let preview = plan_deploy([&unrelated], &snapshot, PlanOptions::default()).unwrap();
    assert!(matches!(
        operations(&preview).as_slice(),
        [DeployOperation::RunContainer { .. }]
    ));
    assert_eq!(
        preview.prune_refusal,
        Some(PruneRefusal::IncompleteSnapshot)
    );
}

#[test]
fn named_volume_planning_keeps_a_machine_with_a_complete_inventory() {
    let mut service = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut service, "data");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "incomplete"), machine('2', "healthy")],
        volume_snapshot: VolumeSnapshot::try_from_parts(
            Vec::new(),
            Vec::new(),
            vec![MachineFailure {
                machine_id: machine_id('1'),
                error: RpcError {
                    code: RpcErrorCode::Unavailable,
                    message: "Docker did not answer".into(),
                    details: Default::default(),
                },
            }],
            Vec::new(),
        )
        .expect("valid Volume Snapshot fixture"),
        ..Default::default()
    };

    let plan = plan_deploy([&service], &snapshot, PlanOptions::default()).unwrap();

    assert!(matches!(
        operations(&plan).as_slice(),
        [DeployOperation::RunContainer { machine_id: run, .. }]
            if *run == machine_id('2')
    ));
    assert_eq!(
        plan.volumes_to_create
            .first()
            .expect("missing managed Volume is previewed")
            .machine_id,
        machine_id('2')
    );
}

#[test]
fn named_volume_planning_rejects_an_only_incomplete_candidate() {
    let mut dependent = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut dependent, "data");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "incomplete")],
        volume_snapshot: VolumeSnapshot::try_from_parts(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![machine_id('1')],
        )
        .expect("valid Volume Snapshot fixture"),
        ..Default::default()
    };

    let error = plan_deploy([&dependent], &snapshot, PlanOptions::default()).unwrap_err();
    assert!(error.to_string().contains("app_data"), "{error}");
    assert!(
        error.to_string().contains("no terminal response"),
        "{error}"
    );
    assert!(error.to_string().contains("incomplete"), "{error}");

    let unrelated = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    assert!(plan_deploy([&unrelated], &snapshot, PlanOptions::default()).is_ok());
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
    make_provisioned(&mut service, "data", 1_073_741_824);
    add_named_volume(&mut service, "cache");
    service.pre_deploy = Some(PreDeployHook {
        command: vec!["prepare".into()],
        environment: Default::default(),
        privileged: None,
        timeout_millis: None,
        user: None,
    });
    let intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&service],
        PlanOptions::default(),
    );
    let mut target = machine('1', "first");
    target.storage = storage;
    preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![target],
            volume_snapshot: VolumeSnapshot::try_from_observations(volumes)
                .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
        IngressContext::default(),
    )
}

#[test]
fn missing_managed_volumes_are_informational_and_container_order_stays_exact() {
    let preview =
        explicitly_targeted_provisioned_deploy(Some(MachineStorageObservation::Ready), Vec::new())
            .unwrap();

    assert!(matches!(
        operations(&preview).as_slice(),
        [
            DeployOperation::RunHook { .. },
            DeployOperation::RunContainer { .. }
        ]
    ));
    assert_eq!(preview.volumes_to_create.len(), 2);
    let cache = preview
        .volumes_to_create
        .first()
        .expect("ordinary Volume is previewed first");
    let data = preview
        .volumes_to_create
        .get(1)
        .expect("Provisioned Volume is previewed second");
    assert_eq!(cache.machine_id, machine_id('1'));
    assert_eq!(
        cache.machine_name.as_ref().map(MachineName::as_str),
        Some("first")
    );
    assert_eq!(cache.name.as_str(), "app_cache");
    assert!(cache.maximum_bytes.is_none());
    assert_eq!(data.name.as_str(), "app_data");
    assert_eq!(data.maximum_bytes, Some(maximum_bytes(1_073_741_824)));
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
fn missing_storage_evidence_reports_that_storage_could_not_be_checked() {
    assert_eq!(
        explicitly_targeted_provisioned_deploy(None, Vec::new()),
        Err(PlanError::ProvisionedVolumeStorageUnknown {
            names: vec![MachineName::parse("first").unwrap()],
        })
    );
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
fn ordinary_volume_does_not_adopt_an_existing_provisioned_volume() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    requested.placement.machines = vec![MachineTarget::parse("first").unwrap()];
    add_named_volume(&mut requested, "data");
    let mut existing = observed_volume(machine_id('1'), "data");
    existing.storage = DockerVolumeStorageObservation::Provisioned {
        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/app_data").unwrap(),
        bound_bytes: NonZeroU64::new(1_073_741_824).unwrap(),
        used_bytes: 0,
    };

    let error = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![existing]).unwrap(),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("app_data"), "{error}");
    assert!(error.contains("no machines available"), "{error}");
}

#[test]
fn omitted_driver_means_exactly_local_with_no_options() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut requested, "data");
    for mut existing in [
        {
            let mut volume = observed_volume(machine_id('1'), "data");
            volume.storage = DockerVolumeStorageObservation::Plain {
                driver: "foreign".into(),
            };
            volume
        },
        {
            let mut volume = observed_volume(machine_id('1'), "data");
            volume.options.insert("type".into(), "tmpfs".into());
            volume
        },
    ] {
        existing.id.machine_id = machine_id('1');
        let error = plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "first")],
                volume_snapshot: VolumeSnapshot::try_from_observations(vec![existing]).unwrap(),
                ..Default::default()
            },
            PlanOptions::default(),
        )
        .unwrap_err();
        assert!(matches!(error, PlanError::NoEligibleMachines { .. }));
    }
}

#[test]
fn existing_matching_provisioned_volume_is_reused_without_creation() {
    let mut existing = observed_volume(machine_id('1'), "data");
    existing.options = BTreeMap::from([("size".into(), "1073741824b".into())]);
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

    assert_eq!(preview.volumes_to_create.len(), 1);
    assert_eq!(
        preview
            .volumes_to_create
            .first()
            .expect("missing ordinary Volume is previewed")
            .name
            .as_str(),
        "app_cache"
    );
}

#[test]
fn provisioned_volume_requires_requested_labels() {
    let mut existing = observed_volume(machine_id('1'), "data");
    existing.options = BTreeMap::from([("size".into(), "1073741824b".into())]);
    existing.labels.remove(PROJECT_NAME_LABEL);
    existing.storage = DockerVolumeStorageObservation::Provisioned {
        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/app_data").unwrap(),
        bound_bytes: NonZeroU64::new(1_073_741_824).unwrap(),
        used_bytes: 0,
    };

    let error = explicitly_targeted_provisioned_deploy(
        Some(MachineStorageObservation::Ready),
        vec![existing],
    )
    .unwrap_err();

    assert!(matches!(
        error,
        PlanError::ExistingProvisionedVolumeMismatch { .. }
    ));
}

#[test]
fn existing_provisioned_volume_is_not_implicitly_resized() {
    let mut existing = observed_volume(machine_id('1'), "data");
    existing.options = BTreeMap::from([("size".into(), "1073741824b".into())]);
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
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![observed_volume(
                machine_id('1'),
                "data",
            )])
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap();

    assert!(matches!(
        operations(&preview).as_slice(),
        [DeployOperation::RunContainer { machine_id: target, .. }] if target == &machine_id('2')
    ));
    assert!(matches!(
        &preview.volumes_to_create[..],
        [item] if item.machine_id == machine_id('2')
            && item.maximum_bytes.is_some()
    ));
}

#[test]
fn automatic_provisioned_volume_uses_known_eligible_and_warns_about_unknown() {
    let intent = automatic_provisioned_intent();
    let mut ready = machine('1', "ready");
    ready.storage = Some(MachineStorageObservation::Ready);
    let unknown = machine('2', "unknown");

    let preview = preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![ready, unknown],
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![observed_volume(
                machine_id('2'),
                "data",
            )])
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
        IngressContext::default(),
    )
    .unwrap();

    assert!(matches!(
        operations(&preview).as_slice(),
        [DeployOperation::RunContainer { machine_id: target, .. }] if target == &machine_id('1')
    ));
    assert_eq!(
        preview.warnings,
        [ployz_core::DeployWarning::StorageObservationUnknown {
            machine_id: machine_id('2'),
        }]
    );
}

#[test]
fn automatic_provisioned_volume_reports_unknown_storage_guidance() {
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

    assert_eq!(
        error,
        PlanError::ProvisionedVolumeStorageUnknown {
            names: vec![MachineName::parse("unobserved").unwrap()],
        }
    );
    let error = error.to_string();
    assert!(error.contains("storage could not be checked"), "{error}");
    assert!(error.contains("unobserved"), "{error}");
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
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![observed_volume(
                machine_id('1'),
                "data",
            )])
            .expect("valid Volume Snapshot fixture"),
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
    existing.options = BTreeMap::from([("size".into(), "1073741824b".into())]);
    existing.storage = DockerVolumeStorageObservation::Provisioned {
        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/app_data").unwrap(),
        bound_bytes: NonZeroU64::new(1_073_741_824).unwrap(),
        used_bytes: 0,
    };

    assert_no_eligible(
        preview_deploy(
            &intent,
            &DeploySnapshot {
                machines: vec![pinned, other],
                volume_snapshot: VolumeSnapshot::try_from_observations(vec![existing])
                    .expect("valid Volume Snapshot fixture"),
                ..Default::default()
            },
            IngressContext::default(),
        ),
        &[EliminatingConstraint::VolumeAlreadyOn {
            volume: app_volume("data"),
            located_on: vec![MachineName::parse("pinned").unwrap()],
        }],
        &["app_data", "pinned"],
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
    make_provisioned(&mut unchanged, "data", 1_073_741_824);
    let intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&applied, &unchanged],
        PlanOptions {
            selected: vec![ServiceAttempt {
                name: applied.name.clone(),
            }],
            ..PlanOptions::default()
        },
    );
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

    assert!(
        operations(&preview)
            .iter()
            .all(|operation| operation.service_name() != Some(&unchanged.name))
    );
    assert!(preview.volumes_to_create.is_empty());
}

fn global_service(name: &str, machine_name: &str, bytes: u64) -> RequestedServiceSpec {
    let mut service = requested(ServiceMode::Global);
    service.name = ServiceName::parse(name).unwrap();
    service.placement.machines = vec![MachineTarget::parse(machine_name).unwrap()];
    add_named_volume(&mut service, "data");
    make_provisioned(&mut service, "data", bytes);
    service
}

#[test]
fn provisioned_volume_aliases_cannot_conflict_on_one_docker_volume() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut requested, "data");
    make_provisioned(&mut requested, "data", 1_073_741_824);
    let mut volumes = requested.volume_graph.volumes().to_vec();
    let mut mounts = requested.volume_graph.mounts().to_vec();
    let mut alias = ServiceVolume {
        reference: ServiceVolumeReference::parse("data-alias").unwrap(),
        source: volumes.first().unwrap().source.clone(),
    };
    let VolumeSource::Provisioned {
        maximum_bytes: alias_maximum,
        ..
    } = &mut alias.source
    else {
        unreachable!("data fixture is provisioned")
    };
    *alias_maximum = maximum_bytes(2_147_483_648);
    volumes.push(alias);
    mounts.push(ServiceMount {
        volume: ServiceVolumeReference::parse("data-alias").unwrap(),
        target: ContainerPath::parse("/alias").unwrap(),
        read_only: false,
        no_copy: false,
        subpath: None,
    });
    assert_eq!(
        ployz_core::ServiceVolumeGraph::parse(volumes, mounts),
        Err(
            ployz_core::ServiceVolumeGraphError::IncompatibleVolumeAliases {
                name: DockerVolumeName::parse("data").unwrap(),
            }
        )
    );
}

#[test]
fn disjoint_global_volumes_may_have_different_bounds() {
    let first = global_service("first", "first", 1_073_741_824);
    let second = global_service("second", "second", 2_147_483_648);
    let intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&first, &second],
        PlanOptions::default(),
    );

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
    let first = global_service("first", "first", 1_073_741_824);
    let second = global_service("second", "first", 2_147_483_648);
    let intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&first, &second],
        PlanOptions {
            selected: vec![ServiceAttempt {
                name: first.name.clone(),
            }],
            ..PlanOptions::default()
        },
    );

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
                if matches!(source.as_ref(), PlanError::ConflictingDockerVolumeDefinitions { .. })
        ),
        "unexpected result: {result:?}"
    );
}

#[test]
fn colocated_global_services_reject_conflicting_provisioned_labels() {
    let first = global_service("first", "first", 1_073_741_824);
    let mut second = global_service("second", "first", 1_073_741_824);
    let mut volumes = second.volume_graph.volumes().to_vec();
    let mounts = second.volume_graph.mounts().to_vec();
    let VolumeSource::Provisioned { labels, .. } = &mut volumes
        .first_mut()
        .expect("global_service adds one volume")
        .source
    else {
        unreachable!("global_service adds a Provisioned Volume")
    };
    labels.insert("backup".into(), "daily".into());
    second.volume_graph = ployz_core::ServiceVolumeGraph::parse(volumes, mounts).unwrap();
    let intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&first, &second],
        PlanOptions::default(),
    );

    let result = preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            ..Default::default()
        },
        IngressContext::default(),
    );

    assert!(matches!(
        result,
        Err(PlanError::Service { source, .. })
            if matches!(source.as_ref(), PlanError::ConflictingDockerVolumeDefinitions { .. })
    ));
}

#[test]
fn profile_filtered_service_still_contributes_to_bound_conflicts() {
    let first = global_service("first", "first", 1_073_741_824);
    let second = global_service("second", "first", 2_147_483_648);
    let intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&first, &second],
        PlanOptions::default(),
    )
    .with_service_profiles(BTreeMap::from([(
        second.name.clone(),
        vec!["tools".into()],
    )]));

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
                if matches!(source.as_ref(), PlanError::ConflictingDockerVolumeDefinitions { .. })
        ),
        "unexpected result: {result:?}"
    );
}

#[test]
fn preview_distinguishes_provisioned_and_ordinary_volume_creates() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "data");
    make_provisioned(&mut requested, "data", 1_073_741_824);
    add_named_volume(&mut requested, "cache");
    let intent = DeployIntent::apply_all(
        ProjectName::parse("app").unwrap(),
        [&requested],
        PlanOptions::default(),
    );

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

    assert!(matches!(
        operations(&preview).as_slice(),
        [DeployOperation::RunContainer { .. }]
    ));
    assert!(preview.volumes_to_create.iter().any(|item| {
        item.name.as_str() == "app_data" && item.maximum_bytes == Some(maximum_bytes(1_073_741_824))
    }));
    assert!(
        preview
            .volumes_to_create
            .iter()
            .any(|item| { item.name.as_str() == "app_cache" && item.maximum_bytes.is_none() })
    );
}

#[test]
fn duplicate_target_services_fail_before_volume_resolution() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let intent = DeployIntent::new(
        ProjectName::parse("app").unwrap(),
        vec![requested.clone(), requested],
        PlanOptions::default(),
    );
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
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![owned_volume(
                machine_id('1'),
                "data",
            )])
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();
    assert!(plan.volumes_to_create.is_empty());
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
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![
                owned_volume(machine_id('1'), "web-data"),
                owned_volume(machine_id('1'), "worker-data"),
            ])
            .expect("valid Volume Snapshot fixture"),
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
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![
                owned_volume(machine_id('1'), "keep-b"),
                owned_volume(machine_id('1'), "keep-a"),
            ])
            .expect("valid Volume Snapshot fixture"),
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
