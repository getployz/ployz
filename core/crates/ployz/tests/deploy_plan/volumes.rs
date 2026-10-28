use super::support::*;
use ployz_core::{
    ContainerKind, CopyRole, DependencyCondition, DockerVolume, DockerVolumeStorageObservation,
    HttpProtocol, IngressHost, MachineFailure, MachineStorageObservation, PortPublication,
    PreDeployHook, PreservedVolume, ProvisionedVolumeMaximumBytes, PruneRefusal, RpcError,
    RpcErrorCode, ServiceAttempt, ServiceDependency, ServiceName, VolumeObservationFailure,
};
use std::{
    collections::{BTreeMap, BTreeSet},
    num::{NonZeroU32, NonZeroU64},
};

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
        Namespace::parse("app").unwrap(),
        [&service],
        PlanOptions::default(),
    )
}

fn replicated(name: &str) -> RequestedServiceSpec {
    let mut service = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    service.name = ServiceName::parse(name).unwrap();
    service
}

/// Plans `services` with an unrelated stateless `web` beside them, as a Deploy runner
/// does, and returns the Services it blocked.
fn blocked_beside_web(
    services: &[&RequestedServiceSpec],
    snapshot: &DeploySnapshot,
) -> (DeployPreview, Vec<(String, String)>) {
    let web = replicated("web");
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        services.iter().copied().chain([&web]),
        PlanOptions::default(),
    );
    let preview = runner_preview(&intent, snapshot).unwrap();
    assert!(
        operations(&preview).iter().any(|operation| matches!(
            operation,
            DeployOperation::RunContainer { spec, .. } if spec.name.as_str() == "web"
        )),
        "{:?}",
        operations(&preview)
    );
    let blocked = preview
        .blocked
        .iter()
        .map(|blocked| (blocked.service.to_string(), blocked.error.message.clone()))
        .collect();
    (preview, blocked)
}

fn assert_only_web_runs(preview: &DeployPreview) {
    assert!(
        preview.volumes_to_create.is_empty(),
        "{:?}",
        preview.volumes_to_create
    );
    assert!(
        operations(preview).iter().all(|operation| operation
            .spec()
            .is_none_or(|spec| spec.name.as_str() == "web")),
        "{:?}",
        operations(preview)
    );
}

fn listing_failure(machine: char) -> MachineFailure<RpcError> {
    MachineFailure {
        machine_id: machine_id(machine),
        error: RpcError {
            code: RpcErrorCode::Unavailable,
            message: "target Machine RPC timed out".into(),
            details: Default::default(),
            cause: Vec::new(),
        },
    }
}

/// A Docker Volume whose inspect failed on the only Server that can run its Service may
/// hold data there, so only that Service waits.
#[test]
fn unavailable_named_volume_blocks_only_a_dependent_service() {
    let mut dependent = replicated("api");
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
                    cause: Vec::new(),
                },
            }],
            Vec::new(),
            Vec::new(),
        )
        .expect("valid Volume Snapshot fixture"),
        ..storage_snapshot()
    };

    let (preview, blocked) = blocked_beside_web(&[&dependent], &snapshot);

    assert_eq!(
        blocked,
        [(
            "api".to_owned(),
            "first did not answer, so whether it holds Docker volume data is unknown. \
             Bring first back, or deploy the other services by name"
                .to_owned()
        )]
    );
    assert_only_web_runs(&preview);
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
                    cause: Vec::new(),
                },
            }],
            Vec::new(),
        )
        .expect("valid Volume Snapshot fixture"),
        ..storage_snapshot()
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

/// A Service whose every candidate went unread can only start where its Volume may
/// already be, so it waits and the rest of the Deploy goes ahead (#3561).
#[test]
fn a_service_whose_only_candidates_went_unread_blocks_only_itself() {
    for (provisioned, expected) in [
        (
            false,
            "machine-3 did not answer, so whether it holds Docker volume data is unknown. \
             Bring machine-3 back, or deploy the other services by name",
        ),
        (
            true,
            "machine-3 did not answer, so Volume data's copies are unknown. \
             Bring machine-3 back, or deploy the other services by name",
        ),
    ] {
        let mut pinned = replicated("api");
        pinned.placement.constraints = [label_constraint("machine-3")].into();
        add_named_volume(&mut pinned, "data");
        if provisioned {
            make_provisioned(&mut pinned, "data", ployz_core::STORAGE_GIB);
        }
        let snapshot = listing_gap_snapshot(&[]);

        let (preview, blocked) = blocked_beside_web(&[&pinned], &snapshot);

        assert_eq!(blocked, [("api".to_owned(), expected.to_owned())]);
        assert_only_web_runs(&preview);
    }
}
/// Three Servers with Managed volumes. machine-3 is Up but its Docker Volume listing
/// failed. Its ZFS storage still answered, so `holder` and its Volumes are located.
fn listing_gap_snapshot(holders: &[(char, &[&str])]) -> DeploySnapshot {
    let machines =
        [('1', "machine-1"), ('2', "machine-2"), ('3', "machine-3")].map(|(id, name)| {
            let mut machine = machine(id, name);
            machine.storage = Some(MachineStorageObservation::Ready);
            machine
        });
    let mut snapshot = DeploySnapshot {
        machines: machines.to_vec(),
        volume_snapshot: VolumeSnapshot::try_from_parts(
            Vec::new(),
            Vec::new(),
            vec![listing_failure('3')],
            Vec::new(),
        )
        .expect("valid Volume Snapshot fixture"),
        ..storage_snapshot()
    };
    for (holder, names) in holders {
        let capacity = snapshot
            .storage_capacity
            .get_mut(&machine_id(*holder))
            .expect("fixture has capacity for the holder")
            .as_mut()
            .expect("fixture capacity is observed");
        for name in *names {
            capacity
                .volumes
                .insert(app_volume(name), maximum_bytes(ployz_core::STORAGE_GIB));
        }
    }
    snapshot
}

/// A Managed volume whose Up holder failed its Docker Volume listing blocks only its
/// Service; unrelated Services deploy (#3561).
#[test]
fn a_managed_volume_whose_holder_listing_failed_blocks_only_its_service() {
    let mut service = replicated("mu");
    for name in ["data", "more"] {
        add_named_volume(&mut service, name);
        make_provisioned(&mut service, name, ployz_core::STORAGE_GIB);
    }
    let snapshot = listing_gap_snapshot(&[('3', &["data", "more"])]);

    let (preview, blocked) = blocked_beside_web(&[&service], &snapshot);

    assert_eq!(
        blocked,
        [(
            "mu".to_owned(),
            "machine-3 did not answer, so Volume data's copies are unknown. \
             Bring machine-3 back, or deploy the other services by name"
                .to_owned()
        )]
    );
    assert_only_web_runs(&preview);
}

/// Volumes held on different Servers have no common Server whether or not the unread one
/// answers, so the conflict still fails the Deploy.
#[test]
fn volumes_split_across_an_unread_server_still_conflict() {
    let mut service = replicated("mu");
    for name in ["data", "more"] {
        add_named_volume(&mut service, name);
        make_provisioned(&mut service, name, ployz_core::STORAGE_GIB);
    }
    let web = replicated("web");
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&service, &web],
        PlanOptions::default(),
    );
    for holders in [
        [('1', &["data"][..]), ('3', &["more"][..])],
        [('1', &["more"][..]), ('3', &["data"][..])],
    ] {
        let PlanError::Service { source, .. } =
            runner_preview(&intent, &listing_gap_snapshot(&holders)).unwrap_err()
        else {
            panic!("the conflict names its Service")
        };
        let error = source.to_string();

        assert!(error.contains("no machines available"), "{error}");
        assert!(
            error.contains("is already on Machine 'machine-1'"),
            "{error}"
        );
    }
}

/// machine-3 lists its Docker Volumes, but the inspect of each of `failed` there fails.
/// `listed` are Docker Volumes machine-1 lists, and `holders` hold Managed volumes.
fn inspect_gap_snapshot(
    holders: &[(char, &[&str])],
    listed: &[&str],
    failed: &[&str],
) -> DeploySnapshot {
    let mut snapshot = listing_gap_snapshot(holders);
    snapshot.volume_snapshot = VolumeSnapshot::try_from_parts(
        listed
            .iter()
            .map(|name| observed_volume(machine_id('1'), name))
            .collect(),
        failed
            .iter()
            .map(|name| VolumeObservationFailure {
                id: DockerVolumeId {
                    machine_id: machine_id('3'),
                    name: app_volume(name),
                },
                error: RpcError {
                    code: RpcErrorCode::Unavailable,
                    message: "volume detail failed".into(),
                    details: Default::default(),
                    cause: Vec::new(),
                },
            })
            .collect(),
        Vec::new(),
        Vec::new(),
    )
    .expect("valid Volume Snapshot fixture");
    snapshot
}

fn assert_conflict(services: &[&RequestedServiceSpec], snapshot: &DeploySnapshot) {
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        services.iter().copied(),
        PlanOptions::default(),
    );
    let error = runner_preview(&intent, snapshot).unwrap_err();
    let error = if let PlanError::Service { source, .. } = &error {
        source.to_string()
    } else {
        error.to_string()
    };
    assert!(error.contains("no machines available"), "{error}");
    assert!(
        error.contains("is already on Machine 'machine-1'"),
        "{error}"
    );
}

/// A failed inspect leaves a split between Servers as fatal as a failed listing does.
#[test]
fn volumes_split_across_a_failed_inspect_still_conflict() {
    let mut managed = replicated("mu");
    let mut docker = replicated("dv");
    for name in ["data", "more"] {
        add_named_volume(&mut managed, name);
        make_provisioned(&mut managed, name, ployz_core::STORAGE_GIB);
        add_named_volume(&mut docker, name);
    }
    for (data, more) in [('1', '3'), ('3', '1')] {
        let snapshot = inspect_gap_snapshot(
            &[(data, &["data"]), (more, &["more"])],
            &[],
            &[if more == '3' { "more" } else { "data" }],
        );
        assert_conflict(&[&managed], &snapshot);
    }
    for (listed, failed) in [("data", "more"), ("more", "data")] {
        assert_conflict(&[&docker], &inspect_gap_snapshot(&[], &[listed], &[failed]));
    }
}

