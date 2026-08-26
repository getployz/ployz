use super::*;

#[tokio::test]
async fn dispatches_the_complete_algebra() {
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
        DeployOperation::RunContainer {
            machine_id: first,
            spec: service.clone(),
            skip_health_monitor: true,
        },
        DeployOperation::StopContainer {
            machine_id: first,
            container_id: old,
        },
        DeployOperation::RemoveContainer {
            machine_id: first,
            container_id: removed,
        },
        DeployOperation::ReplaceContainer(ReplacementOperation {
            machine_id: first,
            old_container_id: old,
            spec: service.clone(),
            skip_health_monitor: true,
        }),
        DeployOperation::StopHook {
            machine_id: second,
            container_id: hook,
        },
        DeployOperation::RunHook {
            machine_id: first,
            spec: hook_spec,
            old_hook_containers: vec![(second, hook)],
        },
        DeployOperation::RunContainer {
            machine_id: second,
            spec: service,
            skip_health_monitor: true,
        },
        DeployOperation::RemoveVolume {
            id: DockerVolumeId {
                machine_id: first,
                name: DockerVolumeName::parse("data").unwrap(),
            },
        },
    ];
    let plan = operations.clone();
    let client = Scripted::new(vec![
        created(
            Call::Create(first, ContainerKind::ServiceContainer),
            &new_run,
        ),
        ok(Call::Start(first, new_run)),
        ok(Call::Stop(first, old)),
        ok(Call::Stop(first, removed)),
        ok(Call::Remove(first, removed)),
        created(
            Call::Create(first, ContainerKind::ServiceContainer),
            &replacement,
        ),
        ok(Call::Start(first, replacement)),
        ok(Call::Stop(first, old)),
        ok(Call::Remove(first, old)),
        ok(Call::Stop(second, hook)),
        ok(Call::Remove(second, hook)),
        created(Call::Create(first, ContainerKind::PreDeployHook), &new_hook),
        ok(Call::Start(first, new_hook)),
        observed(Call::Inspect(first, new_hook), exited(0)),
        created(
            Call::Create(second, ContainerKind::ServiceContainer),
            &nested,
        ),
        ok(Call::Start(second, nested)),
        ok(Call::RemoveVolume(DockerVolumeId {
            machine_id: first,
            name: DockerVolumeName::parse("data").unwrap(),
        })),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome,
        DeployOutcome::Success {
            completed: operations,
        }
    );
    client.assert_done();
}

#[tokio::test]
async fn volume_ensure_failure_is_the_container_operation_failure() {
    let machine_id = machine('1');
    let operation = run(&machine_id, spec(None, None, None), true);
    let client = Scripted::new(vec![failed(
        Call::Create(machine_id, ContainerKind::ServiceContainer),
        "Volume Ensure failed after creating data",
    )]);

    let outcome = execute_with(
        std::slice::from_ref(&operation),
        &client,
        &CancellationToken::new(),
    )
    .await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            completed,
            failed: FailedOperation::Operation {
                operation: failed,
                error: ExecutionError::Machine {
                    action: MachineAction::CreateContainer,
                    error,
                },
            },
            unexecuted,
        } if completed.is_empty()
            && failed == operation
            && error.message == "Volume Ensure failed after creating data"
            && unexecuted.is_empty()
    ));
    client.assert_done();
}

#[tokio::test]
async fn a_failure_at_each_position_keeps_the_exact_prefix_and_suffix() {
    let machine = machine('1');
    let operations = ['a', 'b', 'c']
        .map(|id| DeployOperation::StopContainer {
            machine_id: machine,
            container_id: container(id),
        })
        .to_vec();
    let plan = operations.clone();

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
                    failed(Call::Stop(machine, *container_id), "boom")
                } else {
                    ok(Call::Stop(machine, *container_id))
                }
            })
            .collect();
        let client = Scripted::new(steps);
        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        let DeployOutcome::Failed {
            completed,
            failed,
            unexecuted,
        } = &outcome
        else {
            panic!("expected failure at {failed_index}");
        };
        assert_eq!(completed, operations.get(..failed_index).unwrap());
        assert!(matches!(
            failed,
            FailedOperation::Operation {
                operation: DeployOperation::StopContainer { .. },
                error: ExecutionError::Machine { error, .. },
            } if error.message == "boom"
        ));
        assert_eq!(unexecuted, operations.get(failed_index + 1..).unwrap());
        client.assert_done();
    }
}

#[tokio::test]
async fn create_then_start_failure_keeps_the_container_without_cleanup() {
    let machine = machine('1');
    let created_id = container('a');
    let plan = vec![run(&machine, spec(None, None, None), false)];
    let client = Scripted::new(vec![
        created(
            Call::Create(machine, ContainerKind::ServiceContainer),
            &created_id,
        ),
        failed(Call::Start(machine, created_id), "start failed"),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Operation {
                error: ExecutionError::Machine {
                    action: MachineAction::StartContainer,
                    ..
                },
                ..
            },
            ..
        }
    ));
    client.assert_done();
}

#[tokio::test]
async fn standalone_stop_and_remove_tolerate_missing_targets() {
    let machine = machine('1');
    let stopped = container('9');
    let removed = container('a');
    let suffix = container('b');
    let volume = DockerVolumeId {
        machine_id: machine,
        name: DockerVolumeName::parse("data").unwrap(),
    };
    let mut missing = error("not found");
    missing.code = RpcErrorCode::NotFound;
    let plan = vec![
        DeployOperation::StopContainer {
            machine_id: machine,
            container_id: stopped,
        },
        DeployOperation::RemoveContainer {
            machine_id: machine,
            container_id: removed,
        },
        DeployOperation::RemoveVolume { id: volume.clone() },
        stop(&machine, &suffix),
    ];
    let client = Scripted::new(vec![
        Step(Call::Stop(machine, stopped), Reply::Error(missing.clone())),
        Step(Call::Stop(machine, removed), Reply::Error(missing.clone())),
        Step(
            Call::Remove(machine, removed),
            Reply::Error(missing.clone()),
        ),
        Step(Call::RemoveVolume(volume), Reply::Error(missing)),
        ok(Call::Stop(machine, suffix)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}
