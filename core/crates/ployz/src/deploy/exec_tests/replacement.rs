use super::*;

#[tokio::test]
async fn start_first_health_failure_removes_the_replacement_and_never_flips() {
    for remove_succeeds in [true, false] {
        let machine = machine('1');
        let old = container('a');
        let new = container('b');
        let plan = vec![replacement(&machine, &old, UpdateOrder::StartFirst)];
        let mut steps = vec![unhealthy_run(machine, &new)];
        if remove_succeeds {
            steps.push(ok(Call::Discard(machine, new)));
        } else {
            steps.push(failed(Call::Discard(machine, new), "remove new failed"));
        }
        let client = Scripted::new(steps);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Health { container_id, .. },
                    ..
                },
                ..
            } if container_id == new
        ));
        assert!(client.publications().is_empty());
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
        steps.push(created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &new,
        ));
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
        steps.push(created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &new,
        ));
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

fn swap(
    machine_id: &MachineId,
    old_container_id: &ContainerId,
    order: UpdateOrder,
) -> DeployOperation {
    let mut spec = spec(None, None, None);
    spec.update.order = order;
    DeployOperation::ReplaceContainer(ReplacementOperation {
        machine_id: *machine_id,
        old_container_id: *old_container_id,
        spec,
        skip_health_monitor: true,
    })
}

#[tokio::test]
async fn stop_first_publishes_the_new_copies_and_retires_the_stopped_ones() {
    let (first, second) = (machine('1'), machine('2'));
    let (old_first, old_second) = (container('a'), container('b'));
    let (new_first, new_second) = (container('c'), container('d'));
    let plan = vec![
        swap(&first, &old_first, UpdateOrder::StopFirst),
        swap(&second, &old_second, UpdateOrder::StopFirst),
    ];
    let client = Scripted::new(vec![
        observed(Call::Inspect(first, old_first), running()),
        observed(Call::Inspect(second, old_second), running()),
        ok(Call::Stop(first, old_first)),
        created(
            Call::Run(first, ContainerKind::ServiceContainer),
            &new_first,
        ),
        ok(Call::Stop(second, old_second)),
        created(
            Call::Run(second, ContainerKind::ServiceContainer),
            &new_second,
        ),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::New)]));
    let publications = client.publications();
    let [publication] = publications.as_slice() else {
        panic!("one pointer flip: {publications:?}");
    };
    assert_eq!(publication.pointer.containers, [new_first, new_second]);
    assert_eq!(publication.pointer.retiring, [old_first, old_second]);
    client.assert_done();
}

#[tokio::test]
async fn a_stop_first_or_rolling_failure_restores_every_old_copy_and_removes_every_new_one() {
    for order in [UpdateOrder::StopFirst, UpdateOrder::Rolling] {
        let (first, second) = (machine('1'), machine('2'));
        let (old_first, old_second) = (container('a'), container('b'));
        let new_first = container('c');
        let plan = vec![
            swap(&first, &old_first, order),
            swap(&second, &old_second, order),
        ];
        let inspect_second = || observed(Call::Inspect(second, old_second), running());
        let mut steps = vec![observed(Call::Inspect(first, old_first), running())];
        if order == UpdateOrder::StopFirst {
            steps.push(inspect_second());
        }
        steps.extend([
            ok(Call::Stop(first, old_first)),
            created(
                Call::Run(first, ContainerKind::ServiceContainer),
                &new_first,
            ),
        ]);
        if order == UpdateOrder::Rolling {
            steps.push(inspect_second());
        }
        steps.extend([
            ok(Call::Stop(second, old_second)),
            failed(
                Call::Run(second, ContainerKind::ServiceContainer),
                "start failed",
            ),
            ok(Call::DiscardSlot(second, ContainerKind::ServiceContainer)),
            ok(Call::Restart(second, old_second)),
            ok(Call::Discard(first, new_first)),
            ok(Call::Restart(first, old_first)),
        ]);
        let client = Scripted::new(steps);
        let row = ServingPointer {
            deploy: DeploymentLogId::parse("older").unwrap(),
            containers: vec![old_first, old_second],
            previous: None,
            retiring: Vec::new(),
            deciding: None,
        };
        client.pointers.lock().unwrap().insert(api(), row.clone());

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(
            matches!(&outcome, DeployOutcome::Failed { .. }),
            "{order:?}: {outcome:?}"
        );
        assert_eq!(outcome.services(), ended(&[("api", ServiceResult::Old)]));
        assert!(client.publications().is_empty());
        assert_eq!(
            client.pointers.lock().unwrap().get(&api()),
            Some(&row),
            "{order:?}: the restarted copies serve again"
        );
        client.assert_done();
    }
}

