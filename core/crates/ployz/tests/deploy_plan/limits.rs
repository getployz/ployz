//! A limits-only change updates a running Container in place on a Server
//! whose daemon can, and replaces it everywhere else.

use super::support::*;
use ployz_core::{
    ByteQuantity, CpuNanos, LimitsUpdateOperation, UPDATE_CONTAINER_LIMITS_CAPABILITY,
};

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
        assert_eq!(update.container_id, container_id('b'));
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
                DeployOperation::UpdateContainer(LimitsUpdateOperation { container_id: id, .. })
                    if id == container_id('b')
            ),
            "{:?}",
            operations(&plan)
        );
    }
}

fn two_replicas() -> ServiceMode {
    ServiceMode::Replicated {
        replicas: NonZeroU32::new(2).unwrap(),
    }
}

fn kinds(preview: &DeployPreview) -> Vec<String> {
    let mut kinds = operations(preview)
        .iter()
        .map(|operation| match operation {
            DeployOperation::UpdateContainer(update) => {
                format!("update {} on {}", update.container_id, update.machine_id)
            }
            DeployOperation::ReplaceContainer(replacement) => format!(
                "replace {} on {}",
                replacement.old_container_id, replacement.machine_id
            ),
            DeployOperation::RunContainer { machine_id, .. } => format!("run on {machine_id}"),
            DeployOperation::RemoveContainer {
                machine_id,
                container_id,
                ..
            } => format!("remove {container_id} on {machine_id}"),
            other @ (DeployOperation::PrepareVolumes { .. }
            | DeployOperation::WaitHealthy { .. }
            | DeployOperation::StopContainer { .. }
            | DeployOperation::StopHook { .. }
            | DeployOperation::RunHook { .. }
            | DeployOperation::RemoveVolume { .. }) => {
                panic!("unexpected operation: {other:?}")
            }
        })
        .collect::<Vec<_>>();
    kinds.sort();
    kinds
}

#[test]
fn a_lower_or_first_memory_limit_replaces_while_cpu_moves_both_ways() {
    let mut less_cpu = limited(one_replica(), 64);
    less_cpu.container.resources.cpu_nanos = Some(CpuNanos::from_cpus(0.25).unwrap());
    let mut current_cpu = limited(one_replica(), 64);
    current_cpu.container.resources.cpu_nanos = Some(CpuNanos::from_cpus(0.5).unwrap());
    for (name, current, requested, update) in [
        (
            "lower memory",
            limited(one_replica(), 64),
            limited(one_replica(), 32),
            false,
        ),
        (
            "first memory limit",
            requested(one_replica()),
            limited(one_replica(), 64),
            false,
        ),
        (
            "cleared memory",
            limited(one_replica(), 64),
            requested(one_replica()),
            false,
        ),
        ("less cpu", current_cpu, less_cpu, true),
    ] {
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
        let expected = if update { "update" } else { "replace" };
        assert_eq!(
            kinds(&plan),
            [format!(
                "{expected} {} on {}",
                container_id('b'),
                machine_id('1')
            )],
            "{name}"
        );
    }
}

#[test]
fn a_memory_raise_alone_updates_every_copy_in_place() {
    let plan = plan_deploy(
        [&limited(two_replicas(), 128)],
        &DeploySnapshot {
            machines: vec![capable('1', "first"), capable('2', "second")],
            containers: vec![
                container('b', '1', &limited(two_replicas(), 64), &service_id('a')),
                container('c', '2', &limited(two_replicas(), 64), &service_id('a')),
            ],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(
        kinds(&plan),
        [
            format!("update {} on {}", container_id('b'), machine_id('1')),
            format!("update {} on {}", container_id('c'), machine_id('2')),
        ]
    );
}

/// A copy updated in place keeps its gen, and a Machine coming back takes it
/// out of serving once a newer sibling is routed everywhere. So no Deploy
/// mixes updated copies with new ones in one Service.
#[test]
fn a_memory_raise_that_also_starts_a_copy_replaces_every_copy() {
    let plan = plan_deploy(
        [&limited(two_replicas(), 128)],
        &DeploySnapshot {
            machines: vec![capable('1', "first"), capable('2', "second")],
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

    assert_eq!(
        kinds(&plan),
        [
            format!("replace {} on {}", container_id('b'), machine_id('1')),
            format!("run on {}", machine_id('2')),
        ]
    );
}

#[test]
fn a_server_without_the_capability_makes_the_whole_service_replace() {
    let plan = plan_deploy(
        [&limited(two_replicas(), 128)],
        &DeploySnapshot {
            machines: vec![capable('1', "first"), machine('2', "second")],
            containers: vec![
                container('b', '1', &limited(two_replicas(), 64), &service_id('a')),
                container('c', '2', &limited(two_replicas(), 64), &service_id('a')),
            ],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(
        kinds(&plan),
        [
            format!("replace {} on {}", container_id('b'), machine_id('1')),
            format!("replace {} on {}", container_id('c'), machine_id('2')),
        ]
    );
}

/// Spreading replicas onto a Server that joined since would start a copy
/// there, so the copies that stay take the new gen too.
#[test]
fn a_memory_raise_that_spreads_onto_a_new_server_replaces_every_copy() {
    let plan = plan_deploy(
        [&limited(two_replicas(), 128)],
        &DeploySnapshot {
            machines: vec![capable('1', "first"), capable('2', "second")],
            containers: vec![
                container('b', '1', &limited(two_replicas(), 64), &service_id('a')),
                container('c', '1', &limited(two_replicas(), 64), &service_id('a')),
            ],
            ..Default::default()
        },
        PlanOptions::default(),
    )
    .unwrap();

    assert_eq!(
        kinds(&plan),
        [
            format!("remove {} on {}", container_id('b'), machine_id('1')),
            format!("replace {} on {}", container_id('c'), machine_id('1')),
            format!("run on {}", machine_id('2')),
        ]
    );
}