/// A Docker Volume whose inspect failed exists on that Server, so it is never created
/// elsewhere, even when no Deploy applied it yet.
#[test]
fn a_docker_volume_whose_inspect_failed_is_never_created_elsewhere() {
    let mut service = replicated("dv");
    add_named_volume(&mut service, "data");

    let (preview, blocked) =
        blocked_beside_web(&[&service], &inspect_gap_snapshot(&[], &[], &["data"]));

    assert_eq!(
        blocked,
        [(
            "dv".to_owned(),
            "machine-3 did not answer, so whether it holds Docker volume data is unknown. \
             Bring machine-3 back, or deploy the other services by name"
                .to_owned()
        )]
    );
    assert_only_web_runs(&preview);
}

/// Services sharing a Managed volume on the unread holder wait together.
#[test]
fn services_sharing_a_managed_volume_on_an_unread_holder_wait_together() {
    let mut services = ["mu", "mv"].map(replicated);
    for service in &mut services {
        add_named_volume(service, "data");
        make_provisioned(service, "data", ployz_core::STORAGE_GIB);
    }
    let snapshot = listing_gap_snapshot(&[('3', &["data"])]);

    let (preview, blocked) = blocked_beside_web(&[&services[0], &services[1]], &snapshot);

    assert_eq!(
        blocked
            .iter()
            .map(|(service, _)| service.as_str())
            .collect::<Vec<_>>(),
        ["mu", "mv"]
    );
    assert_only_web_runs(&preview);
}

/// A Managed volume held on an answering Server is provably unaffected by another
/// Server's failed listing.
#[test]
fn a_managed_volume_on_an_answering_server_deploys_beside_a_listing_gap() {
    let mut service = replicated("mu");
    add_named_volume(&mut service, "data");
    make_provisioned(&mut service, "data", ployz_core::STORAGE_GIB);
    let snapshot = listing_gap_snapshot(&[('2', &["data"])]);

    let (preview, blocked) = blocked_beside_web(&[&service], &snapshot);

    assert!(blocked.is_empty(), "{blocked:?}");
    assert!(
        operations(&preview).iter().any(|operation| matches!(
            operation,
            DeployOperation::RunContainer { spec, machine_id: run, .. }
                if spec.name.as_str() == "mu" && *run == machine_id('2')
        )),
        "{:?}",
        operations(&preview)
    );
}
fn explicitly_targeted_provisioned_deploy(
    storage: Option<MachineStorageObservation>,
    volumes: Vec<DockerVolume>,
) -> Result<DeployPreview, PlanError> {
    let mut service = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    service.placement.constraints = [label_constraint("first")].into();
    add_named_volume(&mut service, "data");
    make_provisioned(&mut service, "data", 1_073_741_824);
    add_named_volume(&mut service, "cache");
    service.pre_deploy = Some(PreDeployHook {
        command: vec!["prepare".into()].try_into().unwrap(),
        environment: Default::default(),
        privileged: None,
        timeout_millis: None,
        user: None,
    });
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
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
            ..storage_snapshot()
        },
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
            DeployOperation::PrepareVolumes { .. },
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
    assert!(error.contains("cannot host Managed volumes"), "{error}");
    assert!(
        error.contains("pick a Server with Managed volumes"),
        "{error}"
    );
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
    assert!(error.contains("already uses Docker storage"), "{error}");
    assert!(
        error.contains("Automatic conversion to managed storage is unavailable"),
        "{error}"
    );
}

#[test]
fn ordinary_volume_does_not_adopt_an_existing_provisioned_volume() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    requested.placement.constraints = [label_constraint("first")].into();
    add_named_volume(&mut requested, "data");
    let mut existing = observed_volume(machine_id('1'), "data");
    existing.storage = DockerVolumeStorageObservation::Provisioned {
        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/app_data").unwrap(),
        bound_bytes: NonZeroU64::new(1_073_741_824).unwrap(),
        used_bytes: 0,
        role: None,
        slot_role: None,
    };

    let error = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![existing]).unwrap(),
            ..storage_snapshot()
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
                ..storage_snapshot()
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
        role: None,
        slot_role: None,
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
fn a_grown_provisioned_volume_is_reused_despite_the_size_docker_froze_at_create() {
    let mut existing = observed_volume(machine_id('1'), "data");
    existing.options = BTreeMap::from([("size".into(), "536870912b".into())]);
    existing.storage = DockerVolumeStorageObservation::Provisioned {
        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/app_data").unwrap(),
        bound_bytes: NonZeroU64::new(1_073_741_824).unwrap(),
        used_bytes: 0,
        role: None,
        slot_role: None,
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
        preview
            .volumes_to_create
            .iter()
            .all(|volume| volume.name.as_str() != "app_data"),
        "{:?}",
        preview.volumes_to_create
    );
}

#[test]
fn provisioned_volume_requires_requested_labels() {
    let mut existing = observed_volume(machine_id('1'), "data");
    existing.options = BTreeMap::from([("size".into(), "1073741824b".into())]);
    existing.labels.remove(NAMESPACE_LABEL);
    existing.storage = DockerVolumeStorageObservation::Provisioned {
        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/app_data").unwrap(),
        bound_bytes: NonZeroU64::new(1_073_741_824).unwrap(),
        used_bytes: 0,
        role: None,
        slot_role: None,
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

fn deploy_over_live_bound(bound_bytes: u64) -> Result<DeployPreview, PlanError> {
    let mut existing = observed_volume(machine_id('1'), "data");
    existing.options = BTreeMap::from([("size".into(), "1073741824b".into())]);
    existing.storage = DockerVolumeStorageObservation::Provisioned {
        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/app_data").unwrap(),
        bound_bytes: NonZeroU64::new(bound_bytes).unwrap(),
        used_bytes: 0,
        role: None,
        slot_role: None,
    };
    explicitly_targeted_provisioned_deploy(
        Some(MachineStorageObservation::Pool {
            size_bytes: NonZeroU64::new(10 * 1024_u64.pow(3)).unwrap(),
            used_bytes: 0,
            free_bytes: 10 * 1024_u64.pow(3),
        }),
        vec![existing],
    )
}

#[test]
fn a_volume_grown_above_its_requested_size_is_reused_by_a_later_deploy() {
    let preview = deploy_over_live_bound(2_147_483_648).unwrap();

    assert!(
        preview
            .volumes_to_create
            .iter()
            .all(|volume| volume.name.as_str() != "app_data"),
        "{:?}",
        preview.volumes_to_create
    );
}

#[test]
fn a_volume_whose_live_bound_is_below_the_request_is_refused_not_grown() {
    let error = deploy_over_live_bound(536_870_912).unwrap_err();

    assert!(
        matches!(error, PlanError::ExistingProvisionedVolumeMismatch { .. }),
        "{error}"
    );
    assert!(
        error.to_string().contains("neither grows nor replaces"),
        "{error}"
    );
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
            ..storage_snapshot()
        },
    )
    .unwrap();

    assert!(matches!(
        operations(&preview).as_slice(),
        [DeployOperation::PrepareVolumes { .. }, DeployOperation::RunContainer { machine_id: target, .. }] if target == &machine_id('2')
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
            ..storage_snapshot()
        },
    )
    .unwrap();

    assert!(matches!(
        operations(&preview).as_slice(),
        [DeployOperation::PrepareVolumes { .. }, DeployOperation::RunContainer { machine_id: target, .. }] if target == &machine_id('1')
    ));
    let warning = ployz_core::DeployWarning::StorageObservationUnknown {
        machine_id: machine_id('2'),
        machine_name: Some(MachineName::parse("unknown").unwrap()),
    };
    assert!(preview.warnings.contains(&warning));
    assert_eq!(
        warning.to_string(),
        "Server unknown did not report its storage, so this deploy placed no Managed volume on it. Bring unknown back to place Managed volumes there again."
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
            ..storage_snapshot()
        },
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
            ..storage_snapshot()
        },
    )
    .unwrap_err()
    .to_string();

    assert!(error.contains("app_data"), "{error}");
    assert!(error.contains("pinned"), "{error}");
    assert!(
        error.contains("Automatic conversion to managed storage is unavailable"),
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
        role: None,
        slot_role: None,
    };

    assert_no_eligible(
        preview_deploy(
            &intent,
            &DeploySnapshot {
                machines: vec![pinned, other],
                volume_snapshot: VolumeSnapshot::try_from_observations(vec![existing])
                    .expect("valid Volume Snapshot fixture"),
                ..storage_snapshot()
            },
        ),
        &[EliminatingConstraint::VolumeAlreadyOn {
            volume: app_volume("data"),
            located_on: vec![MachineName::parse("pinned").unwrap()],
        }],
        &["app_data", "pinned"],
    );
}

fn provisioned_row(machine: char) -> DockerVolume {
    let mut existing = observed_volume(machine_id(machine), "data");
    existing.options = BTreeMap::from([("size".into(), "1073741824b".into())]);
    existing.storage = DockerVolumeStorageObservation::Provisioned {
        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/app_data").unwrap(),
        bound_bytes: NonZeroU64::new(1_073_741_824).unwrap(),
        used_bytes: 0,
        role: None,
        slot_role: None,
    };
    existing
}

/// Two ready Machines whose storage reports `copies` of `app_data`, and Docker `rows`.
fn copies_snapshot(copies: &[(char, CopyRole)], rows: Vec<DockerVolume>) -> DeploySnapshot {
    copies_snapshot_of(&app_volume("data"), copies, rows)
}