#[tokio::test]
async fn a_dropped_release_of_a_held_copy_is_retried() {
    let first = machine('1');
    let old = container('a');
    let plan = vec![swap(&first, &old, UpdateOrder::StopFirst)];
    let client = Scripted::new(vec![
        observed(Call::Inspect(first, old), running()),
        ok(Call::Stop(first, old)),
        failed(
            Call::Run(first, ContainerKind::ServiceContainer),
            "start failed",
        ),
        ok(Call::DiscardSlot(first, ContainerKind::ServiceContainer)),
        ok(Call::Restart(first, old)),
    ]);
    *client.dropped_releases.lock().unwrap() = 1;
    let row = ServingPointer {
        deploy: DeploymentLogId::parse("older").unwrap(),
        containers: vec![old],
        previous: None,
        retiring: Vec::new(),
        deciding: None,
    };
    client.pointers.lock().unwrap().insert(api(), row.clone());

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::Old)]));
    assert_eq!(
        client.pointers.lock().unwrap().get(&api()),
        Some(&row),
        "the restarted copy serves again and no hold is left behind"
    );
    client.assert_done();
}

#[tokio::test]
async fn a_failed_deploy_leaves_another_deploys_hold_in_place() {
    let first = machine('1');
    let old = container('a');
    let plan = vec![swap(&first, &old, UpdateOrder::StopFirst)];
    let rival = DeploymentLogId::parse("rival").unwrap();
    let client = Scripted {
        hold_taken_by: Some(rival.clone()),
        ..Scripted::new(vec![
            observed(Call::Inspect(first, old), running()),
            ok(Call::Stop(first, old)),
            failed(
                Call::Run(first, ContainerKind::ServiceContainer),
                "start failed",
            ),
            ok(Call::DiscardSlot(first, ContainerKind::ServiceContainer)),
            ok(Call::Restart(first, old)),
        ])
    };
    client.pointers.lock().unwrap().insert(
        api(),
        ServingPointer {
            deploy: DeploymentLogId::parse("older").unwrap(),
            containers: vec![old],
            previous: None,
            retiring: Vec::new(),
            deciding: None,
        },
    );

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::Old)]));
    let row = client
        .pointers
        .lock()
        .unwrap()
        .get(&api())
        .cloned()
        .unwrap();
    assert_eq!(row.containers, vec![old], "the restarted copy serves again");
    assert_eq!(
        row.deciding.map(|hold| hold.deploy),
        Some(rival),
        "the other deploy still holds its copies"
    );
    client.assert_done();
}

#[tokio::test]
async fn a_hold_whose_reply_is_lost_is_released() {
    let first = machine('1');
    let old = container('a');
    let plan = vec![swap(&first, &old, UpdateOrder::StopFirst)];
    let client = Scripted {
        lost_hold_reply: true,
        ..Scripted::new(vec![observed(Call::Inspect(first, old), running())])
    };
    let row = ServingPointer {
        deploy: DeploymentLogId::parse("older").unwrap(),
        containers: vec![old],
        previous: Some(DeploymentLogId::parse("oldest").unwrap()),
        retiring: Vec::new(),
        deciding: None,
    };
    client.pointers.lock().unwrap().insert(api(), row.clone());

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::Old)]));
    assert_eq!(
        client.pointers.lock().unwrap().get(&api()),
        Some(&row),
        "the untouched copy serves again and no hold is left behind"
    );
    client.assert_done();
}

#[tokio::test]
async fn a_stop_first_step_retires_every_old_copy_in_one_amendment() {
    let machines = [machine('1'), machine('2'), machine('3')];
    let olds = [container('a'), container('b'), container('c')];
    let news = [container('d'), container('e'), container('f')];
    let plan: Vec<_> = machines
        .iter()
        .zip(&olds)
        .map(|(machine, old)| swap(machine, old, UpdateOrder::StopFirst))
        .collect();
    let mut steps: Vec<_> = machines
        .iter()
        .zip(&olds)
        .map(|(machine, old)| observed(Call::Inspect(*machine, *old), running()))
        .collect();
    for ((machine, old), new) in machines.iter().zip(&olds).zip(&news) {
        steps.push(ok(Call::Stop(*machine, *old)));
        steps.push(created(
            Call::Run(*machine, ContainerKind::ServiceContainer),
            new,
        ));
    }
    let client = Scripted::new(steps);
    let older = DeploymentLogId::parse("older").unwrap();
    client.pointers.lock().unwrap().insert(
        api(),
        ServingPointer {
            deploy: older.clone(),
            containers: olds.to_vec(),
            previous: None,
            retiring: Vec::new(),
            deciding: None,
        },
    );

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::New)]));
    let amendments = client.pins();
    let [amendment] = amendments.as_slice() else {
        panic!("one amendment for the step: {amendments:?}");
    };
    assert_eq!(amendment.pointer.retiring, olds);
    assert_eq!(
        client.pointers.lock().unwrap().get(&api()),
        Some(&ServingPointer {
            deploy: run_id(),
            containers: news.to_vec(),
            previous: Some(older),
            retiring: olds.to_vec(),
            deciding: None,
        })
    );
    client.assert_done();
}

