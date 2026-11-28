use std::{
    collections::{BTreeMap, BTreeSet},
    num::NonZeroU64,
};

use super::support::*;
use ployz_core::{MachineStorageObservation, Region, RegionWait};
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
        operations(&plan).as_slice(),
        [
            DeployOperation::RunContainer { machine_id: first, spec: first_spec, .. },
            DeployOperation::RunContainer { machine_id: second, spec: second_spec, .. },
        ] if first == &machine_id('1')
            && second == &machine_id('2')
            && first_spec.service_id == second_spec.service_id
    ));
}

#[test]
fn new_container_keeps_an_explicit_order_in_its_resolved_spec() {
    for (name, order, with_volume) in [
        ("start-first named volume", UpdateOrder::StartFirst, true),
        ("stop-first stateless", UpdateOrder::StopFirst, false),
    ] {
        let mut requested = requested(ServiceMode::Global);
        requested.update.order = Some(order);
        if with_volume {
            add_named_volume(&mut requested, "data");
        }
        let plan = plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "first")],
                volume_snapshot: VolumeSnapshot::try_from_observations(
                    with_volume.then(|| observed_volume(machine_id('1'), "data")),
                )
                .expect("valid Volume Snapshot fixture"),
                ..Default::default()
            },
            PlanOptions::default(),
        )
        .unwrap();

        assert!(
            matches!(
                operations(&plan).as_slice(),
                [DeployOperation::RunContainer { spec, .. }] if spec.update.order == order
            ),
            "{name}"
        );
    }
}

#[test]
fn new_named_volume_containers_default_to_stop_first_in_every_mode() {
    let cases = [
        (
            "single replica",
            ServiceMode::Replicated {
                replicas: NonZeroU32::new(1).unwrap(),
            },
            1,
        ),
        (
            "multiple replicas",
            ServiceMode::Replicated {
                replicas: NonZeroU32::new(3).unwrap(),
            },
            3,
        ),
        ("global", ServiceMode::Global, 1),
    ];

    for (name, mode, expected) in cases {
        let mut requested = requested(mode);
        add_named_volume(&mut requested, "data");
        let plan = plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "first")],
                volume_snapshot: VolumeSnapshot::try_from_observations(vec![observed_volume(
                    machine_id('1'),
                    "data",
                )])
                .expect("valid Volume Snapshot fixture"),
                ..Default::default()
            },
            PlanOptions::default(),
        )
        .unwrap();
        let orders = operations(&plan)
            .into_iter()
            .filter_map(|operation| match operation {
                DeployOperation::RunContainer { spec, .. } => Some(spec.update.order),
                DeployOperation::WaitHealthy { .. }
                | DeployOperation::StopContainer { .. }
                | DeployOperation::RemoveContainer { .. }
                | DeployOperation::ReplaceContainer(_)
                | DeployOperation::UpdateContainer(_)
                | DeployOperation::StopHook { .. }
                | DeployOperation::RunHook { .. }
                | DeployOperation::PrepareVolumes { .. }
                | DeployOperation::RemoveVolume { .. } => None,
            })
            .collect::<Vec<_>>();

        assert_eq!(orders.len(), expected, "{name}");
        assert!(
            orders.iter().all(|order| *order == UpdateOrder::StopFirst),
            "{name}"
        );
    }
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
fn unchanged_replica_needs_no_capacity_observation() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let current_service_id = service_id('a');
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "unknown")],
        containers: vec![container('b', '1', &requested, &current_service_id)],
        capacity: capacity([]),
        ..Default::default()
    };

    assert!(
        plan_deploy([&requested], &snapshot, PlanOptions::default())
            .unwrap()
            .operations
            .is_empty()
    );
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
        operations(&plan).as_slice(),
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
fn explicit_order_is_deferred_until_the_next_replacement() {
    let cases = [
        (
            "start-first named volume",
            true,
            UpdateOrder::StopFirst,
            UpdateOrder::StartFirst,
        ),
        (
            "stop-first stateless",
            false,
            UpdateOrder::StartFirst,
            UpdateOrder::StopFirst,
        ),
    ];

    for (name, with_volume, current_order, requested_order) in cases {
        let mut requested = requested(ServiceMode::Replicated {
            replicas: NonZeroU32::new(1).unwrap(),
        });
        if with_volume {
            add_named_volume(&mut requested, "data");
        }
        let mut current = container('b', '1', &requested, &service_id('a'));
        current
            .try_update(|parts| parts.resolved_spec.update.order = current_order)
            .unwrap();
        requested.update.order = Some(requested_order);
        let snapshot = DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![current],
            volume_snapshot: VolumeSnapshot::try_from_observations(
                with_volume.then(|| observed_volume(machine_id('1'), "data")),
            )
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        };

        let policy_only = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();
        assert!(policy_only.operations.is_empty(), "{name}");

        requested.container.image = "ghcr.io/getployz/api:2".into();
        let replacement = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();
        assert!(
            matches!(
                operations(&replacement).as_slice(),
                [DeployOperation::ReplaceContainer(ReplacementOperation { spec, .. })]
                    if spec.update.order == requested_order
            ),
            "{name}"
        );
    }
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
        old.try_update(|parts| parts.runtime = runtime).unwrap();
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
            operations(&plan).as_slice(),
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
        operations(&plan).as_slice(),
        [DeployOperation::RemoveContainer {
            machine_id: target_machine_id,
            container_id: removed,
            purpose: ployz_core::RemoveContainerPurpose::Rollout,
            anonymous_volumes,
        }] if target_machine_id == &machine_id('1')
            && anonymous_volumes.replaced_by().is_none()
            && [container_id('b'), container_id('c')].contains(removed)
    ));
}

/// A copy on Server `1` of `spec`, which that Server reports was replaced by `by`.
fn copy_of(
    hex: char,
    spec: &RequestedServiceSpec,
    by: Option<char>,
) -> ployz_core::ContainerObservation {
    let mut copy = container(hex, '1', spec, &service_id('a'));
    copy.try_update(|parts| parts.replaced_by = by.map(container_id))
        .unwrap();
    copy
}

/// The spec `requested` with another image, as an earlier Deploy ran it.
fn image(requested: &RequestedServiceSpec, tag: &str) -> RequestedServiceSpec {
    let mut older = requested.clone();
    older.container.image = format!("ghcr.io/getployz/api:{tag}");
    older
}

/// Each copy a one-replica Deploy of `requested` removes from `copies`, all on one
/// Server, with the copy it names as having taken that one's place.
fn removals(
    requested: &RequestedServiceSpec,
    copies: Vec<ployz_core::ContainerObservation>,
) -> BTreeMap<ContainerId, Option<ContainerId>> {
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: copies,
        ..Default::default()
    };
    let plan = plan_deploy([requested], &snapshot, PlanOptions::default()).unwrap();
    operations(&plan)
        .into_iter()
        .filter_map(|operation| match operation {
            DeployOperation::RemoveContainer {
                container_id,
                anonymous_volumes,
                ..
            } => Some((container_id, anonymous_volumes.replaced_by())),
            DeployOperation::PrepareVolumes { .. }
            | DeployOperation::WaitHealthy { .. }
            | DeployOperation::RunContainer { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::ReplaceContainer(_)
            | DeployOperation::UpdateContainer(_)
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::RemoveVolume { .. } => None,
        })
        .collect()
}

fn one_replica() -> RequestedServiceSpec {
    requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    })
}

/// An attempt made `b` to replace `a` and stopped before removing `a`. The retry keeps
/// `b`, so it names `b`, and the Server removes `a`'s anonymous Volumes only if its own
/// record agrees.
#[test]
fn a_leftover_names_its_recorded_replacement_when_the_deploy_keeps_it() {
    let requested = one_replica();
    let older = image(&requested, "old");
    assert_eq!(
        removals(
            &requested,
            vec![
                copy_of('a', &older, Some('b')),
                copy_of('b', &requested, None)
            ]
        ),
        BTreeMap::from([(container_id('a'), Some(container_id('b')))])
    );
}

/// A Server older than the replacement record reports none, so it is only asked to keep.
#[test]
fn a_leftover_with_no_recorded_replacement_keeps_its_volumes() {
    let requested = one_replica();
    let older = image(&requested, "old");
    assert_eq!(
        removals(
            &requested,
            vec![copy_of('a', &older, None), copy_of('b', &requested, None)]
        ),
        BTreeMap::from([(container_id('a'), None)])
    );
}

/// `b` replaced `a`, and `c` is an unrelated copy of the same spec. The scale-down keeps
/// only `c`, so `a`'s anonymous Volumes stay: `b` goes in the same Deploy.
#[test]
fn a_scale_down_that_removes_the_recorded_replacement_keeps_the_leftovers_volumes() {
    let requested = one_replica();
    let older = image(&requested, "old");
    let mut b = copy_of('b', &requested, None);
    b.try_update(|parts| {
        parts.runtime = ContainerRuntimeObservation::Running {
            health: HealthObservation::Starting,
        }
    })
    .unwrap();
    assert_eq!(
        removals(
            &requested,
            vec![
                copy_of('a', &older, Some('b')),
                b,
                copy_of('c', &requested, None),
            ]
        ),
        BTreeMap::from([(container_id('a'), None), (container_id('b'), None)])
    );
}

/// `b` replaced `a`, and this Deploy replaces `b` in turn, so nothing that stays took
/// `a`'s place.
#[test]
fn a_leftover_whose_replacement_this_deploy_replaces_keeps_its_volumes() {
    let requested = one_replica();
    assert_eq!(
        removals(
            &requested,
            vec![
                copy_of('a', &image(&requested, "v1"), Some('b')),
                copy_of('b', &image(&requested, "v2"), None),
            ]
        ),
        BTreeMap::from([(container_id('a'), None)])
    );
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
            .all(|row| matches!(row.operation, DeployOperation::RunContainer { .. }))
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
        operations(&plan),
        vec![DeployOperation::RemoveContainer {
            machine_id: machine_id('2'),
            container_id: container_id('c'),
            purpose: ployz_core::RemoveContainerPurpose::Rollout,
            anonymous_volumes: ployz_core::AnonymousVolumes::keep(),
        }]
    );
}

#[test]
fn placement_by_shared_label_keeps_every_match() {
    let mut requested = requested(ServiceMode::Global);
    requested.placement.constraints = [label_constraint("edge")].into();
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
        .map(|row| match &row.operation {
            DeployOperation::RunContainer { machine_id, .. } => *machine_id,
            other @ (DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::ReplaceContainer(..)
            | DeployOperation::UpdateContainer(..)
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::PrepareVolumes { .. }
            | DeployOperation::RemoveVolume { .. }) => panic!("unexpected operation: {other:?}"),
        })
        .collect::<Vec<_>>();
    assert_eq!(targets, vec![machine_id('1'), machine_id('2')]);
}

#[test]
fn empty_placement_keeps_every_eligible_machine_and_all_is_a_label_value() {
    let requested = requested(ServiceMode::Global);
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "all"), machine('2', "edge")],
        ..Default::default()
    };
    let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();
    let targets = |plan: &ployz::deploy::DeployPreview| {
        plan.operations
            .iter()
            .map(|row| match &row.operation {
                DeployOperation::RunContainer { machine_id, .. } => *machine_id,
                other @ (DeployOperation::WaitHealthy { .. }
                | DeployOperation::StopContainer { .. }
                | DeployOperation::RemoveContainer { .. }
                | DeployOperation::ReplaceContainer(..)
                | DeployOperation::UpdateContainer(..)
                | DeployOperation::StopHook { .. }
                | DeployOperation::RunHook { .. }
                | DeployOperation::PrepareVolumes { .. }
                | DeployOperation::RemoveVolume { .. }) => {
                    panic!("unexpected operation: {other:?}")
                }
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(targets(&plan), vec![machine_id('1'), machine_id('2')]);

    let mut named_all = requested;
    named_all.placement.constraints = [label_constraint("all")].into();
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
        volume_snapshot: VolumeSnapshot::try_from_observations(vec![observed_volume(
            machine_id('2'),
            "data",
        )])
        .expect("valid Volume Snapshot fixture"),
        ..Default::default()
    };

    let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();

    assert!(plan.operations.iter().all(|row| matches!(
        &row.operation,
        DeployOperation::RunContainer { machine_id: target, .. } if target == &machine_id('2')
    )));
    assert_eq!(plan.operations.len(), 2);
}

#[test]
fn missing_mounted_volume_is_previewed_for_replicas_on_one_machine() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    });
    add_named_volume(&mut requested, "data");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), machine('2', "second")],
        ..Default::default()
    };

    let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();
    let volume = plan
        .volumes_to_create
        .first()
        .expect("missing managed Volume is previewed");

    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::RunContainer { machine_id: first, .. },
            DeployOperation::RunContainer { machine_id: second, .. },
        ] if first == &machine_id('1')
            && second == first
            && volume.name.as_str() == "app_data"
            && volume.maximum_bytes.is_none()
    ));
}

#[test]
fn missing_named_volume_is_previewed_for_three_replicas() {
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
        operations(&plan).first(),
        Some(DeployOperation::RunContainer { .. })
    ));
    assert_eq!(plan.operations.len(), 3);
    assert_eq!(plan.volumes_to_create.len(), 1);
}