/// Two ready Machines whose storage reports `copies` of `volume`, and Docker `rows`.
fn copies_snapshot_of(
    volume: &DockerVolumeName,
    copies: &[(char, CopyRole)],
    rows: Vec<DockerVolume>,
) -> DeploySnapshot {
    let mut snapshot = storage_snapshot();
    for (machine, role) in copies {
        let Some(Ok(capacity)) = snapshot.storage_capacity.get_mut(&machine_id(*machine)) else {
            panic!("fixture capacity for {machine}")
        };
        capacity.copies.insert(
            volume.clone(),
            ployz_core::ProvisionedCopy {
                role: *role,
                maximum_bytes: maximum_bytes(1_073_741_824),
                used_bytes: 0,
                slot_role: None,
            },
        );
    }
    let mut first = machine('1', "first");
    first.storage = Some(MachineStorageObservation::Ready);
    let mut second = machine('2', "second");
    second.storage = Some(MachineStorageObservation::Ready);
    snapshot.machines = vec![first, second];
    snapshot.volume_snapshot =
        VolumeSnapshot::try_from_observations(rows).expect("valid Volume Snapshot fixture");
    snapshot
}

#[test]
fn only_a_writer_copy_pins_a_provisioned_volume() {
    let intent = automatic_provisioned_intent();
    for (case, copies) in [
        ("a daemon that reports no copies", vec![]),
        ("a writer", vec![('1', CopyRole::Writer)]),
        (
            "a writer beside a slot",
            vec![('1', CopyRole::Writer), ('2', CopyRole::Slot)],
        ),
    ] {
        let preview = preview_deploy(
            &intent,
            &copies_snapshot(&copies, vec![provisioned_row('1')]),
        )
        .unwrap_or_else(|error| panic!("{case}: {error}"));
        assert!(
            matches!(
                operations(&preview).as_slice(),
                [DeployOperation::PrepareVolumes { .. }, DeployOperation::RunContainer { machine_id: target, .. }] if target == &machine_id('1')
            ),
            "{case}: {:?}",
            operations(&preview)
        );
        assert!(preview.volumes_to_create.is_empty(), "{case}");
    }
}

/// Plan `intent` as the Store runner does: a Service whose Volume nothing can mount
/// is blocked while the rest are planned.
fn runner_preview(
    intent: &DeployIntent,
    snapshot: &DeploySnapshot,
) -> Result<DeployPreview, PlanError> {
    let mut intent = intent.clone();
    intent.options.block_unmountable = true;
    preview_deploy(&intent, snapshot)
}

#[test]
fn a_direct_preview_refuses_a_volume_without_a_writer_as_a_whole() {
    let intent = automatic_provisioned_intent();

    let error =
        preview_deploy(&intent, &copies_snapshot(&[('2', CopyRole::Slot)], vec![])).unwrap_err();

    assert!(
        matches!(
            &error,
            PlanError::NoWriter { .. } | PlanError::Service { .. }
        ),
        "{error}"
    );
    assert!(
        error
            .to_string()
            .contains("Volume data has no writer; second holds its only copy"),
        "{error}"
    );
}

#[test]
fn a_provisioned_volume_without_a_writer_refuses_and_names_the_way_back() {
    let intent = automatic_provisioned_intent();
    for (case, copies, rows, needle) in [
        (
            "a slot alone",
            vec![('2', CopyRole::Slot)],
            vec![],
            "Volume data has no writer; second holds its only copy. Make it the writer: ployz volume restore data --from second",
        ),
        (
            "a switching root",
            vec![('1', CopyRole::Switching)],
            vec![provisioned_row('1')],
            "Volume data is mid-Move; it is held as first (switching); ployz volume release data stops it",
        ),
        (
            "slots on both",
            vec![('1', CopyRole::Slot), ('2', CopyRole::Slot)],
            vec![],
            "Restore needs a single copy: keep the newest, delete each other one from the Volume's page in Ployz Cloud, which shows whether it is a mirror or an old copy, then run ployz volume restore data --from <server you kept>",
        ),
    ] {
        let preview = runner_preview(&intent, &copies_snapshot(&copies, rows)).unwrap();
        let rpc = only_blocked(&preview);
        assert!(rpc.message.contains(needle), "{case}: {}", rpc.message);
        assert_eq!(rpc.code, RpcErrorCode::Conflict, "{case}");
        assert_eq!(rpc.details.get("reason").unwrap(), "no_writer", "{case}");
    }
}

/// The one Service `preview` blocked, and why. Fails when it planned work instead.
fn only_blocked(preview: &DeployPreview) -> RpcError {
    assert!(preview.operations.is_empty(), "{:?}", operations(preview));
    let [blocked] = preview.blocked.as_slice() else {
        panic!("{:?}", preview.blocked)
    };
    blocked.error.clone()
}

#[test]
fn a_silent_machine_leaves_the_writer_unknown_instead_of_naming_a_copy() {
    let intent = automatic_provisioned_intent();
    for (case, silent) in [
        ("not asked", None),
        (
            "storage read failed",
            Some(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "connection lost".into(),
                details: Default::default(),
                cause: Vec::new(),
            }),
        ),
    ] {
        let mut snapshot = copies_snapshot(&[('2', CopyRole::Slot)], vec![]);
        snapshot.machines.push(machine('3', "third"));
        match silent {
            None => snapshot.storage_capacity.remove(&machine_id('3')),
            Some(error) => snapshot
                .storage_capacity
                .insert(machine_id('3'), Err(error)),
        };

        let rpc = only_blocked(&runner_preview(&intent, &snapshot).unwrap());

        assert_eq!(
            rpc.message,
            "third did not answer, so Volume data's copies are unknown. Bring third back, or deploy the other services by name",
            "{case}"
        );
        assert_eq!(rpc.code, RpcErrorCode::Unavailable, "{case}");
    }
}

#[test]
fn a_cloud_volume_without_a_writer_is_named_as_authored() {
    let volume = "3f2a6c1e-8d4b-4e7a-9c15-2b6d8e0f4a71";
    let intent = ployz_core::config::lower_deployment(
        serde_json::from_value(serde_json::json!({
            "namespace": "app",
            "snapshots": [{
                "serviceId": "web",
                "config": {"version": 2, "privateDns": "web", "healthcheck": {"type": "none"},
                    "restartPolicy": "on-failure",
                    "source": {"type": "image", "version": 1, "image": "nginx:latest", "credentials": {"type": "none"}},
                    "mounts": [{"volumeResourceId": volume, "volumeName": "echo", "mountPath": "/data"}]}
            }],
            "volumes": [{"volumeResourceId": volume, "storage": {"kind": "provisioned", "maximumBytes": 1_073_741_824}}]
        }))
        .unwrap(),
    )
    .unwrap();
    let physical = app_volume(&format!("vol-{volume}"));

    let preview = runner_preview(
        &intent,
        &copies_snapshot_of(&physical, &[('1', CopyRole::Switching)], vec![]),
    )
    .unwrap();

    assert_eq!(
        only_blocked(&preview).message,
        "Volume echo is mid-Move; it is held as first (switching); ployz volume release echo stops it"
    );
}

/// The Deploy Intent of one Service mounting Managed Volume `echo`, which a Deploy
/// applied before when `deployed`.
fn lowered_echo(deployed: bool) -> (DeployIntent, DockerVolumeName) {
    lowered_echo_in(
        serde_json::json!({"kind": "provisioned", "maximumBytes": 1_073_741_824}),
        deployed,
    )
}

/// The Deploy Intent of one Service mounting Volume `echo` kept in `storage`, which a
/// Deploy applied before when `deployed`.
fn lowered_echo_in(storage: serde_json::Value, deployed: bool) -> (DeployIntent, DockerVolumeName) {
    let volume = "3f2a6c1e-8d4b-4e7a-9c15-2b6d8e0f4a71";
    let intent = ployz_core::config::lower_deployment(
        serde_json::from_value(serde_json::json!({
            "namespace": "app",
            "snapshots": [{
                "serviceId": "web",
                "config": {"version": 2, "privateDns": "web", "healthcheck": {"type": "none"},
                    "restartPolicy": "on-failure",
                    "source": {"type": "image", "version": 1, "image": "nginx:latest", "credentials": {"type": "none"}},
                    "mounts": [{"volumeResourceId": volume, "volumeName": "echo", "mountPath": "/data"}]}
            }],
            "volumes": [{"volumeResourceId": volume, "storage": storage, "deployed": deployed}]
        }))
        .unwrap(),
    )
    .unwrap();
    (intent, app_volume(&format!("vol-{volume}")))
}

/// Server `live` up and holding nothing, and Server `holder` in `membership`, holding
/// Docker Volume `physical` when it answers.
fn docker_holder_snapshot(
    physical: &DockerVolumeName,
    membership: MembershipObservation,
) -> DeploySnapshot {
    let mut holder = machine('3', "holder");
    holder.membership = membership.clone();
    let mut snapshot = DeploySnapshot {
        machines: vec![machine('1', "live"), holder],
        ..Default::default()
    };
    if membership == MembershipObservation::Down {
        snapshot.machines.get_mut(1).unwrap().membership_evidence =
            Some(ployz_core::MembershipEvidence::Down);
        snapshot.container_omissions.push(machine_id('3'));
        snapshot.volume_snapshot = VolumeSnapshot::try_from_parts(
            Vec::new(),
            Vec::new(),
            Vec::new(),
            vec![machine_id('3')],
        )
        .unwrap();
    } else {
        let mut row = observed_volume(machine_id('3'), "unused");
        row.id.name = physical.clone();
        snapshot.volume_snapshot = VolumeSnapshot::try_from_observations(vec![row]).unwrap();
    }
    snapshot
}

