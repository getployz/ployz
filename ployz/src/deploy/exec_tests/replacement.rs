use super::*;

#[tokio::test]
async fn start_first_health_failure_records_stop_success_or_failure_and_never_touches_old() {
    for stop_succeeds in [true, false] {
        let machine = machine('1');
        let old = container('a');
        let new = container('b');
        let plan = vec![replacement(&machine, &old, UpdateOrder::StartFirst)];
        let mut steps = vec![
            created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
            ok(Call::Start(machine, new)),
            observed(Call::Inspect(machine, new), unhealthy()),
        ];
        steps.push(if stop_succeeds {
            ok(Call::StopWithGrace(machine, new, 0))
        } else {
            failed(Call::StopWithGrace(machine, new, 0), "stop new failed")
        });
        let client = Scripted::new(steps);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome,
            DeployOutcome::Failed { failed: FailedOperation::ReplacementHealth {
                compensation: ReplacementCompensation::StartFirst {
                    stop_new_container,
                },
                ..
            }, .. } if stop_new_container.stopped() == stop_succeeds
        ));
        client.assert_done();
    }
}

#[tokio::test]
async fn replacement_compensation_tolerates_a_missing_new_container() {
    let machine = machine('1');
    let old = container('a');
    let new = container('b');
    let plan = vec![replacement(&machine, &old, UpdateOrder::StartFirst)];
    let mut missing = error("not found");
    missing.code = RpcErrorCode::NotFound;
    let client = Scripted::new(vec![
        created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Start(machine, new)),
        observed(Call::Inspect(machine, new), unhealthy()),
        Step(Call::StopWithGrace(machine, new, 0), Reply::Error(missing)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::ReplacementHealth {
                compensation: ReplacementCompensation::StartFirst {
                    stop_new_container: StopAttempt::Stopped,
                },
                ..
            },
            ..
        }
    ));
    client.assert_done();
}

#[tokio::test]
async fn replacement_inspect_failure_runs_no_health_compensation() {
    let machine = machine('1');
    let old = container('a');
    let new = container('b');
    let plan = vec![replacement(&machine, &old, UpdateOrder::StartFirst)];
    let client = Scripted::new(vec![
        created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Start(machine, new)),
        failed(Call::Inspect(machine, new), "inspect failed"),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Operation {
                error: ExecutionError::Machine {
                    action: MachineAction::InspectContainer,
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
async fn stop_first_health_failure_records_both_compensation_attempts() {
    for (stop_succeeds, restart_succeeds) in
        [(true, true), (true, false), (false, true), (false, false)]
    {
        let machine = machine('1');
        let old = container('a');
        let new = container('b');
        let plan = vec![replacement(&machine, &old, UpdateOrder::StopFirst)];
        let mut steps = vec![
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Stop(machine, old)),
            created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
            ok(Call::Start(machine, new)),
            observed(Call::Inspect(machine, new), unhealthy()),
        ];
        steps.push(if stop_succeeds {
            ok(Call::StopWithGrace(machine, new, 0))
        } else {
            failed(Call::StopWithGrace(machine, new, 0), "stop new failed")
        });
        steps.push(if restart_succeeds {
            ok(Call::Start(machine, old))
        } else {
            failed(Call::Start(machine, old), "restart old failed")
        });
        let client = Scripted::new(steps);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome,
            DeployOutcome::Failed { failed: FailedOperation::ReplacementHealth {
                compensation: ReplacementCompensation::StopFirst {
                    stop_new_container,
                    restart_old_container: restart,
                },
                ..
            }, .. } if stop_new_container.stopped() == stop_succeeds
                && restart.restarted() == restart_succeeds
        ));
        client.assert_done();
    }
}

#[tokio::test]
async fn stop_first_does_not_restart_a_previously_stopped_old_container() {
    let machine = machine('1');
    let old = container('a');
    let new = container('b');
    let plan = vec![replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine, old), exited(1)),
        created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Start(machine, new)),
        observed(Call::Inspect(machine, new), unhealthy()),
        ok(Call::StopWithGrace(machine, new, 0)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::ReplacementHealth {
                compensation: ReplacementCompensation::StopFirst {
                    restart_old_container: RestartAttempt::NotAttempted,
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
async fn stop_first_stops_and_can_restart_active_old_container_states() {
    for runtime in [
        ContainerRuntimeObservation::Paused,
        ContainerRuntimeObservation::Restarting,
    ] {
        let machine = machine('1');
        let old = container('a');
        let new = container('b');
        let plan = vec![replacement(&machine, &old, UpdateOrder::StopFirst)];
        let client = Scripted::new(vec![
            observed(Call::Inspect(machine, old), runtime),
            ok(Call::Stop(machine, old)),
            created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
            ok(Call::Start(machine, new)),
            observed(Call::Inspect(machine, new), unhealthy()),
            ok(Call::StopWithGrace(machine, new, 0)),
            ok(Call::Start(machine, old)),
        ]);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::ReplacementHealth {
                    compensation: ReplacementCompensation::StopFirst {
                        restart_old_container: RestartAttempt::Restarted,
                        ..
                    },
                    ..
                },
                ..
            }
        ));
        client.assert_done();
    }
}