#[test]
fn inferred_update_order_preserves_the_two_stop_first_heuristics() {
    let cases = [
        ("stateless", false, false, false, UpdateOrder::StartFirst),
        (
            "single named volume",
            true,
            false,
            false,
            UpdateOrder::StopFirst,
        ),
        (
            "single provisioned volume",
            true,
            true,
            false,
            UpdateOrder::StopFirst,
        ),
        (
            "conflicting host port",
            false,
            false,
            true,
            UpdateOrder::StopFirst,
        ),
    ];

    for (name, with_volume, provisioned, with_port, expected) in cases {
        let mut requested = requested(ServiceMode::Replicated {
            replicas: NonZeroU32::new(1).unwrap(),
        });
        if with_volume {
            add_named_volume(&mut requested, "data");
        }
        if provisioned {
            make_provisioned(&mut requested, "data", 1_073_741_824);
        }
        if with_port {
            requested.ports.push(host_port(8080));
        }
        let mut current = requested.clone();
        current.container.image = "ghcr.io/getployz/api:old".into();
        let current_service_id = service_id('a');
        let mut target_machine = machine('1', "first");
        let mut snapshot = DeploySnapshot {
            machines: vec![target_machine.clone()],
            containers: vec![container('b', '1', &current, &current_service_id)],
            ..Default::default()
        };
        if with_volume {
            let mut existing = observed_volume(machine_id('1'), "data");
            if provisioned {
                existing.options = BTreeMap::from([("size".into(), "1073741824b".into())]);
                existing.storage = DockerVolumeStorageObservation::Provisioned {
                    mountpoint: MachinePath::parse("/var/lib/ployz-volumes/app_data").unwrap(),
                    bound_bytes: NonZeroU64::new(1_073_741_824).unwrap(),
                    used_bytes: 0,
                    role: None,
                    slot_role: None,
                };
                target_machine.storage = Some(MachineStorageObservation::Ready);
                snapshot.machines = vec![target_machine.clone()];
                snapshot.storage_capacity.insert(
                    target_machine.machine.id,
                    Ok(ployz_core::StorageCapacity {
                        backing: ployz_core::StorageBacking::Fixed {
                            pool_size_bytes: 10 * ployz_core::STORAGE_GIB,
                        },
                        unmanaged_used_bytes: 0,
                        slot_bound_bytes: 0,
                        volumes: BTreeMap::from([(
                            app_volume("data"),
                            ployz_core::ProvisionedVolumeMaximumBytes::new(
                                NonZeroU64::new(1_073_741_824).unwrap(),
                            ),
                        )]),
                        copies: BTreeMap::new(),
                    }),
                );
            }
            snapshot.volume_snapshot = VolumeSnapshot::try_from_observations(vec![existing])
                .expect("valid Volume Snapshot fixture");
        }

        let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();
        let order = plan
            .operations
            .iter()
            .find_map(|row| match &row.operation {
                DeployOperation::ReplaceContainer(operation) => Some(operation.spec.update.order),
                DeployOperation::UpdateContainer(update) => Some(update.spec.update.order),
                DeployOperation::WaitHealthy { .. }
                | DeployOperation::RunContainer { .. }
                | DeployOperation::StopContainer { .. }
                | DeployOperation::RemoveContainer { .. }
                | DeployOperation::StopHook { .. }
                | DeployOperation::RunHook { .. }
                | DeployOperation::PrepareVolumes { .. }
                | DeployOperation::RemoveVolume { .. } => None,
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
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![observed_volume(
                machine_id('1'),
                "data",
            )])
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        operations(&plan).as_slice(),
        [DeployOperation::ReplaceContainer(ReplacementOperation { spec, .. })]
            if spec.update.order == UpdateOrder::StopFirst
    ));
}

#[test]
fn two_global_services_sharing_a_missing_volume_create_it_once_per_machine() {
    let mut first = requested(ServiceMode::Global);
    first.name = ServiceName::parse("first").unwrap();
    add_named_volume(&mut first, "data");
    let mut second = requested(ServiceMode::Global);
    second.name = ServiceName::parse("second").unwrap();
    add_named_volume(&mut second, "data");
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), machine('2', "second")],
        ..Default::default()
    };
    let before = snapshot.clone();

    let plan = plan_deploy([&first, &second], &snapshot, PlanOptions::default()).unwrap();

    assert_eq!(snapshot, before);
    let created_on = plan
        .volumes_to_create
        .iter()
        .map(|item| item.machine_id)
        .collect::<Vec<_>>();
    assert_eq!(created_on.len(), 2);
    assert!(created_on.contains(&machine_id('1')));
    assert!(created_on.contains(&machine_id('2')));
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
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), machine('2', "second")],
        ..Default::default()
    };
    let before = snapshot.clone();
    let plan = plan_deploy([&first, &second], &snapshot, PlanOptions::default()).unwrap();

    assert_eq!(snapshot, before);
    assert_eq!(plan.volumes_to_create.len(), 1);
}

#[test]
fn shared_volume_anchor_skips_machine_without_capacity_for_all_services() {
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
            machines: vec![machine('1', "one-slot"), machine('2', "two-slots")],
            capacity: capacity([('1', 1), ('2', 2)]),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::RunContainer { machine_id: first, .. },
            DeployOperation::RunContainer { machine_id: second, .. },
        ] if first == &machine_id('2') && second == first
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
fn shared_volume_anchor_accounts_for_replacements_and_hooks() {
    let mut first = replicated("first", 1);
    let mut second = replicated("second", 1);
    let mut old_first = first.clone();
    let mut old_second = second.clone();
    old_first.container.image = "old:first".into();
    old_second.container.image = "old:second".into();
    for spec in [&mut first, &mut second] {
        add_named_volume(spec, "data");
        spec.pre_deploy = Some(PreDeployHook {
            command: vec!["prepare".into()].try_into().unwrap(),
            environment: Default::default(),
            privileged: None,
            timeout_millis: None,
            user: None,
        });
    }
    let plan = plan_deploy(
        [&first, &second],
        &DeploySnapshot {
            machines: vec![
                machine('1', "old-containers"),
                machine('2', "enough-capacity"),
            ],
            containers: vec![
                container('b', '1', &old_first, &service_id('a')),
                container('c', '1', &old_second, &service_id('d')),
            ],
            capacity: capacity([('1', 1), ('2', 4)]),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(
        plan.volumes_to_create
            .first()
            .expect("missing managed Volume is previewed")
            .machine_id,
        machine_id('2')
    );
    assert!(operations(&plan).iter().filter(|operation| {
        matches!(
            operation,
            DeployOperation::RunHook { machine_id: target, .. } if target == &machine_id('2')
        )
    }).count() == 2);
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
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![observed_volume(
                machine_id('1'),
                "multi_existing",
            )])
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::RunContainer { machine_id: first, .. },
            DeployOperation::RunContainer { machine_id: second, .. },
        ] if first == &machine_id('1') && second == &machine_id('1')
    ));
    assert!(matches!(
        &plan.volumes_to_create[..],
        [item] if item.machine_id == machine_id('1')
            && item.name.as_str() == "app_multi_missing"
            && item.maximum_bytes.is_none()
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
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![
                observed_volume(machine_id('1'), "intersect_a"),
                observed_volume(machine_id('1'), "intersect_b"),
            ])
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(plan.operations.len(), 2);
    assert!(plan.operations.iter().all(|row| matches!(
        &row.operation,
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
        volume_snapshot: VolumeSnapshot::try_from_observations(vec![
            observed_volume(machine_id('1'), "split_a"),
            observed_volume(machine_id('2'), "split_b"),
        ])
        .expect("valid Volume Snapshot fixture"),
        ..Default::default()
    };

    assert_no_eligible(
        plan_deploy([&requested], &snapshot, PlanOptions::default()),
        &[
            EliminatingConstraint::VolumeAlreadyOn {
                volume: app_volume("split_a"),
                located_on: vec![MachineName::parse("first").unwrap()],
            },
            EliminatingConstraint::VolumeAlreadyOn {
                volume: app_volume("split_b"),
                located_on: vec![MachineName::parse("second").unwrap()],
            },
        ],
        &[
            "Docker Volume 'app_split_a' is already on Machine 'first'",
            "Docker Volume 'app_split_b' is already on Machine 'second'",
        ],
    );
}

#[test]
fn unmatched_constraints_name_the_missing_label() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    requested.placement.constraints = [label_constraint("missing-machine")].into();

    assert_no_eligible(
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "ewr1")],
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        &[EliminatingConstraint::UnknownPlacement {
            targets: vec![label_constraint("missing-machine")],
        }],
        &["placement constraints 'node.labels.fixture==missing-machine' matched no Machine"],
    );
}

#[test]
fn constraints_name_the_down_machine() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    requested.placement.constraints = [label_constraint("ord1")].into();
    let mut ord1 = machine('2', "ord1");
    ord1.membership = MembershipObservation::Down;

    assert_no_eligible(
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "ewr1"), ord1],
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        &[EliminatingConstraint::MachineDown {
            names: vec![MachineName::parse("ord1").unwrap()],
        }],
        &["Machine 'ord1' is down"],
    );
}

#[test]
fn constraints_name_the_machine_that_did_not_answer_its_listing() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    requested.placement.constraints = [label_constraint("ord1")].into();
    let mut ord1 = machine('2', "ord1");
    ord1.membership = MembershipObservation::Suspect;

    assert_no_eligible(
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "ewr1"), ord1],
                container_failures: vec![ployz_core::MachineFailure {
                    machine_id: machine_id('2'),
                    error: ployz_core::RpcError::unanswered_target(
                        ployz_core::RpcErrorCode::Unavailable,
                        "target Machine RPC timed out".into(),
                    ),
                }],
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        &[EliminatingConstraint::Unanswered {
            names: vec![MachineName::parse("ord1").unwrap()],
        }],
        &["Machine 'ord1' did not answer when this Deploy listed its containers"],
    );
}

#[test]
fn constraints_name_each_excluded_machine_under_the_first_reason_that_applies_to_it() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let mut down = machine('1', "down");
    down.membership = MembershipObservation::Down;
    let mut refusing = machine('2', "refusing");
    refusing.membership = MembershipObservation::Unknown;
    refusing.machine.accepts_services = false;
    let mut slow = machine('3', "slow");
    slow.membership = MembershipObservation::Suspect;
    let mut up_and_refusing = machine('4', "both");
    up_and_refusing.membership = MembershipObservation::Up;
    up_and_refusing.machine.accepts_services = false;

    assert_no_eligible(
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![down, refusing, slow, up_and_refusing],
                container_failures: ["3", "4"]
                    .map(|hex| ployz_core::MachineFailure {
                        machine_id: machine_id(hex.chars().next().unwrap()),
                        error: ployz_core::RpcError::unanswered_target(
                            ployz_core::RpcErrorCode::Unavailable,
                            "target Machine RPC timed out".into(),
                        ),
                    })
                    .to_vec(),
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        &[
            EliminatingConstraint::MachineDown {
                names: vec![MachineName::parse("down").unwrap()],
            },
            EliminatingConstraint::Unanswered {
                names: vec![
                    MachineName::parse("slow").unwrap(),
                    MachineName::parse("both").unwrap(),
                ],
            },
            EliminatingConstraint::WorkNotAccepted {
                names: vec![MachineName::parse("refusing").unwrap()],
            },
        ],
        &[
            "Machine 'down' is down; Machines 'slow', 'both' did not answer when this Deploy listed its containers; Machine 'refusing' do not accept this work",
        ],
    );
}

/// A placement-matched Machine whose membership is Unknown or Unrecognized.
fn unreached_ord1(unrecognized: bool) -> MachineObservation {
    let mut ord1 = machine('2', "ord1");
    if unrecognized {
        ord1.membership = MembershipObservation::Unrecognized("Left".into());
        ord1.membership_evidence =
            Some(ployz_core::MembershipEvidence::Unrecognized { raw: "Left".into() });
    } else {
        ord1.membership = MembershipObservation::Unknown;
    }
    ord1
}

/// The message of a refusal through both public entry points, which must agree.
fn refusal_through_both_entries(
    requested: &RequestedServiceSpec,
    snapshot: &DeploySnapshot,
) -> String {
    let helper = plan_deploy([requested], snapshot, PlanOptions::default())
        .expect_err("the helper should refuse")
        .to_string();
    let public = ployz::deploy::plan_deploy(
        &DeployIntent::apply_all(
            Namespace::parse("app").unwrap(),
            [requested],
            PlanOptions::default(),
        ),
        snapshot,
    )
    .expect_err("plan_deploy should refuse")
    .to_string();
    assert_eq!(helper, public);
    helper
}

#[test]
fn a_placement_matched_machine_with_unknown_membership_is_not_named_in_the_refusal() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    requested.placement.constraints = [label_constraint("ord1")].into();

    for unrecognized in [false, true] {
        let snapshot = DeploySnapshot {
            machines: vec![machine('1', "ewr1"), unreached_ord1(unrecognized)],
            ..Default::default()
        };
        assert_no_eligible(
            plan_deploy([&requested], &snapshot, PlanOptions::default()),
            &[],
            &[],
        );
        assert_eq!(
            refusal_through_both_entries(&requested, &snapshot),
            "no machines available that satisfy all constraints: no Machines in the Deploy Snapshot",
            "unrecognized: {unrecognized}"
        );
    }
}

#[test]
fn a_machine_with_unknown_membership_leaves_the_other_constraints_as_they_were() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let mut down = machine('3', "down");
    down.membership = MembershipObservation::Down;
    let mut refusing = machine('4', "refusing");
    refusing.machine.accepts_services = false;

    for unrecognized in [false, true] {
        let snapshot = DeploySnapshot {
            machines: vec![unreached_ord1(unrecognized), down.clone(), refusing.clone()],
            ..Default::default()
        };
        assert_no_eligible(
            plan_deploy([&requested], &snapshot, PlanOptions::default()),
            &[
                EliminatingConstraint::MachineDown {
                    names: vec![MachineName::parse("down").unwrap()],
                },
                EliminatingConstraint::WorkNotAccepted {
                    names: vec![MachineName::parse("refusing").unwrap()],
                },
            ],
            &[],
        );
        assert_eq!(
            refusal_through_both_entries(&requested, &snapshot),
            "no machines available that satisfy all constraints: Machine 'down' is down; Machine 'refusing' do not accept this work",
            "unrecognized: {unrecognized}"
        );
    }
}

#[test]
fn volume_on_another_machine_names_the_volume_and_the_conflict() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    requested.placement.constraints = [label_constraint("ord1")].into();
    add_named_volume(&mut requested, "data");

    assert_no_eligible(
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "ewr1"), machine('2', "ord1")],
                volume_snapshot: VolumeSnapshot::try_from_observations(vec![observed_volume(
                    machine_id('1'),
                    "data",
                )])
                .expect("valid Volume Snapshot fixture"),
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        &[EliminatingConstraint::VolumeConflictsWithPlacement {
            volume: app_volume("data"),
            located_on: vec![MachineName::parse("ewr1").unwrap()],
            requested: vec![label_constraint("ord1")],
        }],
        &[
            "Docker Volume 'app_data' is already on Machine 'ewr1', which conflicts with placement constraints 'node.labels.fixture==ord1'",
        ],
    );
}

