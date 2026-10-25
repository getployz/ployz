use super::flip::{execute_reported, left_running, reported_warnings};
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
        assert!(client.brought_in().is_empty());
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
        let client = Scripted::new(steps).with_copy(&machine, &old, CopyRotation::In);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome,
            DeployOutcome::Failed { failed: FailedOperation::Replacement {
                compensation: ReplacementCompensation::OldStopped {
                    stop_new_container: Some(stop_new_container),
                    restore_old_container:
                        restart @ (RestoreAttempt::Restored | RestoreAttempt::Failed { .. }),
                },
                ..
            }, .. } if stop_new_container.stopped() == stop_succeeds
                && restart.restored() == restart_succeeds
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
                    restore_old_container: restart,
                },
                ..
            }, .. } if restart.restored() == restart_succeeds
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
                        restore_old_container: RestoreAttempt::Restored,
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
                    restore_old_container: RestoreAttempt::Restored,
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
                        restore_old_container: RestoreAttempt::Restored,
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
        Step(Call::Inspect(machine, old), Reply::Error(missing.clone())),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}

/// A Server being removed stops and removes its copies itself, and the deploy's own
/// stop or remove of such a copy finds it gone, which is what it asked for. Nothing
/// is reported left running on that Server (#2924).
#[tokio::test]
async fn an_old_copy_its_leaving_server_took_is_not_left_running() {
    for order in [UpdateOrder::StartFirst, UpdateOrder::StopFirst] {
        let machine = machine('1');
        let old = container('a');
        let new = container('b');
        let plan = vec![replacement(&machine, &old, order)];
        // Each stop or remove of `old` answers not found without a scripted step.
        let mut steps = Vec::new();
        if order == UpdateOrder::StopFirst {
            steps.push(observed(Call::Inspect(machine, old), running()));
        }
        steps.extend([
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            observed(Call::Inspect(machine, old), running()),
        ]);
        let client = Scripted::new(steps).with_copy_gone(&machine, &old);

        let (outcome, warnings) =
            execute_reported(&plan, &[machine], &client, &CancellationToken::new()).await;

        assert!(
            matches!(outcome, DeployOutcome::Success { .. }),
            "{order:?}: {outcome:?}"
        );
        assert!(warnings.is_empty(), "{order:?}: {warnings:?}");
        client.assert_done();
    }
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
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            Step(Call::Inspect(machine, old), Reply::Error(missing.clone())),
        ]);
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
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Remove(machine, old)),
        ]);
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
    ])
    .with_copy(&machine, &old, CopyRotation::In);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Replacement {
                compensation: ReplacementCompensation::OldStopped {
                    stop_new_container: None,
                    restore_old_container: RestoreAttempt::Restored,
                },
                ..
            },
            ..
        }
    ));
    assert_eq!(client.rotation(&old), Some(CopyRotation::In));
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
        observed(Call::Inspect(first, old_first), running()),
        observed(Call::Inspect(second, old_second), running()),
        ok(Call::Remove(first, old_first)),
        ok(Call::Remove(second, old_second)),
    ])
    .with_copy(&first, &old_first, CopyRotation::In)
    .with_copy(&second, &old_second, CopyRotation::In);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::New)]));
    assert_eq!(client.brought_in(), [new_first, new_second]);
    for old in [old_first, old_second] {
        assert_eq!(
            client.rotation(&old),
            None,
            "the stopped copies are retired"
        );
    }
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_stop_first_step_withdraws_beside_a_pending_copy_a_dead_run_left() {
    let (first, second) = (machine('1'), machine('2'));
    let (old_first, old_second) = (container('a'), container('b'));
    let (new_first, new_second) = (container('c'), container('d'));
    let plan = vec![
        swap(&first, &old_first, UpdateOrder::StopFirst),
        swap(&second, &old_second, UpdateOrder::StopFirst),
    ];
    let mut missing = error("not found");
    missing.code = RpcErrorCode::NotFound;
    let client = Scripted::new(vec![
        observed(Call::Inspect(first, old_first), running()),
        observed(Call::Inspect(second, old_second), running()),
        ok(Call::Remove(second, old_second)),
        ok(Call::Stop(first, old_first)),
        created(
            Call::Run(first, ContainerKind::ServiceContainer),
            &new_first,
        ),
        created(
            Call::Run(second, ContainerKind::ServiceContainer),
            &new_second,
        ),
        observed(Call::Inspect(first, old_first), running()),
        Step(Call::Inspect(second, old_second), Reply::Error(missing)),
        ok(Call::Remove(first, old_first)),
    ])
    .with_copy(&first, &old_first, CopyRotation::In)
    .with_copy(&second, &old_second, CopyRotation::Pending)
    .routing(&api());

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome.services(),
        ended(&[("api", ServiceResult::New)]),
        "{outcome:?}"
    );
    assert_eq!(client.brought_in(), [new_first, new_second]);
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_stop_first_scale_up_withdraws_the_old_copy_beside_pending_new_ones() {
    let (first, second, third) = (machine('1'), machine('2'), machine('3'));
    let old = container('a');
    let (new_first, new_second, new_third) = (container('c'), container('d'), container('e'));
    let mut added = spec(None, None, None);
    added.update.order = UpdateOrder::StopFirst;
    let plan = vec![
        swap(&third, &old, UpdateOrder::StopFirst),
        run(&first, added.clone(), true),
        run(&second, added, true),
    ];
    let mut client = Scripted::new(vec![
        observed(Call::Inspect(third, old), running()),
        created(
            Call::Run(first, ContainerKind::ServiceContainer),
            &new_first,
        ),
        created(
            Call::Run(second, ContainerKind::ServiceContainer),
            &new_second,
        ),
        ok(Call::Stop(third, old)),
        created(
            Call::Run(third, ContainerKind::ServiceContainer),
            &new_third,
        ),
        observed(Call::Inspect(third, old), running()),
        ok(Call::Remove(third, old)),
    ])
    .with_copy(&third, &old, CopyRotation::In)
    .routing(&api());
    client.acks.insert(third, Ack::After(1));

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome.services(),
        ended(&[("api", ServiceResult::New)]),
        "{outcome:?}"
    );
    assert_eq!(
        client.brought_in().into_iter().collect::<BTreeSet<_>>(),
        BTreeSet::from([new_first, new_second, new_third])
    );
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
        ]);
        // A Rolling step brought its new copy In, so it is held and stopped
        // before the old copy comes back, and leaves only after.
        if order == UpdateOrder::Rolling {
            steps.extend([
                ok(Call::Stop(first, new_first)),
                ok(Call::Restart(first, old_first)),
                ok(Call::Remove(first, new_first)),
            ]);
        } else {
            steps.extend([
                ok(Call::Discard(first, new_first)),
                ok(Call::Restart(first, old_first)),
            ]);
        }
        let client = Scripted::new(steps)
            .with_copy(&first, &old_first, CopyRotation::In)
            .with_copy(&second, &old_second, CopyRotation::In);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(
            matches!(&outcome, DeployOutcome::Failed { .. }),
            "{order:?}: {outcome:?}"
        );
        assert_eq!(outcome.services(), ended(&[("api", ServiceResult::Old)]));
        let restored = [old_second, old_first];
        if order == UpdateOrder::Rolling {
            assert_eq!(client.brought_in().get(1..), Some(&restored[..]));
        } else {
            assert_eq!(client.brought_in(), restored);
        }
        for old in [old_first, old_second] {
            assert_eq!(
                client.rotation(&old),
                Some(CopyRotation::In),
                "{order:?}: the restarted copies serve again"
            );
        }
        assert_eq!(client.rotation(&new_first), None);
        client.assert_done();
    }
}