/// A Docker Volume an earlier Deploy applied may sit on a Down Server that no listing
/// reached. Creating it on another Server would start the Service on an empty copy,
/// and a later Deploy would remove the one holding the data (#3526).
#[test]
fn a_deployed_docker_volume_whose_server_is_down_is_not_attempted() {
    let (intent, physical) = lowered_echo_in(serde_json::json!({"kind": "docker"}), true);

    let preview = runner_preview(
        &intent,
        &docker_holder_snapshot(&physical, MembershipObservation::Down),
    )
    .unwrap();

    assert!(
        preview.volumes_to_create.is_empty(),
        "{:?}",
        preview.volumes_to_create
    );
    assert_eq!(
        only_blocked(&preview).message,
        "holder did not answer, so whether it holds Docker volume echo is unknown. \
         Bring holder back, or deploy the other services by name"
    );
}

/// Once the Server is back, the Deploy runs the Service beside its original copy and
/// creates no other.
#[test]
fn a_deployed_docker_volume_whose_server_returned_stays_on_it() {
    let (intent, physical) = lowered_echo_in(serde_json::json!({"kind": "docker"}), true);

    let preview = runner_preview(
        &intent,
        &docker_holder_snapshot(&physical, MembershipObservation::Up),
    )
    .unwrap();

    assert!(preview.blocked.is_empty(), "{:?}", preview.blocked);
    assert!(
        preview.volumes_to_create.is_empty(),
        "{:?}",
        preview.volumes_to_create
    );
    let targets = operations(&preview)
        .into_iter()
        .filter_map(|operation| {
            let DeployOperation::RunContainer { machine_id, .. } = operation else {
                return None;
            };
            Some(machine_id)
        })
        .collect::<Vec<_>>();
    assert_eq!(targets, [machine_id('3')]);
}

/// A Docker Volume no Deploy applied yet has no copy anywhere, so a Down Server
/// holds none, and the Deploy creates it on a Server that answers.
#[test]
fn a_new_docker_volume_is_created_while_another_server_is_down() {
    let (intent, physical) = lowered_echo_in(serde_json::json!({"kind": "docker"}), false);

    let preview = runner_preview(
        &intent,
        &docker_holder_snapshot(&physical, MembershipObservation::Down),
    )
    .unwrap();

    assert!(preview.blocked.is_empty(), "{:?}", preview.blocked);
    assert_eq!(
        preview.volumes_to_create.len(),
        1,
        "{:?}",
        operations(&preview)
    );
}

/// A Service mounting a deployed Docker Volume and a deployed Managed one, both on a
/// Down Server, is Not attempted while another Service still deploys.
#[test]
fn a_service_with_both_volume_kinds_on_a_down_server_blocks_only_itself() {
    let image = serde_json::json!({"type": "image", "version": 1, "image": "nginx:latest", "credentials": {"type": "none"}});
    let intent = ployz_core::config::lower_deployment(
        serde_json::from_value(serde_json::json!({
            "namespace": "app",
            "snapshots": [
                {"serviceId": "db", "config": {"version": 2, "privateDns": "db", "healthcheck": {"type": "none"},
                    "restartPolicy": "on-failure", "source": image,
                    "mounts": [
                        {"volumeResourceId": "a-cache", "volumeName": "cache", "mountPath": "/cache"},
                        {"volumeResourceId": "b-data", "volumeName": "data", "mountPath": "/data"}
                    ]}},
                {"serviceId": "web", "config": {"version": 2, "privateDns": "web", "healthcheck": {"type": "none"},
                    "restartPolicy": "on-failure", "source": image}}
            ],
            "volumes": [
                {"volumeResourceId": "a-cache", "storage": {"kind": "docker"}, "deployed": true},
                {"volumeResourceId": "b-data", "storage": {"kind": "provisioned", "maximumBytes": 1_073_741_824}, "deployed": true}
            ]
        }))
        .unwrap(),
    )
    .unwrap();
    let mut snapshot =
        docker_holder_snapshot(&app_volume("vol-a-cache"), MembershipObservation::Down);
    let mut live = machine('1', "live");
    live.storage = Some(MachineStorageObservation::Ready);
    *snapshot.machines.get_mut(0).unwrap() = live;
    snapshot.storage_capacity = storage_snapshot()
        .storage_capacity
        .into_iter()
        .filter(|(machine, _)| *machine == machine_id('1'))
        .collect();

    let preview = runner_preview(&intent, &snapshot).unwrap();

    let [blocked] = preview.blocked.as_slice() else {
        panic!("{:?}", preview.blocked)
    };
    assert_eq!(blocked.service.as_str(), "db", "{blocked:?}");
    assert!(
        operations(&preview).iter().any(|operation| matches!(
            operation,
            DeployOperation::RunContainer { spec, .. } if spec.name.as_str() == "web"
        )),
        "{:?}",
        operations(&preview)
    );
}

/// A Server this Deploy could not read, beside an answering Server `live`: no
/// listing shows the Volume anywhere.
fn unread_holder_snapshot(
    unread: impl FnOnce(&mut MachineObservation, &mut VolumeSnapshot),
) -> DeploySnapshot {
    let mut holder = machine('3', "holder");
    holder.machine.runtime.daemon_version = "0.3.8".into();
    let mut volumes = VolumeSnapshot::default();
    unread(&mut holder, &mut volumes);
    DeploySnapshot {
        machines: vec![machine('1', "live"), holder],
        volume_snapshot: volumes,
        ..Default::default()
    }
}

fn unrecognized(holder: &mut MachineObservation, volumes: &mut VolumeSnapshot) {
    holder.membership = MembershipObservation::Unrecognized("drifting".into());
    holder.membership_evidence = Some(ployz_core::MembershipEvidence::Unrecognized {
        raw: "drifting".into(),
    });
    *volumes = omitted();
}

/// The Volume listing a Server that invites no RPC leaves out.
fn omitted() -> VolumeSnapshot {
    VolumeSnapshot::try_from_parts(Vec::new(), Vec::new(), Vec::new(), vec![machine_id('3')])
        .unwrap()
}

fn unknown(holder: &mut MachineObservation, volumes: &mut VolumeSnapshot) {
    holder.membership = MembershipObservation::Unknown;
    *volumes = omitted();
}

fn listing_failed(_: &mut MachineObservation, volumes: &mut VolumeSnapshot) {
    *volumes = VolumeSnapshot::try_from_parts(
        Vec::new(),
        Vec::new(),
        vec![MachineFailure {
            machine_id: machine_id('3'),
            error: RpcError {
                code: RpcErrorCode::Unavailable,
                message: "Docker did not answer".into(),
                details: Default::default(),
                cause: Vec::new(),
            },
        }],
        Vec::new(),
    )
    .unwrap();
}

fn assert_holder_unknown(preview: &DeployPreview) {
    assert!(
        preview.volumes_to_create.is_empty(),
        "{:?}",
        preview.volumes_to_create
    );
    assert_eq!(
        only_blocked(preview).message,
        "holder did not answer, so whether it holds Docker volume echo is unknown. \
         Bring holder back, or deploy the other services by name"
    );
}

/// No RPC reaches a Server whose membership this Entry cannot decode, so its Volumes
/// are never listed (#3540).
#[test]
fn a_deployed_docker_volume_whose_server_is_unrecognized_is_not_attempted() {
    let (intent, _) = lowered_echo_in(serde_json::json!({"kind": "docker"}), true);

    let preview = runner_preview(&intent, &unread_holder_snapshot(unrecognized)).unwrap();

    assert_holder_unknown(&preview);
}

/// A Server that ran a daemon but whose membership is Unknown is asked nothing.
#[test]
fn a_deployed_docker_volume_whose_server_is_unknown_is_not_attempted() {
    let (intent, _) = lowered_echo_in(serde_json::json!({"kind": "docker"}), true);

    let preview = runner_preview(&intent, &unread_holder_snapshot(unknown)).unwrap();

    assert_holder_unknown(&preview);
}

/// An Up Server whose Docker Volume listing failed may hold the copy it did not list
/// (#3540).
#[test]
fn a_deployed_docker_volume_whose_server_listing_failed_is_not_attempted() {
    let (intent, _) = lowered_echo_in(serde_json::json!({"kind": "docker"}), true);

    let preview = runner_preview(&intent, &unread_holder_snapshot(listing_failed)).unwrap();

    assert_holder_unknown(&preview);
}

/// A Docker Volume no Deploy applied yet has no copy on any unread Server, and an
/// Unarrived Server never held one.
#[test]
fn a_new_docker_volume_is_created_beside_an_unread_server() {
    let (new, _) = lowered_echo_in(serde_json::json!({"kind": "docker"}), false);
    let (deployed, _) = lowered_echo_in(serde_json::json!({"kind": "docker"}), true);
    let unarrived = |holder: &mut MachineObservation, volumes: &mut VolumeSnapshot| {
        unknown(holder, volumes);
        holder.machine.runtime.daemon_version = String::new();
    };

    for (intent, snapshot) in [
        (&new, unread_holder_snapshot(unrecognized)),
        (&new, unread_holder_snapshot(unknown)),
        (&new, unread_holder_snapshot(listing_failed)),
        (&deployed, unread_holder_snapshot(unarrived)),
    ] {
        let preview = runner_preview(intent, &snapshot).unwrap();

        assert!(preview.blocked.is_empty(), "{:?}", preview.blocked);
        assert_eq!(
            preview.volumes_to_create.len(),
            1,
            "{:?}",
            operations(&preview)
        );
    }
}

#[test]
fn a_deployed_volume_no_member_holds_refuses_instead_of_starting_empty() {
    let (intent, physical) = lowered_echo(true);

    let preview = runner_preview(&intent, &copies_snapshot_of(&physical, &[], vec![])).unwrap();

    let rpc = only_blocked(&preview);
    assert_eq!(
        rpc.message,
        "Volume echo was deployed, but no Server in this Cluster holds a copy of it. If a Server you removed still holds its data, add that Server back, then run ployz volume restore echo --from <server>"
    );
    assert_eq!(rpc.details.get("reason").unwrap(), "no_writer");
}