#[test]
fn a_region_no_server_is_in_stops_only_the_services_kept_in_it() {
    let api = kept_in(replicated("api", 1), "lhr");
    let web = kept_in(replicated("web", 1), "fsn");
    let snapshot = DeploySnapshot {
        machines: vec![
            in_region(machine('1', "first"), "syd"),
            in_region(machine('2', "second"), "fsn"),
        ],
        ..Default::default()
    };

    let preview = plan_deploy(
        [&api, &web],
        &snapshot,
        PlanOptions {
            block_unmountable: true,
            ..Default::default()
        },
    )
    .unwrap();

    let [blocked] = preview.blocked.as_slice() else {
        panic!("{:?}", preview.blocked)
    };
    assert_eq!(blocked.service, api.name);
    assert_eq!(blocked.error.message, "no Server in lhr");
    assert_eq!(
        blocked.error.details.get("reason"),
        Some(&serde_json::json!("region_unmatched"))
    );
    assert!(
        matches!(
            operations(&preview).as_slice(),
            [DeployOperation::RunContainer { machine_id: on, spec, .. }]
                if on == &machine_id('2') && spec.name == web.name
        ),
        "{:?}",
        operations(&preview)
    );

    let whole = plan_deploy([&api, &web], &snapshot, PlanOptions::default()).unwrap_err();
    assert_eq!(
        whole.into_rpc_error().message,
        "Service api: no Server in lhr"
    );
}

#[test]
fn a_region_whose_servers_cannot_run_services_stops_only_the_services_kept_in_it() {
    let api = kept_in(replicated("api", 1), "fsn");
    let web = kept_in(replicated("web", 1), "syd");
    let mut down = in_region(machine('1', "first"), "fsn");
    down.membership = MembershipObservation::Down;
    let mut refusing = in_region(machine('3', "third"), "fsn");
    refusing.machine.accepts_services = false;
    let snapshot = DeploySnapshot {
        machines: vec![down, in_region(machine('2', "second"), "syd"), refusing],
        ..Default::default()
    };

    let preview = plan_deploy(
        [&api, &web],
        &snapshot,
        PlanOptions {
            block_unmountable: true,
            ..Default::default()
        },
    )
    .unwrap();

    let [blocked] = preview.blocked.as_slice() else {
        panic!("{:?}", preview.blocked)
    };
    assert_eq!(blocked.service, api.name);
    assert_eq!(
        blocked.error.message,
        "first is down, third takes no services"
    );
    assert_eq!(
        blocked.error.details.get("reason"),
        Some(&serde_json::json!("region_unmatched"))
    );
    assert!(
        matches!(
            operations(&preview).as_slice(),
            [DeployOperation::RunContainer { machine_id: on, spec, .. }]
                if on == &machine_id('2') && spec.name == web.name
        ),
        "{:?}",
        operations(&preview)
    );
}

fn records_placements(mut machine: MachineObservation) -> MachineObservation {
    machine
        .machine
        .runtime
        .capabilities
        .insert(ployz_core::UPDATE_CONTAINER_PLACEMENT_CAPABILITY.to_owned());
    machine
}

fn placement_updates(operations: &[DeployOperation]) -> Vec<&ployz_core::ContainerUpdateOperation> {
    operations
        .iter()
        .filter_map(|operation| {
            if let DeployOperation::UpdateContainer(update) = operation
                && update.change == ployz_core::InPlaceChange::Placement
            {
                Some(update)
            } else {
                None
            }
        })
        .collect()
}

fn replaces_a_container(operations: &[DeployOperation]) -> bool {
    operations
        .iter()
        .any(|operation| matches!(operation, DeployOperation::ReplaceContainer(_)))
}

#[test]
fn a_new_region_keeps_every_replica_already_in_it_and_moves_only_the_one_outside() {
    let db = replicated("db", 1);
    let web = replicated("web", 2);
    let snapshot = DeploySnapshot {
        machines: vec![
            records_placements(in_region(machine('1', "first"), "fsn")),
            records_placements(in_region(machine('2', "second"), "fsn")),
            records_placements(in_region(machine('3', "third"), "syd")),
        ],
        containers: vec![
            container('a', '1', &db, &service_id('a')),
            container('b', '2', &web, &service_id('b')),
            container('c', '3', &web, &service_id('b')),
        ],
        ..Default::default()
    };
    let (db, web) = (kept_in(db, "fsn"), kept_in(web, "fsn"));

    let preview = plan_deploy([&db, &web], &snapshot, PlanOptions::default()).unwrap();

    let operations = operations(&preview);
    assert_eq!(
        placement_updates(&operations)
            .iter()
            .map(|update| (update.container_id, &update.spec.placement))
            .collect::<Vec<_>>(),
        [
            (container_id('a'), &db.placement),
            (container_id('b'), &web.placement)
        ],
        "{operations:?}"
    );
    assert!(!replaces_a_container(&operations), "{operations:?}");
    assert!(
        operations.iter().any(|operation| matches!(
            operation,
            DeployOperation::RunContainer { machine_id: on, spec, .. }
                if *on == machine_id('1') && spec.name == web.name
        )),
        "the replica outside fsn starts again inside it: {operations:?}"
    );
    assert!(
        operations.iter().any(|operation| matches!(
            operation,
            DeployOperation::RemoveContainer { container_id: removed, .. }
                if *removed == container_id('c')
        )),
        "{operations:?}"
    );
}

