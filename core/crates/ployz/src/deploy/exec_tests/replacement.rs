use super::*;

#[tokio::test]
async fn start_first_health_failure_stops_then_removes_the_replacement_and_never_touches_old() {
    for stop_succeeds in [true, false] {
        let machine = machine('1');
        let old = container('a');
        let new = container('b');
        let plan = vec![replacement(&machine, &old, UpdateOrder::StartFirst)];
        let mut steps = vec![unhealthy_run(machine, &new)];
        if stop_succeeds {
            steps.push(ok(Call::Discard(machine, new)));
        } else {
            steps.push(failed(Call::Discard(machine, new), "stop new failed"));
        }
        let client = Scripted::new(steps);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome,
            DeployOutcome::Failed { failed: FailedOperation::Replacement {
                compensation: ReplacementCompensation::OldUntouched {
                    stop_new_container,
                },
                ..
            }, .. } if stop_new_container.stopped() == stop_succeeds
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
        let plan = vec![replacement(&machine, &old, UpdateOrder::StopFirst)];
        let mut steps = vec![
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Stop(machine, old)),
            unhealthy_run(machine, &new),
        ];
        if stop_succeeds {
            steps.push(ok(Call::Discard(machine, new)));
        } else {
            steps.push(failed(Call::Discard(machine, new), "stop new failed"));
        }
        if restart_succeeds {
            steps.push(ok(Call::Restart(machine, old)));
        } else {
            steps.push(failed(Call::Restart(machine, old), "restart old failed"));
        }
        let client = Scripted::new(steps);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome,
            DeployOutcome::Failed { failed: FailedOperation::Replacement {
                compensation: ReplacementCompensation::OldStopped {
                    stop_new_container: Some(stop_new_container),
                    restart_old_container:
                        restart @ (RestartAttempt::Restarted | RestartAttempt::Failed { .. }),
                },
                ..
            }, .. } if stop_new_container.stopped() == stop_succeeds
                && restart.restarted() == restart_succeeds
        ));
        client.assert_done();
    }
}

// The stop still runs so the old container's Server never starts it again, as it
// would an ingress whose first start failed.
#[tokio::test]
async fn stop_first_stops_but_does_not_restart_an_old_container_that_exited() {
    let machine = machine('1');
    let old = container('a');
    let new = container('b');
    let plan = vec![replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine, old), exited(1)),
        ok(Call::Stop(machine, old)),
        unhealthy_run(machine, &new),
        ok(Call::Discard(machine, new)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Replacement {
                compensation: ReplacementCompensation::OldUntouched {
                    stop_new_container: StopAttempt::Stopped,
                },
                ..
            },
            ..
        }
    ));
    client.assert_done();
}

// The stop held an ingress at rest down for the replacement. A start hands it back
// to its Server, which keeps retrying an ingress whose start fails.
#[tokio::test]
async fn stop_first_hands_a_never_started_old_container_back_when_the_new_one_fails() {
    for (restart, restart_succeeds) in [
        (
            vec![failed(
                Call::Restart(machine('1'), container('a')),
                "port 80 in use",
            )],
            false,
        ),
        (vec![ok(Call::Restart(machine('1'), container('a')))], true),
    ] {
        let machine = machine('1');
        let old = container('a');
        let plan = vec![replacement(&machine, &old, UpdateOrder::StopFirst)];
        let mut steps = vec![
            observed_ingress(
                Call::Inspect(machine, old),
                ContainerRuntimeObservation::Created,
            ),
            ok(Call::Stop(machine, old)),
            failed(
                Call::Run(machine, ContainerKind::ServiceContainer),
                "pull failed",
            ),
            ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
        ];
        steps.extend(restart);
        let client = Scripted::new(steps);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome,
            DeployOutcome::Failed { failed: FailedOperation::Replacement {
                compensation: ReplacementCompensation::OldStopped {
                    stop_new_container: None,
                    restart_old_container: restart,
                },
                ..
            }, .. } if restart.restarted() == restart_succeeds
        ));
        client.assert_done();
    }
}

