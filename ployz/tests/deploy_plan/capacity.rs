use super::support::*;

#[test]
fn capacity_filters_a_full_machine_without_rescoring_the_rest() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "full"), machine('2', "free")],
            capacity: capacity([('1', 0), ('2', 1)]),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        operations(&plan).as_slice(),
        [DeployOperation::RunContainer { machine_id: actual, .. }] if actual == &machine_id('2')
    ));
}

#[test]
fn capacity_filters_before_new_volume_placement() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut requested, "data");
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "full"), machine('2', "free")],
            capacity: capacity([('1', 0), ('2', 1)]),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::CreateVolume { machine_id: volume, .. },
            DeployOperation::RunContainer { machine_id: container, .. }
        ] if volume == &machine_id('2') && container == volume
    ));
}

#[test]
fn unknown_machine_excluded_by_volume_does_not_make_capacity_unknown() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    requested.name = ServiceName::parse("api").unwrap();
    add_named_volume(&mut requested, "data");

    assert_eq!(
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "full-volume-host"), machine('2', "unknown")],
                volume_snapshot: VolumeSnapshot::try_from_observations(vec![observed_volume(
                    machine_id('1'),
                    "data"
                )])
                .expect("valid Volume Snapshot fixture"),
                capacity: capacity([('1', 0)]),
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        Err(PlanError::InsufficientCapacity)
    );
}

#[test]
fn capacity_distinguishes_sufficient_known_unknown_and_insufficient() {
    let one = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let two = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    });
    let machines = vec![machine('1', "known"), machine('2', "unknown")];

    assert!(
        plan_deploy(
            [&one],
            &DeploySnapshot {
                machines: machines.clone(),
                capacity: capacity([('1', 1)]),
                ..Default::default()
            },
            PlanOptions::default(),
        )
        .is_ok()
    );
    assert_eq!(
        plan_deploy(
            [&two],
            &DeploySnapshot {
                machines: machines.clone(),
                capacity: capacity([('1', 1)]),
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        Err(PlanError::CapacityUnknown)
    );
    assert_eq!(
        plan_deploy(
            [&one],
            &DeploySnapshot {
                machines,
                capacity: capacity([('1', 0), ('2', 0)]),
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        Err(PlanError::InsufficientCapacity)
    );
}

#[test]
fn apply_one_run_and_scale_path_rejects_full_capacity() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "full")],
        capacity: capacity([('1', 0)]),
        ..Default::default()
    };
    let intent = DeployIntent::apply_one(
        ProjectName::parse("app").unwrap(),
        requested,
        PlanOptions::default(),
    );

    assert_eq!(
        preview_deploy(&intent, &snapshot, IngressContext::default()),
        Err(PlanError::InsufficientCapacity)
    );
}

#[test]
fn scale_down_releases_capacity_before_a_later_service_creates() {
    let scaled = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let mut later = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    later.name = ServiceName::parse("later").unwrap();
    let scaled_id = service_id('a');
    let plan = plan_deploy(
        [&scaled, &later],
        &DeploySnapshot {
            machines: vec![machine('1', "full")],
            containers: vec![
                container('b', '1', &scaled, &scaled_id),
                container('c', '1', &scaled, &scaled_id),
            ],
            capacity: capacity([('1', 0)]),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .expect("the scale-down removal frees one endpoint for the later Service");

    assert!(matches!(
        operations(&plan).as_slice(),
        [DeployOperation::RemoveContainer { machine_id: removed_from, .. },
         DeployOperation::RunContainer { machine_id: created_on, spec, .. }]
            if removed_from == &machine_id('1')
                && created_on == &machine_id('1')
                && spec.name == ServiceName::parse("later").unwrap()
    ));
}

#[test]
fn all_unknown_global_capacity_is_reported_as_unknown() {
    let requested = requested(ServiceMode::Global);
    assert_eq!(
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "unknown")],
                capacity: capacity([]),
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        Err(PlanError::CapacityUnknown)
    );
}

#[test]
fn global_missing_slot_distinguishes_unknown_and_full_capacity() {
    let requested = requested(ServiceMode::Global);
    let current_service_id = service_id('a');
    let snapshot = |capacity| DeploySnapshot {
        machines: vec![machine('1', "current"), machine('2', "missing")],
        containers: vec![container('b', '1', &requested, &current_service_id)],
        capacity,
        ..Default::default()
    };

    assert_eq!(
        plan_deploy(
            [&requested],
            &snapshot(capacity([('1', 0)])),
            PlanOptions::default(),
        ),
        Err(PlanError::CapacityUnknown)
    );
    assert_eq!(
        plan_deploy(
            [&requested],
            &snapshot(capacity([('1', 0), ('2', 0)])),
            PlanOptions::default(),
        ),
        Err(PlanError::InsufficientCapacity)
    );
}

#[test]
fn unchanged_unknown_global_is_irrelevant_to_a_full_missing_slot() {
    let requested = requested(ServiceMode::Global);
    let current_service_id = service_id('a');
    assert_eq!(
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![
                    machine('1', "unchanged-unknown"),
                    machine('2', "missing-full")
                ],
                containers: vec![container('b', '1', &requested, &current_service_id)],
                capacity: capacity([('2', 0)]),
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        Err(PlanError::InsufficientCapacity)
    );
}

#[test]
fn huge_replica_request_is_rejected_before_planning_operations() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(u32::MAX).unwrap(),
    });
    assert_eq!(
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "first")],
                capacity: capacity([('1', u64::from(u32::MAX) - 1)]),
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        Err(PlanError::InsufficientCapacity)
    );
}

#[test]
fn huge_replica_request_with_an_unknown_existing_host_fails_preflight() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(u32::MAX).unwrap(),
    });
    let current_service_id = service_id('a');
    assert_eq!(
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "unknown"), machine('2', "known")],
                containers: vec![container('a', '1', &requested, &current_service_id)],
                capacity: capacity([('2', u64::from(u32::MAX) - 2)]),
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        Err(PlanError::CapacityUnknown)
    );
}

#[test]
fn huge_replica_request_preflights_the_persistent_hook_endpoint() {
    let replicas = u32::MAX - 1;
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(replicas).unwrap(),
    });
    requested.pre_deploy = Some(PreDeployHook {
        command: vec!["migrate".into()],
        environment: Default::default(),
        privileged: None,
        timeout_millis: None,
        user: None,
    });
    assert_eq!(
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "first")],
                capacity: capacity([('1', u64::from(replicas))]),
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        Err(PlanError::InsufficientCapacity)
    );
}
