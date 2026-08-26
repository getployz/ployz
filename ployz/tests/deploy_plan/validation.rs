use super::support::*;
#[test]
fn missing_volume_and_config_references_cannot_enter_a_requested_spec() {
    let mut dangling_volume = serde_json::to_value(requested(ServiceMode::Global)).unwrap();
    *dangling_volume
        .get_mut("mounts")
        .expect("serialized spec has mounts") =
        serde_json::json!([{"volume":"missing","target":"/missing"}]);
    assert!(
        serde_json::from_value::<RequestedServiceSpec>(dangling_volume)
            .unwrap_err()
            .to_string()
            .contains("undeclared Service Volume")
    );

    let mut dangling_config = serde_json::to_value(requested(ServiceMode::Global)).unwrap();
    *dangling_config
        .get_mut("container")
        .and_then(|container| container.get_mut("config_mounts"))
        .expect("serialized spec has config mounts") =
        serde_json::json!([{"config_name":"missing"}]);
    assert!(
        serde_json::from_value::<RequestedServiceSpec>(dangling_config)
            .unwrap_err()
            .to_string()
            .contains("undeclared config")
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
        [
            DeployOperation::RunContainer { .. },
            DeployOperation::RunContainer { .. },
        ]
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
fn global_named_volume_existing_on_every_machine_is_not_created() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "data");
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first"), machine('2', "second")],
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![
                observed_volume(machine_id('1'), "data"),
                observed_volume(machine_id('2'), "data"),
            ])
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        operations(&plan).as_slice(),
        [
            DeployOperation::RunContainer { .. },
            DeployOperation::RunContainer { .. },
        ]
    ));
    assert!(plan.volumes_to_create.is_empty());
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
        .map(|row| match &row.operation {
            DeployOperation::RunContainer { machine_id, .. } => *machine_id,
            other @ (DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::RemoveContainer { .. }
            | DeployOperation::ReplaceContainer(..)
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::RemoveVolume { .. }) => panic!("unexpected operation: {other:?}"),
        })
        .collect::<Vec<_>>()
    };

    assert_ne!(targets(0), targets(1));
}

#[test]
fn compatible_named_volume_aliases_and_repeated_mounts_create_once() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "data");
    let mut volumes = requested.volume_graph.volumes().to_vec();
    let mut mounts = requested.volume_graph.mounts().to_vec();
    mounts.push(mounts.first().unwrap().clone());
    let source = volumes.first().unwrap().source.clone();
    let alias = ServiceVolumeReference::parse("data-alias").unwrap();
    volumes.push(ServiceVolume {
        reference: alias.clone(),
        source,
    });
    mounts.push(ServiceMount {
        volume: alias,
        target: ContainerPath::parse("/alias").unwrap(),
        read_only: false,
        no_copy: false,
        subpath: None,
    });
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
        [DeployOperation::RunContainer { .. }]
    ));
    assert_eq!(plan.volumes_to_create.len(), 1);
}

#[test]
fn unused_volume_definition_does_not_create_a_docker_volume() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "data");
    let mut volumes = requested.volume_graph.volumes().to_vec();
    let mounts = requested.volume_graph.mounts().to_vec();
    volumes.push(ServiceVolume {
        reference: ServiceVolumeReference::parse("logs").unwrap(),
        source: VolumeSource::Ordinary {
            name: DockerVolumeName::parse("logs").unwrap(),
            driver: ployz_core::VolumeDriver::parse("local", Default::default()).unwrap(),
            labels: Default::default(),
        },
    });
    requested.volume_graph = ployz_core::ServiceVolumeGraph::parse(volumes, mounts).unwrap();

    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            volume_snapshot: VolumeSnapshot::try_from_observations(vec![owned_volume(
                machine_id('1'),
                "logs",
            )])
            .expect("valid Volume Snapshot fixture"),
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        operations(&plan).as_slice(),
        [DeployOperation::RunContainer { .. }]
    ));
    assert_eq!(plan.volumes_to_create.len(), 1);
    assert_eq!(
        plan.volumes_to_create
            .first()
            .expect("missing managed Volume is previewed")
            .name
            .as_str(),
        "app_data"
    );
    assert_eq!(
        plan.preserved_volumes,
        vec![ployz_core::PreservedVolume {
            id: owned_volume(machine_id('1'), "logs").id,
            machine_name: Some(MachineName::parse("first").unwrap()),
        }]
    );
}

#[test]
fn project_scoping_rejects_incompatible_physical_volume_aliases() {
    let mut requested = requested(ServiceMode::Global);
    add_named_volume(&mut requested, "data");
    let mut volumes = requested.volume_graph.volumes().to_vec();
    let mounts = requested.volume_graph.mounts().to_vec();
    volumes.push(ServiceVolume {
        reference: ServiceVolumeReference::parse("external").unwrap(),
        source: VolumeSource::External {
            name: DockerVolumeName::parse("app_data").unwrap(),
        },
    });
    requested.volume_graph = ployz_core::ServiceVolumeGraph::parse(volumes, mounts).unwrap();

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
            name: DockerVolumeName::parse("app_data").unwrap(),
        })
    );
}

#[test]
fn empty_spec_iterator_returns_an_empty_plan() {
    let plan = plan_deploy([], &DeploySnapshot::default(), PlanOptions::default()).unwrap();

    assert!(plan.operations.is_empty());
}