// An ingress whose boot-time start failed is exited, and its Server restarts it
// only once nothing holds it down.
#[tokio::test]
async fn stop_first_hands_an_exited_ingress_back_when_its_create_fails() {
    for runtime in [exited(255), ContainerRuntimeObservation::Dead] {
        let machine = machine('1');
        let old = container('a');
        let plan = vec![replacement(&machine, &old, UpdateOrder::StopFirst)];
        let client = Scripted::new(vec![
            observed_ingress(Call::Inspect(machine, old), runtime),
            ok(Call::Stop(machine, old)),
            failed(
                Call::Run(machine, ContainerKind::ServiceContainer),
                "pull failed",
            ),
            ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
            ok(Call::Restart(machine, old)),
        ]);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Replacement {
                    compensation: ReplacementCompensation::OldStopped {
                        stop_new_container: None,
                        restart_old_container: RestartAttempt::Restarted,
                    },
                    ..
                },
                ..
            }
        ));
        client.assert_done();
    }
}

// A runtime Docker reports that this client does not recognize may still be serving,
// so a stop of it is undone like the stop of a running container.
#[tokio::test]
async fn stop_first_hands_back_an_unrecognized_old_container_when_its_create_fails() {
    let machine = machine('1');
    let old = container('a');
    let plan = vec![replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted::new(vec![
        observed(
            Call::Inspect(machine, old),
            ContainerRuntimeObservation::Unknown {
                raw: serde_json::json!("draining"),
            },
        ),
        ok(Call::Stop(machine, old)),
        failed(
            Call::Run(machine, ContainerKind::ServiceContainer),
            "pull failed",
        ),
        ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
        ok(Call::Restart(machine, old)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Replacement {
                compensation: ReplacementCompensation::OldStopped {
                    stop_new_container: None,
                    restart_old_container: RestartAttempt::Restarted,
                },
                ..
            },
            ..
        }
    ));
    client.assert_done();
}

// Only an ingress is handed back from rest. A Service container that never ran,
// such as one a cancelled Deploy left created, stays down when its replacement fails.
#[tokio::test]
async fn stop_first_leaves_a_never_started_service_container_down_when_the_new_one_fails() {
    for create_fails in [true, false] {
        let machine = machine('1');
        let old = container('a');
        let new = container('b');
        let plan = vec![replacement(&machine, &old, UpdateOrder::StopFirst)];
        let mut steps = vec![
            observed(
                Call::Inspect(machine, old),
                ContainerRuntimeObservation::Created,
            ),
            ok(Call::Stop(machine, old)),
        ];
        if create_fails {
            steps.extend([
                failed(
                    Call::Run(machine, ContainerKind::ServiceContainer),
                    "pull failed",
                ),
                ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
            ]);
        } else {
            steps.extend([
                unhealthy_run(machine, &new),
                ok(Call::Discard(machine, new)),
            ]);
        }
        let client = Scripted::new(steps);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        let DeployOutcome::Failed { failed, .. } = outcome else {
            panic!("the replacement fails");
        };
        if create_fails {
            assert!(matches!(failed, FailedOperation::Operation { .. }));
        } else {
            assert!(matches!(
                failed,
                FailedOperation::Replacement {
                    compensation: ReplacementCompensation::OldUntouched {
                        stop_new_container: StopAttempt::Stopped,
                    },
                    ..
                }
            ));
        }
        client.assert_done();
    }
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
            unhealthy_run(machine, &new),
            ok(Call::Discard(machine, new)),
            ok(Call::Restart(machine, old)),
        ]);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Replacement {
                    compensation: ReplacementCompensation::OldStopped {
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
        created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Remove(machine, old)),
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
        steps.extend([created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &new,
        )]);
        steps.push(ok(Call::Remove(machine, old)));
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
        steps.extend([created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &new,
        )]);
        steps.push(ok(Call::Remove(machine, old)));
        let client = Scripted::new(steps);

        assert!(matches!(
            execute_with(&plan, &client, &CancellationToken::new()).await,
            DeployOutcome::Success { .. }
        ));
        client.assert_done();
    }
}

#[tokio::test]
async fn stop_first_run_failure_restores_the_old_container() {
    let machine = machine('1');
    let old = container('a');
    let plan = vec![replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine, old), running()),
        ok(Call::Stop(machine, old)),
        failed(
            Call::Run(machine, ContainerKind::ServiceContainer),
            "start failed",
        ),
        ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
        ok(Call::Restart(machine, old)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Replacement {
                compensation: ReplacementCompensation::OldStopped {
                    stop_new_container: None,
                    restart_old_container: RestartAttempt::Restarted,
                },
                ..
            },
            ..
        }
    ));
    client.assert_done();
}
