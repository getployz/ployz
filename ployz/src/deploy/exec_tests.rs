use std::{collections::VecDeque, sync::Mutex};

use ployz_core::{
    ContainerRuntimeObservation, DockerVolumeName, HealthObservation, ServiceVolumeReference,
    VolumeSource,
};

use crate::deploy::FailedOperation;

use super::*;

#[derive(Clone, Debug, Eq, PartialEq)]
enum Call {
    CreateVolume(MachineId),
    Create(MachineId, ContainerKind),
    Start(MachineId, ContainerId),
    Inspect(MachineId, ContainerId),
    Stop(MachineId, ContainerId),
    Remove(MachineId, ContainerId),
}

#[derive(Clone)]
enum Reply {
    Ok,
    Created(ContainerId),
    Observed(ContainerRuntimeObservation),
    Pending,
    Error(RpcError),
}

struct Step(Call, Reply);

struct Scripted {
    steps: Mutex<VecDeque<Step>>,
}

impl Scripted {
    fn new(steps: Vec<Step>) -> Self {
        Self {
            steps: Mutex::new(steps.into()),
        }
    }

    fn next(&self, call: Call) -> Reply {
        let Step(expected, reply) = self
            .steps
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| panic!("unexpected call: {call:?}"));
        assert_eq!(call, expected);
        reply
    }

    fn assert_done(&self) {
        assert!(self.steps.lock().unwrap().is_empty());
    }
}

impl MachineOperations for Scripted {
    async fn create_volume(
        &self,
        machine_id: &MachineId,
        _volume: &ServiceVolume,
    ) -> Result<(), RpcError> {
        unit(self.next(Call::CreateVolume(machine_id.clone())))
    }

    async fn create_container(
        &self,
        machine_id: &MachineId,
        kind: ContainerKind,
        _spec: &ResolvedServiceSpec,
    ) -> Result<ContainerCreated, RpcError> {
        match self.next(Call::Create(machine_id.clone(), kind)) {
            Reply::Created(container_id) => Ok(ContainerCreated {
                display_name: container_id.to_string(),
                container_id,
            }),
            Reply::Error(error) => Err(error),
            Reply::Ok | Reply::Observed(_) | Reply::Pending => {
                panic!("scripted create requires Created or Error")
            }
        }
    }

    async fn start_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<(), RpcError> {
        unit(self.next(Call::Start(machine_id.clone(), container_id.clone())))
    }

    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerObservation, RpcError> {
        match self.next(Call::Inspect(machine_id.clone(), container_id.clone())) {
            Reply::Observed(runtime) => Ok(observation(machine_id, container_id, runtime)),
            Reply::Pending => std::future::pending().await,
            Reply::Error(error) => Err(error),
            Reply::Ok | Reply::Created(_) => {
                panic!("scripted inspect requires Observed or Error")
            }
        }
    }

    async fn stop_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        _grace_period_seconds: Option<i32>,
    ) -> Result<(), RpcError> {
        unit(self.next(Call::Stop(machine_id.clone(), container_id.clone())))
    }

    async fn remove_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<(), RpcError> {
        unit(self.next(Call::Remove(machine_id.clone(), container_id.clone())))
    }
}

fn unit(reply: Reply) -> Result<(), RpcError> {
    match reply {
        Reply::Ok => Ok(()),
        Reply::Error(error) => Err(error),
        Reply::Created(_) | Reply::Observed(_) | Reply::Pending => {
            panic!("scripted mutation requires Ok or Error")
        }
    }
}