#[tokio::test]
async fn a_failed_deploy_leaves_another_deploys_hold_in_place() {
    let first = machine('1');
    let old = container('a');
    let plan = vec![swap(&first, &old, UpdateOrder::StopFirst)];
    let rival = DeploymentLogId::parse("rival").unwrap();
    let client = Scripted {
        moved: Mutex::new(BTreeMap::from([(
            (old, Ask::In),
            CopyRotation::Held(rival.clone()),
        )])),
        ..Scripted::new(vec![
            observed(Call::Inspect(first, old), running()),
            ok(Call::Stop(first, old)),
            failed(
                Call::Run(first, ContainerKind::ServiceContainer),
                "start failed",
            ),
            ok(Call::DiscardSlot(first, ContainerKind::ServiceContainer)),
        ])
    }
    .with_copy(&first, &old, CopyRotation::In);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    let DeployOutcome::Failed {
        failed:
            FailedOperation::Replacement {
                compensation:
                    ReplacementCompensation::OldStopped {
                        restore_old_container,
                        ..
                    },
                ..
            },
        ..
    } = &outcome
    else {
        panic!("{outcome:?}");
    };
    assert!(!restore_old_container.restored(), "{outcome:?}");
    assert!(client.brought_in().is_empty());
    assert_eq!(
        client.rotation(&old),
        Some(CopyRotation::Held(rival)),
        "the other deploy still holds the copy"
    );
    client.assert_done();
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
        observed(Call::Inspect(machine, old), running()),
        ok(Call::Remove(machine, old)),
    ])
    .with_copy(&machine, &old, CopyRotation::In);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::New)]));
    assert_eq!(
        client.moves(),
        [
            Move::To(old, CopyRotation::Held(run_id())),
            Move::To(new, CopyRotation::In),
            Move::To(old, CopyRotation::Out),
            Move::Gone(old),
        ],
        "the stop holds its copy for this deploy until the new copy is In"
    );
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
    ])
    .with_copy(&first, &old_first, CopyRotation::In);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(&outcome, DeployOutcome::Failed { unexecuted, .. } if unexecuted.len() == 1),
        "{outcome:?}"
    );
    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::Old)]));
    client.assert_done();
}