#[test]
fn a_deployed_volume_with_a_silent_machine_leaves_its_copies_unknown() {
    let (intent, physical) = lowered_echo(true);
    let mut snapshot = copies_snapshot_of(&physical, &[], vec![]);
    snapshot.storage_capacity.remove(&machine_id('2'));

    let rpc = only_blocked(&runner_preview(&intent, &snapshot).unwrap());

    assert_eq!(rpc.code, RpcErrorCode::Unavailable, "{}", rpc.message);
    assert!(
        rpc.message.contains("copies are unknown"),
        "{}",
        rpc.message
    );
}

#[test]
fn a_volume_no_deploy_applied_yet_is_created() {
    let (intent, physical) = lowered_echo(false);

    let preview = runner_preview(&intent, &copies_snapshot_of(&physical, &[], vec![])).unwrap();

    assert_eq!(
        preview.volumes_to_create.len(),
        1,
        "{:?}",
        operations(&preview)
    );
}

#[test]
fn a_deployed_volume_no_member_holds_leaves_a_deploy_of_another_service_alone() {
    let mut db = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    db.name = ServiceName::parse("db").unwrap();
    add_named_volume(&mut db, "data");
    make_provisioned(&mut db, "data", 1_073_741_824);
    let web = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&web, &db],
        PlanOptions {
            selected: vec![ServiceAttempt {
                name: web.name.clone(),
            }],
            ..PlanOptions::default()
        },
    )
    .with_deployed_volumes([app_volume("data")].into());

    let preview = runner_preview(&intent, &copies_snapshot(&[], vec![])).unwrap();

    assert!(
        operations(&preview)
            .iter()
            .all(|operation| operation.service_name() != Some(&db.name))
    );
}

/// Deploys `selected` (all of `web` and `db` when empty) onto one Machine whose `data`
/// Volume lost its labels.
fn deploy_over_mismatched_volume(selected: &[&ServiceName]) -> Result<DeployPreview, PlanError> {
    let (web, db) = web_and_db();
    let mut existing = observed_volume(machine_id('1'), "data");
    existing.options = BTreeMap::from([("size".into(), "1073741824b".into())]);
    existing.labels.remove(NAMESPACE_LABEL);
    existing.storage = DockerVolumeStorageObservation::Provisioned {
        mountpoint: MachinePath::parse("/var/lib/ployz-volumes/app_data").unwrap(),
        bound_bytes: NonZeroU64::new(1_073_741_824).unwrap(),
        used_bytes: 0,
        role: None,
        slot_role: None,
    };
    let mut target = machine('1', "first");
    target.storage = Some(MachineStorageObservation::Ready);
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&web, &db],
        PlanOptions {
            selected: selected
                .iter()
                .map(|name| ServiceAttempt {
                    name: (*name).clone(),
                })
                .collect(),
            ..PlanOptions::default()
        },
    );
    preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![target],
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![existing])
                .expect("valid Volume Snapshot fixture"),
            ..storage_snapshot()
        },
    )
}

#[test]
fn a_mismatched_volume_leaves_a_deploy_of_another_service_alone() {
    let (web, db) = web_and_db();

    let preview = deploy_over_mismatched_volume(&[&web.name]).unwrap();
    let planned: Vec<_> = operations(&preview)
        .iter()
        .filter_map(|operation| operation.service_name().cloned())
        .collect();
    assert_eq!(planned, std::slice::from_ref(&web.name));
    assert!(preview.volumes_to_create.is_empty());

    for selected in [vec![&db.name], vec![]] {
        let error = deploy_over_mismatched_volume(&selected).unwrap_err();
        let PlanError::Service { service, source } = &error else {
            panic!("{error}")
        };
        assert_eq!(service, "db");
        assert!(
            matches!(
                **source,
                PlanError::ExistingProvisionedVolumeMismatch { .. }
            ),
            "{source}"
        );
    }
}

/// `web`, which mounts nothing, and `db`, which mounts Managed Volume `data`.
fn web_and_db() -> (RequestedServiceSpec, RequestedServiceSpec) {
    let mut db = spec("db");
    add_named_volume(&mut db, "data");
    make_provisioned(&mut db, "data", 1_073_741_824);
    (spec("web"), db)
}

#[test]
fn a_full_deploy_blocks_only_the_service_whose_volume_has_no_writer() {
    let (web, db) = web_and_db();
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&web, &db],
        PlanOptions::default(),
    );

    let preview =
        runner_preview(&intent, &copies_snapshot(&[('2', CopyRole::Slot)], vec![])).unwrap();

    let planned: Vec<_> = operations(&preview)
        .iter()
        .filter_map(|operation| operation.service_name().cloned())
        .collect();
    assert_eq!(
        planned,
        std::slice::from_ref(&web.name),
        "{:?}",
        operations(&preview)
    );
    assert!(preview.volumes_to_create.is_empty());
    let [blocked] = preview.blocked.as_slice() else {
        panic!("{:?}", preview.blocked)
    };
    assert_eq!(blocked.service, db.name);
    assert_eq!(blocked.error.code, RpcErrorCode::Conflict);
    assert_eq!(blocked.error.details.get("reason").unwrap(), "no_writer");
    assert!(
        blocked
            .error
            .message
            .starts_with("Volume data has no writer; second holds its only copy"),
        "{}",
        blocked.error.message
    );
    assert_eq!(preview.depths.keys().collect::<Vec<_>>(), [&web.name]);
    assert_eq!(preview.placed_on.keys().collect::<Vec<_>>(), [&web.name]);
}

/// db runs on `second`, which holds its Volume and no longer accepts services, as after
/// `ployz server drain second`.
fn drained_volume_snapshot(db: &RequestedServiceSpec) -> DeploySnapshot {
    let mut snapshot = copies_snapshot(&[('2', CopyRole::Writer)], vec![provisioned_row('2')]);
    snapshot
        .machines
        .get_mut(1)
        .unwrap()
        .machine
        .accepts_services = false;
    snapshot.containers = vec![container('a', '2', db, &service_id('a'))];
    snapshot
}

#[test]
fn a_volume_on_a_server_that_accepts_no_services_blocks_only_its_service() {
    let (web, db) = web_and_db();
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&web, &db],
        PlanOptions::default(),
    );

    let preview = runner_preview(&intent, &drained_volume_snapshot(&db)).unwrap();

    let planned: Vec<_> = operations(&preview)
        .iter()
        .filter_map(|operation| operation.service_name().cloned())
        .collect();
    assert_eq!(
        planned,
        std::slice::from_ref(&web.name),
        "{:?}",
        operations(&preview)
    );
    let [blocked] = preview.blocked.as_slice() else {
        panic!("{:?}", preview.blocked)
    };
    assert_eq!(blocked.service, db.name);
    assert_eq!(blocked.error.code, RpcErrorCode::Conflict);
    assert_eq!(
        blocked.error.details.get("reason").unwrap(),
        "volume_server_takes_no_services"
    );
    assert_eq!(
        blocked.error.message,
        "Volume data is on second, which doesn't run services. Turn them back on with ployz \
         server set second --accepts-services=true"
    );
}

#[test]
fn a_direct_preview_names_the_volume_its_server_and_the_way_back() {
    let (_, db) = web_and_db();
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&db],
        PlanOptions::default(),
    );

    let error = preview_deploy(&intent, &drained_volume_snapshot(&db)).unwrap_err();

    assert_eq!(
        error.to_string(),
        "Volume data is on second, which doesn't run services. Turn them back on with ployz \
         server set second --accepts-services=true"
    );
}

#[test]
fn a_service_that_depends_on_a_blocked_one_is_blocked_too() {
    let (web, db) = web_and_db();
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&web, &db],
        PlanOptions::default(),
    )
    .with_dependencies(BTreeMap::from([(
        web.name.clone(),
        vec![ServiceDependency {
            service: db.name.clone(),
            condition: DependencyCondition::ServiceHealthy,
        }],
    )]));

    let preview =
        runner_preview(&intent, &copies_snapshot(&[('2', CopyRole::Slot)], vec![])).unwrap();

    assert!(
        operations(&preview).is_empty(),
        "{:?}",
        operations(&preview)
    );
    let blocked: Vec<_> = preview
        .blocked
        .iter()
        .map(|blocked| (blocked.service.as_str(), blocked.error.message.as_str()))
        .collect();
    assert_eq!(blocked.first().map(|entry| entry.0), Some("db"));
    assert_eq!(
        blocked.get(1).copied(),
        Some(("web", "depends on db, which was not attempted"))
    );
    assert!(
        preview.dependencies.is_empty(),
        "{:?}",
        preview.dependencies
    );
    assert!(preview.depths.is_empty(), "{:?}", preview.depths);
}

#[test]
fn a_service_handing_a_hostname_to_a_blocked_one_is_blocked_too() {
    let publishing = |name: &str| {
        let mut spec = spec(name);
        spec.ports = vec![PortPublication::Ingress {
            hostname: IngressHost::parse("api.example.com").unwrap(),
            load_balancer_port: NonZeroU16::new(80).unwrap(),
            container_port: NonZeroU16::new(80).unwrap(),
            http_protocol: HttpProtocol::Http,
        }];
        spec
    };
    let web = spec("web");
    let mut api = publishing("api");
    add_named_volume(&mut api, "data");
    make_provisioned(&mut api, "data", 1_073_741_824);
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&web, &api],
        PlanOptions::default(),
    );
    let mut snapshot = copies_snapshot(&[('2', CopyRole::Slot)], vec![]);
    snapshot
        .containers
        .push(container('c', '1', &publishing("web"), &service_id('a')));

    let preview = runner_preview(&intent, &snapshot).unwrap();

    assert!(
        operations(&preview).is_empty(),
        "web keeps publishing the hostname: {:?}",
        operations(&preview)
    );
    let blocked: Vec<_> = preview
        .blocked
        .iter()
        .map(|blocked| (blocked.service.as_str(), blocked.error.message.as_str()))
        .collect();
    assert_eq!(blocked.first().map(|entry| entry.0), Some("api"));
    assert_eq!(
        blocked.get(1).copied(),
        Some((
            "web",
            "hands hostname api.example.com to api, which was not attempted"
        ))
    );
}