#[tokio::test]
async fn dispatches_the_complete_algebra_and_flattens_nested_sequences() {
    let first = machine('1');
    let second = machine('2');
    let old = container('a');
    let removed = container('b');
    let hook = container('c');
    let new_run = container('d');
    let replacement = container('e');
    let new_hook = container('f');
    let nested = container('9');
    let service = spec(None, None, None);
    let hook_spec = spec(None, None, Some(5_000));
    let operations = vec![
        DeployOperation::CreateVolume {
            machine_id: first.clone(),
            volume: volume(),
        },
        DeployOperation::RunContainer {
            machine_id: first.clone(),
            spec: service.clone(),
            skip_health_monitor: true,
        },
        DeployOperation::StopContainer {
            machine_id: first.clone(),
            container_id: old.clone(),
        },
        DeployOperation::RemoveContainer {
            machine_id: first.clone(),
            container_id: removed.clone(),
        },
        DeployOperation::ReplaceContainer(ReplacementOperation {
            machine_id: first.clone(),
            old_container_id: old.clone(),
            spec: service.clone(),
            skip_health_monitor: true,
        }),
        DeployOperation::StopHook {
            machine_id: second.clone(),
            container_id: hook.clone(),
        },
        DeployOperation::RunHook {
            machine_id: first.clone(),
            spec: hook_spec,
            old_hook_containers: vec![(second.clone(), hook.clone())],
        },
        DeployOperation::Sequence {
            operations: vec![DeployOperation::RunContainer {
                machine_id: second.clone(),
                spec: service,
                skip_health_monitor: true,
            }],
        },
    ];
    let plan = plan(operations.clone());
    let client = Scripted::new(vec![
        ok(Call::CreateVolume(first.clone())),
        created(
            Call::Create(first.clone(), ContainerKind::ServiceContainer),
            &new_run,
        ),
        ok(Call::Start(first.clone(), new_run)),
        ok(Call::Stop(first.clone(), old.clone())),
        ok(Call::Stop(first.clone(), removed.clone())),
        ok(Call::Remove(first.clone(), removed)),
        created(
            Call::Create(first.clone(), ContainerKind::ServiceContainer),
            &replacement,
        ),
        ok(Call::Start(first.clone(), replacement)),
        ok(Call::Stop(first.clone(), old.clone())),
        ok(Call::Remove(first.clone(), old)),
        ok(Call::Stop(second.clone(), hook.clone())),
        ok(Call::Remove(second.clone(), hook)),
        created(
            Call::Create(first.clone(), ContainerKind::PreDeployHook),
            &new_hook,
        ),
        ok(Call::Start(first.clone(), new_hook.clone())),
        observed(Call::Inspect(first.clone(), new_hook), exited(0)),
        created(
            Call::Create(second.clone(), ContainerKind::ServiceContainer),
            &nested,
        ),
        ok(Call::Start(second, nested)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    let mut expected = operations.get(..7).unwrap().to_vec();
    let DeployOperation::Sequence { operations: tail } = operations.get(7).unwrap() else {
        unreachable!()
    };
    expected.extend(tail.clone());
    assert_eq!(outcome.completed, expected);
    assert!(outcome.failed.is_none());
    assert!(outcome.unexecuted.is_empty());
    client.assert_done();
}

#[tokio::test]
async fn a_failure_at_each_position_keeps_the_exact_prefix_and_suffix() {
    let machine = machine('1');
    let operations = ['a', 'b', 'c']
        .map(|id| DeployOperation::StopContainer {
            machine_id: machine.clone(),
            container_id: container(id),
        })
        .to_vec();
    let plan = plan(operations.clone());

    for failed_index in 0..operations.len() {
        let steps = operations
            .iter()
            .take(failed_index + 1)
            .enumerate()
            .map(|(index, operation)| {
                let DeployOperation::StopContainer { container_id, .. } = operation else {
                    unreachable!()
                };
                if index == failed_index {
                    failed(Call::Stop(machine.clone(), container_id.clone()), "boom")
                } else {
                    ok(Call::Stop(machine.clone(), container_id.clone()))
                }
            })
            .collect();
        let client = Scripted::new(steps);
        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert_eq!(outcome.completed, operations.get(..failed_index).unwrap());
        assert!(matches!(
            outcome.failed,
            Some(FailedOperation::Operation {
                operation: DeployOperation::StopContainer { .. },
                error: ExecutionError::Machine { ref error, .. },
            }) if error.message == "boom"
        ));
        assert_eq!(
            outcome.unexecuted,
            operations.get(failed_index + 1..).unwrap()
        );
        client.assert_done();
    }
}

#[tokio::test]
async fn create_then_start_failure_keeps_the_container_without_cleanup() {
    let machine = machine('1');
    let created_id = container('a');
    let plan = plan(vec![run(&machine, spec(None, None, None), false)]);
    let client = Scripted::new(vec![
        created(
            Call::Create(machine.clone(), ContainerKind::ServiceContainer),
            &created_id,
        ),
        failed(Call::Start(machine.clone(), created_id), "start failed"),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(
        outcome.failed,
        Some(FailedOperation::Operation {
            error: ExecutionError::Machine {
                action: MachineAction::StartContainer,
                ..
            },
            ..
        })
    ));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn health_monitor_accepts_running_no_check_early_healthy_and_transient_unhealthy() {
    let machine = machine('1');
    let no_check = container('a');
    let early = container('b');
    let transient = container('c');
    let plan = plan(vec![
        run(&machine, spec(Some(25), None, None), false),
        run(
            &machine,
            spec(Some(5_000), Some(healthcheck()), None),
            false,
        ),
        run(
            &machine,
            spec(Some(2_000), Some(healthcheck()), None),
            false,
        ),
    ]);
    let client = Scripted::new(vec![
        created(
            Call::Create(machine.clone(), ContainerKind::ServiceContainer),
            &no_check,
        ),
        ok(Call::Start(machine.clone(), no_check.clone())),
        observed(Call::Inspect(machine.clone(), no_check), running()),
        created(
            Call::Create(machine.clone(), ContainerKind::ServiceContainer),
            &early,
        ),
        ok(Call::Start(machine.clone(), early.clone())),
        observed(Call::Inspect(machine.clone(), early), healthy()),
        created(
            Call::Create(machine.clone(), ContainerKind::ServiceContainer),
            &transient,
        ),
        ok(Call::Start(machine.clone(), transient.clone())),
        observed(
            Call::Inspect(machine.clone(), transient.clone()),
            unhealthy(),
        ),
        observed(Call::Inspect(machine, transient), healthy()),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(outcome.failed.is_none());
    client.assert_done();
}

#[tokio::test]
async fn health_monitor_fails_terminal_unhealthy_and_crash_but_skip_bypasses_inspection() {
    for (healthcheck, runtime) in [
        (Some(healthcheck()), unhealthy()),
        (None, ContainerRuntimeObservation::Exited { code: 17 }),
    ] {
        let machine = machine('1');
        let new = container('a');
        let plan = plan(vec![run(&machine, spec(Some(0), healthcheck, None), false)]);
        let client = Scripted::new(vec![
            created(
                Call::Create(machine.clone(), ContainerKind::ServiceContainer),
                &new,
            ),
            ok(Call::Start(machine.clone(), new.clone())),
            observed(Call::Inspect(machine, new), runtime),
        ]);
        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;
        assert!(matches!(
            outcome.failed,
            Some(FailedOperation::Operation {
                error: ExecutionError::Health { .. },
                ..
            })
        ));
        client.assert_done();
    }

    let machine = machine('2');
    let new = container('b');
    let plan = plan(vec![run(
        &machine,
        spec(Some(0), Some(healthcheck()), None),
        true,
    )]);
    let client = Scripted::new(vec![
        created(
            Call::Create(machine.clone(), ContainerKind::ServiceContainer),
            &new,
        ),
        ok(Call::Start(machine, new)),
    ]);
    assert!(
        execute_with(&plan, &client, &CancellationToken::new())
            .await
            .failed
            .is_none()
    );
    client.assert_done();
}

#[tokio::test]
async fn start_first_health_failure_records_stop_success_or_failure_and_never_touches_old() {
    for stop_succeeds in [true, false] {
        let machine = machine('1');
        let old = container('a');
        let new = container('b');
        let plan = plan(vec![replacement(&machine, &old, UpdateOrder::StartFirst)]);
        let mut steps = vec![
            created(
                Call::Create(machine.clone(), ContainerKind::ServiceContainer),
                &new,
            ),
            ok(Call::Start(machine.clone(), new.clone())),
            observed(Call::Inspect(machine.clone(), new.clone()), unhealthy()),
        ];
        steps.push(if stop_succeeds {
            ok(Call::Stop(machine.clone(), new))
        } else {
            failed(Call::Stop(machine.clone(), new), "stop new failed")
        });
        let client = Scripted::new(steps);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome.failed,
            Some(FailedOperation::ReplacementHealth {
                compensation: ReplacementCompensation::StartFirst {
                    stop_new_container,
                },
                ..
            }) if stop_new_container.is_ok() == stop_succeeds
        ));
        client.assert_done();
    }
}

#[tokio::test]
async fn stop_first_health_failure_records_both_compensation_attempts() {
    for (stop_succeeds, restart_succeeds) in
        [(true, true), (true, false), (false, true), (false, false)]
    {
        let machine = machine('1');
        let old = container('a');
        let new = container('b');
        let plan = plan(vec![replacement(&machine, &old, UpdateOrder::StopFirst)]);
        let mut steps = vec![
            observed(Call::Inspect(machine.clone(), old.clone()), running()),
            ok(Call::Stop(machine.clone(), old.clone())),
            created(
                Call::Create(machine.clone(), ContainerKind::ServiceContainer),
                &new,
            ),
            ok(Call::Start(machine.clone(), new.clone())),
            observed(Call::Inspect(machine.clone(), new.clone()), unhealthy()),
        ];
        steps.push(if stop_succeeds {
            ok(Call::Stop(machine.clone(), new))
        } else {
            failed(Call::Stop(machine.clone(), new), "stop new failed")
        });
        steps.push(if restart_succeeds {
            ok(Call::Start(machine.clone(), old))
        } else {
            failed(Call::Start(machine.clone(), old), "restart old failed")
        });
        let client = Scripted::new(steps);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome.failed,
            Some(FailedOperation::ReplacementHealth {
                compensation: ReplacementCompensation::StopFirst {
                    stop_new_container,
                    restart_old_container: RestartAttempt::Attempted(restart),
                },
                ..
            }) if stop_new_container.is_ok() == stop_succeeds
                && restart.is_ok() == restart_succeeds
        ));
        client.assert_done();
    }
}

#[tokio::test]
async fn stop_first_does_not_restart_a_previously_stopped_old_container() {
    let machine = machine('1');
    let old = container('a');
    let new = container('b');
    let plan = plan(vec![replacement(&machine, &old, UpdateOrder::StopFirst)]);
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine.clone(), old), exited(1)),
        created(
            Call::Create(machine.clone(), ContainerKind::ServiceContainer),
            &new,
        ),
        ok(Call::Start(machine.clone(), new.clone())),
        observed(Call::Inspect(machine.clone(), new.clone()), unhealthy()),
        ok(Call::Stop(machine, new)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

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
    client.assert_done();
}

#[tokio::test]
async fn stop_first_create_or_start_failure_runs_no_compensation() {
    for start_fails in [false, true] {
        let machine = machine('1');
        let old = container('a');
        let new = container('b');
        let plan = plan(vec![replacement(&machine, &old, UpdateOrder::StopFirst)]);
        let mut steps = vec![
            observed(Call::Inspect(machine.clone(), old.clone()), running()),
            ok(Call::Stop(machine.clone(), old)),
        ];
        if start_fails {
            steps.extend([
                created(
                    Call::Create(machine.clone(), ContainerKind::ServiceContainer),
                    &new,
                ),
                failed(Call::Start(machine.clone(), new), "start failed"),
            ]);
        } else {
            steps.push(failed(
                Call::Create(machine.clone(), ContainerKind::ServiceContainer),
                "create failed",
            ));
        }
        let client = Scripted::new(steps);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome.failed,
            Some(FailedOperation::Operation { .. })
        ));
        client.assert_done();
    }
}