#[tokio::test]
async fn a_failed_group_stops_its_new_copies_before_it_restores_the_held_ones() {
    let (first, second) = (machine('1'), machine('2'));
    let (old_first, old_second) = (container('a'), container('b'));
    let (new_first, new_second) = (container('c'), container('d'));
    let plan = vec![
        swap(&first, &old_first, UpdateOrder::StopFirst),
        swap(&second, &old_second, UpdateOrder::StopFirst),
    ];
    let client = Scripted {
        unhealthy_in: BTreeMap::from([(
            new_second,
            HealthVerdict::Failed {
                observation: unhealthy(),
                last_check: None,
                oom_killed: false,
                departing: None,
            },
        )]),
        ..Scripted::new(vec![
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
            ok(Call::Stop(first, new_first)),
            ok(Call::Discard(second, new_second)),
            ok(Call::Restart(first, old_first)),
            ok(Call::Restart(second, old_second)),
            ok(Call::Remove(first, new_first)),
        ])
    }
    .with_copy(&first, &old_first, CopyRotation::In)
    .with_copy(&second, &old_second, CopyRotation::In);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::Old)]));
    assert_eq!(
        client.moves(),
        [
            Move::To(old_first, CopyRotation::Held(run_id())),
            Move::To(old_second, CopyRotation::Held(run_id())),
            Move::To(new_first, CopyRotation::In),
            Move::To(new_first, CopyRotation::Held(run_id())),
            Move::Gone(new_second),
            Move::To(old_first, CopyRotation::In),
            Move::To(old_second, CopyRotation::In),
            Move::To(new_first, CopyRotation::Out),
            Move::Gone(new_first),
        ],
        "a new copy already In is held and stopped, the old copies come back In, \
         and only then does the new copy leave"
    );
    client.assert_done();
}