#[test]
fn a_blocked_dependent_prepares_none_of_its_volumes() {
    let (mut web, db) = web_and_db();
    add_named_volume(&mut web, "shared");
    make_provisioned(&mut web, "shared", 1_073_741_824);
    let mut api = spec("api");
    add_named_volume(&mut api, "shared");
    make_provisioned(&mut api, "shared", 1_073_741_824);
    add_named_volume(&mut api, "own");
    make_provisioned(&mut api, "own", 1_073_741_824);
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&web, &db, &api],
        PlanOptions::default(),
    )
    .with_dependencies(BTreeMap::from([(
        api.name.clone(),
        vec![ServiceDependency {
            service: db.name.clone(),
            condition: DependencyCondition::ServiceHealthy,
        }],
    )]));

    let preview =
        runner_preview(&intent, &copies_snapshot(&[('2', CopyRole::Slot)], vec![])).unwrap();

    assert!(
        preview
            .blocked
            .iter()
            .any(|blocked| blocked.service == api.name),
        "{:?}",
        preview.blocked
    );
    let own = app_volume("own");
    assert!(
        preview
            .volumes_to_create
            .iter()
            .all(|volume| volume.name != own),
        "{:?}",
        preview.volumes_to_create
    );
    assert!(
        !format!("{:?}", operations(&preview)).contains(own.as_str()),
        "{:?}",
        operations(&preview)
    );
}

#[test]
fn a_silent_volume_server_leaves_a_deploy_of_another_service_alone() {
    let mut db = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    db.name = ServiceName::parse("db").unwrap();
    add_named_volume(&mut db, "data");
    make_provisioned(&mut db, "data", 1_073_741_824);
    let web = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&web, &db],
        PlanOptions {
            selected: vec![ServiceAttempt {
                name: web.name.clone(),
            }],
            ..PlanOptions::default()
        },
    )
    .with_deployed_volumes([app_volume("data")].into());
    let mut snapshot = copies_snapshot(&[], vec![]);
    snapshot.storage_capacity.remove(&machine_id('2'));

    let preview = preview_deploy(&intent, &snapshot).unwrap();

    assert!(
        operations(&preview)
            .iter()
            .all(|operation| operation.service_name() != Some(&db.name))
    );

    let depends = intent.clone().with_dependencies(BTreeMap::from([(
        web.name.clone(),
        vec![ployz_core::ServiceDependency {
            service: db.name.clone(),
            condition: ployz_core::DependencyCondition::ServiceStarted,
        }],
    )]));
    assert!(matches!(
        preview_deploy(&depends, &snapshot),
        Err(PlanError::Service { source, .. })
            if matches!(*source, PlanError::CopiesUnknown { .. })
    ));

    let mut pinned = intent.clone();
    for service in &mut pinned.target {
        if service.name == db.name {
            service.placement.constraints = [label_constraint("second")].into();
        }
    }
    snapshot.machines.last_mut().unwrap().membership = MembershipObservation::Down;
    let preview = preview_deploy(&pinned, &snapshot).unwrap();
    assert!(
        operations(&preview)
            .iter()
            .all(|operation| operation.service_name() != Some(&db.name))
    );
}

#[test]
fn a_blocked_dependent_gives_back_the_endpoint_its_shared_reservation_took() {
    let (web, db) = web_and_db();
    let mut api = spec("api");
    let mut worker = spec("worker");
    for spec in [&mut api, &mut worker] {
        add_named_volume(spec, "shared");
        make_provisioned(spec, "shared", 1_073_741_824);
    }
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&db, &api, &worker, &web],
        PlanOptions::default(),
    )
    .with_dependencies(BTreeMap::from([(
        api.name.clone(),
        vec![ServiceDependency {
            service: db.name.clone(),
            condition: DependencyCondition::ServiceHealthy,
        }],
    )]));
    // Exactly the two endpoints api and worker reserve together; web needs the one api gives back.
    let snapshot = DeploySnapshot {
        capacity: capacity([('1', 2), ('2', 0)]),
        ..copies_snapshot(&[('2', CopyRole::Slot)], vec![])
    };

    let preview = runner_preview(&intent, &snapshot).unwrap();

    let blocked: Vec<_> = preview
        .blocked
        .iter()
        .map(|blocked| blocked.service.as_str())
        .collect();
    assert_eq!(blocked, ["db", "api"]);
    let planned: Vec<_> = operations(&preview)
        .iter()
        .filter_map(|operation| operation.service_name().cloned())
        .collect();
    assert_eq!(planned, [worker.name.clone(), web.name.clone()]);
    assert_eq!(
        preview.placed_on.get(&web.name),
        Some(&[machine_id('1')].into())
    );
}

#[test]
fn a_blocked_service_keeps_the_endpoint_its_old_hook_was_credited_for() {
    let (mut web, db) = web_and_db();
    let mut api = spec("api");
    let mut worker = spec("worker");
    for spec in [&mut api, &mut worker] {
        add_named_volume(spec, "shared");
        make_provisioned(spec, "shared", 1_073_741_824);
    }
    api.pre_deploy = Some(PreDeployHook {
        command: vec!["migrate".into()].try_into().unwrap(),
        environment: Default::default(),
        privileged: None,
        timeout_millis: None,
        user: None,
    });
    let mut old_api = api.clone();
    old_api.container.image = "ghcr.io/getployz/api:0".into();
    let mut old_hook = container('8', '1', &old_api, &service_id('a'));
    old_hook
        .try_update(|parts| parts.kind = ContainerKind::PreDeployHook)
        .unwrap();
    // On first, 2 endpoints are free. api's reservation first credits its old hook, takes
    // one for a new hook and none for the Container it replaces, and worker takes one.
    // Blocked, api gives back only its hook: its old Container and hook keep running, so
    // one endpoint is left for web.
    let snapshot = DeploySnapshot {
        capacity: capacity([('1', 2), ('2', 0)]),
        containers: vec![container('a', '1', &old_api, &service_id('a')), old_hook],
        ..copies_snapshot(&[('2', CopyRole::Slot)], vec![])
    };
    for (replicas, fits) in [(1, true), (2, false)] {
        web.mode = ServiceMode::Replicated {
            replicas: NonZeroU32::new(replicas).unwrap(),
        };
        let intent = DeployIntent::apply_all(
            Namespace::parse("app").unwrap(),
            [&db, &api, &worker, &web],
            PlanOptions::default(),
        )
        .with_dependencies(BTreeMap::from([(
            api.name.clone(),
            vec![ServiceDependency {
                service: db.name.clone(),
                condition: DependencyCondition::ServiceHealthy,
            }],
        )]));

        let preview = runner_preview(&intent, &snapshot);

        if fits {
            let preview = preview.unwrap_or_else(|error| panic!("{replicas} replicas: {error}"));
            let blocked: Vec<_> = preview
                .blocked
                .iter()
                .map(|blocked| blocked.service.as_str())
                .collect();
            assert_eq!(blocked, ["db", "api"]);
            assert_eq!(
                preview.placed_on.get(&web.name),
                Some(&[machine_id('1')].into())
            );
        } else {
            assert!(
                matches!(
                    preview,
                    Err(PlanError::Service { ref service, ref source })
                        if service == "web" && matches!(**source, PlanError::InsufficientCapacity)
                ),
                "{replicas} replicas: {preview:?}"
            );
        }
    }
}

#[test]
fn a_blocked_service_taking_back_its_credited_hook_keeps_the_machine_in_debt() {
    let (mut web, db) = web_and_db();
    let mut api = spec("api");
    let mut worker = spec("worker");
    for spec in [&mut api, &mut worker] {
        add_named_volume(spec, "a_shared");
        make_provisioned(spec, "a_shared", 1_073_741_824);
        spec.placement.constraints = [label_constraint("first")].into();
    }
    let mut c = spec("c");
    let mut d = spec("d");
    for spec in [&mut c, &mut d, &mut web] {
        spec.placement.constraints = [label_constraint("second")].into();
    }
    for spec in [&mut c, &mut d] {
        add_named_volume(spec, "z_shared");
        make_provisioned(spec, "z_shared", 1_073_741_824);
    }
    api.pre_deploy = Some(PreDeployHook {
        command: vec!["migrate".into()].try_into().unwrap(),
        environment: Default::default(),
        privileged: None,
        timeout_millis: None,
        user: None,
    });
    let mut old_api = api.clone();
    old_api.container.image = "ghcr.io/getployz/api:0".into();
    let mut old_hook = container('8', '2', &old_api, &service_id('a'));
    old_hook
        .try_update(|parts| parts.kind = ContainerKind::PreDeployHook)
        .unwrap();
    // second has one free endpoint. api's reservation credits its old hook there, and c and
    // d spend both. Blocked, api takes its credit back, which leaves second one endpoint in
    // debt; blocked c's refund only pays that debt, so d's Container takes the last one.
    let snapshot = DeploySnapshot {
        capacity: capacity([('1', 3), ('2', 1)]),
        containers: vec![container('a', '1', &old_api, &service_id('a')), old_hook],
        ..copies_snapshot(&[('2', CopyRole::Slot)], vec![])
    };
    let dependency = vec![ServiceDependency {
        service: db.name.clone(),
        condition: DependencyCondition::ServiceHealthy,
    }];
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&db, &api, &worker, &c, &d, &web],
        PlanOptions::default(),
    )
    .with_dependencies(BTreeMap::from([
        (api.name.clone(), dependency.clone()),
        (c.name.clone(), dependency),
    ]));

    let preview = runner_preview(&intent, &snapshot);

    assert!(
        matches!(
            preview,
            Err(PlanError::Service { ref service, ref source })
                if service == "web" && matches!(**source, PlanError::InsufficientCapacity)
        ),
        "{preview:?}"
    );
}

