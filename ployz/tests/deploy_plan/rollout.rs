use super::support::*;
#[test]
fn pre_deploy_hook_stops_running_predecessors_and_runs_before_replacement() {
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
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![
            container('b', '1', &current, &current_service_id),
            running_hook,
            stopped_hook,
        ],
        ..Default::default()
    };

    let plan = plan_deploy(
        &requested,
        &snapshot,
        service_id('f'),
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        plan.operations(),
        [
            DeployOperation::StopHook { container_id: stopped, .. },
            DeployOperation::RunHook { old_hook_container_ids, .. },
            DeployOperation::ReplaceContainer(..),
        ] if stopped == &container_id('c')
            && old_hook_container_ids == &vec![container_id('c'), container_id('d')]
    ));
}

#[test]
fn sequence_failure_keeps_completed_failed_and_unexecuted_operations_exact() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(3).unwrap(),
    });
    let plan = plan_deploy(
        &requested,
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            ..Default::default()
        },
        service_id('a'),
        PlanOptions::default(),
    )
    .unwrap();

    let outcome = plan.failure_outcome(1, "container failed").unwrap();

    assert_eq!(outcome.completed, plan.operations().get(..1).unwrap());
    assert!(matches!(
        outcome.failed.as_ref(),
        Some(FailedOperation::Operation { operation, error })
            if operation == plan.operations().get(1).unwrap() && *error == "container failed"
    ));
    assert_eq!(outcome.unexecuted, plan.operations().get(2..).unwrap());
}

#[test]
fn duplicate_service_names_are_reported_without_selecting_a_winner() {
    let requested = requested(ServiceMode::Global);
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![
            container('b', '1', &requested, &service_id('a')),
            container('c', '1', &requested, &service_id('d')),
        ],
        ..Default::default()
    };

    assert_eq!(
        plan_deploy(
            &requested,
            &snapshot,
            service_id('f'),
            PlanOptions::default(),
        ),
        Err(PlanError::AmbiguousService {
            matches: vec![service_id('a'), service_id('d')],
        })
    );
}

#[test]
fn unmatched_placement_returns_no_eligible_machines() {
    let mut requested = requested(ServiceMode::Global);
    requested.placement.machines = vec![MachineSelector::parse("missing").unwrap()];

    assert_eq!(
        plan_deploy(
            &requested,
            &DeploySnapshot {
                machines: vec![machine('1', "first")],
                ..Default::default()
            },
            service_id('a'),
            PlanOptions::default(),
        ),
        Err(PlanError::NoEligibleMachines)
    );
}

#[test]
fn global_missing_volume_is_created_on_every_eligible_machine_before_containers() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "data");
    let plan = plan_deploy(
        &requested,
        &DeploySnapshot {
            machines: vec![machine('1', "first"), machine('2', "second")],
            ..Default::default()
        },
        service_id('a'),
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        plan.operations(),
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
        &requested,
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container('b', '1', &requested, &current_service_id)],
            ..Default::default()
        },
        service_id('f'),
        PlanOptions {
            force_recreate: true,
            skip_health_monitor: false,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(matches!(
        plan.operations(),
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
        &requested,
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container('b', '1', &requested, &current_service_id)],
            ..Default::default()
        },
        service_id('f'),
        PlanOptions::default(),
    )
    .unwrap();

    assert!(plan.operations().is_empty());
}

#[test]
fn replacement_failure_can_record_its_only_allowed_compensation() {
    let mut requested = requested(ServiceMode::Global);
    requested.update.order = Some(UpdateOrder::StopFirst);
    let mut current = requested.clone();
    current.container.image = "ghcr.io/getployz/api:old".into();
    let current_service_id = service_id('a');
    let plan = plan_deploy(
        &requested,
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container('b', '1', &current, &current_service_id)],
            ..Default::default()
        },
        service_id('f'),
        PlanOptions::default(),
    )
    .unwrap();
    let compensation = ReplacementCompensation::StopFirst {
        stop_new_container: Err("stop failed"),
        restart_old_container: RestartAttempt::Attempted(Err("restart failed")),
    };

    let outcome = plan
        .replacement_health_failure_outcome(0, "health failed", compensation.clone())
        .unwrap();

    assert!(matches!(
        outcome.failed,
        Some(FailedOperation::ReplacementHealth {
            error: "health failed",
            compensation: actual,
            ..
        }) if actual == compensation
    ));
    assert!(outcome.completed.is_empty());
    assert!(outcome.unexecuted.is_empty());
}