#[tokio::test]
async fn hook_exit_zero_runs_suffix_nonzero_and_inspect_failure_retain_the_hook() {
    for reply in [Reply::Observed(exited(7)), Reply::Error(error("inspect"))] {
        let machine = machine('1');
        let hook_id = container('a');
        let suffix = container('b');
        let operations = vec![
            hook(&machine, spec(None, None, Some(5_000))),
            stop(&machine, &suffix),
        ];
        let plan = plan(operations);
        let client = Scripted::new(vec![
            created(
                Call::Create(machine.clone(), ContainerKind::PreDeployHook),
                &hook_id,
            ),
            ok(Call::Start(machine.clone(), hook_id.clone())),
            Step(Call::Inspect(machine.clone(), hook_id), reply),
        ]);
        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;
        assert_eq!(outcome.unexecuted, vec![stop(&machine, &suffix)]);
        client.assert_done();
    }

    let machine = machine('2');
    let hook_id = container('c');
    let suffix = container('d');
    let plan = plan(vec![
        hook(&machine, spec(None, None, Some(5_000))),
        stop(&machine, &suffix),
    ]);
    let client = Scripted::new(vec![
        created(
            Call::Create(machine.clone(), ContainerKind::PreDeployHook),
            &hook_id,
        ),
        ok(Call::Start(machine.clone(), hook_id.clone())),
        observed(Call::Inspect(machine.clone(), hook_id), exited(0)),
        ok(Call::Stop(machine, suffix)),
    ]);
    assert!(
        execute_with(&plan, &client, &CancellationToken::new())
            .await
            .failed
            .is_none()
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn hook_timeout_and_cancellation_attempt_stop_and_retain_the_container() {
    let machine = machine('1');
    let hook_id = container('a');
    let plan = plan(vec![hook(&machine, spec(None, None, Some(10)))]);
    let client = Scripted::new(vec![
        created(
            Call::Create(machine.clone(), ContainerKind::PreDeployHook),
            &hook_id,
        ),
        ok(Call::Start(machine.clone(), hook_id.clone())),
        Step(
            Call::Inspect(machine.clone(), hook_id.clone()),
            Reply::Pending,
        ),
        ok(Call::Stop(machine.clone(), hook_id.clone())),
    ]);
    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;
    assert!(matches!(
        outcome.failed,
        Some(FailedOperation::Operation {
            error: ExecutionError::Hook {
                failure: HookFailure::TimedOut { stop_error: None },
                ..
            },
            ..
        })
    ));
    client.assert_done();

    let cancellation = CancellationToken::new();
    let client = Scripted::new(vec![
        created(
            Call::Create(machine.clone(), ContainerKind::PreDeployHook),
            &hook_id,
        ),
        ok(Call::Start(machine.clone(), hook_id.clone())),
        Step(
            Call::Inspect(machine.clone(), hook_id.clone()),
            Reply::Pending,
        ),
        failed(Call::Stop(machine, hook_id), "stop failed"),
    ]);
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        cancel.cancel();
    });
    let outcome = execute_with(&plan, &client, &cancellation).await;
    assert!(matches!(
        outcome.failed,
        Some(FailedOperation::Operation {
            error: ExecutionError::Hook {
                failure: HookFailure::Cancelled {
                    stop_error: Some(_)
                },
                ..
            },
            ..
        })
    ));
    client.assert_done();
}