#[test]
fn blocked_services_are_reported_in_deploy_order() {
    let (_, db) = web_and_db();
    let mut api = spec("api");
    let mut worker = spec("worker");
    for spec in [&mut api, &mut worker] {
        add_named_volume(spec, "shared");
        make_provisioned(spec, "shared", 1_073_741_824);
    }
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&db, &api, &worker],
        PlanOptions::default(),
    );
    let mut snapshot = copies_snapshot(&[('2', CopyRole::Slot)], vec![]);
    let Some(Ok(second)) = snapshot.storage_capacity.get_mut(&machine_id('2')) else {
        panic!("fixture capacity for second")
    };
    second.copies.insert(
        app_volume("shared"),
        ployz_core::ProvisionedCopy {
            role: CopyRole::Slot,
            maximum_bytes: maximum_bytes(1_073_741_824),
            used_bytes: 0,
            slot_role: None,
        },
    );

    let preview = runner_preview(&intent, &snapshot).unwrap();

    let blocked: Vec<_> = preview
        .blocked
        .iter()
        .map(|blocked| blocked.service.as_str())
        .collect();
    assert_eq!(blocked, ["db", "api", "worker"]);
}

#[test]
fn a_shared_volume_without_a_writer_blocks_only_the_services_that_share_it() {
    let other = spec("other");
    let mut api = spec("api");
    let mut worker = spec("worker");
    for spec in [&mut api, &mut worker] {
        add_named_volume(spec, "data");
        make_provisioned(spec, "data", 1_073_741_824);
    }
    let web = spec("web");
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&other, &api, &worker, &web],
        PlanOptions::default(),
    )
    .with_dependencies(BTreeMap::from([(
        web.name.clone(),
        vec![ServiceDependency {
            service: api.name.clone(),
            condition: DependencyCondition::ServiceHealthy,
        }],
    )]));
    let mut silent = copies_snapshot(&[('2', CopyRole::Slot)], vec![]);
    silent.machines.push(machine('3', "third"));
    silent.storage_capacity.remove(&machine_id('3'));
    for (case, snapshot, reason, code) in [
        (
            "no writer",
            copies_snapshot(&[('2', CopyRole::Slot)], vec![]),
            "Volume data has no writer; second holds its only copy",
            RpcErrorCode::Conflict,
        ),
        (
            "copies unknown",
            silent,
            "third did not answer, so Volume data's copies are unknown",
            RpcErrorCode::Unavailable,
        ),
    ] {
        let preview = runner_preview(&intent, &snapshot).unwrap();

        let planned: Vec<_> = operations(&preview)
            .iter()
            .filter_map(|operation| operation.service_name().cloned())
            .collect();
        assert_eq!(planned, std::slice::from_ref(&other.name), "{case}");
        let [api, worker, web] = preview.blocked.as_slice() else {
            panic!("{case}: {:?}", preview.blocked)
        };
        for sharer in [api, worker] {
            assert_eq!(sharer.error.code, code, "{case}");
            assert!(
                sharer.error.message.starts_with(reason),
                "{case}: {}",
                sharer.error.message
            );
        }
        assert_eq!(
            (api.service.as_str(), worker.service.as_str()),
            ("api", "worker"),
            "{case}"
        );
        assert_eq!(web.service.as_str(), "web", "{case}");
        assert_eq!(web.error.code, RpcErrorCode::Conflict, "{case}");
        assert_eq!(
            web.error.message, "depends on api, which was not attempted",
            "{case}"
        );
        assert!(preview.volumes_to_create.is_empty(), "{case}");
        assert!(
            preview_deploy(&intent, &snapshot).is_err(),
            "{case}: a direct preview still refuses the whole Deploy"
        );
    }
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
    unchanged.placement.constraints = [label_constraint("stateless")].into();
    add_named_volume(&mut unchanged, "data");
    make_provisioned(&mut unchanged, "data", 1_073_741_824);
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
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
            ..storage_snapshot()
        },
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
    service.placement.constraints = [label_constraint(machine_name)].into();
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
    let mut volumes = requested.volume_graph().volumes().to_vec();
    let mut mounts = requested.volume_graph().mounts().to_vec();
    let mut alias = ServiceVolume {
        reference: ServiceVolumeReference::parse("data-alias").unwrap(),
        source: volumes.first().unwrap().source.clone(),
    };
    let mut raw = alias.source.kind().clone();
    let ployz_core::RawVolumeSource::Provisioned {
        maximum_bytes: alias_maximum,
        ..
    } = &mut raw
    else {
        unreachable!("data fixture is provisioned")
    };
    *alias_maximum = maximum_bytes(2_147_483_648);
    alias.source = raw.admit().unwrap();
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
        Namespace::parse("app").unwrap(),
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
            ..storage_snapshot()
        },
    )
    .unwrap();
}

#[test]
fn partial_apply_rejects_different_bounds_for_colocated_global_volumes() {
    let first = global_service("first", "first", 1_073_741_824);
    let second = global_service("second", "first", 2_147_483_648);
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
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
            ..storage_snapshot()
        },
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
fn a_volume_definition_conflict_placement_decides_keeps_its_retry() {
    let mut first = global_service("first", "first", 1_073_741_824);
    first.placement.constraints =
        [ployz_core::PlacementConstraint::parse("node.labels.a == yes").unwrap()].into();
    let mut second = global_service("second", "first", 2_147_483_648);
    second.placement.constraints =
        [ployz_core::PlacementConstraint::parse("node.labels.b == yes").unwrap()].into();
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&first, &second],
        PlanOptions::default(),
    );
    let labelled = |hex, name, labels: &[&str]| {
        let mut machine = machine(hex, name);
        machine.machine.labels = labels
            .iter()
            .map(|label| (label.parse().unwrap(), "yes".parse().unwrap()))
            .collect();
        machine.storage = Some(MachineStorageObservation::Ready);
        machine
    };

    let refused = preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![
                labelled('1', "both", &["a", "b"]),
                labelled('2', "second", &["b"]),
            ],
            ..storage_snapshot()
        },
    )
    .unwrap_err();
    assert!(
        matches!(
            &refused,
            PlanError::Service { source, .. }
                if matches!(source.as_ref(), PlanError::ConflictingDockerVolumeDefinitions { .. })
        ),
        "{refused:?}"
    );
    assert!(!refused.same_again(), "{refused}");

    preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![
                labelled('1', "both", &["a"]),
                labelled('2', "second", &["b"]),
            ],
            ..storage_snapshot()
        },
    )
    .expect("a label taken off the Server lets the identical Deploy through");
}

#[test]
fn colocated_global_services_reject_conflicting_provisioned_labels() {
    let first = global_service("first", "first", 1_073_741_824);
    let mut second = global_service("second", "first", 1_073_741_824);
    let mut volumes = second.volume_graph().volumes().to_vec();
    let mounts = second.volume_graph().mounts().to_vec();
    let mut raw = volumes.first().unwrap().source.kind().clone();
    let ployz_core::RawVolumeSource::Provisioned { labels, .. } = &mut raw else {
        unreachable!("global_service adds a Provisioned Volume")
    };
    labels.insert("backup".into(), "daily".into());
    volumes.first_mut().unwrap().source = raw.admit().unwrap();
    second
        .set_volume_graph(ployz_core::ServiceVolumeGraph::parse(volumes, mounts).unwrap())
        .unwrap();
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&first, &second],
        PlanOptions::default(),
    );

    let result = preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            ..storage_snapshot()
        },
    );

    assert!(matches!(
        result,
        Err(PlanError::Service { source, .. })
            if matches!(source.as_ref(), PlanError::ConflictingDockerVolumeDefinitions { .. })
    ));
}

#[test]
fn preview_distinguishes_provisioned_and_ordinary_volume_creates() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "data");
    make_provisioned(&mut requested, "data", 1_073_741_824);
    add_named_volume(&mut requested, "cache");
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&requested],
        PlanOptions::default(),
    );

    let mut ready = machine('1', "first");
    ready.storage = Some(MachineStorageObservation::Ready);
    let preview = preview_deploy(
        &intent,
        &DeploySnapshot {
            machines: vec![ready],
            ..storage_snapshot()
        },
    )
    .unwrap();

    assert!(matches!(
        operations(&preview).as_slice(),
        [
            DeployOperation::PrepareVolumes { .. },
            DeployOperation::RunContainer { .. }
        ]
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
        Namespace::parse("app").unwrap(),
        vec![requested.clone(), requested],
        PlanOptions::default(),
    );
    assert_eq!(
        preview_deploy(&intent, &DeploySnapshot::default()),
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
            ..storage_snapshot()
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
            Namespace::parse("app").unwrap(),
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
            ..storage_snapshot()
        },
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
            ..storage_snapshot()
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

// These fixtures exercise volume identity and eligibility with ample physical capacity.
fn storage_snapshot() -> DeploySnapshot {
    DeploySnapshot {
        storage_capacity: "0123456789abcdef"
            .chars()
            .map(|id| {
                let machine = machine(id, "fixture");
                (
                    machine.machine.id,
                    Ok(ployz_core::StorageCapacity {
                        backing: ployz_core::StorageBacking::Fixed {
                            pool_size_bytes: 1024 * ployz_core::STORAGE_GIB,
                        },
                        unmanaged_used_bytes: 0,
                        slot_bound_bytes: 0,
                        volumes: BTreeMap::new(),
                        copies: BTreeMap::new(),
                    }),
                )
            })
            .collect(),
        ..Default::default()
    }
}

/// A full Deploy whose `db` mounts a Volume on a Server that didn't return its storage
/// capacity blocks only `db`: `web` is still planned, and `db` is reported not
/// attempted. That holds whether the Volume's copies are known or not, and however
/// the Server went silent.
#[test]
fn a_silent_volume_server_blocks_only_the_service_that_mounts_its_volume() {
    let mut db = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    db.name = ServiceName::parse("db").unwrap();
    add_named_volume(&mut db, "data");
    make_provisioned(&mut db, "data", 1_073_741_824);
    let web = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&web, &db],
        PlanOptions::default(),
    )
    .with_deployed_volumes([app_volume("data")].into());
    let timed_out = RpcError {
        code: RpcErrorCode::Unavailable,
        message: "target Machine RPC timed out".into(),
        details: Default::default(),
        cause: Vec::new(),
    };
    for (case, copies, silent, down) in [
        ("copies unknown, not asked", vec![], None, false),
        (
            "writer known, not asked",
            vec![('2', CopyRole::Writer)],
            None,
            false,
        ),
        (
            "writer known, timed out",
            vec![('2', CopyRole::Writer)],
            Some(timed_out.clone()),
            false,
        ),
        (
            "writer known, Server down",
            vec![('2', CopyRole::Writer)],
            None,
            true,
        ),
    ] {
        let rows = if copies.is_empty() {
            vec![]
        } else {
            vec![provisioned_row('2')]
        };
        let mut snapshot = copies_snapshot(&copies, rows);
        match silent {
            None => snapshot.storage_capacity.remove(&machine_id('2')),
            Some(error) => snapshot
                .storage_capacity
                .insert(machine_id('2'), Err(error)),
        };
        if down {
            for machine in &mut snapshot.machines {
                if machine.machine.id == machine_id('2') {
                    machine.membership = MembershipObservation::Down;
                }
            }
        }

        // A direct preview still refuses as a whole.
        assert!(preview_deploy(&intent, &snapshot).is_err(), "{case}");
        let preview =
            runner_preview(&intent, &snapshot).unwrap_or_else(|error| panic!("{case}: {error:?}"));
        let [blocked] = preview.blocked.as_slice() else {
            panic!("{case}: {:?}", preview.blocked)
        };
        assert_eq!(blocked.service, db.name, "{case}");
        assert!(
            operations(&preview)
                .iter()
                .any(|operation| operation.service_name() == Some(&web.name)),
            "{case}: {:?}",
            operations(&preview)
        );
        assert!(
            operations(&preview)
                .iter()
                .all(|operation| operation.service_name() != Some(&db.name)),
            "{case}"
        );
    }
}

