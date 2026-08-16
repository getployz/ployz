use super::support::*;
#[test]
fn new_replicated_service_runs_the_requested_count_across_available_machines() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    });
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), machine('2', "second")],
        ..Default::default()
    };

    let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();

    assert!(matches!(
        plan.operations.as_slice(),
        [
            DeployOperation::RunContainer { machine_id: first, spec: first_spec, .. },
            DeployOperation::RunContainer { machine_id: second, spec: second_spec, .. },
        ] if first == &machine_id('1')
            && second == &machine_id('2')
            && first_spec.service_id == second_spec.service_id
    ));
}

#[test]
fn new_container_keeps_an_explicit_stop_first_order_in_its_resolved_spec() {
    let mut requested = requested(ServiceMode::Global);
    requested.update.order = Some(UpdateOrder::StopFirst);
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
        plan.operations.as_slice(),
        [DeployOperation::RunContainer { spec, .. }]
            if spec.update.order == UpdateOrder::StopFirst
    ));
}

#[test]
fn matching_running_container_is_left_untouched() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let current_service_id = service_id('a');
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![container('b', '1', &requested, &current_service_id)],
        ..Default::default()
    };

    let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();

    assert!(plan.operations.is_empty());
}

#[test]
fn changed_running_container_is_replaced_on_its_machine() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let mut current = requested.clone();
    current.container.image = "ghcr.io/getployz/api:old".into();
    let current_service_id = service_id('a');
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![container('b', '1', &current, &current_service_id)],
        ..Default::default()
    };

    requested.update.order = Some(UpdateOrder::StartFirst);
    let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();

    assert!(matches!(
        plan.operations.as_slice(),
        [DeployOperation::ReplaceContainer(ReplacementOperation {
            machine_id: target_machine_id,
            old_container_id,
            spec,
            ..
        })] if target_machine_id == &machine_id('1')
            && old_container_id == &container_id('b')
            && spec.service_id == current_service_id
            && spec.update.order == UpdateOrder::StartFirst
    ));
}

#[test]
fn global_active_non_running_container_is_replaced_before_reusing_its_host_port() {
    for runtime in [
        ContainerRuntimeObservation::Paused,
        ContainerRuntimeObservation::Restarting,
    ] {
        let mut requested = requested(ServiceMode::Global);
        requested.ports.push(host_port(8080));
        let mut current = requested.clone();
        current.container.image = "ghcr.io/getployz/api:old".into();
        let current_service_id = service_id('a');
        let mut old = container('b', '1', &current, &current_service_id);
        old.runtime = runtime;
        let plan = plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "first")],
                containers: vec![old],
                ..Default::default()
            },
            PlanOptions::default(),
        )
        .unwrap();

        assert!(matches!(
            plan.operations.as_slice(),
            [DeployOperation::ReplaceContainer(ReplacementOperation {
                old_container_id,
                spec,
                ..
            })] if old_container_id == &container_id('b')
                && spec.update.order == UpdateOrder::StopFirst
        ));
    }
}

#[test]
fn replicated_plan_removes_containers_beyond_the_requested_count() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let current_service_id = service_id('a');
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![
            container('b', '1', &requested, &current_service_id),
            container('c', '1', &requested, &current_service_id),
        ],
        ..Default::default()
    };

    let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();

    assert!(matches!(
        plan.operations.as_slice(),
        [DeployOperation::RemoveContainer {
            machine_id: target_machine_id,
            container_id: removed,
        }] if target_machine_id == &machine_id('1')
            && [container_id('b'), container_id('c')].contains(removed)
    ));
}

#[test]
fn changing_replica_count_keeps_matching_existing_containers() {
    let current = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let mut requested = current.clone();
    requested.mode = ServiceMode::Replicated {
        replicas: NonZeroU32::new(3).unwrap(),
    };
    let current_service_id = service_id('a');
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container('b', '1', &current, &current_service_id)],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();
    assert_eq!(plan.operations.len(), 2);
    assert!(
        plan.operations
            .iter()
            .all(|operation| matches!(operation, DeployOperation::RunContainer { .. }))
    );
}

#[test]
fn global_plan_is_exactly_one_container_per_currently_available_machine() {
    let requested = requested(ServiceMode::Global);
    let current_service_id = service_id('a');
    let mut unavailable = machine('2', "second");
    unavailable.membership = MembershipObservation::Down;
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), unavailable],
        containers: vec![
            container('b', '1', &requested, &current_service_id),
            container('c', '2', &requested, &current_service_id),
        ],
        ..Default::default()
    };

    let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();

    assert_eq!(
        plan.operations,
        vec![DeployOperation::RemoveContainer {
            machine_id: machine_id('2'),
            container_id: container_id('c'),
        }]
    );
}

