use super::support::*;
#[test]
fn pre_deploy_hook_stops_active_predecessors_and_runs_before_replacement() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    requested.pre_deploy = Some(PreDeployHook {
        command: vec!["db".into(), "migrate".into()],
        environment: Default::default(),
        privileged: None,
        timeout_millis: None,
        user: None,
    });
    let mut current = requested.clone();
    current.container.image = "ghcr.io/getployz/api:old".into();
    let current_service_id = service_id('a');
    let mut running_hook = container('c', '1', &current, &current_service_id);
    running_hook.kind = ContainerKind::PreDeployHook;
    let mut stopped_hook = container('d', '1', &current, &current_service_id);
    stopped_hook.kind = ContainerKind::PreDeployHook;
    stopped_hook.runtime = ContainerRuntimeObservation::Exited { code: 0 };
    let mut paused_hook = container('e', '1', &current, &current_service_id);
    paused_hook.kind = ContainerKind::PreDeployHook;
    paused_hook.runtime = ContainerRuntimeObservation::Paused;
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![
            container('b', '1', &current, &current_service_id),
            running_hook,
            stopped_hook,
            paused_hook,
        ],
        ..Default::default()
    };

    let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();

    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::StopHook { container_id: running, .. },
            DeployOperation::StopHook { container_id: paused, .. },
            DeployOperation::RunHook { old_hook_containers, .. },
            DeployOperation::ReplaceContainer(..),
        ] if running == &container_id('c')
            && paused == &container_id('e')
            && old_hook_containers == &vec![
                (machine_id('1'), container_id('c')),
                (machine_id('1'), container_id('d')),
                (machine_id('1'), container_id('e')),
            ]
    ));
}

#[test]
fn replacement_requires_one_temporary_endpoint() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let mut current = requested.clone();
    current.container.image = "ghcr.io/getployz/api:old".into();
    requested.update.order = Some(UpdateOrder::StartFirst);
    let snapshot = |free| DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![container('b', '1', &current, &service_id('a'))],
        capacity: capacity([('1', free)]),
        ..Default::default()
    };

    assert_eq!(
        plan_deploy([&requested], &snapshot(0), PlanOptions::default()),
        Err(PlanError::InsufficientCapacity)
    );
    assert!(plan_deploy([&requested], &snapshot(1), PlanOptions::default()).is_ok());
}

#[test]
fn hook_and_replacement_each_require_a_spare_endpoint() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    requested.pre_deploy = Some(PreDeployHook {
        command: vec!["db".into(), "migrate".into()],
        environment: Default::default(),
        privileged: None,
        timeout_millis: None,
        user: None,
    });
    let mut current = requested.clone();
    current.container.image = "ghcr.io/getployz/api:old".into();
    let snapshot = |free| DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![container('b', '1', &current, &service_id('a'))],
        capacity: capacity([('1', free)]),
        ..Default::default()
    };

    assert_eq!(
        plan_deploy([&requested], &snapshot(1), PlanOptions::default()),
        Err(PlanError::InsufficientCapacity)
    );
    let plan = plan_deploy([&requested], &snapshot(2), PlanOptions::default()).unwrap();
    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::RunHook { .. },
            DeployOperation::ReplaceContainer(_)
        ]
    ));
}

#[test]
fn replaced_hooks_credit_all_reclaimed_endpoints() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    requested.pre_deploy = Some(PreDeployHook {
        command: vec!["db".into(), "migrate".into()],
        environment: Default::default(),
        privileged: None,
        timeout_millis: None,
        user: None,
    });
    let mut current = requested.clone();
    current.container.image = "ghcr.io/getployz/api:old".into();
    let current_service_id = service_id('a');
    let mut old_hook = container('c', '1', &current, &current_service_id);
    old_hook.kind = ContainerKind::PreDeployHook;
    let mut other_old_hook = container('d', '1', &current, &current_service_id);
    other_old_hook.kind = ContainerKind::PreDeployHook;
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![
                container('b', '1', &current, &current_service_id),
                old_hook,
                other_old_hook,
            ],
            capacity: capacity([('1', 0)]),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::StopHook { .. },
            DeployOperation::StopHook { .. },
            DeployOperation::RunHook { .. },
            DeployOperation::ReplaceContainer(_)
        ]
    ));
}

