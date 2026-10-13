use super::*;

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
            Call::Run(machine('1'), ContainerKind::ServiceContainer),
            &new,
        ),
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
async fn dependency_gate_accepts_a_container_that_passed_once_and_is_failing_now() {
    let dependency = QualifiedService::parse("app/api").unwrap();
    let id = container('a');
    let mut observed = observation(&machine('1'), &id, failing());
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
        let mut observed = observation(&machine('1'), &id, runtime);
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
    // The script holds no Inspect step, so any poll of the new Container panics.
    let client = Scripted::new(vec![Step(
        Call::Run(machine, ContainerKind::ServiceContainer),
        Reply::Settled(
            new,
            HealthVerdict::Failed {
                observation: unhealthy(),
                last_check: Some(last_check.clone()),
            },
        ),
    )]);

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
    let client = Scripted::new(vec![Step(
        Call::Run(machine, ContainerKind::ServiceContainer),
        Reply::Settled(new, HealthVerdict::TimedOut { last_check: None }),
    )]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Operation {
                error: ExecutionError::Health {
                    container_id,
                    failure: HealthFailure::TimedOut { last_check: None },
                },
                ..
            },
            ..
        } if container_id == new
    ));
    client.assert_done();
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