#[test]
fn stop_first_failure_can_record_that_a_stopped_old_container_was_not_restarted() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    requested.update.order = Some(UpdateOrder::StopFirst);
    let mut current = requested.clone();
    current.container.image = "ghcr.io/getployz/api:old".into();
    let mut stopped = container('b', '1', &current, &service_id('a'));
    stopped.runtime = ContainerRuntimeObservation::Exited { code: 1 };
    let plan = plan_deploy(
        &requested,
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![stopped],
            ..Default::default()
        },
        service_id('f'),
        PlanOptions::default(),
    )
    .unwrap();

    let outcome = plan
        .replacement_health_failure_outcome(
            0,
            "health failed",
            ReplacementCompensation::StopFirst {
                stop_new_container: Ok(()),
                restart_old_container: RestartAttempt::NotAttempted,
            },
        )
        .unwrap();

    assert!(matches!(
        outcome.failed,
        Some(FailedOperation::ReplacementHealth {
            compensation: ReplacementCompensation::StopFirst {
                restart_old_container: RestartAttempt::NotAttempted,
                ..
            },
            ..
        })
    ));
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
            &requested,
            &DeploySnapshot {
                machines: vec![machine('1', "first")],
                containers: vec![container('b', '1', &current, &current_service_id)],
                ..Default::default()
            },
            service_id('f'),
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
    let VolumeSource::Named { driver, .. } = &mut requested.volumes.first_mut().unwrap().source
    else {
        unreachable!();
    };
    *driver = Some(ployz_core::VolumeDriver {
        name: "nfs".into(),
        options: Default::default(),
    });

    let plan = plan_deploy(
        &requested,
        &DeploySnapshot {
            machines: vec![machine('1', "first"), machine('2', "second")],
            volumes: vec![ployz::deploy::ObservedDockerVolume {
                id: DockerVolumeId {
                    machine_id: machine_id('1'),
                    name: DockerVolumeName::parse("data").unwrap(),
                },
                driver: "local".into(),
                options: Default::default(),
            }],
            ..Default::default()
        },
        service_id('a'),
        PlanOptions::default(),
    )
    .unwrap();
    assert!(
        matches!(
            plan.operations(),
            [
                DeployOperation::CreateVolume { machine_id: volume_machine, .. },
                DeployOperation::RunContainer { machine_id: container_machine, .. },
            ] if volume_machine == &machine_id('2') && container_machine == &machine_id('2')
        ),
        "unexpected operations: {:?}",
        plan.operations()
    );
}

#[test]
fn multi_replica_named_volume_replacement_defaults_to_start_first() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(3).unwrap(),
    });
    add_named_volume(&mut requested, "data");
    let mut current = requested.clone();
    current.container.image = "ghcr.io/getployz/api:old".into();
    let current_service_id = service_id('a');
    let plan = plan_deploy(
        &requested,
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container('b', '1', &current, &current_service_id)],
            volumes: vec![ployz::deploy::ObservedDockerVolume {
                id: DockerVolumeId {
                    machine_id: machine_id('1'),
                    name: DockerVolumeName::parse("data").unwrap(),
                },
                driver: "local".into(),
                options: Default::default(),
            }],
        },
        service_id('f'),
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        plan.operations().first(),
        Some(DeployOperation::ReplaceContainer(ReplacementOperation { spec, .. }))
            if spec.update.order == UpdateOrder::StartFirst
    ));
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
        &requested,
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![
                container('b', '1', &first_old, &current_service_id),
                container('c', '1', &second_old, &current_service_id),
            ],
            ..Default::default()
        },
        service_id('f'),
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        plan.operations(),
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

#[test]
fn successful_outcome_has_no_failed_or_unexecuted_operation() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    });
    let plan = plan_deploy(
        &requested,
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            ..Default::default()
        },
        service_id('a'),
        PlanOptions::default(),
    )
    .unwrap();
    let outcome = plan.success_outcome::<&str>();

    assert_eq!(outcome.completed, plan.operations());
    assert!(outcome.failed.is_none());
    assert!(outcome.unexecuted.is_empty());
}
