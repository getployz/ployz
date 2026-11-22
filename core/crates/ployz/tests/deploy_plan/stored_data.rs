use super::support::*;

const DATA_LOSS_PROTOCOL: &str = "Deploy Plan destroyed stored data; Deploy is a Data Loss path and must adopt the named-confirmation protocol from #355 (see #354)";

fn destroys_stored_data(operation: &DeployOperation) -> bool {
    match operation {
        DeployOperation::RemoveVolume { .. } => true,
        DeployOperation::WaitHealthy { .. }
        | DeployOperation::RunContainer { .. }
        | DeployOperation::StopContainer { .. }
        | DeployOperation::RemoveContainer { .. }
        | DeployOperation::ReplaceContainer(_)
        | DeployOperation::UpdateContainer(_)
        | DeployOperation::StopHook { .. }
        | DeployOperation::PrepareVolumes { .. }
        | DeployOperation::RunHook { .. } => false,
    }
}

fn assert_plan_cannot_destroy_stored_data(plan: &ployz::deploy::DeployPreview, label: &str) {
    for row in &plan.operations {
        assert!(
            !destroys_stored_data(&row.operation),
            "{label}: {DATA_LOSS_PROTOCOL}: {row:?}"
        );
    }
}

#[test]
fn deploy_plan_cannot_emit_an_operation_that_destroys_stored_data() {
    let unused_volume = {
        let requested = requested(ServiceMode::Global);
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "first")],
                volume_snapshot: VolumeSnapshot::try_from_observations(vec![observed_volume(
                    machine_id('1'),
                    "orphan",
                )])
                .expect("valid Volume Snapshot fixture"),
                ..Default::default()
            },
            PlanOptions::default(),
        )
        .unwrap()
    };
    assert_plan_cannot_destroy_stored_data(&unused_volume, "orphan Docker Volume left behind");
    assert!(unused_volume.volumes_to_create.is_empty());

    let mut scaled = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut scaled, "data");
    let mut previous = scaled.clone();
    previous.mode = ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    };
    let sid = service_id('a');
    let scale_down = plan_deploy(
        [&scaled],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![
                container('b', '1', &previous, &sid),
                container('c', '1', &previous, &sid),
            ],
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
    assert_plan_cannot_destroy_stored_data(&scale_down, "scale down keeps the named volume");

    let mut replacement = requested(ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    });
    add_named_volume(&mut replacement, "data");
    let mut current = replacement.clone();
    current.container.image = "ghcr.io/getployz/api:old".into();
    let replace = plan_deploy(
        [&replacement],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container('b', '1', &current, &sid)],
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
    assert_plan_cannot_destroy_stored_data(&replace, "replace keeps the named volume");
}