#[test]
fn placement_by_ambiguous_machine_name_keeps_every_match() {
    let mut requested = requested(ServiceMode::Global);
    requested.placement.machines = vec![MachineTarget::parse("edge").unwrap()];
    let snapshot = DeploySnapshot {
        machines: vec![
            machine('1', "edge"),
            machine('2', "edge"),
            machine('3', "other"),
        ],
        ..Default::default()
    };

    let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();

    let targets = plan
        .operations
        .iter()
        .map(|operation| match operation {
            DeployOperation::RunContainer { machine_id, .. } => *machine_id,
            other @ (DeployOperation::CreateVolume { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::ReplaceContainer(..)
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }) => panic!("unexpected operation: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(targets, vec![machine_id('1'), machine_id('2')]);
}

#[test]
fn empty_placement_keeps_every_eligible_machine_and_all_is_a_name() {
    let requested = requested(ServiceMode::Global);
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "all"), machine('2', "edge")],
        ..Default::default()
    };
    let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();
    let targets = |plan: &ployz::deploy::DeployPlan| {
        plan.operations
            .iter()
            .map(|operation| match operation {
                DeployOperation::RunContainer { machine_id, .. } => *machine_id,
                other @ (DeployOperation::CreateVolume { .. }
                | DeployOperation::StopContainer { .. }
                | DeployOperation::RemoveContainer { .. }
                | DeployOperation::ReplaceContainer(..)
                | DeployOperation::StopHook { .. }
                | DeployOperation::RunHook { .. }) => panic!("unexpected operation: {other:?}"),
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(targets(&plan), vec![machine_id('1'), machine_id('2')]);

    let mut named_all = requested;
    named_all.placement.machines = vec![MachineTarget::parse("all").unwrap()];
    let plan = plan_deploy([&named_all], &snapshot, PlanOptions::default()).unwrap();
    assert_eq!(targets(&plan), vec![machine_id('1')]);
}

#[test]
fn mounted_docker_volume_anchors_all_replicas_to_its_machine() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    });
    add_named_volume(&mut requested, "data");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), machine('2', "second")],
        volumes: vec![ployz::deploy::ObservedDockerVolume {
            id: DockerVolumeId {
                machine_id: machine_id('2'),
                name: DockerVolumeName::parse("data").unwrap(),
            },
            driver: "local".into(),
            options: Default::default(),
        }],
        ..Default::default()
    };

    let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();

    assert!(plan.operations.iter().all(|operation| matches!(
        operation,
        DeployOperation::RunContainer { machine_id: target, .. } if target == &machine_id('2')
    )));
    assert_eq!(plan.operations.len(), 2);
}

#[test]
fn missing_mounted_volume_is_created_before_replicas_on_one_machine() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    });
    add_named_volume(&mut requested, "data");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), machine('2', "second")],
        ..Default::default()
    };

    let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();

    assert!(matches!(
        plan.operations.as_slice(),
        [
            DeployOperation::CreateVolume { machine_id: volume_machine, .. },
            DeployOperation::RunContainer { machine_id: first, .. },
            DeployOperation::RunContainer { machine_id: second, .. },
        ] if volume_machine == &machine_id('1')
            && first == volume_machine
            && second == volume_machine
    ));
}

#[test]
fn missing_named_volume_is_created_before_three_replicas() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(3).unwrap(),
    });
    add_named_volume(&mut requested, "auto_data");
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
        plan.operations.first(),
        Some(DeployOperation::CreateVolume { .. })
    ));
    assert_eq!(plan.operations.len(), 4);
}

#[test]
fn inferred_update_order_preserves_the_two_stop_first_heuristics() {
    let cases = [
        ("stateless", false, false, UpdateOrder::StartFirst),
        ("single named volume", true, false, UpdateOrder::StopFirst),
        ("conflicting host port", false, true, UpdateOrder::StopFirst),
    ];

    for (name, with_volume, with_port, expected) in cases {
        let mut requested = requested(ServiceMode::Replicated {
            replicas: NonZeroU32::new(1).unwrap(),
        });
        if with_volume {
            add_named_volume(&mut requested, "data");
        }
        if with_port {
            requested.ports.push(host_port(8080));
        }
        let mut current = requested.clone();
        current.container.image = "ghcr.io/getployz/api:old".into();
        let current_service_id = service_id('a');
        let mut snapshot = DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container('b', '1', &current, &current_service_id)],
            ..Default::default()
        };
        if with_volume {
            snapshot.volumes.push(ployz::deploy::ObservedDockerVolume {
                id: DockerVolumeId {
                    machine_id: machine_id('1'),
                    name: DockerVolumeName::parse("data").unwrap(),
                },
                driver: "local".into(),
                options: Default::default(),
            });
        }

        let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();
        let order = plan
            .operations
            .iter()
            .find_map(|operation| match operation {
                DeployOperation::ReplaceContainer(operation) => Some(operation.spec.update.order),
                DeployOperation::CreateVolume { .. }
                | DeployOperation::RunContainer { .. }
                | DeployOperation::StopContainer { .. }
                | DeployOperation::RemoveContainer { .. }
                | DeployOperation::StopHook { .. }
                | DeployOperation::RunHook { .. } => None,
            })
            .unwrap();
        assert_eq!(order, expected, "{name}");
    }
}

