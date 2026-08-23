use super::support::*;
use ployz_core::{ReplacementOperation, ResolvedUpdateConfig, UpdateOrder};

const DATA_LOSS_PROTOCOL: &str = "Deploy Plan destroyed stored data; Deploy is a Data Loss path and must adopt the named-confirmation protocol from #355 (see #354)";

fn destroys_stored_data(operation: &DeployOperation) -> bool {
    match operation {
        DeployOperation::RemoveVolume { .. } => true,
        DeployOperation::CreateVolume { .. }
        | DeployOperation::CreateProvisionedVolume { .. }
        | DeployOperation::WaitHealthy { .. }
        | DeployOperation::RunContainer { .. }
        | DeployOperation::StopContainer { .. }
        | DeployOperation::RemoveContainer { .. }
        | DeployOperation::ReplaceContainer(_)
        | DeployOperation::StopHook { .. }
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

fn resolved(spec: &RequestedServiceSpec) -> ployz_core::ResolvedServiceSpec {
    spec.to_resolved(
        service_id('a'),
        ResolvedUpdateConfig {
            order: UpdateOrder::StartFirst,
            monitor_millis: spec.update.monitor_millis,
        },
    )
}

#[test]
fn deploy_operation_variants_do_not_destroy_stored_data() {
    let mut spec = requested(ServiceMode::Global);
    add_named_volume(&mut spec, "data");
    let volume = spec
        .volume_graph
        .volumes()
        .first()
        .expect("named volume was added")
        .clone();
    let spec = resolved(&spec);
    let machine_id = machine_id('1');
    let container_id = container_id('b');
    let operations = [
        DeployOperation::CreateVolume { machine_id, volume },
        DeployOperation::RunContainer {
            machine_id,
            spec: spec.clone(),
            skip_health_monitor: true,
        },
        DeployOperation::StopContainer {
            machine_id,
            container_id,
        },
        DeployOperation::RemoveContainer {
            machine_id,
            container_id,
        },
        DeployOperation::ReplaceContainer(ReplacementOperation {
            machine_id,
            old_container_id: container_id,
            spec: spec.clone(),
            skip_health_monitor: true,
        }),
        DeployOperation::StopHook {
            machine_id,
            container_id,
        },
        DeployOperation::RunHook {
            machine_id,
            spec,
            old_hook_containers: vec![(machine_id, container_id)],
        },
    ];
    for operation in operations {
        assert!(
            !destroys_stored_data(&operation),
            "{DATA_LOSS_PROTOCOL}: {operation:?}"
        );
    }
    assert!(destroys_stored_data(&DeployOperation::RemoveVolume {
        id: DockerVolumeId {
            machine_id,
            name: DockerVolumeName::parse("data").unwrap(),
        },
    }));
}

#[test]
fn deploy_plan_cannot_emit_an_operation_that_destroys_stored_data() {
    let unused_volume = {
        let requested = requested(ServiceMode::Global);
        plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![machine('1', "first")],
                volumes: vec![observed_volume(machine_id('1'), "orphan")],
                ..Default::default()
            },
            PlanOptions::default(),
        )
        .unwrap()
    };
    assert_plan_cannot_destroy_stored_data(&unused_volume, "orphan Docker Volume left behind");
    assert!(
        unused_volume
            .operations
            .iter()
            .all(|row| !matches!(row.operation, DeployOperation::CreateVolume { .. })),
        "an unused snapshot volume must not be created or removed: {DATA_LOSS_PROTOCOL}"
    );

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
            volumes: vec![observed_volume(machine_id('1'), "data")],
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
            volumes: vec![observed_volume(machine_id('1'), "data")],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();
    assert_plan_cannot_destroy_stored_data(&replace, "replace keeps the named volume");
}
