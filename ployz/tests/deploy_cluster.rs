use std::{process, time::Duration};

use ployz::{
    connect::Client,
    deploy::{
        DeployOperation, DeployPlan, ExecutionError, FailedOperation, HookFailure,
        ReplacementCompensation, ReplacementOperation, execute_plan,
    },
};
use ployz_core::{
    ContainerAction, ContainerId, ContainerKind, ContainerObservation, ContainerRuntimeObservation,
    DockerVolumeName, Machine, ResolvedServiceSpec, ServiceId, ServiceVolume,
    ServiceVolumeReference, UpdateOrder, VolumeSource,
};
use ployz_testkit::{Cluster, ClusterPlan};
use tokio_util::sync::CancellationToken;

#[tokio::test]
#[ignore = "Layer 3: requires the privileged Ployz testkit image"]
async fn deploy_execution_preserves_partial_effects_and_never_repairs_them() {
    let plan = ClusterPlan::new(&format!("l3-deploy-{}", process::id()), 2).unwrap();
    let cluster = Cluster::create(plan).unwrap();
    let machines = cluster.initialize_two().await.unwrap();
    let direct = cluster.api_address(0).unwrap();
    let mut client = ployz::connect::connect(
        std::path::Path::new("/missing-ployz-test-config"),
        Some(&direct),
        None,
    )
    .await
    .unwrap();
    assert_target_local_volume(&cluster, &client, &machines[1]).await;
    assert_unreachable_middle_keeps_prefix(&cluster, &mut client, &machines).await;
    assert_replacement_health_compensation(&cluster, &mut client, &machines[0]).await;
    assert_failed_hooks_are_retained_and_rerun(&cluster, &mut client, &machines[0]).await;
    assert_unhealthy_service_is_not_repaired(&cluster, &mut client, &machines).await;
}

async fn assert_target_local_volume(cluster: &Cluster, client: &Client, machine: &Machine) {
    let volume = named_volume("deploy-data", "ployz-l3-deploy-data");
    let plan = deploy_plan(
        ServiceId::random(),
        vec![DeployOperation::CreateVolume {
            machine_id: machine.id.clone(),
            volume,
        }],
    );

    let outcome = execute_plan(&plan, client, &CancellationToken::new()).await;

    assert!(outcome.failed.is_none());
    assert!(
        cluster
            .machine_shell(1, "docker volume inspect ployz-l3-deploy-data")
            .is_ok()
    );
    assert!(
        cluster
            .machine_shell(0, "docker volume inspect ployz-l3-deploy-data")
            .is_err()
    );
    cluster
        .machine_shell(1, "docker volume rm ployz-l3-deploy-data")
        .unwrap();
}

async fn assert_unreachable_middle_keeps_prefix(
    cluster: &Cluster,
    client: &mut Client,
    machines: &[Machine; 2],
) {
    let service_id = ServiceId::random();
    let spec = service_spec(&service_id, "partial-deploy");
    let operations = vec![
        run_without_health_monitor(&machines[0], &spec),
        DeployOperation::CreateVolume {
            machine_id: machines[1].id.clone(),
            volume: named_volume("unreachable", "ployz-l3-unreachable"),
        },
        run_without_health_monitor(&machines[0], &spec),
    ];
    let plan = deploy_plan(service_id.clone(), operations.clone());
    cluster.remote_machine_api_rule(0, "--insert").unwrap();

    let outcome = execute_plan(&plan, client, &CancellationToken::new()).await;

    cluster.remote_machine_api_rule(0, "--delete").unwrap();
    assert_eq!(outcome.completed, operations.get(..1).unwrap());
    assert!(matches!(
        outcome.failed,
        Some(FailedOperation::Operation {
            error: ExecutionError::Machine { ref error, .. },
            ..
        }) if error.code == ployz_core::RpcErrorCode::Unavailable
    ));
    assert_eq!(outcome.unexecuted, operations.get(2..).unwrap());
    let containers = wait_for_service(client, &service_id, 1).await;
    assert_eq!(containers.len(), 1);
    assert_eq!(containers.first().unwrap().machine_id, machines[0].id);
    remove_all(client, containers).await;
}