#[tokio::test]
async fn executing_the_same_plan_twice_runs_a_fresh_hook_each_time() {
    let machine = machine('1');
    let first = container('a');
    let second = container('b');
    let plan = plan(vec![hook(&machine, spec(None, None, Some(5_000)))]);
    let client = Scripted::new(vec![
        created(
            Call::Create(machine.clone(), ContainerKind::PreDeployHook),
            &first,
        ),
        ok(Call::Start(machine.clone(), first.clone())),
        observed(Call::Inspect(machine.clone(), first), exited(0)),
        created(
            Call::Create(machine.clone(), ContainerKind::PreDeployHook),
            &second,
        ),
        ok(Call::Start(machine.clone(), second.clone())),
        observed(Call::Inspect(machine, second), exited(0)),
    ]);

    assert!(
        execute_with(&plan, &client, &CancellationToken::new())
            .await
            .failed
            .is_none()
    );
    assert!(
        execute_with(&plan, &client, &CancellationToken::new())
            .await
            .failed
            .is_none()
    );
    client.assert_done();
}

fn plan(operations: Vec<DeployOperation>) -> DeployPlan {
    DeployPlan {
        service_id: service_id(),
        is_new_service: true,
        operation: DeployOperation::Sequence { operations },
    }
}

fn run(
    machine_id: &MachineId,
    spec: ResolvedServiceSpec,
    skip_health_monitor: bool,
) -> DeployOperation {
    DeployOperation::RunContainer {
        machine_id: machine_id.clone(),
        spec,
        skip_health_monitor,
    }
}

