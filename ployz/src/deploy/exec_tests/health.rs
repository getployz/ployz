use super::*;

#[test]
fn parse_monitor_period_accepts_zero_and_seconds() {
    assert_eq!(parse_monitor_period("0"), Some(Duration::ZERO));
    assert_eq!(parse_monitor_period("0s"), Some(Duration::ZERO));
    assert_eq!(parse_monitor_period("10s"), Some(Duration::from_secs(10)));
    assert_eq!(parse_monitor_period("bogus"), None);
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
        created(
            Call::Create(machine, ContainerKind::ServiceContainer),
            &early,
        ),
        ok(Call::Start(machine, early)),
        observed(Call::Inspect(machine, early), healthy()),
        created(
            Call::Create(machine, ContainerKind::ServiceContainer),
            &transient,
        ),
        ok(Call::Start(machine, transient)),
        observed(Call::Inspect(machine, transient), unhealthy()),
        observed(Call::Inspect(machine, transient), healthy()),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}

#[tokio::test]
async fn health_monitor_accepts_a_clean_exit_instead_of_failing_as_restarting() {
    let machine = machine('1');
    let new = container('a');
    let plan = vec![run(&machine, spec(Some(0), None, None), false)];
    let client = Scripted::new(vec![
        created(Call::Create(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Start(machine, new)),
        observed(Call::Inspect(machine, new), exited(0)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
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
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}