#[tokio::test]
async fn stop_first_tolerates_disappearance_between_inspect_and_stop() {
    let machine = machine('1');
    let old = container('a');
    let new = container('b');
    let plan = vec![replacement(&machine, &old, UpdateOrder::StopFirst)];
    let mut missing = error("not found");
    missing.code = RpcErrorCode::NotFound;
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine, old), running()),
        Step(Call::Stop(machine, old), Reply::Error(missing.clone())),
        created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Start(machine, new)),
        observed(Call::Inspect(machine, new), healthy()),
        serving(new),
        Step(Call::Remove(machine, old), Reply::Error(missing)),
        dropped(old),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}

#[tokio::test]
async fn replacement_tolerates_an_old_container_missing_from_its_machine() {
    for order in [UpdateOrder::StartFirst, UpdateOrder::StopFirst] {
        let machine = machine('1');
        let old = container('a');
        let new = container('b');
        let plan = vec![replacement(&machine, &old, order)];
        let mut missing = error("not found");
        missing.code = RpcErrorCode::NotFound;
        let mut steps = Vec::new();
        if order == UpdateOrder::StopFirst {
            steps.push(Step(
                Call::Inspect(machine, old),
                Reply::Error(missing.clone()),
            ));
        }
        steps.extend([
            created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
            ok(Call::Start(machine, new)),
            observed(Call::Inspect(machine, new), healthy()),
            serving(new),
        ]);
        if order == UpdateOrder::StartFirst {
            steps.push(Step(
                Call::Stop(machine, old),
                Reply::Error(missing.clone()),
            ));
        }
        steps.push(Step(Call::Remove(machine, old), Reply::Error(missing)));
        steps.push(dropped(old));
        let client = Scripted::new(steps);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(outcome, DeployOutcome::Success { .. }));
        client.assert_done();
    }
}

#[tokio::test]
async fn replacement_does_not_apply_the_new_stop_grace_to_the_old_container() {
    for order in [UpdateOrder::StartFirst, UpdateOrder::StopFirst] {
        let machine = machine('1');
        let old = container('a');
        let new = container('b');
        let mut operation = replacement(&machine, &old, order);
        let DeployOperation::ReplaceContainer(replacement) = &mut operation else {
            unreachable!()
        };
        replacement.spec.container.stop_timeout_secs = Some(30);
        let plan = vec![operation];
        let mut steps = Vec::new();
        if order == UpdateOrder::StopFirst {
            steps.extend([
                observed(Call::Inspect(machine, old), running()),
                ok(Call::Stop(machine, old)),
            ]);
        }
        steps.extend([
            created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
            ok(Call::Start(machine, new)),
            observed(Call::Inspect(machine, new), healthy()),
            serving(new),
        ]);
        if order == UpdateOrder::StartFirst {
            steps.push(ok(Call::Stop(machine, old)));
        }
        steps.push(ok(Call::Remove(machine, old)));
        steps.push(dropped(old));
        let client = Scripted::new(steps);

        assert!(matches!(
            execute_with(&plan, &client, &CancellationToken::new()).await,
            DeployOutcome::Success { .. }
        ));
        client.assert_done();
    }
}

#[tokio::test]
async fn stop_first_create_failure_stays_partial_and_start_failure_removes_only_the_candidate() {
    for start_fails in [false, true] {
        let machine = machine('1');
        let old = container('a');
        let new = container('b');
        let plan = vec![replacement(&machine, &old, UpdateOrder::StopFirst)];
        let mut steps = vec![
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Stop(machine, old)),
        ];
        if start_fails {
            steps.extend([
                created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
                failed(Call::Start(machine, new), "start failed"),
                ok(Call::Remove(machine, new)),
            ]);
        } else {
            steps.push(failed(
                Call::Create(machine, ContainerKind::ServiceContainer),
                "create failed",
            ));
        }
        let client = Scripted::new(steps);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation { .. },
                ..
            }
        ));
        client.assert_done();
    }
}