#[test]
fn clearing_a_region_spreads_stacked_replicas_without_replacing_one() {
    let web = replicated("web", 2);
    let stacked = kept_in(web.clone(), "fsn");
    let plan = plan_deploy(
        [&web],
        &DeploySnapshot {
            machines: vec![
                records_placements(in_region(machine('1', "first"), "fsn")),
                records_placements(in_region(machine('2', "second"), "syd")),
            ],
            containers: vec![
                container('b', '1', &stacked, &service_id('b')),
                container('c', '1', &stacked, &service_id('b')),
            ],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    let operations = operations(&plan);
    let updates = placement_updates(&operations);
    let [kept] = updates.as_slice() else {
        panic!("one stacked replica stays under its own id: {operations:?}")
    };
    assert_eq!(kept.machine_id, machine_id('1'));
    assert!(!replaces_a_container(&operations), "{operations:?}");
    assert!(
        operations.iter().any(|operation| matches!(
            operation,
            DeployOperation::RunContainer { machine_id: on, .. } if *on == machine_id('2')
        )),
        "{operations:?}"
    );
    assert!(
        operations.iter().any(|operation| matches!(
            operation,
            DeployOperation::RemoveContainer { container_id: removed, .. }
                if *removed != kept.container_id
        )),
        "{operations:?}"
    );
}

#[test]
fn a_new_region_with_one_more_replica_keeps_the_running_one_and_changes_only_its_placement() {
    let running = container('b', '1', &replicated("web", 1), &service_id('b'));
    let web = kept_in(replicated("web", 2), "fsn");
    let plan = plan_deploy(
        [&web],
        &DeploySnapshot {
            machines: vec![
                records_placements(in_region(machine('1', "first"), "fsn")),
                records_placements(in_region(machine('2', "second"), "fsn")),
            ],
            containers: vec![running.clone()],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    let operations = operations(&plan);
    let updates = placement_updates(&operations);
    let [kept] = updates.as_slice() else {
        panic!("the running replica stays under its own id: {operations:?}")
    };
    assert_eq!(kept.container_id, container_id('b'));
    let mut recorded = running.resolved_spec.clone();
    recorded.placement = web.placement.clone();
    assert_eq!(kept.spec, recorded);
    assert!(!replaces_a_container(&operations), "{operations:?}");
    assert!(
        operations.iter().any(|operation| matches!(
            operation,
            DeployOperation::RunContainer { machine_id: on, spec, .. }
                if *on == machine_id('2') && spec.mode == web.mode
        )),
        "{operations:?}"
    );
}

#[test]
fn replicas_recorded_under_two_service_ids_are_replaced_when_the_region_changes() {
    let web = replicated("web", 2);
    let plan = plan_deploy(
        [&kept_in(web.clone(), "fsn")],
        &DeploySnapshot {
            machines: vec![
                records_placements(in_region(machine('1', "first"), "fsn")),
                records_placements(in_region(machine('2', "second"), "fsn")),
            ],
            containers: vec![
                container('b', '1', &web, &service_id('b')),
                container('c', '2', &web, &service_id('c')),
            ],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    let operations = operations(&plan);
    assert!(
        placement_updates(&operations).is_empty(),
        "a Server refuses a placement sent under another Service's id: {operations:?}"
    );
    assert_eq!(
        operations
            .iter()
            .filter(|operation| matches!(operation, DeployOperation::ReplaceContainer(_)))
            .count(),
        2,
        "{operations:?}"
    );
    assert_eq!(
        operations
            .iter()
            .filter_map(|operation| operation.spec().map(|spec| spec.service_id))
            .collect::<BTreeSet<_>>()
            .len(),
        1,
        "both replicas come back under one Service id: {operations:?}"
    );
}

#[test]
fn clearing_a_region_keeps_the_running_container() {
    let db = replicated("db", 1);
    let plan = plan_deploy(
        [&db],
        &DeploySnapshot {
            machines: vec![records_placements(in_region(machine('1', "first"), "fsn"))],
            containers: vec![container(
                'a',
                '1',
                &kept_in(db.clone(), "fsn"),
                &service_id('a'),
            )],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(
        matches!(
            operations(&plan).as_slice(),
            [DeployOperation::UpdateContainer(update)]
                if update.container_id == container_id('a')
                    && update.change == ployz_core::InPlaceChange::Placement
        ),
        "{:?}",
        operations(&plan)
    );
}

#[test]
fn a_server_that_cannot_record_a_placement_replaces_the_container_inside_the_region() {
    let db = replicated("db", 1);
    let mut limits_only = in_region(machine('1', "first"), "fsn");
    limits_only
        .machine
        .runtime
        .capabilities
        .insert(ployz_core::UPDATE_CONTAINER_LIMITS_CAPABILITY.to_owned());
    let plan = plan_deploy(
        [&kept_in(db.clone(), "fsn")],
        &DeploySnapshot {
            machines: vec![limits_only, in_region(machine('2', "second"), "syd")],
            containers: vec![container('a', '1', &db, &service_id('a'))],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(
        matches!(
            operations(&plan).as_slice(),
            [DeployOperation::ReplaceContainer(ReplacementOperation { machine_id: on, old_container_id, .. })]
                if *on == machine_id('1') && *old_container_id == container_id('a')
        ),
        "{:?}",
        operations(&plan)
    );
}

#[test]
fn replicas_by_region_run_each_count_on_that_regions_servers() {
    let api = by_region(replicated("api", 1), &[("fsn", 1), ("syd", 2)]);
    let snapshot = DeploySnapshot {
        machines: vec![
            in_region(machine('1', "first"), "fsn"),
            in_region(machine('2', "second"), "lhr"),
            in_region(machine('3', "third"), "syd"),
            in_region(machine('4', "fourth"), "fsn"),
        ],
        ..Default::default()
    };

    let plan = plan_deploy([&api], &snapshot, PlanOptions::default()).unwrap();

    let mut ran = run_machine_ids(&plan);
    ran.sort();
    let fsn = [machine_id('1'), machine_id('4')];
    assert_eq!(ran.iter().filter(|id| fsn.contains(id)).count(), 1);
    assert_eq!(
        ran.iter().filter(|id| **id == machine_id('3')).count(),
        2,
        "{ran:?}"
    );
    assert_eq!(ran.len(), 3);
}

#[test]
fn counts_by_region_keep_the_replicas_already_in_those_regions() {
    let was = kept_in(replicated("api", 2), "fsn");
    let api = by_region(replicated("api", 1), &[("fsn", 2)]);
    let id = service_id('a');
    let snapshot = DeploySnapshot {
        machines: vec![
            records_placements(in_region(machine('1', "first"), "fsn")),
            records_placements(in_region(machine('2', "second"), "fsn")),
            records_placements(in_region(machine('3', "third"), "syd")),
        ],
        containers: vec![
            container('b', '1', &was, &id),
            container('c', '2', &was, &id),
        ],
        ..Default::default()
    };

    let plan = plan_deploy([&api], &snapshot, PlanOptions::default()).unwrap();

    assert_eq!(
        placements_recorded(&plan, &api),
        [container_id('b'), container_id('c')]
    );
}

#[test]
fn counts_set_back_to_a_plain_count_keep_every_replica() {
    let was = by_region(replicated("api", 1), &[("fsn", 2)]);
    let api = replicated("api", 2);
    let id = service_id('a');
    let snapshot = DeploySnapshot {
        machines: vec![
            records_placements(in_region(machine('1', "first"), "fsn")),
            records_placements(in_region(machine('2', "second"), "fsn")),
            records_placements(in_region(machine('3', "third"), "syd")),
        ],
        containers: vec![
            container('b', '1', &was, &id),
            container('c', '2', &was, &id),
        ],
        ..Default::default()
    };

    let plan = plan_deploy([&api], &snapshot, PlanOptions::default()).unwrap();

    assert_eq!(
        placements_recorded(&plan, &api),
        [container_id('b'), container_id('c')]
    );
}

/// The Containers `plan` updates in place to `requested`'s placement and counts
/// by region, when it does nothing else.
fn placements_recorded(plan: &DeployPreview, requested: &RequestedServiceSpec) -> Vec<ContainerId> {
    operations(plan)
        .iter()
        .map(|operation| {
            let DeployOperation::UpdateContainer(update) = operation else {
                panic!("only in-place placement updates: {operation:?}");
            };
            assert_eq!(update.change, ployz_core::InPlaceChange::Placement);
            assert_eq!(update.spec.placement, requested.placement);
            assert_eq!(update.spec.regions, requested.regions);
            update.container_id
        })
        .collect()
}

#[test]
fn a_region_that_loses_a_replica_keeps_its_other_one_running() {
    let was = by_region(replicated("api", 1), &[("fsn", 2)]);
    let api = by_region(replicated("api", 1), &[("fsn", 1), ("syd", 1)]);
    let id = service_id('a');
    let snapshot = DeploySnapshot {
        machines: vec![
            records_placements(in_region(machine('1', "first"), "fsn")),
            records_placements(in_region(machine('2', "second"), "fsn")),
            records_placements(in_region(machine('3', "third"), "syd")),
        ],
        containers: vec![
            container('b', '1', &was, &id),
            container('c', '2', &was, &id),
        ],
        ..Default::default()
    };

    let plan = plan_deploy([&api], &snapshot, PlanOptions::default()).unwrap();

    assert_eq!(run_machine_ids(&plan), [machine_id('3')]);
    let operations = operations(&plan);
    assert!(
        !operations.iter().any(|operation| matches!(
            operation,
            DeployOperation::ReplaceContainer(_) | DeployOperation::StopContainer { .. }
        )),
        "{operations:?}"
    );
    let updated: Vec<_> = operations
        .iter()
        .filter_map(|operation| {
            if let DeployOperation::UpdateContainer(update) = operation {
                assert_eq!(update.change, ployz_core::InPlaceChange::Placement);
                assert_eq!(update.spec.regions, api.regions);
                Some(update.container_id)
            } else {
                None
            }
        })
        .collect();
    let removed: Vec<_> = operations
        .iter()
        .filter_map(|operation| {
            if let DeployOperation::RemoveContainer { container_id, .. } = operation {
                Some(*container_id)
            } else {
                None
            }
        })
        .collect();
    assert!(
        matches!((removed.as_slice(), updated.as_slice()), ([gone], [kept]) if gone != kept),
        "removed {removed:?}, updated {updated:?}"
    );
}

#[test]
fn a_region_with_no_server_waits_while_the_service_starts_in_the_others() {
    let was = by_region(replicated("api", 1), &[("fsn", 1), ("lhr", 1)]);
    let mut api = by_region(replicated("api", 1), &[("fsn", 1), ("lhr", 1), ("nrt", 1)]);
    api.container.image = "ghcr.io/getployz/api:new".into();
    let web = kept_in(replicated("web", 1), "fsn");
    let snapshot = DeploySnapshot {
        machines: vec![
            in_region(machine('1', "first"), "syd"),
            in_region(machine('2', "second"), "fsn"),
        ],
        containers: vec![container('b', '2', &was, &service_id('a'))],
        ..Default::default()
    };

    let preview = plan_deploy(
        [&api, &web],
        &snapshot,
        PlanOptions {
            block_unmountable: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(preview.blocked.is_empty(), "{:?}", preview.blocked);
    let [waiting] = preview.waiting.as_slice() else {
        panic!("{:?}", preview.waiting)
    };
    assert_eq!(waiting.service, api.name);
    let region = |name: &str| Region::parse(name).unwrap();
    assert_eq!(
        waiting.regions,
        BTreeMap::from([
            (region("lhr"), not_running(1)),
            (region("nrt"), not_running(1))
        ])
    );
    assert_eq!(waiting.error.message, "no Server in lhr or nrt");
    assert_eq!(
        waiting.error.details.get("reason"),
        Some(&serde_json::json!("region_unmatched"))
    );
    let operations = operations(&preview);
    assert!(
        operations.iter().any(|operation| matches!(
            operation,
            DeployOperation::ReplaceContainer(replacement)
                if replacement.machine_id == machine_id('2') && replacement.spec.name == api.name
        )),
        "{operations:?}"
    );
    assert!(
        operations.iter().any(|operation| matches!(
            operation,
            DeployOperation::RunContainer { machine_id: on, spec, .. }
                if on == &machine_id('2') && spec.name == web.name
        )),
        "{operations:?}"
    );
}

#[test]
fn a_replica_running_in_a_region_with_no_server_up_keeps_running_while_it_waits() {
    let was = by_region(replicated("api", 1), &[("fsn", 1), ("syd", 1)]);
    let api = by_region(replicated("api", 1), &[("fsn", 1), ("syd", 1), ("lhr", 1)]);
    let mut down = in_region(machine('1', "first"), "syd");
    down.membership = MembershipObservation::Down;
    let snapshot = DeploySnapshot {
        machines: vec![down, in_region(machine('2', "second"), "fsn")],
        containers: vec![
            container('b', '1', &was, &service_id('a')),
            container('c', '2', &was, &service_id('a')),
        ],
        ..Default::default()
    };

    let preview = plan_deploy(
        [&api],
        &snapshot,
        PlanOptions {
            block_unmountable: true,
            ..Default::default()
        },
    )
    .unwrap();

    let operations = operations(&preview);
    assert!(
        !operations.iter().any(|operation| matches!(
            operation,
            DeployOperation::StopContainer { .. } | DeployOperation::RemoveContainer { .. }
        )),
        "{operations:?}"
    );
    assert_eq!(preview.waiting.len(), 1, "{:?}", preview.waiting);
}

#[test]
fn replicas_in_a_region_the_counts_dropped_stop_past_the_count_of_the_region_that_waits() {
    let was = by_region(replicated("api", 1), &[("fsn", 1), ("nrt", 2)]);
    let api = by_region(replicated("api", 1), &[("fsn", 1), ("lhr", 1)]);
    let snapshot = DeploySnapshot {
        machines: vec![
            in_region(machine('1', "first"), "nrt"),
            in_region(machine('2', "second"), "fsn"),
        ],
        containers: vec![
            container('b', '1', &was, &service_id('a')),
            container('c', '2', &was, &service_id('a')),
            container('d', '1', &was, &service_id('a')),
        ],
        ..Default::default()
    };

    let preview = plan_deploy([&api], &snapshot, runner_options()).unwrap();

    assert_eq!(preview.waiting.len(), 1, "{:?}", preview.waiting);
    assert_eq!(
        removed_from(&preview, '1').len(),
        1,
        "{:?}",
        operations(&preview)
    );
}

#[test]
fn a_region_whose_servers_are_down_waits_and_names_them() {
    let api = by_region(replicated("api", 1), &[("fsn", 1), ("lhr", 1), ("syd", 1)]);
    let mut down = in_region(machine('1', "first"), "syd");
    down.membership = MembershipObservation::Down;
    let snapshot = DeploySnapshot {
        machines: vec![down, in_region(machine('2', "second"), "fsn")],
        ..Default::default()
    };

    let preview = plan_deploy(
        [&api],
        &snapshot,
        PlanOptions {
            block_unmountable: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(preview.blocked.is_empty(), "{:?}", preview.blocked);
    let [waiting] = preview.waiting.as_slice() else {
        panic!("{:?}", preview.waiting)
    };
    assert_eq!(waiting.waiting(), 2);
    assert_eq!(waiting.error.message, "no Server in lhr; first is down");
    assert_eq!(run_machine_ids(&preview), [machine_id('2')]);
}

#[test]
fn a_replica_on_a_server_that_takes_no_services_is_reported_on_the_old_version() {
    let was = by_region(replicated("api", 1), &[("fsn", 1), ("syd", 1)]);
    let mut api = was.clone();
    api.container.image = "ghcr.io/getployz/api:new".into();
    let mut drained = in_region(machine('1', "first"), "syd");
    drained.machine.accepts_services = false;
    let snapshot = DeploySnapshot {
        machines: vec![drained, in_region(machine('2', "second"), "fsn")],
        containers: vec![
            container('b', '1', &was, &service_id('a')),
            container('c', '2', &was, &service_id('a')),
        ],
        ..Default::default()
    };
    let runner = || PlanOptions {
        block_unmountable: true,
        ..Default::default()
    };

    let preview = plan_deploy([&api], &snapshot, runner()).unwrap();

    let [waiting] = preview.waiting.as_slice() else {
        panic!("{:?}", preview.waiting)
    };
    assert_eq!(
        waiting.regions,
        BTreeMap::from([(
            Region::parse("syd").unwrap(),
            RegionWait {
                waiting: 0,
                outdated: 1
            }
        )])
    );
    assert_eq!(waiting.error.message, "first takes no services");
    assert!(
        matches!(
            operations(&preview).as_slice(),
            [DeployOperation::ReplaceContainer(replacement)] if replacement.machine_id == machine_id('2')
        ),
        "{:?}",
        operations(&preview)
    );

    let unchanged = plan_deploy([&was], &snapshot, runner()).unwrap();
    assert!(unchanged.waiting.is_empty(), "{:?}", unchanged.waiting);
    assert!(unchanged.noop(), "{:?}", operations(&unchanged));
}

#[test]
fn a_waiting_region_counts_a_copy_admitted_unsigned_as_outdated_in_a_signed_deploy() {
    let api = by_region(replicated("api", 1), &[("fsn", 1), ("syd", 1)]);
    let mut drained = in_region(machine('1', "first"), "syd");
    drained.machine.accepts_services = false;
    let mut free = container('b', '1', &api, &service_id('a'));
    free.try_update(|parts| parts.admission = Some(ployz_core::Admission::Free))
        .unwrap();
    let snapshot = DeploySnapshot {
        machines: vec![drained, in_region(machine('2', "second"), "fsn")],
        containers: vec![free, container('c', '2', &api, &service_id('a'))],
        ..Default::default()
    };
    let signing = PlanOptions::default().as_runner(ployz_core::RunnerOptions {
        operator: ployz_core::Operator::Cloud,
        ended_runs: BTreeSet::new(),
        recreate: BTreeSet::new(),
        signed: BTreeSet::from([api.name.clone()]),
    });

    let preview = plan_deploy([&api], &snapshot, signing).unwrap();

    let [waiting] = preview.waiting.as_slice() else {
        panic!("{:?}", preview.waiting)
    };
    assert_eq!(
        waiting.regions,
        BTreeMap::from([(
            Region::parse("syd").unwrap(),
            RegionWait {
                waiting: 0,
                outdated: 1
            }
        )])
    );
}

#[test]
fn a_signed_global_deploy_replaces_a_stopped_copy_admitted_unsigned() {
    let requested = requested(ServiceMode::Global);
    let mut free = container('b', '1', &requested, &service_id('a'));
    free.try_update(|parts| {
        parts.runtime = ContainerRuntimeObservation::Exited { code: 0 };
        parts.admission = Some(ployz_core::Admission::Free);
    })
    .unwrap();
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![free],
        ..Default::default()
    };
    let signing = PlanOptions::default().as_runner(ployz_core::RunnerOptions {
        operator: ployz_core::Operator::Cloud,
        ended_runs: BTreeSet::new(),
        recreate: BTreeSet::new(),
        signed: BTreeSet::from([requested.name.clone()]),
    });

    let plan = plan_deploy([&requested], &snapshot, signing).unwrap();

    assert!(
        matches!(
            operations(&plan).as_slice(),
            [DeployOperation::ReplaceContainer(ReplacementOperation { old_container_id, .. })]
                if old_container_id == &container_id('b')
        ),
        "{:?}",
        operations(&plan)
    );
}

/// A Server that takes no services starts nothing new. It still gives up a replica its
/// region no longer counts.
#[test]
fn a_smaller_count_removes_the_extra_replica_from_a_server_that_takes_no_services() {
    let was = by_region(replicated("api", 1), &[("fsn", 1), ("syd", 2)]);
    let api = by_region(replicated("api", 1), &[("fsn", 1), ("syd", 1)]);
    let mut drained = in_region(machine('1', "first"), "syd");
    drained.machine.accepts_services = false;
    let snapshot = DeploySnapshot {
        machines: vec![drained, in_region(machine('2', "second"), "fsn")],
        containers: vec![
            container('b', '1', &was, &service_id('a')),
            container('c', '1', &was, &service_id('a')),
            container('d', '2', &was, &service_id('a')),
        ],
        ..Default::default()
    };

    let preview = plan_deploy(
        [&api],
        &snapshot,
        PlanOptions {
            block_unmountable: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(
        removed_from(&preview, '1').len(),
        1,
        "{:?}",
        operations(&preview)
    );
    assert_eq!(removed_from(&preview, '2'), []);
    assert!(preview.waiting.is_empty(), "{:?}", preview.waiting);
    assert!(preview.blocked.is_empty(), "{:?}", preview.blocked);
}

/// The replica a waiting region keeps is one already on this Deploy's version, so the
/// count the user asked for is met there and nothing is left on the old one.
#[test]
fn the_extra_replica_removed_from_a_waiting_region_is_the_one_on_the_old_version() {
    let was = by_region(replicated("api", 1), &[("fsn", 1), ("syd", 2)]);
    let mut api = by_region(replicated("api", 1), &[("fsn", 1), ("syd", 1)]);
    api.container.image = "ghcr.io/getployz/api:new".into();
    let mut drained = in_region(machine('1', "first"), "syd");
    drained.machine.accepts_services = false;
    let snapshot = DeploySnapshot {
        machines: vec![drained, in_region(machine('2', "second"), "fsn")],
        containers: vec![
            container('b', '1', &was, &service_id('a')),
            container('c', '1', &api, &service_id('a')),
            container('d', '2', &api, &service_id('a')),
        ],
        ..Default::default()
    };

    let preview = plan_deploy(
        [&api],
        &snapshot,
        PlanOptions {
            block_unmountable: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert_eq!(removed_from(&preview, '1'), [container_id('b')]);
    assert!(preview.waiting.is_empty(), "{:?}", preview.waiting);
}

/// The only region a Service counts has no Server that takes services. A smaller count
/// still removes the replica it no longer asks for, and the one left is no reason to wait.
#[test]
fn a_smaller_count_removes_the_extra_replica_when_every_counted_region_waits() {
    let was = by_region(replicated("api", 1), &[("syd", 2)]);
    let api = by_region(replicated("api", 1), &[("syd", 1)]);
    let mut drained = in_region(machine('1', "first"), "syd");
    drained.machine.accepts_services = false;
    let snapshot = DeploySnapshot {
        machines: vec![drained],
        containers: vec![
            container('b', '1', &was, &service_id('a')),
            container('c', '1', &was, &service_id('a')),
        ],
        ..Default::default()
    };

    let preview = plan_deploy([&api], &snapshot, runner_options()).unwrap();

    assert_eq!(
        removed_from(&preview, '1').len(),
        1,
        "{:?} {:?}",
        operations(&preview),
        preview.blocked
    );
    assert!(preview.waiting.is_empty(), "{:?}", preview.waiting);
    assert!(preview.blocked.is_empty(), "{:?}", preview.blocked);

    let unchanged = plan_deploy(
        [&api],
        &DeploySnapshot {
            containers: vec![container('c', '1', &api, &service_id('a'))],
            ..snapshot
        },
        runner_options(),
    )
    .unwrap();
    assert!(unchanged.noop(), "{:?}", operations(&unchanged));
    assert!(unchanged.waiting.is_empty(), "{:?}", unchanged.waiting);
    assert!(unchanged.blocked.is_empty(), "{:?}", unchanged.blocked);
}

/// With every counted region waiting, the replica on the old version goes first.
#[test]
fn the_replica_on_the_old_version_is_removed_when_every_counted_region_waits() {
    let was = by_region(replicated("api", 1), &[("syd", 2)]);
    let mut api = by_region(replicated("api", 1), &[("syd", 1)]);
    api.container.image = "ghcr.io/getployz/api:new".into();
    let mut drained = in_region(machine('1', "first"), "syd");
    drained.machine.accepts_services = false;
    let snapshot = DeploySnapshot {
        machines: vec![drained],
        containers: vec![
            container('b', '1', &was, &service_id('a')),
            container('c', '1', &api, &service_id('a')),
        ],
        ..Default::default()
    };

    let preview = plan_deploy([&api], &snapshot, runner_options()).unwrap();

    assert_eq!(removed_from(&preview, '1'), [container_id('b')]);
    assert!(preview.waiting.is_empty(), "{:?}", preview.waiting);
    assert!(preview.blocked.is_empty(), "{:?}", preview.blocked);
}

/// A Service that never started has nothing to remove, so it still waits for a Server.
#[test]
fn a_service_never_started_in_a_region_that_takes_no_services_still_waits() {
    let api = by_region(replicated("api", 1), &[("syd", 1)]);
    let mut drained = in_region(machine('1', "first"), "syd");
    drained.machine.accepts_services = false;
    let snapshot = DeploySnapshot {
        machines: vec![drained],
        ..Default::default()
    };

    let preview = plan_deploy([&api], &snapshot, runner_options()).unwrap();

    let [blocked] = preview.blocked.as_slice() else {
        panic!("{:?}", preview.blocked)
    };
    assert_eq!(blocked.kind, ployz_core::BlockedKind::Wait);
    assert_eq!(blocked.error.message, "first takes no services");
    assert!(
        operations(&preview).is_empty(),
        "{:?}",
        operations(&preview)
    );
}

#[test]
fn a_server_that_leaves_its_region_keeps_its_replicas_while_the_region_waits() {
    let api = by_region(replicated("api", 1), &[("syd", 2)]);
    for left in [
        machine('1', "first"),
        in_region(machine('1', "first"), "nrt"),
    ] {
        let snapshot = DeploySnapshot {
            machines: vec![left, in_region(machine('2', "second"), "fsn")],
            containers: vec![
                container('b', '1', &api, &service_id('a')),
                container('c', '1', &api, &service_id('a')),
            ],
            ..Default::default()
        };

        let preview = plan_deploy([&api], &snapshot, runner_options()).unwrap();

        assert!(
            operations(&preview).is_empty(),
            "{:?}",
            operations(&preview)
        );
        let [blocked] = preview.blocked.as_slice() else {
            panic!("{:?}", preview.blocked)
        };
        assert_eq!(blocked.kind, ployz_core::BlockedKind::Wait);
        assert_eq!(blocked.error.message, "no Server in syd");
    }
}

#[test]
fn a_replica_on_a_server_that_left_its_region_keeps_running_beside_the_regions_that_start() {
    let api = by_region(replicated("api", 1), &[("fsn", 1), ("syd", 1)]);
    let snapshot = DeploySnapshot {
        machines: vec![
            machine('1', "first"),
            in_region(machine('2', "second"), "fsn"),
        ],
        containers: vec![
            container('b', '1', &api, &service_id('a')),
            container('c', '2', &api, &service_id('a')),
        ],
        ..Default::default()
    };

    let preview = plan_deploy([&api], &snapshot, runner_options()).unwrap();

    assert!(
        operations(&preview).is_empty(),
        "{:?}",
        operations(&preview)
    );
    let [waiting] = preview.waiting.as_slice() else {
        panic!("{:?}", preview.waiting)
    };
    assert_eq!(
        waiting.regions,
        BTreeMap::from([(Region::parse("syd").unwrap(), not_running(1))])
    );
    assert_eq!(waiting.error.message, "no Server in syd");
}

#[test]
fn a_smaller_count_removes_only_the_extra_replica_from_a_server_that_left_its_region() {
    let was = by_region(replicated("api", 1), &[("syd", 2)]);
    let api = by_region(replicated("api", 1), &[("syd", 1)]);
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![
            container('b', '1', &was, &service_id('a')),
            container('c', '1', &was, &service_id('a')),
        ],
        ..Default::default()
    };

    let preview = plan_deploy([&api], &snapshot, runner_options()).unwrap();

    assert_eq!(
        removed_from(&preview, '1').len(),
        1,
        "{:?}",
        operations(&preview)
    );
    assert_eq!(preview.waiting.len(), 1, "{:?}", preview.waiting);
}

#[test]
fn a_waiting_region_keeps_the_replica_on_its_own_server_first() {
    let was = by_region(replicated("api", 1), &[("syd", 2)]);
    let api = by_region(replicated("api", 1), &[("syd", 1)]);
    let mut drained = in_region(machine('1', "first"), "syd");
    drained.machine.accepts_services = false;
    let snapshot = DeploySnapshot {
        machines: vec![drained, machine('2', "second")],
        containers: vec![
            container('b', '2', &was, &service_id('a')),
            container('c', '1', &was, &service_id('a')),
        ],
        ..Default::default()
    };

    let preview = plan_deploy([&api], &snapshot, runner_options()).unwrap();

    assert_eq!(removed_from(&preview, '1'), []);
    assert_eq!(removed_from(&preview, '2'), [container_id('b')]);
}

#[test]
fn a_server_moved_into_a_region_whose_count_is_met_keeps_its_replica_while_its_old_region_waits() {
    let api = by_region(replicated("api", 1), &[("fsn", 1), ("syd", 1)]);
    let snapshot = DeploySnapshot {
        machines: vec![
            in_region(machine('1', "first"), "fsn"),
            in_region(machine('2', "second"), "fsn"),
        ],
        containers: vec![
            container('b', '1', &api, &service_id('a')),
            container('c', '2', &api, &service_id('a')),
        ],
        ..Default::default()
    };

    let preview = plan_deploy([&api], &snapshot, runner_options()).unwrap();

    assert!(
        operations(&preview).is_empty(),
        "{:?}",
        operations(&preview)
    );
    let [waiting] = preview.waiting.as_slice() else {
        panic!("{:?}", preview.waiting)
    };
    assert_eq!(
        waiting.regions,
        BTreeMap::from([(Region::parse("syd").unwrap(), not_running(1))])
    );
    assert_eq!(waiting.error.message, "no Server in syd");
}

#[test]
fn a_smaller_count_keeps_its_extra_replica_in_place_of_one_that_waits_in_another_region() {
    let was = by_region(replicated("api", 1), &[("fsn", 2), ("syd", 1)]);
    let api = by_region(replicated("api", 1), &[("fsn", 1), ("syd", 1)]);
    let snapshot = DeploySnapshot {
        machines: vec![
            records_placements(in_region(machine('1', "first"), "fsn")),
            records_placements(in_region(machine('2', "second"), "fsn")),
        ],
        containers: vec![
            container('b', '1', &was, &service_id('a')),
            container('c', '2', &was, &service_id('a')),
        ],
        ..Default::default()
    };

    let preview = plan_deploy([&api], &snapshot, runner_options()).unwrap();

    let removed = [removed_from(&preview, '1'), removed_from(&preview, '2')].concat();
    assert_eq!(removed, [], "{:?}", operations(&preview));
    assert_eq!(preview.waiting.len(), 1, "{:?}", preview.waiting);
}

#[test]
fn a_smaller_count_removes_its_extra_replica_when_the_region_that_cant_start_has_its_count() {
    let was = by_region(replicated("api", 1), &[("fsn", 2), ("syd", 1)]);
    let api = by_region(replicated("api", 1), &[("fsn", 1), ("syd", 1)]);
    let mut drained = records_placements(in_region(machine('3', "third"), "syd"));
    drained.machine.accepts_services = false;
    let snapshot = DeploySnapshot {
        machines: vec![
            records_placements(in_region(machine('1', "first"), "fsn")),
            records_placements(in_region(machine('2', "second"), "fsn")),
            drained,
        ],
        containers: vec![
            container('b', '1', &was, &service_id('a')),
            container('c', '2', &was, &service_id('a')),
            container('d', '3', &was, &service_id('a')),
        ],
        ..Default::default()
    };

    let preview = plan_deploy([&api], &snapshot, runner_options()).unwrap();

    let removed = [removed_from(&preview, '1'), removed_from(&preview, '2')].concat();
    assert_eq!(removed.len(), 1, "{:?}", operations(&preview));
    assert_eq!(removed_from(&preview, '3'), []);
    assert!(preview.waiting.is_empty(), "{:?}", preview.waiting);
}

#[test]
fn a_replica_kept_in_place_of_one_that_waited_is_removed_once_its_region_starts() {
    let was = by_region(replicated("api", 1), &[("fsn", 1), ("nrt", 1)]);
    let api = by_region(replicated("api", 1), &[("fsn", 1), ("lhr", 1)]);
    let snapshot = DeploySnapshot {
        machines: vec![
            records_placements(in_region(machine('1', "first"), "nrt")),
            records_placements(in_region(machine('2', "second"), "fsn")),
            records_placements(in_region(machine('3', "third"), "lhr")),
        ],
        containers: vec![
            container('b', '1', &was, &service_id('a')),
            container('c', '2', &was, &service_id('a')),
        ],
        ..Default::default()
    };

    let preview = plan_deploy([&api], &snapshot, runner_options()).unwrap();

    assert_eq!(removed_from(&preview, '1'), [container_id('b')]);
    assert!(
        operations(&preview).iter().any(|operation| matches!(
            operation,
            DeployOperation::RunContainer { machine_id: on, .. } if on == &machine_id('3')
        )),
        "{:?}",
        operations(&preview)
    );
    assert!(preview.waiting.is_empty(), "{:?}", preview.waiting);
}

#[test]
fn a_wait_names_only_the_regions_still_short_of_a_replica() {
    let api = by_region(replicated("api", 1), &[("lhr", 1), ("syd", 1)]);
    let mut short = in_region(machine('2', "second"), "lhr");
    short.machine.accepts_services = false;
    let mut met = in_region(machine('3', "third"), "syd");
    met.machine.accepts_services = false;
    let snapshot = DeploySnapshot {
        machines: vec![short, met],
        containers: vec![container('b', '3', &api, &service_id('a'))],
        ..Default::default()
    };

    let preview = plan_deploy([&api], &snapshot, runner_options()).unwrap();

    let [blocked] = preview.blocked.as_slice() else {
        panic!("{:?}", preview.blocked)
    };
    assert_eq!(blocked.kind, ployz_core::BlockedKind::Wait);
    assert_eq!(blocked.error.message, "second takes no services");
    assert_eq!(
        blocked.error.details.get("regions"),
        Some(&serde_json::json!(["lhr"]))
    );
    assert!(
        operations(&preview).is_empty(),
        "{:?}",
        operations(&preview)
    );
}

#[test]
fn a_server_joining_a_region_whose_count_is_met_moves_no_replica_while_another_region_waits() {
    let api = by_region(replicated("api", 1), &[("fsn", 2), ("syd", 1)]);
    let snapshot = DeploySnapshot {
        machines: vec![
            in_region(machine('1', "first"), "fsn"),
            in_region(machine('2', "second"), "fsn"),
            machine('3', "third"),
        ],
        containers: vec![
            container('b', '1', &api, &service_id('a')),
            container('c', '1', &api, &service_id('a')),
            container('d', '3', &api, &service_id('a')),
        ],
        ..Default::default()
    };

    let preview = plan_deploy([&api], &snapshot, runner_options()).unwrap();

    assert!(
        operations(&preview).is_empty(),
        "{:?}",
        operations(&preview)
    );
    let [waiting] = preview.waiting.as_slice() else {
        panic!("{:?}", preview.waiting)
    };
    assert_eq!(
        waiting.regions,
        BTreeMap::from([(Region::parse("syd").unwrap(), not_running(1))])
    );
}

#[test]
fn a_replica_kept_in_place_of_one_that_waits_is_reported_on_the_old_version() {
    let was = by_region(replicated("api", 1), &[("fsn", 1), ("syd", 1)]);
    let mut api = was.clone();
    api.container.image = "ghcr.io/getployz/api:new".into();
    let snapshot = DeploySnapshot {
        machines: vec![
            in_region(machine('1', "first"), "fsn"),
            machine('3', "third"),
        ],
        containers: vec![
            container('b', '1', &was, &service_id('a')),
            container('d', '3', &was, &service_id('a')),
        ],
        ..Default::default()
    };

    let preview = plan_deploy([&api], &snapshot, runner_options()).unwrap();

    let [waiting] = preview.waiting.as_slice() else {
        panic!("{:?}", preview.waiting)
    };
    assert_eq!(
        waiting.regions,
        BTreeMap::from([(
            Region::parse("syd").unwrap(),
            RegionWait {
                waiting: 0,
                outdated: 1
            }
        )])
    );
    assert_eq!(waiting.error.message, "no Server in syd");
    assert!(
        matches!(
            operations(&preview).as_slice(),
            [DeployOperation::ReplaceContainer(replacement)] if replacement.machine_id == machine_id('1')
        ),
        "{:?}",
        operations(&preview)
    );
}

fn runner_options() -> PlanOptions {
    PlanOptions {
        block_unmountable: true,
        ..Default::default()
    }
}

/// The Containers `preview` removes from the Machine `hex` names.
fn removed_from(preview: &DeployPreview, hex: char) -> Vec<ContainerId> {
    operations(preview)
        .into_iter()
        .filter_map(|operation| {
            if let DeployOperation::RemoveContainer {
                machine_id: on,
                container_id,
                ..
            } = operation
                && on == machine_id(hex)
            {
                Some(container_id)
            } else {
                None
            }
        })
        .collect()
}

fn not_running(waiting: u32) -> RegionWait {
    RegionWait {
        waiting,
        outdated: 0,
    }
}

#[test]
fn a_service_counted_only_in_regions_with_no_server_is_blocked_not_waiting() {
    let api = by_region(replicated("api", 1), &[("lhr", 1), ("syd", 1)]);
    let snapshot = DeploySnapshot {
        machines: vec![in_region(machine('2', "second"), "fsn")],
        ..Default::default()
    };

    let preview = plan_deploy(
        [&api],
        &snapshot,
        PlanOptions {
            block_unmountable: true,
            ..Default::default()
        },
    )
    .unwrap();

    assert!(preview.waiting.is_empty(), "{:?}", preview.waiting);
    let [blocked] = preview.blocked.as_slice() else {
        panic!("{:?}", preview.blocked)
    };
    assert_eq!(blocked.error.message, "no Server in lhr or syd");
}

fn replicated(name: &str, replicas: u32) -> RequestedServiceSpec {
    let mut spec = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(replicas).unwrap(),
    });
    spec.name = ServiceName::parse(name).unwrap();
    spec
}

fn cluster(ids: impl IntoIterator<Item = char>) -> Vec<MachineObservation> {
    ids.into_iter()
        .map(|id| machine(id, &format!("n{id}")))
        .collect()
}

fn run_machine_ids(plan: &DeployPreview) -> Vec<MachineId> {
    plan.operations
        .iter()
        .filter_map(|row| match &row.operation {
            DeployOperation::RunContainer { machine_id, .. } => Some(*machine_id),
            DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::ReplaceContainer(_)
            | DeployOperation::UpdateContainer(_)
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::PrepareVolumes { .. }
            | DeployOperation::RemoveVolume { .. } => None,
        })
        .collect()
}

#[test]
fn service_identity_changes_equal_priority_machine_order() {
    let snapshot = DeploySnapshot {
        machines: cluster(['1', '2', '3', '4', '5']),
        ..Default::default()
    };
    let options = PlanOptions {
        placement_seed: 7,
        ..PlanOptions::default()
    };
    let order = |name: &str| {
        let spec = replicated(name, 3);
        run_machine_ids(&plan_deploy([&spec], &snapshot, options.clone()).unwrap())
    };

    assert_ne!(order("alpha"), order("bravo"));
}

#[test]
fn ample_capacity_preserves_the_existing_shuffle_order() {
    let requested = replicated("api", 3);
    let machines = cluster(['1', '2', '3', '4']);
    let options = PlanOptions {
        placement_seed: 7,
        ..PlanOptions::default()
    };
    let without_capacity = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: machines.clone(),
            ..Default::default()
        },
        options.clone(),
    )
    .unwrap();
    let with_capacity = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines,
            capacity: capacity([('1', 10), ('2', 10), ('3', 10), ('4', 10)]),
            ..Default::default()
        },
        options,
    )
    .unwrap();

    assert_eq!(
        run_machine_ids(&with_capacity),
        run_machine_ids(&without_capacity)
    );
}

#[test]
fn ample_capacity_preserves_plan_wide_occupancy() {
    let specs = [replicated("alpha", 1), replicated("bravo", 1)];
    let machines = cluster(['1', '2']);
    let options = PlanOptions {
        placement_seed: 0,
        ..PlanOptions::default()
    };
    let without_capacity = plan_deploy(
        specs.iter(),
        &DeploySnapshot {
            machines: machines.clone(),
            ..Default::default()
        },
        options.clone(),
    )
    .unwrap();
    let with_capacity = plan_deploy(
        specs.iter(),
        &DeploySnapshot {
            machines,
            capacity: capacity([('1', 10), ('2', 10)]),
            ..Default::default()
        },
        options,
    )
    .unwrap();

    assert_eq!(
        run_machine_ids(&with_capacity),
        run_machine_ids(&without_capacity)
    );
    assert_eq!(
        run_machine_ids(&with_capacity)
            .into_iter()
            .collect::<BTreeSet<_>>()
            .len(),
        2
    );
}

#[test]
fn later_replicated_service_prefers_machines_unused_by_earlier_services() {
    let alpha = replicated("alpha", 1);
    let bravo = replicated("bravo", 1);
    let plan = plan_deploy(
        [&alpha, &bravo],
        &DeploySnapshot {
            machines: cluster(['1', '2']),
            ..Default::default()
        },
        PlanOptions {
            placement_seed: 0,
            ..PlanOptions::default()
        },
    )
    .unwrap();

    let machines = run_machine_ids(&plan).into_iter().collect::<BTreeSet<_>>();
    assert_eq!(machines.len(), 2);
}

#[test]
fn first_deploy_spreads_replicated_services_past_max_replicas() {
    let specs = [
        replicated("alpha", 2),
        replicated("bravo", 3),
        replicated("charlie", 3),
        replicated("delta", 2),
        replicated("echo", 2),
    ];
    let plan = plan_deploy(
        specs.iter(),
        &DeploySnapshot {
            machines: cluster(['1', '2', '3', '4', '5', '6', '7', '8', '9', 'a']),
            ..Default::default()
        },
        PlanOptions {
            placement_seed: 1,
            ..PlanOptions::default()
        },
    )
    .unwrap();

    let used = run_machine_ids(&plan).into_iter().collect::<BTreeSet<_>>();
    assert!(
        used.len() > 3,
        "expected more than max(replicas)=3 machines, used {}",
        used.len()
    );
    assert_eq!(run_machine_ids(&plan).len(), 12);
}

#[test]
fn host_socket_placement_rejects_one_machine_and_uses_an_alternative() {
    let mut first = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    first.ports.push(host_port(8080));
    let mut second = first.clone();
    second.name = ServiceName::parse("second").unwrap();
    let mut snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        ..Default::default()
    };
    assert!(plan_deploy([&first, &second], &snapshot, PlanOptions::default()).is_err());
    snapshot.machines.push(machine('2', "second"));
    let plan = plan_deploy([&first, &second], &snapshot, PlanOptions::default()).unwrap();
    let targets = operations(&plan)
        .iter()
        .map(DeployOperation::machine_id)
        .collect::<BTreeSet<_>>();
    assert_eq!(targets, BTreeSet::from([machine_id('1'), machine_id('2')]));
}

#[test]
fn host_socket_admission_checks_publications_and_replica_claims() {
    let mut service = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    service.ports = vec![host_port(8080), host_port(8080)];
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        ..Default::default()
    };
    assert!(matches!(
        plan_deploy([&service], &snapshot, PlanOptions::default()),
        Err(PlanError::ConflictingHostPublications { .. })
    ));
    service.ports.pop();
    service.mode = ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    };
    assert!(matches!(
        plan_deploy([&service], &snapshot, PlanOptions::default()),
        Err(PlanError::HostPortConflict { .. })
    ));
}

#[test]
fn host_socket_admission_preserves_protocol_and_disjoint_bind_distinctions() {
    for (first_bind, second_bind, second_protocol, conflict) in [
        (HostBind::All, HostBind::All, TransportProtocol::Udp, false),
        (
            HostBind::Address {
                address: "127.0.0.1".parse().unwrap(),
            },
            HostBind::Address {
                address: "127.0.0.2".parse().unwrap(),
            },
            TransportProtocol::Tcp,
            false,
        ),
        (
            HostBind::Prefix {
                prefix: "127.0.0.0/24".parse().unwrap(),
            },
            HostBind::Address {
                address: "127.0.0.2".parse().unwrap(),
            },
            TransportProtocol::Tcp,
            true,
        ),
    ] {
        let mut first = requested(ServiceMode::Global);
        let mut second = first.clone();
        second.name = ServiceName::parse("second").unwrap();
        let make_port = |bind, transport_protocol| PortPublication::Host {
            bind,
            transport_protocol,
            published_port: NonZeroU16::new(8080).unwrap(),
            container_port: NonZeroU16::new(80).unwrap(),
        };
        first.ports = vec![make_port(first_bind, TransportProtocol::Tcp)];
        second.ports = vec![make_port(second_bind, second_protocol)];
        let snapshot = DeploySnapshot {
            machines: vec![machine('1', "first")],
            ..Default::default()
        };
        assert_eq!(
            plan_deploy([&first, &second], &snapshot, PlanOptions::default()).is_err(),
            conflict
        );
    }
}

#[test]
fn host_socket_replacement_forces_stop_first_but_not_over_other_owners() {
    for mode in [
        ServiceMode::Global,
        ServiceMode::Replicated {
            replicas: NonZeroU32::new(1).unwrap(),
        },
    ] {
        let mut service = requested(mode);
        service.ports.push(host_port(8080));
        let current = container('a', '1', &service, &service_id('a'));
        service.container.image = "changed:latest".into();
        let mut snapshot = DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![current.clone()],
            ..Default::default()
        };
        for order in [UpdateOrder::StartFirst, UpdateOrder::StopFirst] {
            service.update.order = Some(order);
            let plan = plan_deploy([&service], &snapshot, PlanOptions::default()).unwrap();
            assert!(matches!(
                operations(&plan).as_slice(),
                [DeployOperation::ReplaceContainer(replaced)]
                    if replaced.spec.update.order == UpdateOrder::StopFirst
            ));
        }
        let mut foreign = container('b', '1', &service, &service_id('b'));
        foreign
            .try_update(|parts| parts.namespace = Namespace::parse("foreign").unwrap())
            .unwrap();
        snapshot.containers.push(foreign);
        assert!(plan_deploy([&service], &snapshot, PlanOptions::default()).is_err());
    }
}

#[test]
fn host_socket_future_prune_cannot_free_ports_but_hooks_do_not_claim_them() {
    let mut service = requested(ServiceMode::Global);
    service.ports.push(host_port(8080));
    let mut obsolete_spec = service.clone();
    obsolete_spec.name = ServiceName::parse("obsolete").unwrap();
    let mut old = container('a', '1', &obsolete_spec, &service_id('a'));
    let mut snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![old.clone()],
        ..Default::default()
    };
    assert!(plan_deploy([&service], &snapshot, PlanOptions::default()).is_err());
    old.try_update(|parts| parts.kind = ContainerKind::PreDeployHook)
        .unwrap();
    snapshot.containers = vec![old];
    assert!(plan_deploy([&service], &snapshot, PlanOptions::default()).is_ok());
}

#[test]
fn shared_volume_anchor_skips_known_host_socket_occupancy() {
    let mut first = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    first.name = ServiceName::parse("first").unwrap();
    first.ports.push(host_port(8080));
    let mut foreign = container('a', '1', &first, &service_id('a'));
    foreign
        .try_update(|parts| parts.namespace = Namespace::parse("foreign").unwrap())
        .unwrap();
    add_named_volume(&mut first, "data");
    let mut second = first.clone();
    second.name = ServiceName::parse("second").unwrap();
    second.ports.clear();
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "occupied"), machine('2', "free")],
        containers: vec![foreign],
        ..Default::default()
    };
    let plan = plan_deploy([&first, &second], &snapshot, PlanOptions::default()).unwrap();
    assert!(
        matches!(operations(&plan).as_slice(), [DeployOperation::RunContainer { machine_id: first, .. }, DeployOperation::RunContainer { machine_id: second, .. }] if *first == machine_id('2') && first == second)
    );
    second.ports = first.ports.clone();
    assert!(plan_deploy([&first, &second], &snapshot, PlanOptions::default()).is_err());
}

#[test]
fn host_socket_replacement_releases_old_ports_before_the_next_service() {
    let mut first = requested(ServiceMode::Global);
    first.name = ServiceName::parse("first").unwrap();
    first.ports.push(host_port(8080));
    let old = container('a', '1', &first, &service_id('a'));
    first.ports = vec![host_port(9090)];
    let mut second = requested(ServiceMode::Global);
    second.name = ServiceName::parse("second").unwrap();
    second.ports.push(host_port(8080));
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![old],
        ..Default::default()
    };
    let plan = plan_deploy([&first, &second], &snapshot, PlanOptions::default()).unwrap();
    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::ReplaceContainer(_),
            DeployOperation::RunContainer { .. }
        ]
    ));
}