async fn assert_replacement_health_compensation(
    cluster: &Cluster,
    client: &mut Client,
    machine: &Machine,
) {
    for order in [UpdateOrder::StartFirst, UpdateOrder::StopFirst] {
        let service_id = ServiceId::random();
        let old_spec = service_spec(&service_id, "replace-health");
        let old = client
            .create_container(
                machine.id.clone(),
                ContainerKind::ServiceContainer,
                old_spec.clone(),
            )
            .await
            .unwrap();
        client
            .change_container(
                machine.id.clone(),
                old.container_id.clone(),
                ContainerAction::Start,
                None,
                None,
            )
            .await
            .unwrap();
        wait_running(client, machine, &old.container_id).await;

        let mut failing = old_spec;
        failing.container.healthcheck = Some(ployz_core::HealthcheckSpec {
            test: vec!["CMD".into(), "false".into()],
            interval_millis: Some(50),
            timeout_millis: Some(50),
            start_period_millis: None,
            start_interval_millis: None,
            retries: Some(1),
            disabled: false,
        });
        failing.update.order = order;
        failing.update.monitor_millis = Some(0);
        let suffix_spec = service_spec(&ServiceId::random(), "untouched-suffix");
        let operations = vec![
            DeployOperation::ReplaceContainer(ReplacementOperation {
                machine_id: machine.id.clone(),
                old_container_id: old.container_id.clone(),
                spec: failing,
                skip_health_monitor: false,
            }),
            run_without_health_monitor(machine, &suffix_spec),
        ];
        let plan = deploy_plan(service_id.clone(), operations.clone());

        let outcome = execute_plan(&plan, client, &CancellationToken::new()).await;

        let failed_new = match &outcome.failed {
            Some(FailedOperation::ReplacementHealth {
                error: ExecutionError::Health { container_id, .. },
                compensation,
                ..
            }) => {
                assert!(match (order, compensation) {
                    (
                        UpdateOrder::StartFirst,
                        ReplacementCompensation::StartFirst { stop_new_container },
                    ) => stop_new_container.is_ok(),
                    (
                        UpdateOrder::StopFirst,
                        ReplacementCompensation::StopFirst {
                            stop_new_container,
                            restart_old_container: ployz::deploy::RestartAttempt::Attempted(restart),
                        },
                    ) => stop_new_container.is_ok() && restart.is_ok(),
                    _ => false,
                });
                container_id.clone()
            }
            failed => panic!("unexpected replacement failure: {failed:?}"),
        };
        assert_eq!(outcome.unexecuted, operations.get(1..).unwrap());
        assert!(docker_running(cluster, 0, &old.container_id));
        assert!(!docker_running(cluster, 0, &failed_new));
        assert!(
            cluster
                .machine_shell(0, &format!("docker inspect {failed_new}"))
                .is_ok()
        );
        let retained = wait_for_service(client, &service_id, 2).await;
        assert!(
            retained
                .iter()
                .any(|container| container.container_id == old.container_id)
        );
        assert!(
            retained
                .iter()
                .any(|container| container.container_id == failed_new)
        );
        remove_all(client, retained).await;
    }
}

async fn assert_failed_hooks_are_retained_and_rerun(
    cluster: &Cluster,
    client: &mut Client,
    machine: &Machine,
) {
    let nonzero_id = ServiceId::random();
    let mut nonzero = service_spec(&nonzero_id, "hook-nonzero");
    nonzero.pre_deploy = Some(ployz_core::PreDeployHook {
        command: vec!["sh".into(), "-c".into(), "exit 7".into()],
        environment: Default::default(),
        privileged: None,
        timeout_millis: Some(5_000),
        user: None,
    });
    let suffix =
        run_without_health_monitor(machine, &service_spec(&ServiceId::random(), "hook-suffix"));
    let plan = deploy_plan(
        nonzero_id.clone(),
        vec![hook(machine, &nonzero), suffix.clone()],
    );

    let first = execute_plan(&plan, client, &CancellationToken::new()).await;
    let first_id = failed_hook_id(&first, &suffix);
    let second = execute_plan(&plan, client, &CancellationToken::new()).await;
    let second_id = failed_hook_id(&second, &suffix);
    assert_ne!(first_id, second_id);
    assert!(docker_exists(cluster, 0, &first_id));
    assert!(docker_exists(cluster, 0, &second_id));

    let timeout_id = ServiceId::random();
    let mut timeout = service_spec(&timeout_id, "hook-timeout");
    timeout.pre_deploy = Some(ployz_core::PreDeployHook {
        command: vec!["sleep".into(), "30".into()],
        environment: Default::default(),
        privileged: None,
        timeout_millis: Some(50),
        user: None,
    });
    let timeout_plan = deploy_plan(timeout_id.clone(), vec![hook(machine, &timeout), suffix]);
    let timed_out = execute_plan(&timeout_plan, client, &CancellationToken::new()).await;
    let timed_out_id = match timed_out.failed {
        Some(FailedOperation::Operation {
            error:
                ExecutionError::Hook {
                    container_id,
                    failure: HookFailure::TimedOut { stop_error: None },
                },
            ..
        }) => container_id,
        failed => panic!("unexpected timed-out hook outcome: {failed:?}"),
    };
    assert!(docker_exists(cluster, 0, &timed_out_id));
    assert!(!docker_running(cluster, 0, &timed_out_id));

    let retained = wait_for_service(client, &nonzero_id, 2).await;
    remove_all(client, retained).await;
    let retained = wait_for_service(client, &timeout_id, 1).await;
    remove_all(client, retained).await;
}