#[tokio::test]
async fn stop_first_from_held_copy() {
    let rival = DeploymentLogId::parse("rival").unwrap();
    for (before, starts) in [
        (CopyRotation::Held(rival.clone()), true),
        (CopyRotation::Held(rival.clone()), false),
        (CopyRotation::Out, true),
        (CopyRotation::Out, false),
    ] {
        let machine = machine('1');
        let (old, new) = (container('a'), container('b'));
        let plan = vec![swap(&machine, &old, UpdateOrder::StopFirst)];
        let mut steps = vec![
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Stop(machine, old)),
        ];
        if starts {
            steps.extend([
                created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
                observed(Call::Inspect(machine, old), exited(0)),
                ok(Call::Remove(machine, old)),
            ]);
        } else {
            steps.extend([
                failed(
                    Call::Run(machine, ContainerKind::ServiceContainer),
                    "start failed",
                ),
                ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
            ]);
        }
        let client = Scripted::new(steps).with_copy(&machine, &old, before.clone());

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        let mut moves = Vec::new();
        if matches!(before, CopyRotation::Held(_)) {
            moves.push(Move::To(old, CopyRotation::Out));
        }
        if starts {
            assert_eq!(outcome.services(), ended(&[("api", ServiceResult::New)]));
            moves.extend([Move::To(new, CopyRotation::In), Move::Gone(old)]);
        } else {
            assert!(
                matches!(
                    &outcome,
                    DeployOutcome::Failed {
                        failed: FailedOperation::Replacement {
                            compensation: ReplacementCompensation::OldStopped {
                                restore_old_container: RestoreAttempt::NotServing,
                                ..
                            },
                            ..
                        },
                        ..
                    }
                ),
                "{before:?}: an old copy that was not serving is not restored, got {outcome:?}"
            );
            assert_eq!(client.rotation(&old), Some(CopyRotation::Out));
        }
        assert_eq!(
            client.moves(),
            moves,
            "{before:?}: the old copy goes Out then Gone and is never Held by this run"
        );
        client.assert_done();
    }
}

#[tokio::test]
async fn a_refused_or_failed_restore_leaves_the_service_empty() {
    let failed_health = HealthVerdict::Failed {
        observation: unhealthy(),
        last_check: None,
        oom_killed: false,
        departing: None,
    };
    for refused in [true, false] {
        let machine = machine('1');
        let old = container('a');
        let plan = vec![swap(&machine, &old, UpdateOrder::StopFirst)];
        let mut steps = vec![
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Stop(machine, old)),
            failed(
                Call::Run(machine, ContainerKind::ServiceContainer),
                "start failed",
            ),
            ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
        ];
        let mut moved = BTreeMap::new();
        if refused {
            moved.insert((old, Ask::In), CopyRotation::Out);
        } else {
            steps.push(Step(
                Call::Restart(machine, old),
                Reply::Settled(old, failed_health.clone()),
            ));
        }
        let client = Scripted {
            moved: Mutex::new(moved),
            ..Scripted::new(steps)
        }
        .with_copy(&machine, &old, CopyRotation::In);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(
            matches!(
                &outcome,
                DeployOutcome::Failed {
                    failed: FailedOperation::Replacement {
                        compensation: ReplacementCompensation::OldStopped {
                            restore_old_container: RestoreAttempt::Failed { .. },
                            ..
                        },
                        ..
                    },
                    ..
                }
            ),
            "refused {refused}: {outcome:?}"
        );
        assert_eq!(outcome.services(), ended(&[("api", ServiceResult::Old)]));
        assert_eq!(
            client.rotation(&old),
            Some(if refused {
                CopyRotation::Out
            } else {
                CopyRotation::Held(run_id())
            }),
            "refused {refused}: the old copy is not brought back In"
        );
        assert!(
            client.brought_in().is_empty(),
            "refused {refused}: no copy of the Service is In"
        );
        client.assert_done();
    }
}

