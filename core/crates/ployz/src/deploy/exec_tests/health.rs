use ployz_core::ByteQuantity;

use super::*;

#[tokio::test(start_paused = true)]
async fn a_settled_start_waits_out_a_slow_pull() {
    let machine = machine('1');
    let plan = vec![run(
        &machine,
        spec(Some(0), Some(healthcheck()), None),
        false,
    )];
    let client = Scripted::new(vec![Step(
        Call::Run(machine, ContainerKind::ServiceContainer),
        Reply::CreatedAfter(container('a'), || {
            tokio::time::sleep(Duration::from_secs(30 * 60))
        }),
    )]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_machine_that_stops_answering_fails_the_start_and_discards_the_slot() {
    let machine = machine('1');
    let plan = vec![run(
        &machine,
        spec(Some(0), Some(healthcheck()), None),
        false,
    )];
    let lost = || {
        failed_unavailable(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &format!("Machine {machine} did not answer: deadline has elapsed"),
        )
    };
    let attempts =
        usize::try_from(RESTART_WAIT.as_millis() / RESTART_POLL.as_millis()).unwrap() + 1;
    let mut steps: Vec<Step> = std::iter::repeat_with(lost).take(attempts).collect();
    steps.push(ok(Call::DiscardSlot(
        machine,
        ContainerKind::ServiceContainer,
    )));
    let client = Scripted::new(steps);

    let DeployOutcome::Failed {
        failed: FailedOperation::Operation { error, .. },
        ..
    } = execute_with(&plan, &client, &CancellationToken::new()).await
    else {
        panic!("the deploy should fail");
    };

    let ExecutionError::Machine { error, .. } = error else {
        panic!("a silent Machine fails the start as a Machine error, got {error:?}");
    };
    assert_eq!(error.code, RpcErrorCode::Unavailable);
    assert_eq!(
        error.message,
        format!("Machine {machine} did not answer: deadline has elapsed")
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn dependency_gate_waits_for_all_service_containers_before_the_dependent() {
    let dependency = QualifiedService::parse("app/api").unwrap();
    let dependent = QualifiedService::parse("app/web").unwrap();
    let first_id = container('a');
    let second_id = container('b');
    let mut first = routed(&machine('1'), &first_id, healthy());
    first
        .try_update(|parts| parts.resolved_spec.container.healthcheck = Some(healthcheck()))
        .unwrap();
    let mut second = routed(&machine('2'), &second_id, starting());
    second
        .try_update(|parts| parts.resolved_spec.container.healthcheck = Some(healthcheck()))
        .unwrap();
    let mut second_healthy = second.clone();
    second_healthy
        .try_update(|parts| parts.runtime = healthy())
        .unwrap();
    let new = container('c');
    let mut web = spec(Some(0), None, None);
    web.name = ServiceName::parse("web").unwrap();
    let plan = vec![
        DeployOperation::WaitHealthy {
            machine_id: machine('1'),
            dependent,
            dependency: dependency.clone(),
        },
        run(&machine('1'), web, true),
    ];
    let client = Scripted::new(vec![
        listed(&dependency, vec![first.clone(), second]),
        listed(&dependency, vec![first, second_healthy]),
        created(
            Call::Run(machine('1'), ContainerKind::ServiceContainer),
            &new,
        ),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn dependency_gate_ignores_a_copy_out_of_rotation() {
    let dependency = QualifiedService::parse("app/api").unwrap();
    let dependent = QualifiedService::parse("app/web").unwrap();
    let serving_id = container('a');
    let superseded_id = container('b');
    let mut serving = observation(&machine('1'), &serving_id, healthy());
    serving
        .try_update(|parts| parts.resolved_spec.container.healthcheck = Some(healthcheck()))
        .unwrap();
    let mut superseded = observation(&machine('1'), &superseded_id, exited(1));
    superseded
        .try_update(|parts| parts.resolved_spec.container.healthcheck = Some(healthcheck()))
        .unwrap();
    let mut web = spec(Some(0), None, None);
    web.name = ServiceName::parse("web").unwrap();
    let plan = vec![
        DeployOperation::WaitHealthy {
            machine_id: machine('1'),
            dependent,
            dependency: dependency.clone(),
        },
        run(&machine('1'), web, true),
    ];
    let client = Scripted {
        observations: Some(vec![serving, superseded]),
        ..Scripted::new(vec![created(
            Call::Run(machine('1'), ContainerKind::ServiceContainer),
            &container('c'),
        )])
    }
    .with_copy(&machine('1'), &serving_id, CopyRotation::In)
    .with_copy(&machine('1'), &superseded_id, CopyRotation::Out);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "an Out copy awaiting removal must not fail the dependent, got {outcome:?}"
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_dependency_wait_reports_its_elapsed_and_deadline_clocks() {
    let dependency = QualifiedService::parse("app/api").unwrap();
    let mut observed = routed(&machine('1'), &container('a'), starting());
    observed
        .try_update(|parts| parts.resolved_spec.container.healthcheck = Some(healthcheck()))
        .unwrap();
    let plan = vec![DeployOperation::WaitHealthy {
        machine_id: machine('1'),
        dependent: QualifiedService::parse("app/web").unwrap(),
        dependency,
    }];
    let client = Scripted {
        observations: Some(vec![observed]),
        cancel_on_prepare: None,
        ..Scripted::new(Vec::new())
    };
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    super::super::execute_operation_sequence(
        &crate::deploy::DeployPlan::for_execution_test(plan, test_namespace()),
        &run_id(),
        &client,
        &CancellationToken::new(),
        Some(tx),
    )
    .await;

    let mut clocks = Vec::new();
    while let Ok(event) = rx.try_recv() {
        let DeployEvent::Progress { rows, .. } = event else {
            continue;
        };
        clocks.extend(rows.into_iter().filter_map(|row| {
            if let ployz_core::OperationStatus::Running {
                phase:
                    OperationPhase::WaitingForHealth {
                        elapsed_ms,
                        deadline_ms,
                        ..
                    },
            } = row.status
            {
                Some((elapsed_ms, deadline_ms))
            } else {
                None
            }
        }));
    }
    assert!(clocks.len() > 1, "{clocks:?}");
    assert!(
        clocks.iter().all(|(_, deadline_ms)| *deadline_ms > 0),
        "{clocks:?}"
    );
    assert!(
        clocks
            .windows(2)
            .all(|pair| matches!(pair, [(earlier, _), (later, _)] if earlier < later)),
        "{clocks:?}"
    );
}

#[tokio::test]
async fn dependency_gate_rejects_zero_service_containers_and_hooks() {
    let dependency = QualifiedService::parse("app/api").unwrap();
    for containers in [Vec::new(), {
        let id = container('a');
        let mut hook = observation(&machine('1'), &id, healthy());
        hook.try_update(|parts| parts.kind = ContainerKind::PreDeployHook)
            .unwrap();
        vec![hook]
    }] {
        let plan = vec![DeployOperation::WaitHealthy {
            machine_id: machine('1'),
            dependent: QualifiedService::parse("app/web").unwrap(),
            dependency: dependency.clone(),
        }];
        let client = Scripted::new(vec![listed(&dependency, containers)]);

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(matches!(
            outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::DependencyHealth {
                        failure: DependencyHealthFailure::NoContainers,
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

#[tokio::test(start_paused = true)]
async fn dependency_gate_accepts_a_container_that_passed_once_and_is_failing_now() {
    let dependency = QualifiedService::parse("app/api").unwrap();
    let id = container('a');
    let mut observed = routed(&machine('1'), &id, failing());
    observed
        .try_update(|parts| parts.resolved_spec.container.healthcheck = Some(healthcheck()))
        .unwrap();
    let plan = vec![DeployOperation::WaitHealthy {
        machine_id: machine('1'),
        dependent: QualifiedService::parse("app/web").unwrap(),
        dependency: dependency.clone(),
    }];
    let client = Scripted::new(vec![listed(&dependency, vec![observed])]);

    let started = tokio::time::Instant::now();
    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    assert_eq!(started.elapsed(), std::time::Duration::ZERO);
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn dependency_gate_uses_short_unhealthy_and_healthcheck_starting_deadlines() {
    let dependency = QualifiedService::parse("app/api").unwrap();
    for (runtime, deadline_millis, seconds, expected) in [
        (unhealthy(), None, 5, "runtime"),
        (starting(), None, 7, "timed_out"),
        (running(), None, 7, "timed_out"),
        (ContainerRuntimeObservation::Restarting, None, 5, "runtime"),
        (starting(), Some(3_000), 3, "timed_out"),
    ] {
        let id = container('a');
        let mut observed = routed(&machine('1'), &id, runtime);
        observed
            .try_update(|parts| {
                parts.resolved_spec.container.healthcheck = Some(
                    ployz_core::HealthcheckSpec::Configured(ployz_core::ConfiguredHealthcheck {
                        interval_millis: Some(1_000),
                        timeout_millis: Some(1_000),
                        retries: Some(1),
                        deadline_millis,
                        ..configured_healthcheck()
                    }),
                )
            })
            .unwrap();
        let plan = vec![DeployOperation::WaitHealthy {
            machine_id: machine('1'),
            dependent: QualifiedService::parse("app/web").unwrap(),
            dependency: dependency.clone(),
        }];
        let client = Scripted {
            observations: Some(vec![observed]),
            cancel_on_prepare: None,
            ..Scripted::new(Vec::new())
        };

        let started = tokio::time::Instant::now();
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(seconds + 1),
            execute_with(&plan, &client, &CancellationToken::new()),
        )
        .await
        .expect("dependency health must finish by its deadline");
        assert_eq!(started.elapsed(), std::time::Duration::from_secs(seconds));

        let DeployOutcome::Failed {
            failed:
                FailedOperation::Operation {
                    error:
                        ExecutionError::DependencyHealth {
                            failure: DependencyHealthFailure::Container { failure, .. },
                            ..
                        },
                    ..
                },
            ..
        } = outcome
        else {
            panic!("unexpected outcome: {outcome:?}");
        };
        assert_eq!(
            match failure {
                HealthFailure::TimedOut { .. } => "timed_out",
                HealthFailure::Runtime { .. } => "runtime",
                HealthFailure::Cancelled => "cancelled",
            },
            expected
        );
        client.assert_done();
    }
}

#[tokio::test]
async fn a_failed_verdict_fails_the_start_naming_the_container_without_polling_it() {
    let machine = machine('1');
    let new = container('a');
    let last_check = LastHealthCheck::exited(127, "sh: pg_isready: not found\n");
    let plan = vec![run(
        &machine,
        spec(Some(0), Some(healthcheck()), None),
        false,
    )];
    let client = Scripted::new(vec![
        Step(
            Call::Run(machine, ContainerKind::ServiceContainer),
            Reply::Settled(
                new,
                HealthVerdict::Failed {
                    observation: unhealthy(),
                    last_check: Some(last_check.clone()),
                    oom_killed: false,
                },
            ),
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
    assert_eq!(
        error,
        ExecutionError::Health {
            container_id: new,
            failure: HealthFailure::Runtime {
                observation: unhealthy(),
                last_check: Some(last_check),
            },
            out_of_memory: None,
            last_run: None,
        }
    );
    assert!(
        error.to_string().ends_with(
            "running (health: unhealthy); last check exited 127: sh: pg_isready: not found"
        ),
        "{error}"
    );
    client.assert_done();
}

#[tokio::test]
async fn a_timed_out_verdict_fails_the_start() {
    let machine = machine('1');
    let new = container('a');
    let plan = vec![run(
        &machine,
        spec(Some(0), Some(healthcheck()), None),
        false,
    )];
    let client = Scripted::new(vec![
        Step(
            Call::Run(machine, ContainerKind::ServiceContainer),
            Reply::Settled(
                new,
                HealthVerdict::TimedOut {
                    deadline_millis: None,
                    last_check: None,
                    oom_killed: false,
                },
            ),
        ),
        ok(Call::Discard(machine, new)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Operation {
                error: ExecutionError::Health {
                    container_id,
                    failure: HealthFailure::TimedOut {
                        deadline_millis: None,
                        last_check: None,
                    },
                    out_of_memory: None,
                    last_run: None,
                },
                ..
            },
            ..
        } if container_id == new
    ));
    client.assert_done();
}

#[tokio::test]
async fn a_verdict_docker_killed_out_of_memory_fails_with_the_memory_limit() {
    let machine = machine('1');
    let new = container('a');
    let limit = ByteQuantity::try_from(16 * 1024 * 1024).unwrap();
    let last_check = LastHealthCheck::exited(1, "wget: can't connect to remote host");
    for (memory_bytes, oom_killed, out_of_memory) in [
        (
            Some(limit),
            true,
            Some(OutOfMemory {
                memory_bytes: Some(limit),
            }),
        ),
        (None, true, Some(OutOfMemory { memory_bytes: None })),
        (Some(limit), false, None),
    ] {
        let mut spec = spec(Some(0), Some(healthcheck()), None);
        spec.container.resources.memory_bytes = memory_bytes;
        let plan = vec![run(&machine, spec, false)];
        let client = Scripted::new(vec![
            Step(
                Call::Run(machine, ContainerKind::ServiceContainer),
                Reply::Settled(
                    new,
                    HealthVerdict::TimedOut {
                        deadline_millis: Some(30_000),
                        last_check: Some(last_check.clone()),
                        oom_killed,
                    },
                ),
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
        assert_eq!(
            error,
            ExecutionError::Health {
                container_id: new,
                failure: HealthFailure::TimedOut {
                    deadline_millis: Some(30_000),
                    last_check: Some(last_check.clone()),
                },
                out_of_memory,
                last_run: None,
            }
        );
        client.assert_done();
    }
}

#[tokio::test]
async fn skipping_the_health_monitor_answers_once_started() {
    let machine = machine('1');
    let new = container('a');
    let plan = vec![run(
        &machine,
        spec(Some(0), Some(healthcheck()), None),
        true,
    )];
    let client = Scripted::new(vec![created(
        Call::Run(machine, ContainerKind::ServiceContainer),
        &new,
    )]);

    assert!(matches!(
        execute_with(&plan, &client, &CancellationToken::new()).await,
        DeployOutcome::Success { .. }
    ));
    assert!(
        client
            .answers()
            .iter()
            .all(|answer| *answer == AnswerWhen::Started)
    );
    client.assert_done();
}

/// The copy this run starts is labelled by its create, not by the test, so the
/// dependency wait finds it as production does.
#[tokio::test(start_paused = true)]
async fn dependency_prefers_this_runs_copies() {
    let dependency = QualifiedService::parse("app/api").unwrap();
    let other = DeploymentLogId::parse("other").unwrap();
    let copy = |hex: char, deploy: Option<&str>, runtime: ContainerRuntimeObservation| {
        let mut observed = serving(&machine('1'), &container(hex), deploy);
        observed
            .try_update(|parts| {
                parts.runtime = runtime;
                parts.resolved_spec.container.healthcheck = Some(healthcheck());
            })
            .unwrap();
        observed
    };
    let started = container('a');
    let cases = [
        (
            "this run's copy is chosen over another run's In copy",
            Some(started),
            vec![
                (copy('a', None, healthy()), None),
                (copy('b', Some("other"), exited(1)), Some(CopyRotation::In)),
            ],
        ),
        (
            "the In copies are chosen when this run started none",
            None,
            vec![
                (copy('a', Some("other"), healthy()), Some(CopyRotation::In)),
                (
                    copy('b', Some("other"), exited(1)),
                    Some(CopyRotation::Pending),
                ),
                (
                    copy('c', Some("other"), exited(1)),
                    Some(CopyRotation::Held(other.clone())),
                ),
                (copy('d', Some("run"), exited(1)), Some(CopyRotation::Out)),
            ],
        ),
    ];
    for (case, started, copies) in cases {
        let mut web = spec(Some(0), None, None);
        web.name = ServiceName::parse("web").unwrap();
        let mut plan = Vec::new();
        let mut steps = Vec::new();
        if let Some(started) = started {
            plan.push(run(&machine('1'), spec(Some(0), None, None), true));
            steps.push(created(
                Call::Run(machine('1'), ContainerKind::ServiceContainer),
                &started,
            ));
        }
        plan.extend([
            DeployOperation::WaitHealthy {
                machine_id: machine('1'),
                dependent: QualifiedService::parse("app/web").unwrap(),
                dependency: dependency.clone(),
            },
            run(&machine('1'), web, true),
        ]);
        steps.push(created(
            Call::Run(machine('1'), ContainerKind::ServiceContainer),
            &container('e'),
        ));
        let mut client = Scripted {
            observations: Some(
                copies
                    .iter()
                    .map(|(observed, _)| observed.clone())
                    .collect(),
            ),
            ..Scripted::new(steps)
        };
        for (observed, rotation) in copies {
            if let Some(rotation) = rotation {
                client = client.with_copy(&machine('1'), &observed.container_id, rotation);
            }
        }

        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

        assert!(
            matches!(outcome, DeployOutcome::Success { .. }),
            "{case}: {outcome:?}"
        );
        client.assert_done();
    }
}

/// A stop-first replacement fails to start, and the old copy it restores is OOM-killed.
/// The failure names the old copy's own memory limit, not the replacement's.
async fn restored_oom_limit(replacement_mib: i64) {
    let machine = machine('1');
    let old = container('a');
    let old_limit = ByteQuantity::try_from(16 * 1024 * 1024).unwrap();
    let new_limit = ByteQuantity::try_from(replacement_mib * 1024 * 1024).unwrap();
    let mut old_spec = spec(Some(0), Some(healthcheck()), None);
    old_spec.container.resources.memory_bytes = Some(old_limit);
    let mut operation = replacement(&machine, &old, UpdateOrder::StopFirst);
    let DeployOperation::ReplaceContainer(ref mut replacement) = operation else {
        panic!("expected a replacement");
    };
    replacement.spec.container.resources.memory_bytes = Some(new_limit);
    let verdict = HealthVerdict::Failed {
        observation: unhealthy(),
        last_check: None,
        oom_killed: true,
    };
    let client = Scripted {
        observed_specs: BTreeMap::from([(old, old_spec)]),
        ..Scripted::new(vec![
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Stop(machine, old)),
            failed(
                Call::Run(machine, ContainerKind::ServiceContainer),
                "new image creation failed",
            ),
            ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
            Step(
                Call::Restart(machine, old),
                Reply::Settled(old, verdict.clone()),
            ),
        ])
    }
    .with_copy(&machine, &old, CopyRotation::In);
    let outcome = execute_with(&[operation], &client, &CancellationToken::new()).await;
    let DeployOutcome::Failed {
        failed:
            FailedOperation::Replacement {
                compensation:
                    ReplacementCompensation::OldStopped {
                        restore_old_container: RestoreAttempt::Failed { error },
                        ..
                    },
                ..
            },
        ..
    } = outcome
    else {
        panic!("expected a failed restore, got {outcome:?}");
    };
    assert_eq!(
        error,
        ExecutionError::Health {
            container_id: old,
            failure: verdict.into_failure().unwrap(),
            out_of_memory: Some(OutOfMemory {
                memory_bytes: Some(old_limit),
            }),
            last_run: None,
        }
    );
    client.assert_done();
}

#[tokio::test]
async fn a_restored_copy_killed_for_memory_names_its_own_limit() {
    restored_oom_limit(64).await;
}

#[tokio::test]
async fn a_restored_copy_killed_for_memory_under_an_unchanged_limit_names_it() {
    restored_oom_limit(16).await;
}