/// A Volume that sits on an answering Machine it can't use there still refuses the
/// whole Deploy under the runner: only a Volume on a down or silent Server waits.
#[test]
fn a_volume_on_an_answering_server_it_cannot_use_still_refuses_under_the_runner() {
    let intent = automatic_provisioned_intent();
    let mut pinned = machine('1', "pinned");
    pinned.storage = Some(MachineStorageObservation::Stateless);
    let mut other = machine('2', "other");
    other.storage = Some(MachineStorageObservation::Ready);
    let mut existing = provisioned_row('1');
    existing.options = BTreeMap::from([("size".into(), "1073741824b".into())]);
    let snapshot = DeploySnapshot {
        machines: vec![pinned, other],
        volume_snapshot: VolumeSnapshot::try_from_observations(vec![existing])
            .expect("valid Volume Snapshot fixture"),
        ..storage_snapshot()
    };

    let preview = runner_preview(&intent, &snapshot);
    assert!(preview.is_err(), "{:?}", preview.map(|plan| plan.blocked));
}

/// A Service blocked because its Volume's Server is silent gives back what its
/// placement took, so a later Service can use the same host port there.
#[test]
fn a_service_blocked_on_a_silent_volume_server_gives_back_its_host_port() {
    let mut db = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    db.name = ServiceName::parse("db").unwrap();
    add_named_volume(&mut db, "data");
    make_provisioned(&mut db, "data", 1_073_741_824);
    db.ports.push(host_port(8080));
    let mut web = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    web.ports.push(host_port(8080));
    web.placement.constraints =
        [PlacementConstraint::parse(format!("node.id == {}", machine_id('2'))).unwrap()].into();
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&db, &web],
        PlanOptions::default(),
    )
    .with_deployed_volumes([app_volume("data")].into());
    let mut snapshot = copies_snapshot(&[('2', CopyRole::Writer)], vec![provisioned_row('2')]);
    snapshot.storage_capacity.remove(&machine_id('2'));

    let preview = runner_preview(&intent, &snapshot).unwrap_or_else(|error| panic!("{error:?}"));
    let [blocked] = preview.blocked.as_slice() else {
        panic!("{:?}", preview.blocked)
    };
    assert_eq!(blocked.service, db.name);
    assert!(
        operations(&preview)
            .iter()
            .any(|operation| operation.service_name() == Some(&web.name)
                && operation.machine_id() == machine_id('2')),
        "{:?}",
        operations(&preview)
    );
}

/// Services sharing a Volume on a down Server wait together, and the rest of the
/// Deploy goes ahead.
#[test]
fn services_sharing_a_volume_on_a_down_server_block_only_themselves() {
    let mut db = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    db.name = ServiceName::parse("db").unwrap();
    add_named_volume(&mut db, "data");
    make_provisioned(&mut db, "data", 1_073_741_824);
    let mut worker = db.clone();
    worker.name = ServiceName::parse("worker").unwrap();
    let web = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&db, &worker, &web],
        PlanOptions::default(),
    )
    .with_deployed_volumes([app_volume("data")].into());
    let mut snapshot = copies_snapshot(&[('2', CopyRole::Writer)], vec![provisioned_row('2')]);
    for machine in &mut snapshot.machines {
        if machine.machine.id == machine_id('2') {
            machine.membership = MembershipObservation::Down;
        }
    }

    let preview = runner_preview(&intent, &snapshot).unwrap_or_else(|error| panic!("{error:?}"));
    let blocked = preview
        .blocked
        .iter()
        .map(|blocked| blocked.service.clone())
        .collect::<BTreeSet<_>>();
    assert_eq!(
        blocked,
        BTreeSet::from([db.name.clone(), worker.name.clone()])
    );
    assert!(
        operations(&preview)
            .iter()
            .any(|operation| operation.service_name() == Some(&web.name)),
        "{:?}",
        operations(&preview)
    );
}

/// Plain Volumes split across two answering Servers still refuse the whole Deploy under
/// the runner, though no Server was asked for its storage: a plain Volume needs none.
#[test]
fn plain_volumes_on_answering_servers_still_refuse_under_the_runner() {
    let mut db = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    db.name = ServiceName::parse("db").unwrap();
    add_named_volume(&mut db, "a");
    add_named_volume(&mut db, "b");
    let web = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let intent = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&db, &web],
        PlanOptions::default(),
    );
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), machine('2', "second")],
        volume_snapshot: VolumeSnapshot::try_from_observations(vec![
            observed_volume(machine_id('1'), "a"),
            observed_volume(machine_id('2'), "b"),
        ])
        .expect("valid Volume Snapshot fixture"),
        ..Default::default()
    };

    assert!(preview_deploy(&intent, &snapshot).is_err());
    let preview = runner_preview(&intent, &snapshot);
    assert!(preview.is_err(), "{:?}", preview.map(|plan| plan.blocked));
}

/// A Global Service with no replica on the unread Server yet touches nothing there, so it
/// starts on the Servers that answered.
#[test]
fn a_new_global_service_starts_beside_an_unread_server() {
    let mut service = requested(ServiceMode::Global);
    add_named_volume(&mut service, "data");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "machine-1"), machine('3', "machine-3")],
        volume_snapshot: VolumeSnapshot::try_from_parts(
            vec![observed_volume(machine_id('1'), "data")],
            Vec::new(),
            vec![listing_failure('3')],
            Vec::new(),
        )
        .unwrap(),
        ..storage_snapshot()
    };

    let (preview, blocked) = blocked_beside_web(&[&service], &snapshot);

    assert!(blocked.is_empty(), "{blocked:?}");
    assert!(
        operations(&preview).iter().any(|operation| matches!(
            operation,
            DeployOperation::RunContainer { spec, machine_id: run, .. }
                if spec.name.as_str() == "api" && *run == machine_id('1')
        )),
        "{:?}",
        operations(&preview)
    );
}

#[test]
fn a_global_service_with_an_unread_server_keeps_its_replicas() {
    let mut old = requested(ServiceMode::Global);
    add_named_volume(&mut old, "data");
    let mut new = old.clone();
    new.container.image = "ghcr.io/getployz/api:2".into();
    let inspect_failure = VolumeObservationFailure {
        id: DockerVolumeId {
            machine_id: machine_id('3'),
            name: app_volume("data"),
        },
        error: RpcError {
            code: RpcErrorCode::Unavailable,
            message: "volume detail failed".into(),
            details: Default::default(),
            cause: Vec::new(),
        },
    };
    for (named, machines) in [
        (Vec::new(), vec![listing_failure('3')]),
        (vec![inspect_failure], Vec::new()),
    ] {
        let snapshot = DeploySnapshot {
            machines: vec![machine('1', "machine-1"), machine('3', "machine-3")],
            containers: vec![
                container('b', '1', &old, &service_id('a')),
                container('c', '3', &old, &service_id('a')),
            ],
            volume_snapshot: VolumeSnapshot::try_from_parts(
                vec![observed_volume(machine_id('1'), "data")],
                named,
                machines,
                Vec::new(),
            )
            .unwrap(),
            ..storage_snapshot()
        };

        let (preview, blocked) = blocked_beside_web(&[&new], &snapshot);

        assert_eq!(
            blocked,
            [(
                "api".to_owned(),
                "machine-3 did not answer, so whether it holds Docker volume data is unknown. \
                 Bring machine-3 back, or deploy the other services by name"
                    .to_owned()
            )]
        );
        assert!(
            !operations(&preview).iter().any(|operation| matches!(
                operation,
                DeployOperation::RemoveContainer { container_id: removed, .. }
                    if *removed == container_id('c')
            )),
            "{:?}",
            operations(&preview)
        );
        assert_only_web_runs(&preview);
    }
}