#[tokio::test(start_paused = true)]
async fn a_restore_a_newer_deploy_refuses_reads_as_the_newer_deploy_serving() {
    for removes in [true, false] {
        let machine = machine('1');
        let (old, newer) = (container('a'), container('c'));
        let plan = vec![swap(&machine, &old, UpdateOrder::StopFirst)];
        let client = Scripted::new(vec![
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Stop(machine, old)),
            failed(
                Call::Run(machine, ContainerKind::ServiceContainer),
                "start failed",
            ),
            ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
            if removes {
                ok(Call::Remove(machine, old))
            } else {
                failed(Call::Remove(machine, old), "the disk is busy")
            },
        ])
        .with_copy(&machine, &old, CopyRotation::In)
        .with_copy_at(&api(), &machine, &newer, CopyRotation::In, Gen::new(100));

        let (outcome, warnings) =
            execute_reported(&plan, &[machine], &client, &CancellationToken::new()).await;

        assert_eq!(
            outcome.services(),
            ended(&[("api", ServiceResult::Newer)]),
            "removes {removes}"
        );
        let shown = crate::ui::plain(&crate::deploy::report::failure(&outcome, &[], Vec::new()));
        assert!(
            shown.contains(
                "  A newer deploy is already serving; the old Container will not serve again.\n"
            ),
            "removes {removes}: {shown}"
        );
        assert!(
            !shown.contains("Could not restore"),
            "removes {removes}: {shown}"
        );
        assert!(!shown.contains("newer deploy is already serving app/api"));
        if removes {
            assert!(warnings.is_empty(), "{warnings:?}");
            assert_eq!(client.rotation(&old), None);
        } else {
            assert_eq!(
                warnings,
                [left_running(
                    "machine-1",
                    "a newer deploy is already serving app/api, and removing it failed: the disk is busy"
                )]
            );
        }
        client.assert_done();
    }
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_deploy_still_removes_an_old_copy_a_newer_deploy_refuses() {
    let machine = machine('1');
    let (old, newer) = (container('a'), container('c'));
    let plan = vec![swap(&machine, &old, UpdateOrder::StopFirst)];
    let run = Call::Run(machine, ContainerKind::ServiceContainer);
    let cancellation = CancellationToken::new();
    let client = Scripted {
        cancel_on: Some((run.clone(), cancellation.clone())),
        read_latency: true,
        ..Scripted::new(vec![
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Stop(machine, old)),
            failed(run, "start failed"),
            ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
            ok(Call::Remove(machine, old)),
        ])
    }
    .with_copy(&machine, &old, CopyRotation::In)
    .with_copy_at(&api(), &machine, &newer, CopyRotation::In, Gen::new(100));

    let (_, warnings) = execute_reported(&plan, &[machine], &client, &cancellation).await;

    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(client.rotation(&old), None, "the old copy is removed");
    client.assert_done();
}

#[tokio::test]
async fn a_removed_service_retires_every_copy_through_out_and_ends_empty() {
    let (first, second) = (machine('1'), machine('2'));
    let (one, two) = (container('a'), container('b'));
    let retire =
        |machine_id: MachineId, container_id: ContainerId| DeployOperation::RemoveContainer {
            machine_id,
            container_id,
            purpose: ployz_core::RemoveContainerPurpose::Retire,
        };
    let plan = vec![retire(first, one), retire(second, two)];
    let client = Scripted::new(vec![
        observed(Call::Inspect(first, one), running()),
        ok(Call::Remove(first, one)),
        observed(Call::Inspect(second, two), running()),
        ok(Call::Remove(second, two)),
    ])
    .with_copy(&first, &one, CopyRotation::In)
    .with_copy(&second, &two, CopyRotation::In);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        client.moves(),
        [
            Move::To(one, CopyRotation::Out),
            Move::Gone(one),
            Move::To(two, CopyRotation::Out),
            Move::Gone(two),
        ],
        "the last In copy leaves too: a removal may empty the Service"
    );
    client.assert_done();
}