fn replacement(
    machine_id: &MachineId,
    old_container_id: &ContainerId,
    order: UpdateOrder,
) -> DeployOperation {
    let mut spec = spec(Some(0), Some(healthcheck()), None);
    spec.update.order = order;
    DeployOperation::ReplaceContainer(ReplacementOperation {
        machine_id: machine_id.clone(),
        old_container_id: old_container_id.clone(),
        spec,
        skip_health_monitor: false,
    })
}

fn hook(machine_id: &MachineId, spec: ResolvedServiceSpec) -> DeployOperation {
    DeployOperation::RunHook {
        machine_id: machine_id.clone(),
        spec,
        old_hook_containers: Vec::new(),
    }
}

fn stop(machine_id: &MachineId, container_id: &ContainerId) -> DeployOperation {
    DeployOperation::StopContainer {
        machine_id: machine_id.clone(),
        container_id: container_id.clone(),
    }
}

fn spec(
    monitor_millis: Option<u64>,
    healthcheck: Option<ployz_core::HealthcheckSpec>,
    hook_timeout_millis: Option<u64>,
) -> ResolvedServiceSpec {
    let mut spec: ResolvedServiceSpec = serde_json::from_value(serde_json::json!({
        "service_id": service_id(),
        "name": "api",
        "mode": { "mode": "replicated", "replicas": 1 },
        "container": {
            "image": "alpine:3.23.3",
            "pull_policy": "missing",
            "healthcheck": healthcheck,
        },
        "update": {
            "order": "start_first",
            "monitor_millis": monitor_millis,
        }
    }))
    .unwrap();
    spec.pre_deploy = hook_timeout_millis.map(|timeout_millis| ployz_core::PreDeployHook {
        command: vec!["true".into()],
        environment: Default::default(),
        privileged: None,
        timeout_millis: Some(timeout_millis),
        user: None,
    });
    spec
}

