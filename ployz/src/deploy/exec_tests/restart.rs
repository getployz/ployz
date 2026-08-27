use super::*;

#[tokio::test(start_paused = true)]
async fn start_lost_to_a_daemon_restart_starts_the_created_container() {
    let machine = machine('1');
    let created_id = container('a');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let client = Scripted::new(vec![
        created(
            Call::Create(machine, ContainerKind::ServiceContainer),
            &created_id,
        ),
        failed_unavailable(Call::Start(machine, created_id), "transport error"),
        ok(Call::Start(machine, created_id)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn start_lost_to_a_daemon_restart_accepts_a_container_that_is_already_running() {
    let machine = machine('1');
    let created_id = container('a');
    let plan = vec![run(&machine, spec(Some(0), None, None), false)];
    let client = Scripted::new(vec![
        created(
            Call::Create(machine, ContainerKind::ServiceContainer),
            &created_id,
        ),
        failed_unavailable(Call::Start(machine, created_id), "transport error"),
        ok(Call::Start(machine, created_id)),
        observed(Call::Inspect(machine, created_id), running()),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn two_consecutive_run_containers_each_survive_a_target_restart() {
    let first_machine = machine('1');
    let second_machine = machine('2');
    let first = container('a');
    let second = container('b');
    let plan = vec![
        run(&first_machine, spec(None, None, None), true),
        run(&second_machine, spec(None, None, None), true),
    ];
    let client = Scripted::new(vec![
        created(
            Call::Create(first_machine, ContainerKind::ServiceContainer),
            &first,
        ),
        failed_unavailable(Call::Start(first_machine, first), "transport error"),
        ok(Call::Start(first_machine, first)),
        created(
            Call::Create(second_machine, ContainerKind::ServiceContainer),
            &second,
        ),
        failed_unavailable(Call::Start(second_machine, second), "transport error"),
        ok(Call::Start(second_machine, second)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn inspect_lost_to_a_daemon_restart_is_waited_out() {
    let machine = machine('1');
    let created_id = container('a');
    let plan = vec![run(&machine, spec(Some(0), None, None), false)];
    let client = Scripted::new(vec![
        created(
            Call::Create(machine, ContainerKind::ServiceContainer),
            &created_id,
        ),
        ok(Call::Start(machine, created_id)),
        failed_unavailable(Call::Inspect(machine, created_id), "transport error"),
        observed(Call::Inspect(machine, created_id), running()),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn create_unavailable_is_not_retried() {
    let machine = machine('1');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let client = Scripted::new(vec![failed_unavailable(
        Call::Create(machine, ContainerKind::ServiceContainer),
        "transport error",
    )]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Operation {
                error: ExecutionError::Machine {
                    action: MachineAction::CreateContainer,
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
async fn start_unavailable_stops_waiting_when_cancelled() {
    let machine = machine('1');
    let created_id = container('a');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let client = Scripted::new(vec![
        created(
            Call::Create(machine, ContainerKind::ServiceContainer),
            &created_id,
        ),
        failed_unavailable(Call::Start(machine, created_id), "transport error"),
        ok(Call::Remove(machine, created_id)),
    ]);
    let cancellation = CancellationToken::new();
    let execute = execute_with(&plan, &client, &cancellation);
    tokio::pin!(execute);
    tokio::select! {
        outcome = &mut execute => panic!("start retry returned before cancel: {outcome:?}"),
        () = tokio::time::sleep(std::time::Duration::from_millis(1)) => {}
    }
    cancellation.cancel();
    let outcome = execute.await;

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
