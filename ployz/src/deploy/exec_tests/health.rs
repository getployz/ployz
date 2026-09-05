use super::*;

#[test]
fn parse_monitor_period_accepts_zero_and_seconds() {
    assert_eq!(parse_monitor_period("0"), Some(Duration::ZERO));
    assert_eq!(parse_monitor_period("0s"), Some(Duration::ZERO));
    assert_eq!(parse_monitor_period("10s"), Some(Duration::from_secs(10)));
    assert_eq!(parse_monitor_period("bogus"), None);
}

#[tokio::test(start_paused = true)]
async fn dependency_gate_waits_for_all_service_containers_before_the_dependent() {
    let dependency = QualifiedService::parse("app/api").unwrap();
    let dependent = QualifiedService::parse("app/web").unwrap();
    let first_id = container('a');
    let second_id = container('b');
    let mut first = observation(&machine('1'), &first_id, healthy());
    first
        .try_update(|parts| parts.resolved_spec.container.healthcheck = Some(healthcheck()))
        .unwrap();
    let mut second = observation(&machine('2'), &second_id, starting());
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
            Call::Create(machine('1'), ContainerKind::ServiceContainer),
            &new,
        ),
        ok(Call::Start(machine('1'), new)),
        serving(new),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
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
async fn dependency_gate_uses_short_unhealthy_and_healthcheck_starting_deadlines() {
    let dependency = QualifiedService::parse("app/api").unwrap();
    for (runtime, seconds, expected) in [
        (unhealthy(), 5, "runtime"),
        (starting(), 7, "timed_out"),
        (running(), 7, "timed_out"),
        (ContainerRuntimeObservation::Restarting, 5, "runtime"),
    ] {
        let id = container('a');
        let mut observed = observation(&machine('1'), &id, runtime);
        observed
            .try_update(|parts| {
                parts.resolved_spec.container.healthcheck = Some(
                    ployz_core::HealthcheckSpec::Configured(ployz_core::ConfiguredHealthcheck {
                        interval_millis: Some(1_000),
                        timeout_millis: Some(1_000),
                        retries: Some(1),
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
                HealthFailure::TimedOut => "timed_out",
                HealthFailure::Runtime { .. } => "runtime",
                HealthFailure::Cancelled => "cancelled",
            },
            expected
        );
        client.assert_done();
    }
}

#[tokio::test(start_paused = true)]
async fn health_monitor_accepts_running_no_check_inherited_starting_and_transient_unhealthy() {
    let machine = machine('1');
    let no_check = container('a');
    let inherited = container('b');
    let early = container('c');
    let transient = container('d');
    let mut inherited_healthcheck = configured_healthcheck();
    inherited_healthcheck.start_period_millis = Some(300_000);
    let plan = vec![
        run(&machine, spec(Some(25), None, None), false),
        run(&machine, spec(Some(5_000), None, None), false),
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
    ];
    let client = Scripted::new(vec![
        created(
            Call::Create(machine, ContainerKind::ServiceContainer),
            &no_check,
        ),
        ok(Call::Start(machine, no_check)),
        observed(Call::Inspect(machine, no_check), running()),
        serving(no_check),
        created(
            Call::Create(machine, ContainerKind::ServiceContainer),
            &inherited,
        ),
        ok(Call::Start(machine, inherited)),
        observed_with_healthcheck(
            Call::Inspect(machine, inherited),
            starting(),
            ployz_core::HealthcheckSpec::Configured(inherited_healthcheck),
        ),
        observed(Call::Inspect(machine, inherited), healthy()),
        serving(inherited),
        created(
            Call::Create(machine, ContainerKind::ServiceContainer),
            &early,
        ),
        ok(Call::Start(machine, early)),
        observed(Call::Inspect(machine, early), healthy()),
        serving(early),
        created(
            Call::Create(machine, ContainerKind::ServiceContainer),
            &transient,
        ),
        ok(Call::Start(machine, transient)),
        observed(Call::Inspect(machine, transient), unhealthy()),
        observed(Call::Inspect(machine, transient), healthy()),
        serving(transient),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}

#[tokio::test]
async fn health_monitor_fails_a_clean_exit_without_waiting_for_serving() {
    let machine = machine('1');
    let new = container('a');
    let plan = vec![run(&machine, spec(Some(0), None, None), false)];
    let client = Scripted::new(vec![
        created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Start(machine, new)),
        observed(Call::Inspect(machine, new), exited(0)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Operation {
                error: ExecutionError::Health {
                    failure: HealthFailure::Runtime {
                        observation: ContainerRuntimeObservation::Exited { code: 0 },
                    },
                    ..
                },
                ..
            },
            ..
        }
    ));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn health_monitor_succeeds_on_the_first_healthy_probe() {
    let machine = machine('1');
    let new = container('a');
    let plan = vec![run(
        &machine,
        spec(Some(1_000), Some(healthcheck()), None),
        false,
    )];
    let client = Scripted::new(vec![
        created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Start(machine, new)),
        observed(Call::Inspect(machine, new), starting()),
        observed(Call::Inspect(machine, new), healthy()),
        serving(new),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn health_monitor_accepts_running_without_a_healthcheck_after_the_monitor() {
    let machine = machine('1');
    let new = container('a');
    let plan = vec![run(&machine, spec(Some(1_000), None, None), false)];
    let client = Scripted::new(vec![
        created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Start(machine, new)),
        observed(Call::Inspect(machine, new), running()),
        serving(new),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn health_monitor_fails_restarting_without_waiting_out_the_monitor_window() {
    let machine = machine('1');
    let new = container('a');
    let plan = vec![run(
        &machine,
        spec(Some(1_000), Some(healthcheck()), None),
        false,
    )];
    let client = Scripted::new(vec![
        created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Start(machine, new)),
        observed(
            Call::Inspect(machine, new),
            ContainerRuntimeObservation::Restarting,
        ),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Operation {
                error: ExecutionError::Health {
                    failure: HealthFailure::Runtime {
                        observation: ContainerRuntimeObservation::Restarting,
                    },
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
async fn health_monitor_still_fails_a_restart_loop() {
    let machine = machine('1');
    let new = container('a');
    let plan = vec![run(&machine, spec(Some(0), None, None), false)];
    let client = Scripted::new(vec![
        created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Start(machine, new)),
        observed(
            Call::Inspect(machine, new),
            ContainerRuntimeObservation::Restarting,
        ),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Operation {
                error: ExecutionError::Health {
                    failure: HealthFailure::Runtime {
                        observation: ContainerRuntimeObservation::Restarting,
                    },
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
async fn health_monitor_fails_terminal_unhealthy_and_crash_but_skip_bypasses_inspection() {
    for (healthcheck, runtime) in [
        (Some(healthcheck()), unhealthy()),
        (None, unhealthy()),
        (None, ContainerRuntimeObservation::Exited { code: 17 }),
    ] {
        let machine = machine('1');
        let new = container('a');
        let plan = vec![run(&machine, spec(Some(0), healthcheck, None), false)];
        let client = Scripted::new(vec![
            created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
            ok(Call::Start(machine, new)),
            observed(Call::Inspect(machine, new), runtime),
        ]);
        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;
        assert!(matches!(
            outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Health { .. },
                    ..
                },
                ..
            }
        ));
        client.assert_done();
    }

    let machine = machine('2');
    let new = container('b');
    let plan = vec![run(
        &machine,
        spec(Some(0), Some(healthcheck()), None),
        true,
    )];
    let client = Scripted::new(vec![
        created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Start(machine, new)),
        serving(new),
    ]);
    assert!(matches!(
        execute_with(&plan, &client, &CancellationToken::new()).await,
        DeployOutcome::Success { .. }
    ));
    client.assert_done();
}

#[tokio::test]
async fn health_monitor_does_not_inherit_when_spec_disables_the_check() {
    let machine = machine('1');
    let new = container('a');
    let mut inherited = configured_healthcheck();
    inherited.start_period_millis = Some(300_000);
    let plan = vec![run(
        &machine,
        spec(Some(0), Some(ployz_core::HealthcheckSpec::Disabled), None),
        false,
    )];
    let client = Scripted::new(vec![
        created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Start(machine, new)),
        observed_with_healthcheck(
            Call::Inspect(machine, new),
            running(),
            ployz_core::HealthcheckSpec::Configured(inherited),
        ),
        serving(new),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}