#[test]
fn global_socket_stops_release_each_old_sibling_before_replacement() {
    let mut service = requested(ServiceMode::Global);
    service.ports.push(host_port(8080));
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![
            container('a', '1', &service, &service_id('a')),
            container('b', '1', &service, &service_id('a')),
        ],
        ..Default::default()
    };
    service.container.image = "changed:latest".into();
    let plan = plan_deploy([&service], &snapshot, PlanOptions::default()).unwrap();
    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::StopContainer {
                purpose: ployz_core::StopContainerPurpose::FreeHostPorts,
                ..
            },
            DeployOperation::ReplaceContainer(_),
            DeployOperation::RemoveContainer { .. }
        ]
    ));
}

#[test]
fn wildcard_address_payloads_reserve_their_address_family() {
    use serde_json::json;
    for (wildcard, other, conflict) in [
        (
            "0.0.0.0",
            json!({"kind":"address","address":"127.0.0.1"}),
            true,
        ),
        ("::", json!({"kind":"address","address":"::1"}), true),
        (
            "0.0.0.0",
            json!({"kind":"prefix","prefix":"192.0.2.0/24"}),
            true,
        ),
        (
            "::",
            json!({"kind":"prefix","prefix":"2001:db8::/32"}),
            true,
        ),
        ("0.0.0.0", json!({"kind":"address","address":"::1"}), false),
        (
            "::",
            json!({"kind":"prefix","prefix":"192.0.2.0/24"}),
            false,
        ),
    ] {
        for protocol in ["tcp", "udp"] {
            let spec = |name, bind, protocol| {
                serde_json::from_value::<RequestedServiceSpec>(json!({
                "name":name, "mode":{"mode":"replicated","replicas":1},
                "container":{"image":"example.test/api:1","pull_policy":"missing"},
                "ports":[{"mode":"host","bind":bind,"published_port":8080,"container_port":80,"transport_protocol":protocol}]
            })).unwrap()
            };
            let mut first = spec("alpha", json!({"kind":"address","address":wildcard}), "tcp");
            let second = spec("beta", other.clone(), protocol);
            let snapshot = DeploySnapshot {
                machines: vec![machine('1', "first")],
                ..Default::default()
            };
            assert_eq!(
                plan_deploy([&first, &second], &snapshot, PlanOptions::default()).is_err(),
                conflict && protocol == "tcp",
                "{wildcard} {other} {protocol}"
            );
            first.ports.extend(second.ports);
            assert_eq!(
                plan_deploy([&first], &snapshot, PlanOptions::default()).is_err(),
                conflict && protocol == "tcp"
            );
        }
    }
}