/// Every store still lists the In copy on the Down Server, so proxies would keep the
/// live copy routed until that Server is back. The removal takes it out without
/// waiting for that, and names the copy it could not reach for the next full Deploy.
#[tokio::test(start_paused = true)]
async fn a_removal_beside_a_copy_on_an_unobserved_server_takes_the_live_copy_out_and_names_the_server()
 {
    let (live, away) = (machine('1'), machine('3'));
    let (here, there) = (container('a'), container('b'));
    let plan = crate::deploy::DeployPlan::for_execution_test(
        vec![DeployOperation::RemoveContainer {
            machine_id: live,
            container_id: here,
            purpose: ployz_core::RemoveContainerPurpose::Retire,
        }],
        test_namespace(),
    )
    .with_machine_names(&[live, away])
    .with_unobserved(&[away]);
    let client = Scripted {
        down: BTreeSet::from([away]),
        store: Store::Copies,
        ..Scripted::new(vec![
            observed(Call::Inspect(live, here), running()),
            ok(Call::Remove(live, here)),
        ])
    }
    .with_copy(&live, &here, CopyRotation::In)
    .with_copy(&away, &there, CopyRotation::In)
    .routing(&api());

    let started = tokio::time::Instant::now();
    let (outcome, warnings) = reported_warnings(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(
        started.elapsed() < std::time::Duration::from_secs(1),
        "no drain wait on a route the Down Server holds: {:?}",
        started.elapsed()
    );
    assert_eq!(
        client.moves(),
        [Move::To(here, CopyRotation::Out), Move::Gone(here)]
    );
    assert_eq!(
        warnings,
        [DeployWarning::LeftRunning {
            service: api(),
            container_id: there,
            machine: "machine-3".into(),
            reason:
                "machine-3 didn't answer, the next full deploy removes its copy once it is back"
                    .into(),
        }]
    );
    client.assert_done();
}

/// A finished pre-deploy hook stays Pending for life, so removing it drains nothing
/// and a copy of its Service on an unobserved Server can't hold it up.
#[tokio::test(start_paused = true)]
async fn a_pending_copy_is_removed_beside_a_copy_on_an_unobserved_server() {
    let (live, away) = (machine('1'), machine('3'));
    let (hook, there) = (container('a'), container('b'));
    let plan = crate::deploy::DeployPlan::for_execution_test(
        vec![DeployOperation::RemoveContainer {
            machine_id: live,
            container_id: hook,
            purpose: ployz_core::RemoveContainerPurpose::Retire,
        }],
        test_namespace(),
    )
    .with_machine_names(&[live, away])
    .with_unobserved(&[away]);
    let client = Scripted {
        down: BTreeSet::from([away]),
        store: Store::Copies,
        ..Scripted::new(vec![
            observed(Call::Inspect(live, hook), exited(0)),
            ok(Call::Remove(live, hook)),
        ])
    }
    .with_copy(&live, &hook, CopyRotation::Pending)
    .with_copy(&away, &there, CopyRotation::In)
    .routing(&api());

    let outcome = super::execute_operation_sequence(
        &plan,
        &run_id(),
        &client,
        &CancellationToken::new(),
        None,
    )
    .await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.moves(), [Move::Gone(hook)]);
    client.assert_done();
}

/// Proxies drop a Service's route once no copy is In or Pending, so a copy already
/// Out on an unobserved Server doesn't hold up the removal.
#[tokio::test(start_paused = true)]
async fn a_removal_finishes_when_the_copy_on_an_unobserved_server_is_out() {
    let (live, away) = (machine('1'), machine('3'));
    let (here, there) = (container('a'), container('b'));
    let plan = crate::deploy::DeployPlan::for_execution_test(
        vec![DeployOperation::RemoveContainer {
            machine_id: live,
            container_id: here,
            purpose: ployz_core::RemoveContainerPurpose::Retire,
        }],
        test_namespace(),
    )
    .with_machine_names(&[live, away])
    .with_unobserved(&[away]);
    let client = Scripted {
        down: BTreeSet::from([away]),
        store: Store::Copies,
        ..Scripted::new(vec![
            observed(Call::Inspect(live, here), running()),
            ok(Call::Remove(live, here)),
        ])
    }
    .with_copy(&live, &here, CopyRotation::In)
    .with_copy(&away, &there, CopyRotation::Out)
    .routing(&api());

    let outcome = super::execute_operation_sequence(
        &plan,
        &run_id(),
        &client,
        &CancellationToken::new(),
        None,
    )
    .await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        client.moves(),
        [Move::To(here, CopyRotation::Out), Move::Gone(here)]
    );
    client.assert_done();
}