#[test]
fn a_hold_outlasts_every_health_wait_the_plan_configures() {
    let machine = machine('1');
    let slow = Duration::from_secs(3 * 60 * 60);
    let mut check = configured_healthcheck();
    check.deadline_millis = Some(u64::try_from(slow.as_millis()).unwrap());
    let slow_check = spec(
        None,
        Some(ployz_core::HealthcheckSpec::Configured(check)),
        None,
    );
    let mut slow_monitor = spec(None, None, None);
    slow_monitor.update.monitor_millis = Some(u64::try_from(slow.as_millis()).unwrap());
    for mut spec in [slow_check, slow_monitor] {
        spec.update.order = UpdateOrder::StopFirst;
        let plan = [
            DeployOperation::ReplaceContainer(ReplacementOperation {
                machine_id: machine,
                old_container_id: container('a'),
                spec,
                skip_health_monitor: false,
            }),
            swap(&machine, &container('c'), UpdateOrder::StopFirst),
        ];

        assert!(hold_for(&plan) >= HOLD_BEYOND_HEALTH + slow);
    }
}

fn unbounded_monitor_swaps(count: usize) -> Vec<DeployOperation> {
    let mut spec = spec(None, None, None);
    spec.update.order = UpdateOrder::StopFirst;
    spec.update.monitor_millis = Some(u64::MAX);
    (0..count)
        .map(|_| {
            DeployOperation::ReplaceContainer(ReplacementOperation {
                machine_id: machine('1'),
                old_container_id: container('a'),
                spec: spec.clone(),
                skip_health_monitor: false,
            })
        })
        .collect()
}

#[test]
fn an_unbounded_monitor_holds_no_longer_than_the_cap() {
    assert!(hold_for(&unbounded_monitor_swaps(1)) <= HOLD_MAX);
}

#[test]
fn a_hold_over_many_unbounded_monitors_still_lapses() {
    let hold = Hold::lasting(run_id(), hold_for(&unbounded_monitor_swaps(501)));
    let after = std::time::UNIX_EPOCH
        .checked_add(Duration::from_secs(
            hold.until_unix_seconds.saturating_add(1),
        ))
        .expect("the hold expires at a representable time");

    assert!(!hold.holds_at(after));
}

#[tokio::test]
async fn a_stopped_copy_stays_held_until_its_deploy_publishes() {
    let machine = machine('1');
    let (old, new) = (container('a'), container('b'));
    let plan = vec![swap(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine, old), running()),
        ok(Call::Stop(machine, old)),
        created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
    ]);
    let older = DeploymentLogId::parse("older").unwrap();
    client.pointers.lock().unwrap().insert(
        api(),
        ServingPointer {
            deploy: older,
            containers: vec![old],
            previous: None,
            retiring: Vec::new(),
            deciding: None,
        },
    );

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::New)]));
    let amendments = client.pins();
    let [amendment] = amendments.as_slice() else {
        panic!("one amendment: {amendments:?}");
    };
    assert_eq!(
        amendment.pointer.deciding.as_ref().map(|hold| &hold.deploy),
        Some(&run_id()),
        "the stop holds its copy for this deploy"
    );
    let published = client
        .pointers
        .lock()
        .unwrap()
        .get(&api())
        .cloned()
        .unwrap();
    assert_eq!(published.deploy, run_id());
    assert_eq!(published.deciding, None, "publishing lets the copy retire");
    client.assert_done();
}

#[tokio::test]
async fn a_rolling_failure_on_the_first_machine_leaves_the_second_untouched() {
    let (first, second) = (machine('1'), machine('2'));
    let (old_first, old_second) = (container('a'), container('b'));
    let plan = vec![
        swap(&first, &old_first, UpdateOrder::Rolling),
        swap(&second, &old_second, UpdateOrder::Rolling),
    ];
    let client = Scripted::new(vec![
        observed(Call::Inspect(first, old_first), running()),
        ok(Call::Stop(first, old_first)),
        failed(
            Call::Run(first, ContainerKind::ServiceContainer),
            "start failed",
        ),
        ok(Call::DiscardSlot(first, ContainerKind::ServiceContainer)),
        ok(Call::Restart(first, old_first)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(&outcome, DeployOutcome::Failed { unexecuted, .. } if unexecuted.len() == 1),
        "{outcome:?}"
    );
    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::Old)]));
    client.assert_done();
}