#[test]
fn replicated_socket_repair_stops_surplus_siblings_before_replacement() {
    for shared in [false, true] {
        let mut service = requested(ServiceMode::Replicated {
            replicas: NonZeroU32::new(1).unwrap(),
        });
        if shared {
            add_named_volume(&mut service, "data");
        }
        service.ports = vec![host_port(8080)];
        let first = container('a', '1', &service, &service_id('a'));
        service.ports = vec![host_port(9090)];
        let second = container('b', '1', &service, &service_id('b'));
        service.ports.push(host_port(8080));
        service.update.order = Some(UpdateOrder::StopFirst);
        let mut companion = service.clone();
        companion.name = ServiceName::parse("companion").unwrap();
        companion.ports.clear();
        let snapshot = DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![first, second],
            ..Default::default()
        };
        let specs = std::iter::once(&service).chain(shared.then_some(&companion));
        let plan = plan_deploy(specs, &snapshot, PlanOptions::default()).unwrap();
        let ops = operations(&plan);
        assert!(matches!(ops.as_slice(), [
            DeployOperation::StopContainer { container_id: stopped, purpose: ployz_core::StopContainerPurpose::FreeHostPorts, .. },
            DeployOperation::ReplaceContainer(replaced),
            DeployOperation::RemoveContainer { container_id: removed, .. }, ..
        ] if stopped == removed && *stopped != replaced.old_container_id && replaced.spec.update.order == UpdateOrder::StopFirst));
        service.mode = ServiceMode::Replicated {
            replicas: NonZeroU32::new(2).unwrap(),
        };
        assert!(
            plan_deploy(
                std::iter::once(&service).chain(shared.then_some(&companion)),
                &snapshot,
                PlanOptions::default()
            )
            .is_err()
        );
    }
}