#[test]
fn hook_capacity_is_charged_only_to_its_selected_machine() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    });
    requested.pre_deploy = Some(PreDeployHook {
        command: vec!["db".into(), "migrate".into()],
        environment: Default::default(),
        privileged: None,
        timeout_millis: None,
        user: None,
    });
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first"), machine('2', "second")],
            capacity: capacity([('1', 2), ('2', 1)]),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::RunHook { machine_id: hook, .. },
            DeployOperation::RunContainer { machine_id: first, .. },
            DeployOperation::RunContainer { machine_id: second, .. }
        ] if hook == first && first != second
    ));
}

#[test]
fn global_hook_uses_a_changed_machine_with_an_extra_slot() {
    let mut requested = requested(ServiceMode::Global);
    requested.pre_deploy = Some(PreDeployHook {
        command: vec!["db".into(), "migrate".into()],
        environment: Default::default(),
        privileged: None,
        timeout_millis: None,
        user: None,
    });
    let machines = vec![machine('1', "first"), machine('2', "second")];
    let baseline = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: machines.clone(),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();
    let ordered = operations(&baseline)
        .into_iter()
        .filter_map(|operation| match operation {
            DeployOperation::RunContainer { machine_id, .. } => Some(machine_id),
            DeployOperation::CreateVolume { .. }
            | DeployOperation::CreateProvisionedVolume { .. }
            | DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::ReplaceContainer(_)
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::RemoveVolume { .. } => None,
        })
        .collect::<Vec<_>>();
    let [first, hook] = ordered.as_slice() else {
        panic!("Global baseline should create exactly two Containers")
    };
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines,
            capacity: Some(BTreeMap::from([
                (*first, BridgeEndpointCapacity::new(1, 0)),
                (*hook, BridgeEndpointCapacity::new(2, 0)),
            ])),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        operations(&plan).first(),
        Some(DeployOperation::RunHook { machine_id, .. }) if machine_id == hook
    ));
}

#[test]
fn planning_does_not_count_hook_containers_toward_replicated_count() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let current_service_id = service_id('a');
    let mut hook = container('c', '1', &requested, &current_service_id);
    hook.kind = ContainerKind::PreDeployHook;
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![container('b', '1', &requested, &current_service_id), hook],
        ..Default::default()
    };

    let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();

    assert!(plan.operations.is_empty());
}

#[test]
fn two_projects_can_each_own_the_same_service_name() {
    let requested = requested(ServiceMode::Global);
    let mut other = container('c', '1', &requested, &service_id('d'));
    other.project_name = ProjectName::parse("shop-prod").unwrap();
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container('b', '1', &requested, &service_id('a')), other],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(plan.operations.is_empty());
}

#[test]
fn unmatched_placement_returns_no_eligible_machines() {
    let mut requested = requested(ServiceMode::Global);
    requested.placement.machines = vec![MachineTarget::parse("missing").unwrap()];

    assert_no_eligible(
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "first")],
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        &[EliminatingConstraint::UnknownPlacement {
            targets: vec![MachineTarget::parse("missing").unwrap()],
        }],
        &["x-machines 'missing' matched no Machine"],
    );
}

#[test]
fn global_missing_volume_is_created_on_every_eligible_machine_before_containers() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "data");
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first"), machine('2', "second")],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::CreateVolume { machine_id: first_volume, .. },
            DeployOperation::CreateVolume { machine_id: second_volume, .. },
            DeployOperation::RunContainer { machine_id: first_container, .. },
            DeployOperation::RunContainer { machine_id: second_container, .. },
        ] if first_volume == &machine_id('1')
            && second_volume == &machine_id('2')
            && first_container == first_volume
            && second_container == second_volume
    ));
}

