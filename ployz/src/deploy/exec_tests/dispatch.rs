use super::*;

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

#[tokio::test]
async fn standalone_stop_and_remove_tolerate_missing_targets() {
    let machine = machine('1');
    let stopped = container('9');
    let removed = container('a');
    let suffix = container('b');
    let mut missing = error("not found");
    missing.code = RpcErrorCode::NotFound;
    let plan = plan(vec![
        DeployOperation::StopContainer {
            machine_id: machine.clone(),
            container_id: stopped.clone(),
        },
        DeployOperation::RemoveContainer {
            machine_id: machine.clone(),
            container_id: removed.clone(),
        },
        stop(&machine, &suffix),
    ]);
    let client = Scripted::new(vec![
        Step(
            Call::Stop(machine.clone(), stopped),
            Reply::Error(missing.clone()),
        ),
        Step(
            Call::Stop(machine.clone(), removed.clone()),
            Reply::Error(missing.clone()),
        ),
        Step(
            Call::Remove(machine.clone(), removed),
            Reply::Error(missing),
        ),
        ok(Call::Stop(machine, suffix)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(outcome.failed.is_none());
    client.assert_done();
}