#[test]
fn replicated_socket_repair_preserves_each_desired_replica_claim() {
    let mut service = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    });
    service.ports.push(host_port(8080));
    let mut snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), machine('2', "second")],
        containers: vec![
            container('a', '1', &service, &service_id('a')),
            container('b', '1', &service, &service_id('a')),
            container('c', '2', &service, &service_id('a')),
            container('d', '2', &service, &service_id('a')),
        ],
        ..Default::default()
    };
    service.container.image = "changed:latest".into();
    let plan = plan_deploy([&service], &snapshot, PlanOptions::default()).unwrap();
    let ops = operations(&plan);
    assert!(matches!(ops.as_slice(), [
        DeployOperation::StopContainer { machine_id: first, container_id: stop_first, purpose: ployz_core::StopContainerPurpose::FreeHostPorts },
        DeployOperation::ReplaceContainer(replace_first),
        DeployOperation::StopContainer { machine_id: second, container_id: stop_second, purpose: ployz_core::StopContainerPurpose::FreeHostPorts },
        DeployOperation::ReplaceContainer(replace_second),
        DeployOperation::RemoveContainer { container_id: remove_first, .. },
        DeployOperation::RemoveContainer { container_id: remove_second, .. },
    ] if first != second && *first == replace_first.machine_id && *second == replace_second.machine_id
        && stop_first == remove_first && stop_second == remove_second));
    snapshot.machines.truncate(1);
    assert!(plan_deploy([&service], &snapshot, PlanOptions::default()).is_err());
}

#[test]
fn replicated_socket_repair_never_pre_stops_a_used_keeper() {
    let mut service = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    });
    service.ports.push(host_port(8080));
    let keeper = container('a', '1', &service, &service_id('a'));
    let mut old = service.clone();
    old.container.image = "old:latest".into();
    let stale = container('b', '1', &old, &service_id('a'));
    let stale_id = stale.container_id;
    let mut snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![keeper, stale],
        ..Default::default()
    };
    assert!(plan_deploy([&service], &snapshot, PlanOptions::default()).is_err());
    snapshot.machines.push(machine('2', "second"));
    let plan = plan_deploy([&service], &snapshot, PlanOptions::default()).unwrap();
    assert!(matches!(operations(&plan).as_slice(), [
        DeployOperation::RunContainer { machine_id: target, .. },
        DeployOperation::RemoveContainer { container_id: removed, .. },
    ] if *target == machine_id('2') && *removed == stale_id));
}

#[test]
fn replicated_socket_repair_keeps_unforced_order_and_foreign_claims() {
    for shared in [false, true] {
        let mut service = requested(ServiceMode::Replicated {
            replicas: NonZeroU32::new(1).unwrap(),
        });
        if shared {
            add_named_volume(&mut service, "data");
        }
        service.ports = vec![host_port(9090)];
        let surplus = container('a', '1', &service, &service_id('a'));
        service.ports = vec![host_port(8080)];
        let selected = container('b', '1', &service, &service_id('a'));
        let selected_id = selected.container_id;
        let surplus_id = surplus.container_id;
        let mut snapshot = DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![surplus, selected],
            ..Default::default()
        };
        service.container.image = "changed:latest".into();
        service.update.order = Some(UpdateOrder::StartFirst);
        let mut companion = service.clone();
        companion.name = ServiceName::parse("companion").unwrap();
        companion.ports.clear();
        assert!(
            plan_deploy(
                std::iter::once(&service).chain(shared.then_some(&companion)),
                &snapshot,
                PlanOptions::default()
            )
            .is_ok()
        );
        service.ports = vec![host_port(9090)];
        let plan = plan_deploy(
            std::iter::once(&service).chain(shared.then_some(&companion)),
            &snapshot,
            PlanOptions::default(),
        )
        .unwrap();
        assert!(matches!(operations(&plan).as_slice(), [
            DeployOperation::StopContainer { container_id: stopped, purpose: ployz_core::StopContainerPurpose::FreeHostPorts, .. },
            DeployOperation::ReplaceContainer(replaced),
            DeployOperation::RemoveContainer { container_id: removed, .. }, ..
        ] if *stopped == surplus_id && *removed == surplus_id && replaced.old_container_id == selected_id && replaced.spec.update.order == UpdateOrder::StartFirst));
        let mut foreign = container('c', '1', &service, &service_id('c'));
        foreign
            .try_update(|parts| parts.namespace = Namespace::parse("foreign").unwrap())
            .unwrap();
        snapshot.containers.push(foreign);
        assert!(
            plan_deploy(
                std::iter::once(&service).chain(shared.then_some(&companion)),
                &snapshot,
                PlanOptions::default()
            )
            .is_err()
        );
    }
}

#[test]
fn next_deploy_relocates_after_policy_edits_but_preserves_volume_locality() {
    for revoke_role in [false, true] {
        let mut service = replicated("api", 1);
        service.placement.constraints = [label_constraint("runtime")].into();
        let mut first = machine('1', "first");
        let mut second = machine('2', "second");
        first
            .machine
            .labels
            .insert("fixture".parse().unwrap(), "runtime".parse().unwrap());
        second
            .machine
            .labels
            .insert("fixture".parse().unwrap(), "runtime".parse().unwrap());
        let mut snapshot = DeploySnapshot {
            machines: vec![first, second],
            containers: vec![container('a', '1', &service, &service_id('a'))],
            ..Default::default()
        };
        assert!(
            plan_deploy([&service], &snapshot, PlanOptions::default())
                .unwrap()
                .operations
                .is_empty()
        );
        if revoke_role {
            snapshot
                .machines
                .first_mut()
                .unwrap()
                .machine
                .accepts_services = false;
        } else {
            snapshot
                .machines
                .first_mut()
                .unwrap()
                .machine
                .labels
                .remove("fixture");
        }
        let plan = plan_deploy([&service], &snapshot, PlanOptions::default()).unwrap();
        assert!(matches!(operations(&plan).as_slice(), [
            DeployOperation::RunContainer { machine_id: destination, .. },
            DeployOperation::RemoveContainer { machine_id: previous, .. },
        ] if *destination == machine_id('2') && *previous == machine_id('1')));

        add_named_volume(&mut service, "data");
        snapshot.containers = vec![container('a', '1', &service, &service_id('a'))];
        snapshot.volume_snapshot =
            VolumeSnapshot::try_from_observations([observed_volume(machine_id('1'), "data")])
                .unwrap();
        let failure = plan_deploy([&service], &snapshot, PlanOptions::default()).unwrap_err();
        let expected = if revoke_role {
            "Volume data is on first, which doesn't run services"
        } else {
            "app_data"
        };
        assert!(failure.to_string().contains(expected), "{failure}");
    }
}

#[test]
fn explicit_id_constraint_cannot_bypass_service_acceptance() {
    let mut service = replicated("api", 1);
    service.placement.constraints =
        [PlacementConstraint::parse(format!("node.id == {}", machine_id('1'))).unwrap()].into();
    let mut target = machine('1', "builder");
    target.machine.accepts_services = false;
    let snapshot = DeploySnapshot {
        machines: vec![target],
        ..Default::default()
    };
    assert_no_eligible(
        plan_deploy([&service], &snapshot, PlanOptions::default()),
        &[EliminatingConstraint::WorkNotAccepted {
            names: vec![MachineName::parse("builder").unwrap()],
        }],
        &["do not accept this work"],
    );
}

#[test]
fn equivalent_constraint_formatting_keeps_running_containers() {
    let mut service = replicated("api", 1);
    service.placement.constraints =
        [PlacementConstraint::parse("node.labels.fixture==FIRST").unwrap()].into();
    let current = container('a', '1', &service, &service_id('a'));
    service.placement.constraints =
        [PlacementConstraint::parse(" node.labels.fixture == first ").unwrap()].into();
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![current],
        ..Default::default()
    };
    assert!(
        plan_deploy([&service], &snapshot, PlanOptions::default())
            .unwrap()
            .operations
            .is_empty()
    );
}

#[test]
fn reserved_ingress_deploy_uses_ingress_acceptance_independently() {
    let service = ployz_core::caddy_service_spec("caddy:test");
    let mut target = machine('1', "edge");
    target.machine.accepts_services = false;
    let mut snapshot = DeploySnapshot {
        machines: vec![target],
        ..Default::default()
    };
    let intent = DeployIntent::apply_all(
        ployz_core::QualifiedService::system_ingress().namespace,
        [&service],
        PlanOptions::default(),
    );
    let plan = preview_deploy(&intent, &snapshot).unwrap();
    assert!(
        matches!(operations(&plan).as_slice(), [DeployOperation::RunContainer { machine_id: destination, .. }] if *destination == machine_id('1'))
    );
    snapshot
        .machines
        .first_mut()
        .unwrap()
        .machine
        .accepts_services = true;
    snapshot
        .machines
        .first_mut()
        .unwrap()
        .machine
        .accepts_ingress = false;
    assert!(
        preview_deploy(&intent, &snapshot)
            .unwrap_err()
            .to_string()
            .contains("do not accept this work")
    );
}

#[test]
fn unrecognized_membership_cannot_admit_creation_after_wire_roundtrip_or_remove_existing_work() {
    use ployz_core::MembershipEvidence;
    let requested = requested(ServiceMode::Global);
    let existing = container('a', '2', &requested, &ServiceId::random());
    for raw in ["down", "up", "suspect"] {
        let mut peer = machine('2', "future-peer");
        peer.membership = MembershipObservation::Unrecognized(raw.into());
        peer.membership_evidence = Some(MembershipEvidence::Unrecognized { raw: raw.into() });
        let peer: MachineObservation =
            serde_json::from_value(serde_json::to_value(peer).unwrap()).unwrap();
        let snapshot = DeploySnapshot {
            machines: vec![machine('1', "known-peer"), peer],
            containers: vec![existing.clone()],
            ..Default::default()
        };
        let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();
        assert!(
            matches!(operations(&plan).as_slice(), [DeployOperation::RunContainer { machine_id: placed, .. }] if *placed == machine_id('1'))
        );
    }
}

