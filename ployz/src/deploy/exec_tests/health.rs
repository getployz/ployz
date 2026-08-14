use super::*;

#[tokio::test(start_paused = true)]
async fn health_monitor_accepts_running_no_check_inherited_starting_and_transient_unhealthy() {
    let machine = machine('1');
    let no_check = container('a');
    let inherited = container('b');
    let early = container('c');
    let transient = container('d');
    let mut inherited_healthcheck = healthcheck();
    inherited_healthcheck.start_period_millis = Some(300_000);
    let plan = plan(vec![
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
    ]);
    let client = Scripted::new(vec![
        created(
            Call::Create(machine.clone(), ContainerKind::ServiceContainer),
            &no_check,
        ),
        ok(Call::Start(machine.clone(), no_check.clone())),
        observed(Call::Inspect(machine.clone(), no_check), running()),
        created(
            Call::Create(machine.clone(), ContainerKind::ServiceContainer),
            &inherited,
        ),
        ok(Call::Start(machine.clone(), inherited.clone())),
        observed_with_healthcheck(
            Call::Inspect(machine.clone(), inherited.clone()),
            starting(),
            inherited_healthcheck,
        ),
        observed(Call::Inspect(machine.clone(), inherited), healthy()),
        created(
            Call::Create(machine.clone(), ContainerKind::ServiceContainer),
            &early,
        ),
        ok(Call::Start(machine.clone(), early.clone())),
        observed(Call::Inspect(machine.clone(), early), healthy()),
        created(
            Call::Create(machine.clone(), ContainerKind::ServiceContainer),
            &transient,
        ),
        ok(Call::Start(machine.clone(), transient.clone())),
        observed(
            Call::Inspect(machine.clone(), transient.clone()),
            unhealthy(),
        ),
        observed(Call::Inspect(machine, transient), healthy()),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(outcome.failed.is_none());
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
        let plan = plan(vec![run(&machine, spec(Some(0), healthcheck, None), false)]);
        let client = Scripted::new(vec![
            created(
                Call::Create(machine.clone(), ContainerKind::ServiceContainer),
                &new,
            ),
            ok(Call::Start(machine.clone(), new.clone())),
            observed(Call::Inspect(machine, new), runtime),
        ]);
        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;
        assert!(matches!(
            outcome.failed,
            Some(FailedOperation::Operation {
                error: ExecutionError::Health { .. },
                ..
            })
        ));
        client.assert_done();
    }

    let machine = machine('2');
    let new = container('b');
    let plan = plan(vec![run(
        &machine,
        spec(Some(0), Some(healthcheck()), None),
        true,
    )]);
    let client = Scripted::new(vec![
        created(
            Call::Create(machine.clone(), ContainerKind::ServiceContainer),
            &new,
        ),
        ok(Call::Start(machine, new)),
    ]);
    assert!(
        execute_with(&plan, &client, &CancellationToken::new())
            .await
            .failed
            .is_none()
    );
    client.assert_done();
}
