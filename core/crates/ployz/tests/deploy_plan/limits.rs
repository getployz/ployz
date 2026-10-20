//! A limits-only change updates a running Container in place on a Server
//! whose daemon can, and replaces it everywhere else.

use super::support::*;
use ployz_core::{ByteQuantity, CpuNanos, UPDATE_CONTAINER_LIMITS_CAPABILITY};

fn capable(hex: char, name: &str) -> MachineObservation {
    let mut observed = machine(hex, name);
    observed
        .machine
        .runtime
        .capabilities
        .insert(UPDATE_CONTAINER_LIMITS_CAPABILITY.to_owned());
    observed
}

fn limited(mode: ServiceMode, memory_mib: i64) -> RequestedServiceSpec {
    let mut spec = requested(mode);
    spec.container.resources.memory_bytes = Some(ByteQuantity::try_from(memory_mib << 20).unwrap());
    spec
}

fn one_replica() -> ServiceMode {
    ServiceMode::Replicated {
        replicas: NonZeroU32::new(1).unwrap(),
    }
}

fn single_operation(preview: &DeployPreview) -> DeployOperation {
    let operations = operations(preview);
    let [operation] = operations.as_slice() else {
        panic!("one operation, got {operations:?}");
    };
    operation.clone()
}

#[test]
fn a_limits_only_change_updates_the_running_container_on_a_capable_server() {
    for mode in [one_replica(), ServiceMode::Global] {
        let current = limited(mode.clone(), 64);
        let mut requested = limited(mode, 128);
        requested.container.resources.cpu_nanos = Some(CpuNanos::from_cpus(0.5).unwrap());
        let plan = plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![capable('1', "first")],
                containers: vec![container('b', '1', &current, &service_id('a'))],
                ..Default::default()
            },
            PlanOptions::default(),
        )
        .unwrap();

        let DeployOperation::UpdateContainer(update) = single_operation(&plan) else {
            panic!(
                "limits-only change updates in place: {:?}",
                operations(&plan)
            );
        };
        assert_eq!(update.old_container_id, container_id('b'));
        assert_eq!(update.machine_id, machine_id('1'));
        assert_eq!(
            update.spec.container.resources.limits(),
            requested.container.resources.limits()
        );
    }
}

#[test]
fn a_server_without_the_capability_replaces_the_container() {
    let plan = plan_deploy(
        [&limited(one_replica(), 128)],
        &DeploySnapshot {
            machines: vec![machine('1', "first")],
            containers: vec![container(
                'b',
                '1',
                &limited(one_replica(), 64),
                &service_id('a'),
            )],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert!(matches!(
        single_operation(&plan),
        DeployOperation::ReplaceContainer(ReplacementOperation { old_container_id, .. })
            if old_container_id == container_id('b')
    ));
}

#[test]
fn any_other_change_alongside_the_limits_replaces_the_container() {
    let current = limited(one_replica(), 64);
    let mut new_image = limited(one_replica(), 128);
    new_image.container.image = "ghcr.io/getployz/api:2".into();
    let mut new_shm = limited(one_replica(), 128);
    new_shm.container.resources.shared_memory_bytes =
        Some(ByteQuantity::try_from(1 << 20).unwrap());
    let cleared = requested(one_replica());
    let mut forced = limited(one_replica(), 128);
    forced.container.resources.cpu_nanos = None;
    for (name, requested, options) in [
        ("image", new_image, PlanOptions::default()),
        ("shared memory", new_shm, PlanOptions::default()),
        ("cleared limit", cleared, PlanOptions::default()),
        (
            "force recreate",
            forced,
            PlanOptions {
                force_recreate: true,
                ..Default::default()
            },
        ),
    ] {
        let plan = plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![capable('1', "first")],
                containers: vec![container('b', '1', &current, &service_id('a'))],
                ..Default::default()
            },
            options,
        )
        .unwrap();

        assert!(
            matches!(
                single_operation(&plan),
                DeployOperation::ReplaceContainer(ReplacementOperation { old_container_id, .. })
                    if old_container_id == container_id('b')
            ),
            "{name}: {:?}",
            operations(&plan)
        );
    }
}

#[test]
fn an_update_needs_no_spare_endpoint_and_runs_no_pre_deploy_hook() {
    let hook = Some(PreDeployHook {
        command: vec!["db".into(), "migrate".into()].try_into().unwrap(),
        environment: Default::default(),
        privileged: None,
        timeout_millis: None,
        user: None,
    });
    for mode in [one_replica(), ServiceMode::Global] {
        let mut current = limited(mode.clone(), 64);
        current.pre_deploy = hook.clone();
        let mut requested = limited(mode, 128);
        requested.pre_deploy = hook.clone();
        let plan = plan_deploy(
            [&requested],
            &DeploySnapshot {
                machines: vec![capable('1', "first")],
                containers: vec![container('b', '1', &current, &service_id('a'))],
                capacity: capacity([('1', 0)]),
                ..Default::default()
            },
            PlanOptions::default(),
        )
        .unwrap();

        assert!(
            matches!(
                single_operation(&plan),
                DeployOperation::UpdateContainer(ReplacementOperation { old_container_id, .. })
                    if old_container_id == container_id('b')
            ),
            "{:?}",
            operations(&plan)
        );
    }
}

#[test]
fn a_scale_up_updates_the_running_copy_and_starts_the_new_one() {
    let current = limited(one_replica(), 64);
    let requested = limited(
        ServiceMode::Replicated {
            replicas: NonZeroU32::new(2).unwrap(),
        },
        128,
    );
    let plan = plan_deploy(
        [&requested],
        &DeploySnapshot {
            machines: vec![capable('1', "first"), capable('2', "second")],
            containers: vec![container('b', '1', &current, &service_id('a'))],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    let mut kinds = operations(&plan)
        .iter()
        .map(|operation| {
            if let DeployOperation::UpdateContainer(update) = operation {
                format!(
                    "update {} on {}",
                    update.old_container_id, update.machine_id
                )
            } else if let DeployOperation::RunContainer { machine_id, .. } = operation {
                format!("run on {machine_id}")
            } else {
                panic!("unexpected operation: {operation:?}")
            }
        })
        .collect::<Vec<_>>();
    kinds.sort();
    assert_eq!(
        kinds,
        [
            format!("run on {}", machine_id('2')),
            format!("update {} on {}", container_id('b'), machine_id('1')),
        ]
    );
}