#[test]
fn force_recreate_replaces_an_otherwise_matching_container() {
    let requested = requested(ServiceMode::Global);
    let current_service_id = service_id('a');
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container('b', '1', &requested, &current_service_id)],
            ..Default::default()
        },
        PlanOptions {
            force_recreate: true,
            skip_health_monitor: false,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(matches!(
        operations(&plan).as_slice(),
        [DeployOperation::ReplaceContainer(ReplacementOperation { old_container_id, .. })]
            if old_container_id == &container_id('b')
    ));
}

#[test]
fn no_op_plan_does_not_run_a_pre_deploy_hook() {
    let mut requested = requested(ServiceMode::Global);
    requested.pre_deploy = Some(PreDeployHook {
        command: vec!["db".into(), "migrate".into()],
        environment: Default::default(),
        privileged: None,
        timeout_millis: None,
        user: None,
    });
    let current_service_id = service_id('a');
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container('b', '1', &requested, &current_service_id)],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(plan.operations.is_empty());
}

#[test]
fn existing_service_mode_cannot_change() {
    let current = requested(ServiceMode::Global);
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let current_service_id = service_id('a');

    assert_eq!(
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "first")],
                containers: vec![container('b', '1', &current, &current_service_id)],
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        Err(PlanError::ServiceModeCannotChange)
    );
}

#[test]
fn incompatible_volume_excludes_only_its_machine() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut requested, "data");
    let mut volumes = requested.volume_graph.volumes().to_vec();
    let mounts = requested.volume_graph.mounts().to_vec();
    let VolumeSource::Named { driver, .. } = &mut volumes.first_mut().unwrap().source else {
        unreachable!();
    };
    *driver = Some(ployz_core::VolumeDriver {
        name: "nfs".into(),
        options: Default::default(),
    });
    requested.volume_graph = ployz_core::ServiceVolumeGraph::parse(volumes, mounts).unwrap();

    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first"), machine('2', "second")],
            volumes: vec![observed_volume(machine_id('1'), "data")],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();
    assert!(
        matches!(
            operations(&plan).as_slice(),
            [
                DeployOperation::CreateVolume { machine_id: volume_machine, .. },
                DeployOperation::RunContainer { machine_id: container_machine, .. },
            ] if volume_machine == &machine_id('2') && container_machine == &machine_id('2')
        ),
        "unexpected operations: {:?}",
        plan.operations
    );
}

#[test]
fn multi_replica_named_volume_replacement_never_overlaps_requested_replicas() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(3).unwrap(),
    });
    add_named_volume(&mut requested, "data");
    let mut current = requested.clone();
    current.container.image = "ghcr.io/getployz/api:old".into();
    let current_service_id = service_id('a');
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![
                container('b', '1', &current, &current_service_id),
                container('c', '1', &current, &current_service_id),
                container('d', '1', &current, &current_service_id),
            ],
            volumes: vec![observed_volume(machine_id('1'), "data")],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(plan.operations.len(), 3);
    assert!(plan.operations.iter().all(|row| matches!(
        &row.operation,
        DeployOperation::ReplaceContainer(ReplacementOperation { spec, .. })
            if spec.update.order == UpdateOrder::StopFirst
    )));
}

#[test]
fn global_replacement_stops_other_containers_with_conflicting_host_ports_first() {
    let mut requested = requested(ServiceMode::Global);
    requested.ports = vec![host_port(8080), host_port(9090)];
    let mut first_old = requested.clone();
    first_old.container.image = "ghcr.io/getployz/api:old".into();
    first_old.ports = vec![host_port(8080)];
    let mut second_old = first_old.clone();
    second_old.ports = vec![host_port(9090)];
    let current_service_id = service_id('a');

    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![
                container('b', '1', &first_old, &current_service_id),
                container('c', '1', &second_old, &current_service_id),
            ],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::StopContainer { container_id: stopped, .. },
            DeployOperation::ReplaceContainer(ReplacementOperation {
                old_container_id: replaced,
                ..
            }),
            DeployOperation::RemoveContainer { container_id: removed, .. },
        ] if stopped == &container_id('c')
            && replaced == &container_id('b')
            && removed == &container_id('c')
    ));
}
