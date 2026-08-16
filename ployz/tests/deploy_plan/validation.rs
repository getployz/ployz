use super::support::*;
#[test]
fn missing_volume_and_config_references_are_rejected_before_placement() {
    let mut missing_volume = requested(ServiceMode::Global);
    missing_volume.mounts.push(ServiceMount {
        volume: ServiceVolumeReference::parse("missing").unwrap(),
        target: ContainerPath::parse("/missing").unwrap(),
        read_only: false,
    });
    assert_eq!(
        plan_deploy(
            [&missing_volume],
            &DeploySnapshot::default(),
            PlanOptions::default(),
        ),
        Err(PlanError::UnknownVolumeReference {
            reference: ServiceVolumeReference::parse("missing").unwrap(),
        })
    );

    let mut missing_config = requested(ServiceMode::Global);
    missing_config
        .container
        .config_mounts
        .push(ployz_core::ConfigMount {
            config_name: "missing".into(),
            target: None,
            uid: None,
            gid: None,
            mode: None,
        });
    assert_eq!(
        plan_deploy(
            [&missing_config],
            &DeploySnapshot::default(),
            PlanOptions::default(),
        ),
        Err(PlanError::UnknownConfigName {
            name: "missing".into(),
        })
    );
}

#[test]
fn global_volume_existing_on_one_machine_is_created_on_the_other() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "data");
    let plan = plan_deploy(
        [&requested],
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
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        plan.operations.as_slice(),
        [
            DeployOperation::CreateVolume { machine_id: target, .. },
            DeployOperation::RunContainer { .. },
            DeployOperation::RunContainer { .. },
        ] if target == &machine_id('2')
    ));
}

#[test]
fn global_named_volume_existing_on_every_machine_is_not_created() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "data");
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first"), machine('2', "second")],
            volumes: vec![
                ployz::deploy::ObservedDockerVolume {
                    id: DockerVolumeId {
                        machine_id: machine_id('1'),
                        name: DockerVolumeName::parse("data").unwrap(),
                    },
                    driver: "local".into(),
                    options: Default::default(),
                },
                ployz::deploy::ObservedDockerVolume {
                    id: DockerVolumeId {
                        machine_id: machine_id('2'),
                        name: DockerVolumeName::parse("data").unwrap(),
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

    assert!(matches!(
        plan.operations.as_slice(),
        [
            DeployOperation::RunContainer { .. },
            DeployOperation::RunContainer { .. },
        ]
    ));
}

#[test]
fn placement_seed_randomizes_equal_priority_round_robin_order() {
    let requested = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(3).unwrap(),
    });
    let snapshot = DeploySnapshot {
        machines: vec![
            machine('1', "first"),
            machine('2', "second"),
            machine('3', "third"),
        ],
        ..Default::default()
    };
    let targets = |placement_seed| {
        plan_deploy(
            [&requested],
            &snapshot,
            PlanOptions {
                placement_seed,
                ..Default::default()
            },
        )
        .unwrap()
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
        .collect::<Vec<_>>()
    };

    assert_ne!(targets(0), targets(1));
}

#[test]
fn compatible_named_volume_aliases_and_repeated_mounts_create_once() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "data");
    requested
        .mounts
        .push(requested.mounts.first().unwrap().clone());
    let source = requested.volumes.first().unwrap().source.clone();
    let alias = ServiceVolumeReference::parse("data-alias").unwrap();
    requested.volumes.push(ServiceVolume {
        reference: alias.clone(),
        source,
    });
    requested.mounts.push(ServiceMount {
        volume: alias,
        target: ContainerPath::parse("/alias").unwrap(),
        read_only: false,
    });

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
        [
            DeployOperation::CreateVolume { .. },
            DeployOperation::RunContainer { .. }
        ]
    ));
}

#[test]
fn empty_spec_iterator_returns_an_empty_plan() {
    let plan = plan_deploy([], &DeploySnapshot::default(), PlanOptions::default()).unwrap();

    assert!(plan.operations.is_empty());
}

#[test]
fn conflicting_named_volume_aliases_are_rejected() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "data");
    let alias = ServiceVolumeReference::parse("data-alias").unwrap();
    requested.volumes.push(ServiceVolume {
        reference: alias.clone(),
        source: VolumeSource::Named {
            name: DockerVolumeName::parse("data").unwrap(),
            external: false,
            driver: Some(ployz_core::VolumeDriver {
                name: "nfs".into(),
                options: Default::default(),
            }),
            labels: Default::default(),
            no_copy: false,
            subpath: None,
        },
    });
    requested.mounts.push(ServiceMount {
        volume: alias,
        target: ContainerPath::parse("/alias").unwrap(),
        read_only: false,
    });

    assert_eq!(
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "first")],
                ..Default::default()
            },
            PlanOptions::default(),
        ),
        Err(PlanError::ConflictingDockerVolumeDefinitions {
            name: DockerVolumeName::parse("data").unwrap(),
        })
    );
}