async fn assert_unhealthy_service_is_not_repaired(
    cluster: &Cluster,
    client: &mut Client,
    machines: &[Machine; 2],
) {
    let service_id = ServiceId::random();
    let mut spec = service_spec(&service_id, "no-repair");
    spec.container.healthcheck = Some(ployz_core::HealthcheckSpec {
        test: vec!["CMD".into(), "false".into()],
        interval_millis: Some(50),
        timeout_millis: Some(50),
        start_period_millis: None,
        start_interval_millis: None,
        retries: Some(1),
        disabled: false,
    });
    let plan = deploy_plan(
        service_id.clone(),
        vec![run_without_health_monitor(&machines[0], &spec)],
    );
    assert!(
        execute_plan(&plan, client, &CancellationToken::new())
            .await
            .failed
            .is_none()
    );
    let before = wait_for_service(client, &service_id, 1).await;
    let id = before.first().unwrap().container_id.clone();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let after = wait_for_service(client, &service_id, 1).await;
    assert_eq!(after.first().unwrap().container_id, id);
    assert_eq!(after.first().unwrap().machine_id, machines[0].id);
    assert_eq!(docker_health(cluster, 0, &id).as_deref(), Some("unhealthy"));
    remove_all(client, after).await;
}

fn failed_hook_id(
    outcome: &ployz::deploy::DeployOutcome<ExecutionError>,
    suffix: &DeployOperation,
) -> ContainerId {
    assert_eq!(outcome.unexecuted, vec![suffix.clone()]);
    match &outcome.failed {
        Some(FailedOperation::Operation {
            error:
                ExecutionError::Hook {
                    container_id,
                    failure: HookFailure::Exit(7),
                },
            ..
        }) => container_id.clone(),
        failed => panic!("unexpected hook outcome: {failed:?}"),
    }
}

fn deploy_plan(service_id: ServiceId, operations: Vec<DeployOperation>) -> DeployPlan {
    DeployPlan {
        service_id,
        is_new_service: true,
        operation: DeployOperation::Sequence { operations },
    }
}

fn named_volume(reference: &str, name: &str) -> ServiceVolume {
    ServiceVolume {
        reference: ServiceVolumeReference::parse(reference).unwrap(),
        source: VolumeSource::Named {
            name: DockerVolumeName::parse(name).unwrap(),
            external: false,
            driver: None,
            labels: Default::default(),
            no_copy: false,
            subpath: None,
        },
    }
}

fn run_without_health_monitor(machine: &Machine, spec: &ResolvedServiceSpec) -> DeployOperation {
    DeployOperation::RunContainer {
        machine_id: machine.id.clone(),
        spec: spec.clone(),
        skip_health_monitor: true,
    }
}

fn hook(machine: &Machine, spec: &ResolvedServiceSpec) -> DeployOperation {
    DeployOperation::RunHook {
        machine_id: machine.id.clone(),
        spec: spec.clone(),
        old_hook_containers: Vec::new(),
    }
}

fn service_spec(service_id: &ServiceId, name: &str) -> ResolvedServiceSpec {
    serde_json::from_value(serde_json::json!({
        "service_id": service_id,
        "name": name,
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": {
            "image": "alpine:3.23.3",
            "command": ["sleep", "30"],
            "pull_policy": "missing"
        }
    }))
    .unwrap()
}

async fn wait_running(client: &Client, machine: &Machine, container_id: &ContainerId) {
    tokio::time::timeout(Duration::from_secs(10), async {
        loop {
            if client
                .inspect_container(machine.id.clone(), container_id.clone())
                .await
                .is_ok_and(|container| {
                    matches!(
                        container.runtime,
                        ContainerRuntimeObservation::Running { .. }
                    )
                })
            {
                break;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    })
    .await
    .unwrap();
}

async fn wait_for_service(
    client: &mut Client,
    service_id: &ServiceId,
    count: usize,
) -> Vec<ContainerObservation> {
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if let Ok(live) = client.live_services().await
                && let Some(service) = live
                    .services
                    .into_iter()
                    .find(|service| service.service_id == *service_id)
            {
                let containers = service
                    .containers
                    .into_iter()
                    .chain(service.hook_containers)
                    .collect::<Vec<_>>();
                if containers.len() == count {
                    return containers;
                }
            }
            tokio::time::sleep(Duration::from_millis(100)).await;
        }
    })
    .await
    .unwrap()
}

async fn remove_all(client: &Client, containers: Vec<ContainerObservation>) {
    for container in containers {
        client
            .change_container(
                container.machine_id,
                container.container_id,
                ContainerAction::Remove,
                None,
                Some(0),
            )
            .await
            .unwrap();
    }
}

fn docker_exists(cluster: &Cluster, machine_index: usize, container_id: &ContainerId) -> bool {
    cluster
        .machine_shell(machine_index, &format!("docker inspect {container_id}"))
        .is_ok()
}

fn docker_running(cluster: &Cluster, machine_index: usize, container_id: &ContainerId) -> bool {
    cluster
        .machine_shell(
            machine_index,
            &format!("docker inspect --format '{{{{.State.Running}}}}' {container_id}"),
        )
        .is_ok_and(|value| value.trim() == "true")
}

fn docker_health(
    cluster: &Cluster,
    machine_index: usize,
    container_id: &ContainerId,
) -> Option<String> {
    cluster
        .machine_shell(
            machine_index,
            &format!(
                "docker inspect --format '{{{{if .State.Health}}}}{{{{.State.Health.Status}}}}{{{{end}}}}' {container_id}"
            ),
        )
        .ok()
        .map(|value| value.trim().to_owned())
}