#[tokio::test]
async fn stop_error_restores_the_healthy_held_copy() {
    let machine = machine('1');
    let old = container('a');
    let plan = vec![replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine, old), running()),
        failed(
            Call::Stop(machine, old),
            "Docker stop failed before stopping the process",
        ),
    ])
    .with_copy(&machine, &old, CopyRotation::In);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert!(!client.copies.lock().unwrap().get(&old).unwrap().stopped);
    assert_eq!(
        client.rotation(&old),
        Some(CopyRotation::In),
        "a failed stop-first Deploy restores its healthy running copy to In"
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_rolling_step_brings_its_new_copy_in_before_the_next_machine_withdraws() {
    let (first, second) = (machine('1'), machine('2'));
    let (old_first, old_second) = (container('a'), container('b'));
    let (new_first, new_second) = (container('c'), container('d'));
    let plan = vec![
        swap(&first, &old_first, UpdateOrder::Rolling),
        swap(&second, &old_second, UpdateOrder::Rolling),
    ];
    let client = Scripted::new(vec![
        observed(Call::Inspect(first, old_first), running()),
        ok(Call::Stop(first, old_first)),
        created(
            Call::Run(first, ContainerKind::ServiceContainer),
            &new_first,
        ),
        observed(Call::Inspect(second, old_second), running()),
        ok(Call::Stop(second, old_second)),
        created(
            Call::Run(second, ContainerKind::ServiceContainer),
            &new_second,
        ),
        observed(Call::Inspect(first, old_first), running()),
        observed(Call::Inspect(second, old_second), running()),
        ok(Call::Remove(first, old_first)),
        ok(Call::Remove(second, old_second)),
    ])
    .with_copy(&first, &old_first, CopyRotation::In)
    .with_copy(&second, &old_second, CopyRotation::In);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome.services(),
        ended(&[("api", ServiceResult::New)]),
        "{outcome:?}"
    );
    let moves = client.moves();
    let at = |applied: Move| moves.iter().position(|other| *other == applied).unwrap();
    assert!(
        at(Move::To(new_first, CopyRotation::In))
            < at(Move::To(old_second, CopyRotation::Held(run_id()))),
        "the second Machine withdraws only once the first's new copy serves: {moves:?}"
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_refused_hold_fails_the_stop_first_step_before_any_copy_starts() {
    let (first, second) = (machine('1'), machine('2'));
    let (old_first, old_second) = (container('a'), container('b'));
    let plan = vec![
        swap(&first, &old_first, UpdateOrder::StopFirst),
        swap(&second, &old_second, UpdateOrder::StopFirst),
    ];
    let rival = DeploymentLogId::parse("rival").unwrap();
    let client = Scripted {
        moved: Mutex::new(BTreeMap::from([(
            (old_second, Ask::Held),
            CopyRotation::Held(rival.clone()),
        )])),
        ..Scripted::new(vec![
            observed(Call::Inspect(first, old_first), running()),
            observed(Call::Inspect(second, old_second), running()),
        ])
    }
    .with_copy(&first, &old_first, CopyRotation::In)
    .with_copy(&second, &old_second, CopyRotation::In);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::Old)]));
    assert_eq!(
        client.rotation(&old_first),
        Some(CopyRotation::In),
        "the copy the step Held serves again"
    );
    assert_eq!(
        client.rotation(&old_second),
        Some(CopyRotation::Held(rival))
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_cancel_while_the_stop_first_step_withdraws_brings_every_held_copy_back() {
    let (first, second) = (machine('1'), machine('2'));
    let (old_first, old_second) = (container('a'), container('b'));
    let plan = vec![
        swap(&first, &old_first, UpdateOrder::StopFirst),
        swap(&second, &old_second, UpdateOrder::StopFirst),
    ];
    let cancellation = CancellationToken::new();
    let client = Scripted {
        withheld: BTreeMap::from([(api(), usize::MAX)]),
        cancel_on_move: Some((
            Move::To(old_second, CopyRotation::Held(run_id())),
            cancellation.clone(),
        )),
        ..Scripted::new(vec![
            observed(Call::Inspect(first, old_first), running()),
            observed(Call::Inspect(second, old_second), running()),
        ])
    }
    .with_copy(&first, &old_first, CopyRotation::In)
    .with_copy(&second, &old_second, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[first, second], &client, &cancellation).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    for old in [old_first, old_second] {
        assert_eq!(client.rotation(&old), Some(CopyRotation::In));
    }
    assert_eq!(
        warnings,
        [
            left_running("machine-1", "interrupted"),
            left_running("machine-2", "interrupted"),
        ]
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_stop_first_task_the_machine_window_delays_is_withdrawn_with_its_step() {
    let [first, second, third] = [machine('1'), machine('2'), machine('3')];
    let olds = "abcdef01".chars().map(container).collect::<Vec<_>>();
    let news = "23456789".chars().map(container).collect::<Vec<_>>();
    // Six copies on the first Machine: one more than its window starts at once.
    let owners = [first, first, first, first, first, first, second, third];
    let plan = owners
        .iter()
        .zip(&olds)
        .map(|(owner, old)| swap(owner, old, UpdateOrder::StopFirst))
        .collect::<Vec<_>>();
    let order = [0, 1, 2, 3, 4, 6, 7, 5];
    let mut steps = owners
        .iter()
        .zip(&olds)
        .map(|(owner, old)| observed(Call::Inspect(*owner, *old), running()))
        .collect::<Vec<_>>();
    for task in order {
        let (Some(owner), Some(old), Some(new)) =
            (owners.get(task), olds.get(task), news.get(task))
        else {
            panic!("task {task} is in the plan");
        };
        steps.push(ok(Call::Stop(*owner, *old)));
        steps.push(created(
            Call::Run(*owner, ContainerKind::ServiceContainer),
            new,
        ));
    }
    for (owner, old) in owners.iter().zip(&olds) {
        steps.push(observed(Call::Inspect(*owner, *old), running()));
    }
    for (owner, old) in owners.iter().zip(&olds) {
        steps.push(ok(Call::Remove(*owner, *old)));
    }
    let client = owners
        .iter()
        .zip(&olds)
        .fold(Scripted::new(steps), |client, (owner, old)| {
            client.with_copy(owner, old, CopyRotation::In)
        });

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome.services(),
        ended(&[("api", ServiceResult::New)]),
        "{outcome:?}"
    );
    let mut brought_in = client.brought_in();
    brought_in.sort();
    assert_eq!(brought_in, news);
    client.assert_done();
}

/// A start-first replacement copy found unhealthy while its Server was being removed
/// gets the cleanup any unhealthy copy gets: the deploy removes it and never flips,
/// and names the removal (#2924).
#[tokio::test]
async fn a_replacement_copy_found_unhealthy_while_its_server_left_is_removed() {
    let machine = machine('1');
    let old = container('a');
    let new = container('b');
    let plan = vec![replacement(&machine, &old, UpdateOrder::StartFirst)];
    let client = Scripted::new(vec![
        Step(
            Call::Run(machine, ContainerKind::ServiceContainer),
            Reply::Settled(new, exited_leaving()),
        ),
        ok(Call::Discard(machine, new)),
    ]);

    let DeployOutcome::Failed {
        failed: FailedOperation::Operation { error, .. },
        ..
    } = execute_with(&plan, &client, &CancellationToken::new()).await
    else {
        panic!("the deploy should fail");
    };

    assert!(
        matches!(
            &error,
            ExecutionError::Health {
                container_id,
                departing: Some(_),
                ..
            } if *container_id == new
        ),
        "{error:?}"
    );
    assert_eq!(
        ployz_store::Failure::of(&error, &[]).reason,
        "Server lc-3 was removed while the deploy ran"
    );
    assert!(client.brought_in().is_empty());
    client.assert_done();
}