#[test]
fn deploy_replaces_the_serving_container_and_removes_a_requested_one_that_never_passed() {
    for mode in [
        ServiceMode::Replicated {
            replicas: NonZeroU32::new(1).unwrap(),
        },
        ServiceMode::Global,
    ] {
        for (health, stray_sorts_first) in [
            (HealthObservation::Starting, false),
            (HealthObservation::Starting, true),
            (HealthObservation::Unhealthy, false),
            (HealthObservation::Unhealthy, true),
        ] {
            let requested = requested(mode.clone());
            let mut current = requested.clone();
            current.container.image = "ghcr.io/getployz/api:old".into();
            let current_service_id = service_id('a');
            let (serving_id, stray_id) = if stray_sorts_first {
                ('c', 'b')
            } else {
                ('b', 'c')
            };
            let serving = container(serving_id, '1', &current, &current_service_id);
            let mut stray = container(stray_id, '1', &requested, &current_service_id);
            stray
                .try_update(|parts| {
                    parts.runtime = ContainerRuntimeObservation::Running {
                        health: health.clone(),
                    }
                })
                .unwrap();
            let containers = if stray_sorts_first {
                vec![stray, serving]
            } else {
                vec![serving, stray]
            };
            let plan = plan_deploy(
                [&requested],
                &DeploySnapshot {
                    machines: vec![machine('1', "first")],
                    containers,
                    ..Default::default()
                },
                PlanOptions::default(),
            )
            .unwrap();

            let operations = operations(&plan);
            assert!(
                operations.iter().any(|operation| matches!(
                    operation,
                    DeployOperation::ReplaceContainer(ReplacementOperation {
                        old_container_id,
                        ..
                    }) if old_container_id == &container_id(serving_id)
                )),
                "{mode:?} {health:?} stray sorts first {stray_sorts_first}: {operations:?}"
            );
            assert!(
                operations.iter().any(|operation| matches!(
                    operation,
                    DeployOperation::RemoveContainer { container_id: removed, .. }
                        if removed == &container_id(stray_id)
                )),
                "{mode:?} {health:?} stray sorts first {stray_sorts_first}: {operations:?}"
            );
            assert!(
                !operations.iter().any(|operation| matches!(
                    operation,
                    DeployOperation::RemoveContainer { container_id: removed, .. }
                        | DeployOperation::StopContainer { container_id: removed, .. }
                        if removed == &container_id(serving_id)
                )),
                "{mode:?} {health:?} stray sorts first {stray_sorts_first}: {operations:?}"
            );
        }
    }
}

#[test]
fn a_matching_copy_another_run_left_pending_is_replaced_not_counted_as_serving() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    });
    let service = service_id('a');
    let rotated = |hex, machine_hex, rotation| {
        let mut copy = container(hex, machine_hex, &requested, &service);
        copy.try_update(|parts| parts.rotation = Some(rotation))
            .unwrap();
        copy
    };
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first"), machine('2', "second")],
        containers: vec![
            rotated('b', '1', ployz_core::CopyRotation::In),
            rotated('c', '2', ployz_core::CopyRotation::Pending),
        ],
        ..Default::default()
    };

    let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();

    assert!(
        matches!(
            operations(&plan).as_slice(),
            [DeployOperation::ReplaceContainer(replacement)]
                if replacement.machine_id == machine_id('2')
                    && replacement.old_container_id == container_id('c')
        ),
        "{:?}",
        operations(&plan)
    );
}

#[test]
fn the_next_deploy_removes_the_copies_a_failed_run_left_pending() {
    for (replicas, same_spec) in [(1, false), (2, false), (1, true), (2, true)] {
        let requested = requested(ServiceMode::Replicated {
            replicas: NonZeroU32::new(replicas).unwrap(),
        });
        let mut serving = requested.clone();
        serving.container.image = "ghcr.io/getployz/api:old".into();
        let mut failed = requested.clone();
        if !same_spec {
            failed.container.image = "ghcr.io/getployz/api:failed".into();
        }
        let service = service_id('a');
        let copy = |hex, spec, rotation| {
            let mut copy = container(hex, '1', spec, &service);
            copy.try_update(|parts| parts.rotation = Some(rotation))
                .unwrap();
            copy
        };
        let mut containers = Vec::new();
        for (serving_hex, failed_hex) in
            [('b', 'c'), ('d', 'e')].into_iter().take(replicas as usize)
        {
            containers.push(copy(serving_hex, &serving, ployz_core::CopyRotation::In));
            containers.push(copy(failed_hex, &failed, ployz_core::CopyRotation::Pending));
        }
        let snapshot = DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: containers.clone(),
            ..Default::default()
        };

        let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();

        let operations = operations(&plan);
        for existing in &containers {
            let id = existing.container_id;
            assert!(
                operations.iter().any(|operation| matches!(
                    operation,
                    DeployOperation::ReplaceContainer(ReplacementOperation { old_container_id, .. })
                        | DeployOperation::RemoveContainer { container_id: old_container_id, .. }
                        if *old_container_id == id
                )),
                "replicas {replicas}, same spec {same_spec}: {id} stays: {operations:?}"
            );
        }
    }
}

/// A copy a Deploy left Pending, labeled as the daemon labels it: its run, and the
/// slot that run created it in.
fn left_by(mut copy: ContainerObservation, run: &str, key: &str) -> ContainerObservation {
    copy.try_update(|parts| {
        parts.rotation = Some(ployz_core::CopyRotation::Pending);
        parts
            .labels
            .insert(ployz_core::labels::DEPLOYMENT_ID.into(), run.into());
        parts
            .labels
            .insert(ployz_core::labels::CREATION_KEY.into(), key.into());
    })
    .unwrap();
    copy
}

/// Removals in a group every Service waits for, so they run before any copy
/// starts, whatever later fails.
fn removed_first(plan: &ployz::deploy::DeployPlan) -> Vec<ContainerId> {
    let operations = &plan.preview().operations;
    plan.groups()
        .iter()
        .filter(|group| group.role == ployz::deploy::GroupRole::Prelude)
        .flat_map(|group| &group.steps)
        .flat_map(|step| match step {
            ployz::deploy::Stage::Serial(index) => vec![*index],
            ployz::deploy::Stage::Parallel(indices) => indices.clone(),
        })
        .filter_map(|index| {
            let DeployOperation::RemoveContainer { container_id, .. } =
                operations.get(index)?.operation
            else {
                return None;
            };
            Some(container_id)
        })
        .collect()
}

#[test]
fn a_deploy_removes_first_the_copies_ended_runs_left_pending() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let mut serving = requested.clone();
    serving.container.image = "ghcr.io/getployz/api:old".into();
    let service = service_id('a');
    let mut live = container('b', '1', &serving, &service);
    live.try_update(|parts| parts.rotation = Some(ployz_core::CopyRotation::In))
        .unwrap();
    let snapshot = DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![
            live,
            left_by(
                container('c', '1', &requested, &service),
                "run-1",
                "run-1:00000000000000ff:0",
            ),
            left_by(
                container('d', '1', &requested, &service),
                "run-2",
                "run-2:00000000000000ff:0",
            ),
            left_by(
                container('e', '1', &requested, &service),
                "running",
                "running:00000000000000ff:0",
            ),
            left_by(
                container('f', '1', &requested, &service),
                "run-1",
                "move:bbbb",
            ),
        ],
        ..Default::default()
    };
    let options = PlanOptions::default().as_runner(ployz_core::RunnerOptions {
        operator: ployz_core::Operator::Cloud,
        ended_runs: ["run-1", "run-2"]
            .into_iter()
            .map(|run| ployz_core::DeploymentLogId::parse(run).unwrap())
            .collect(),
        recreate: std::collections::BTreeSet::new(),
        signed: BTreeSet::new(),
    });
    let intent = DeployIntent::apply_all(Namespace::parse("app").unwrap(), [&requested], options);

    let plan = ployz::deploy::plan_deploy(&intent, &snapshot).unwrap();

    assert_eq!(removed_first(&plan), [container_id('c'), container_id('d')]);
    let unscoped = DeployIntent::apply_all(
        Namespace::parse("app").unwrap(),
        [&requested],
        PlanOptions::default(),
    );
    let plan = ployz::deploy::plan_deploy(&unscoped, &snapshot).unwrap();
    assert_eq!(removed_first(&plan), []);
}

#[test]
fn a_signed_deploy_replaces_a_copy_its_machine_admitted_unsigned() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    let service = service_id('a');
    let snapshot = |admission| DeploySnapshot {
        machines: vec![machine('1', "first")],
        containers: vec![{
            let mut copy = container('b', '1', &requested, &service);
            copy.try_update(|parts| parts.admission = admission)
                .unwrap();
            copy
        }],
        ..Default::default()
    };
    let signing = PlanOptions::default().as_runner(ployz_core::RunnerOptions {
        operator: ployz_core::Operator::Cloud,
        ended_runs: BTreeSet::new(),
        recreate: BTreeSet::new(),
        signed: BTreeSet::from([requested.name.clone()]),
    });
    let replaced = |admission, options: &PlanOptions| {
        let intent = DeployIntent::apply_all(
            Namespace::parse("app").unwrap(),
            [&requested],
            options.clone(),
        );
        operations(&ployz::deploy::plan_deploy(&intent, &snapshot(admission)).unwrap())
            .iter()
            .any(|operation| matches!(operation, DeployOperation::ReplaceContainer(_)))
    };

    assert!(replaced(Some(ployz_core::Admission::Free), &signing));
    assert!(
        !replaced(Some(ployz_core::Admission::Free), &PlanOptions::default()),
        "an unsigned Deploy keeps it"
    );
    assert!(
        !replaced(None, &signing),
        "a copy from a Machine that doesn't report stays"
    );
    assert!(!replaced(Some(ployz_core::Admission::Signed), &signing));
}

/// Suspect, or Up with a daemon that took the call and never replied, as a frozen one
/// does: either way the read's deadline ran out and nothing new starts there.
#[test]
fn a_machine_that_did_not_answer_its_listing_gets_no_new_copy() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    });
    for membership in [MembershipObservation::Suspect, MembershipObservation::Up] {
        let mut silent = machine('2', "silent");
        silent.membership = membership.clone();
        let snapshot = DeploySnapshot {
            machines: vec![machine('1', "live"), silent],
            container_failures: vec![ployz_core::MachineFailure {
                machine_id: machine_id('2'),
                error: ployz_core::RpcError::unanswered_target(
                    ployz_core::RpcErrorCode::Unavailable,
                    "target Machine RPC timed out".into(),
                ),
            }],
            ..Default::default()
        };

        let plan = plan_deploy([&requested], &snapshot, PlanOptions::default()).unwrap();

        let targets = operations(&plan)
            .iter()
            .filter_map(|operation| {
                let DeployOperation::RunContainer { machine_id, .. } = operation else {
                    return None;
                };
                Some(*machine_id)
            })
            .collect::<Vec<_>>();
        assert_eq!(
            targets,
            [machine_id('1'), machine_id('1')],
            "{membership:?}"
        );
    }
}

#[test]
fn a_docker_volume_is_not_placed_while_a_silent_machine_may_hold_it() {
    let mut requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut requested, "data");
    let unanswered = || {
        ployz_core::RpcError::unanswered_target(
            ployz_core::RpcErrorCode::Unavailable,
            "target Machine RPC timed out".into(),
        )
    };
    // A Suspect Machine that never answered, an Up one whose daemon the entry couldn't
    // dial (#3396), and an Up one whose daemon never replied.
    for (membership, error) in [
        (MembershipObservation::Suspect, unanswered()),
        (MembershipObservation::Up, unanswered().undialed_target()),
        (MembershipObservation::Up, unanswered()),
    ] {
        let mut silent = machine('2', "silent");
        silent.membership = membership;
        let snapshot = DeploySnapshot {
            machines: vec![machine('1', "live"), silent],
            container_failures: vec![ployz_core::MachineFailure {
                machine_id: machine_id('2'),
                error: error.clone(),
            }],
            volume_snapshot: VolumeSnapshot::try_from_parts(
                Vec::new(),
                Vec::new(),
                vec![ployz_core::MachineFailure {
                    machine_id: machine_id('2'),
                    error,
                }],
                Vec::new(),
            )
            .unwrap(),
            ..Default::default()
        };

        let result = plan_deploy([&requested], &snapshot, PlanOptions::default());

        let error = result.expect_err("nothing is placed and no Volume is created");
        assert_eq!(
            error.to_string(),
            "silent did not answer, so whether it holds Docker volume data is unknown. \
             Bring silent back, or deploy the other services by name"
        );
    }
}

/// A Service that mounts a born Volume lands only on a Server that checks where a
/// Volume's data began. With none, the Deploy is refused before anything starts and names
/// the Server to upgrade. A Legacy mount needs no such Server.
#[test]
fn born_volume_lands_only_on_a_server_that_checks_origins() {
    let mut service = replicated("files", 1);
    add_named_volume(&mut service, "data");
    let legacy = service.clone();
    let mut volumes = service.volume_graph().volumes().to_vec();
    let mounts = service.volume_graph().mounts().to_vec();
    let [volume] = volumes.as_mut_slice() else {
        panic!("one volume")
    };
    volume.source = ployz_core::RawVolumeSource::Ordinary {
        name: ployz_core::DockerVolumeName::parse("data").unwrap(),
        driver: ployz_core::VolumeDriver::parse("local", BTreeMap::new()).unwrap(),
        labels: BTreeMap::new(),
        birth: Some(ployz_core::VolumeBirth {
            namespace: ployz_core::Namespace::parse("app").unwrap(),
            name: "data".to_owned(),
            made_at: ployz_core::authority::Timestamp::from_millis(1).unwrap(),
            nonce: ployz_core::BirthNonce::from_bytes([7; 16]),
        }),
    }
    .admit()
    .unwrap();
    service
        .set_volume_graph(ployz_core::ServiceVolumeGraph::parse(volumes, mounts).unwrap())
        .unwrap();
    let with = |capabilities: &[&str], hex, name| {
        let mut server = machine(hex, name);
        server.machine.runtime.capabilities =
            capabilities.iter().map(|c| (*c).to_owned()).collect();
        server
    };
    let old = with(&["ployz.volume.copy.v1"], '1', "old");
    let new = with(
        &["ployz.volume.copy.v1", ployz_core::VOLUME_ORIGIN_CAPABILITY],
        '2',
        "new",
    );

    let both = DeploySnapshot {
        machines: vec![old.clone(), new],
        ..Default::default()
    };
    let plan = plan_deploy([&service], &both, PlanOptions::default()).unwrap();
    assert!(
        matches!(
            operations(&plan).as_slice(),
            [DeployOperation::RunContainer { machine_id: placed, .. }] if *placed == machine_id('2')
        ),
        "{:?}",
        operations(&plan)
    );

    let only_old = DeploySnapshot {
        machines: vec![old],
        ..Default::default()
    };
    let refused = plan_deploy([&service], &only_old, PlanOptions::default()).unwrap_err();
    assert!(
        refused.to_string().starts_with(
            "Upgrade old first. files mounts a volume that needs a newer Ployz on every server"
        ),
        "{refused}"
    );
    plan_deploy([&legacy], &only_old, PlanOptions::default())
        .expect("a Legacy Volume lands on any Server");
}