#[test]
fn global_named_volume_replacement_defaults_to_stop_first() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "data");
    let mut current = requested.clone();
    current.container.image = "ghcr.io/getployz/api:old".into();
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container('b', '1', &current, &service_id('a'))],
            volumes: vec![ployz::deploy::ObservedDockerVolume {
                id: DockerVolumeId {
                    machine_id: machine_id('1'),
                    name: DockerVolumeName::parse("data").unwrap(),
                },
                driver: "local".into(),
                options: Default::default(),
            }],
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        plan.operations.as_slice(),
        [DeployOperation::ReplaceContainer(ReplacementOperation { spec, .. })]
            if spec.update.order == UpdateOrder::StopFirst
    ));
}

#[test]
fn two_services_sharing_a_named_volume_create_it_once() {
    let mut first = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    first.name = ServiceName::parse("first").unwrap();
    add_named_volume(&mut first, "data");
    let mut second = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    second.name = ServiceName::parse("second").unwrap();
    add_named_volume(&mut second, "data");
    let plan = plan_deploy(
        [&first, &second],
        &DeploySnapshot {
            machines: vec![machine('1', "first"), machine('2', "second")],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(
        plan.operations
            .iter()
            .filter(|operation| matches!(operation, DeployOperation::CreateVolume { .. }))
            .count(),
        1
    );
}

#[test]
fn missing_named_volume_is_created_on_the_machine_that_has_the_other() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    });
    add_named_volume(&mut requested, "multi_existing");
    add_named_volume(&mut requested, "multi_missing");
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first"), machine('2', "second")],
            volumes: vec![ployz::deploy::ObservedDockerVolume {
                id: DockerVolumeId {
                    machine_id: machine_id('1'),
                    name: DockerVolumeName::parse("multi_existing").unwrap(),
                },
                driver: "local".into(),
                options: Default::default(),
            }],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        plan.operations.as_slice(),
        [DeployOperation::CreateVolume { machine_id: volume_machine, volume }, rest @ ..]
            if volume_machine == &machine_id('1')
                && matches!(&volume.source, VolumeSource::Named { name, .. } if name.as_str() == "multi_missing")
                && rest.iter().all(|operation| matches!(operation,
                    DeployOperation::RunContainer { machine_id: container_machine, .. }
                        if container_machine == &machine_id('1')))
    ));
}

#[test]
fn replicas_run_on_the_intersection_of_existing_named_volumes() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    });
    add_named_volume(&mut requested, "intersect_a");
    add_named_volume(&mut requested, "intersect_b");
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first"), machine('2', "second")],
            volumes: vec![
                ployz::deploy::ObservedDockerVolume {
                    id: DockerVolumeId {
                        machine_id: machine_id('1'),
                        name: DockerVolumeName::parse("intersect_a").unwrap(),
                    },
                    driver: "local".into(),
                    options: Default::default(),
                },
                ployz::deploy::ObservedDockerVolume {
                    id: DockerVolumeId {
                        machine_id: machine_id('1'),
                        name: DockerVolumeName::parse("intersect_b").unwrap(),
                    },
                    driver: "local".into(),
                    options: Default::default(),
                },
            ],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(plan.operations.len(), 2);
    assert!(plan.operations.iter().all(|operation| matches!(
        operation,
        DeployOperation::RunContainer { machine_id: target, .. } if target == &machine_id('1')
    )));
}

#[test]
fn named_volumes_split_across_machines_return_no_eligible_machines() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    });
    add_named_volume(&mut requested, "split_a");
    add_named_volume(&mut requested, "split_b");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), machine('2', "second")],
        volumes: vec![
            ployz::deploy::ObservedDockerVolume {
                id: DockerVolumeId {
                    machine_id: machine_id('1'),
                    name: DockerVolumeName::parse("split_a").unwrap(),
                },
                driver: "local".into(),
                options: Default::default(),
            },
            ployz::deploy::ObservedDockerVolume {
                id: DockerVolumeId {
                    machine_id: machine_id('2'),
                    name: DockerVolumeName::parse("split_b").unwrap(),
                },
                driver: "local".into(),
                options: Default::default(),
            },
        ],
        ..Default::default()
    };

    assert_eq!(
        plan_deploy([&requested], &snapshot, PlanOptions::default()),
        Err(PlanError::NoEligibleMachines)
    );
}
