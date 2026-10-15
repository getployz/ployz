use super::*;

fn resent(machine: MachineId, id: &ContainerId) -> [Step; 2] {
    [
        failed_unavailable(
            Call::Run(machine, ContainerKind::ServiceContainer),
            "transport error",
        ),
        created(Call::Run(machine, ContainerKind::ServiceContainer), id),
    ]
}

#[tokio::test(start_paused = true)]
async fn a_run_lost_to_a_daemon_restart_is_resent_to_the_same_slot() {
    let machine = machine('1');
    let plan = vec![run(&machine, spec(Some(0), None, None), false)];
    let client = Scripted::new(Vec::from(resent(machine, &container('a'))));

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    let slots = client.slots.lock().unwrap();
    let [first, second] = slots.as_slice() else {
        panic!("expected two creations, got {slots:?}");
    };
    assert_eq!(first, second);
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn two_parallel_run_containers_each_survive_a_target_restart() {
    let first_machine = machine('1');
    let second_machine = machine('2');
    let plan = vec![
        run(&first_machine, spec(None, None, None), true),
        run(&second_machine, spec(None, None, None), true),
    ];
    let [first_lost, first_created] = resent(first_machine, &container('a'));
    let [second_lost, second_created] = resent(second_machine, &container('b'));
    let client = Scripted::new(vec![first_lost, second_lost, first_created, second_created]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(matches!(outcome, DeployOutcome::Success { .. }));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_run_waiting_out_a_restart_stops_when_cancelled_and_removes_its_slot() {
    let machine = machine('1');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let client = Scripted::new(vec![
        failed_unavailable(
            Call::Run(machine, ContainerKind::ServiceContainer),
            "transport error",
        ),
        ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
    ]);
    let cancellation = CancellationToken::new();
    let execute = execute_with(&plan, &client, &cancellation);
    tokio::pin!(execute);
    tokio::select! {
        outcome = &mut execute => panic!("run retry returned before cancel: {outcome:?}"),
        () = tokio::time::sleep(std::time::Duration::from_millis(1)) => {}
    }
    cancellation.cancel();
    let outcome = execute.await;

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Operation {
                error: ExecutionError::Cancelled,
                ..
            },
            ..
        }
    ));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn cancelled_create_waits_for_the_one_shot_then_removes() {
    let machine = machine('1');
    let created_id = container('a');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let client = Scripted::new(vec![
        created_later(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &created_id,
        ),
        ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
    ]);
    let cancellation = CancellationToken::new();
    let execute = execute_with(&plan, &client, &cancellation);
    tokio::pin!(execute);
    tokio::select! {
        outcome = &mut execute => panic!("create returned before cancel: {outcome:?}"),
        () = tokio::time::sleep(std::time::Duration::from_millis(1)) => {}
    }
    cancellation.cancel();
    tokio::select! {
        outcome = &mut execute => panic!("dropped in-flight create on cancel: {outcome:?}"),
        () = tokio::time::sleep(std::time::Duration::from_millis(1)) => {}
    }
    let outcome = tokio::time::timeout(std::time::Duration::from_secs(1), execute)
        .await
        .expect("cancelled create must return after the one-shot completes");

    assert!(matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Operation {
                error: ExecutionError::Cancelled,
                ..
            },
            ..
        }
    ));
    client.assert_done();
}

async fn cancel_while_creating(
    plan: Vec<DeployOperation>,
    client: &Scripted,
) -> DeployOutcome<ExecutionError> {
    let cancellation = CancellationToken::new();
    let execute = execute_with(&plan, client, &cancellation);
    tokio::pin!(execute);
    tokio::select! {
        outcome = &mut execute => panic!("create returned before cancel: {outcome:?}"),
        () = tokio::time::sleep(std::time::Duration::from_millis(1)) => {}
    }
    cancellation.cancel();
    tokio::time::timeout(std::time::Duration::from_secs(1), execute)
        .await
        .expect("a cancelled Settled create returns without its answer")
}

fn cancelled(outcome: &DeployOutcome<ExecutionError>) -> bool {
    matches!(
        outcome,
        DeployOutcome::Failed {
            failed: FailedOperation::Operation {
                error: ExecutionError::Cancelled,
                ..
            },
            ..
        }
    )
}

#[tokio::test(start_paused = true)]
async fn cancelled_settled_create_removes_its_slot() {
    let machine = machine('1');
    let created_id = container('a');
    let plan = vec![run(&machine, spec(None, None, None), false)];
    let client = Scripted::new(vec![
        created_later(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &created_id,
        ),
        ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
    ]);

    let outcome = cancel_while_creating(plan, &client).await;

    assert!(cancelled(&outcome), "{outcome:?}");
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn cancelled_global_health_wait_removes_the_new_container() {
    let machine = machine('1');
    let mut global = spec(None, Some(healthcheck()), None);
    global.mode = ployz_core::ServiceMode::Global;
    let plan = vec![run(&machine, global, false)];
    let client = Scripted::new(vec![
        Step(
            Call::Run(machine, ContainerKind::ServiceContainer),
            Reply::Pending,
        ),
        ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
    ]);

    let outcome = cancel_while_creating(plan, &client).await;

    assert!(cancelled(&outcome), "{outcome:?}");
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn cancelled_global_health_wait_keeps_an_adopted_container() {
    let machine = machine('1');
    let mut global = spec(None, Some(healthcheck()), None);
    global.mode = ployz_core::ServiceMode::Global;
    let plan = vec![run(&machine, global, false)];
    let mut client = Scripted::new(vec![Step(
        Call::Run(machine, ContainerKind::ServiceContainer),
        Reply::Pending,
    )]);
    client.runs_global = true;

    let outcome = cancel_while_creating(plan, &client).await;

    assert!(cancelled(&outcome), "{outcome:?}");
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn storage_preparation_lost_to_a_daemon_restart_is_resent() {
    let machine = machine('1');
    let service = provisioned_spec();
    let plan = vec![
        DeployOperation::PrepareVolumes {
            machine_id: machine,
            specs: vec![ployz_core::ServiceStorageSpec::from(&service)],
        },
        run(&machine, service, false),
    ];
    let client = Scripted::new(vec![
        failed_unavailable(Call::Prepare(machine), "transport error"),
        ok(Call::Prepare(machine)),
        created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &container('a'),
        ),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    client.assert_done();
}