fn healthcheck() -> ployz_core::HealthcheckSpec {
    ployz_core::HealthcheckSpec {
        test: vec!["CMD".into(), "true".into()],
        interval_millis: Some(1_000),
        timeout_millis: Some(1_000),
        start_period_millis: None,
        start_interval_millis: None,
        retries: Some(1),
        disabled: false,
    }
}

fn volume() -> ServiceVolume {
    ServiceVolume {
        reference: ServiceVolumeReference::parse("data").unwrap(),
        source: VolumeSource::Named {
            name: DockerVolumeName::parse("data").unwrap(),
            external: false,
            driver: None,
            labels: Default::default(),
            no_copy: false,
            subpath: None,
        },
    }
}

fn observation(
    machine_id: &MachineId,
    container_id: &ContainerId,
    runtime: ContainerRuntimeObservation,
) -> ContainerObservation {
    let spec = spec(None, None, None);
    ContainerObservation {
        container_id: container_id.clone(),
        display_name: container_id.to_string(),
        machine_id: machine_id.clone(),
        service_id: spec.service_id.clone(),
        service_name: spec.name.clone(),
        kind: ContainerKind::ServiceContainer,
        runtime,
        resolved_spec: spec,
        address: None,
        labels: Default::default(),
    }
}

fn machine(hex: char) -> MachineId {
    MachineId::parse(hex.to_string().repeat(32)).unwrap()
}

fn container(hex: char) -> ContainerId {
    ContainerId::parse(hex.to_string().repeat(64)).unwrap()
}

fn service_id() -> ployz_core::ServiceId {
    ployz_core::ServiceId::parse("f".repeat(32)).unwrap()
}

fn running() -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Running {
        health: HealthObservation::NotConfigured,
    }
}

fn healthy() -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Running {
        health: HealthObservation::Healthy,
    }
}

fn unhealthy() -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Running {
        health: HealthObservation::Unhealthy,
    }
}

fn exited(code: i64) -> ContainerRuntimeObservation {
    ContainerRuntimeObservation::Exited { code }
}

fn error(message: &str) -> RpcError {
    RpcError {
        code: RpcErrorCode::Internal,
        message: message.into(),
        details: serde_json::Value::Null,
    }
}

fn ok(call: Call) -> Step {
    Step(call, Reply::Ok)
}

fn created(call: Call, container_id: &ContainerId) -> Step {
    Step(call, Reply::Created(container_id.clone()))
}

fn observed(call: Call, runtime: ContainerRuntimeObservation) -> Step {
    Step(call, Reply::Observed(runtime))
}

fn failed(call: Call, message: &str) -> Step {
    Step(call, Reply::Error(error(message)))
}
