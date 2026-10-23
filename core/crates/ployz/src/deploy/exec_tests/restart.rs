use super::*;

fn created_later(call: Call, container_id: &ContainerId) -> Step {
    Step(
        call,
        Reply::CreatedAfter(*container_id, || {
            tokio::time::sleep(std::time::Duration::from_millis(5))
        }),
    )
}

#[tokio::test(start_paused = true)]
async fn a_machine_that_answered_then_went_silent_keeps_its_answer() {
    let answered = RpcError {
        code: RpcErrorCode::Unavailable,
        message: "Docker is restarting".into(),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    };
    let silent = || {
        RpcError::unanswered_target(
            RpcErrorCode::Unavailable,
            "Machine did not answer: deadline has elapsed".into(),
        )
    };
    let attempts = std::cell::Cell::new(0);
    let error = wait_out_restart(
        &CancellationToken::new(),
        || {
            attempts.set(attempts.get() + 1);
            let error = if attempts.get() == 1 {
                answered.clone()
            } else {
                silent()
            };
            async move { Err::<(), _>(error) }
        },
        no_redial,
    )
    .await
    .unwrap_err();
    assert!(attempts.get() > 2);
    assert_eq!(error, answered);

    let error = wait_out_restart(
        &CancellationToken::new(),
        || async { Err::<(), _>(silent()) },
        no_redial,
    )
    .await
    .unwrap_err();
    assert!(error.target_unanswered());
}

#[tokio::test(start_paused = true)]
async fn a_call_on_a_closed_session_is_not_resent() {
    let attempts = std::cell::Cell::new(0);
    let error = wait_out_restart(
        &CancellationToken::new(),
        || {
            attempts.set(attempts.get() + 1);
            async { Err::<(), _>(crate::connect::closed_session_error()) }
        },
        no_redial,
    )
    .await
    .unwrap_err();
    assert_eq!(attempts.get(), 1);
    assert_eq!(error.code, RpcErrorCode::Unavailable);
}

#[tokio::test(start_paused = true)]
async fn a_create_whose_entry_daemon_restarted_is_resent_on_a_new_session() {
    let machine = machine('1');
    let plan = vec![run(&machine, spec(Some(0), None, None), false)];
    let mut client = Scripted::new(vec![
        Step(
            Call::Run(machine, ContainerKind::ServiceContainer),
            Reply::Error(crate::connect::closed_session_error()),
        ),
        created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &container('a'),
        ),
    ]);
    client.redial = Some(Ok(()));

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(*client.redials.lock().unwrap(), 1);
    let slots = client.slots.lock().unwrap();
    let [first, second] = slots.as_slice() else {
        panic!("expected two creations, got {slots:?}");
    };
    assert_eq!(
        first, second,
        "the resend lands on the slot the lost create made"
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_create_lost_with_its_entry_daemon_is_removed_on_a_new_session() {
    let machine = machine('1');
    let plan = vec![run(&machine, spec(Some(0), None, None), false)];
    let dropped = crate::cluster::answered_by(
        &machine,
        crate::connect::ConnectError::from(tonic::Status::from_error(Box::new(
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "connection lost"),
        ))),
    );
    let mut client = Scripted::new(vec![
        Step(
            Call::Run(machine, ContainerKind::ServiceContainer),
            Reply::Error(dropped),
        ),
        Step(
            Call::DiscardSlot(machine, ContainerKind::ServiceContainer),
            Reply::Error(crate::connect::closed_session_error()),
        ),
        ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
    ]);
    client.redial = Some(Ok(()));

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(*client.redials.lock().unwrap(), 1);
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_target_silent_after_a_redial_is_reported_not_the_closed_session() {
    let attempts = std::cell::Cell::new(0);
    let error = wait_out_restart(
        &CancellationToken::new(),
        || {
            attempts.set(attempts.get() + 1);
            let error = if attempts.get() == 1 {
                crate::connect::closed_session_error()
            } else {
                RpcError::unanswered_target(
                    RpcErrorCode::Unavailable,
                    "Machine did not answer: deadline has elapsed".into(),
                )
            };
            async move { Err::<(), _>(error) }
        },
        || async { Some(Ok(())) },
    )
    .await
    .unwrap_err();

    assert!(attempts.get() > 2);
    assert!(error.target_unanswered(), "{error:?}");
}

#[tokio::test(start_paused = true)]
async fn a_target_silent_after_a_lost_reply_and_a_redial_is_reported_silent() {
    let lost = RpcError {
        code: RpcErrorCode::Unavailable,
        message: "connection lost".into(),
        details: serde_json::Value::Null,
        cause: Vec::new(),
    }
    .reply_lost_from(&machine('1'));
    let attempts = std::cell::Cell::new(0);
    let error = wait_out_restart(
        &CancellationToken::new(),
        || {
            attempts.set(attempts.get() + 1);
            let error = match attempts.get() {
                1 => lost.clone(),
                2 => crate::connect::closed_session_error(),
                _ => RpcError::unanswered_target(
                    RpcErrorCode::Unavailable,
                    "Machine did not answer: deadline has elapsed".into(),
                ),
            };
            async move { Err::<(), _>(error) }
        },
        || async { Some(Ok(())) },
    )
    .await
    .unwrap_err();

    assert!(attempts.get() > 3);
    assert!(error.target_unanswered(), "{error:?}");
}

#[tokio::test(start_paused = true)]
async fn cancelling_a_deploy_ends_a_redial_still_dialing() {
    let cancellation = CancellationToken::new();
    tokio::spawn({
        let cancellation = cancellation.clone();
        async move {
            tokio::time::sleep(std::time::Duration::from_secs(1)).await;
            cancellation.cancel();
        }
    });
    let started = tokio::time::Instant::now();

    let error = wait_out_restart(
        &cancellation,
        || async { Err::<(), _>(crate::connect::closed_session_error()) },
        std::future::pending,
    )
    .await
    .unwrap_err();

    assert!(crate::connect::on_closed_session(&error));
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test(start_paused = true)]
async fn an_entry_that_never_answers_again_fails_the_create_once_the_restart_wait_ends() {
    let machine = machine('1');
    let plan = vec![run(&machine, spec(Some(0), None, None), false)];
    let closed = || {
        Step(
            Call::Run(machine, ContainerKind::ServiceContainer),
            Reply::Error(crate::connect::closed_session_error()),
        )
    };
    let mut client = Scripted::new(vec![
        closed(),
        Step(
            Call::DiscardSlot(machine, ContainerKind::ServiceContainer),
            Reply::Error(crate::connect::closed_session_error()),
        ),
    ]);
    client.redial = Some(Err(unavailable("entry Machine reconnect failed")));
    let started = tokio::time::Instant::now();

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert!(started.elapsed() >= RESTART_WAIT, "{:?}", started.elapsed());
    assert!(*client.redials.lock().unwrap() > 1);
    client.assert_done();
}

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

#[tokio::test(start_paused = true)]
async fn a_create_whose_connection_drops_fails_without_waiting_out_a_restart() {
    let machine = machine('1');
    let plan = vec![run(&machine, spec(Some(0), None, None), false)];
    let dropped = |message| {
        crate::cluster::answered_by(
            &machine,
            crate::connect::ConnectError::from(tonic::Status::from_error(Box::new(
                std::io::Error::new(std::io::ErrorKind::NotConnected, message),
            ))),
        )
    };
    let client = Scripted::new(vec![
        Step(
            Call::Run(machine, ContainerKind::ServiceContainer),
            Reply::Error(dropped("connection lost")),
        ),
        Step(
            Call::DiscardSlot(machine, ContainerKind::ServiceContainer),
            Reply::Error(dropped("transport session closed")),
        ),
    ]);
    let started = tokio::time::Instant::now();

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert!(started.elapsed() < RESTART_WAIT, "{:?}", started.elapsed());
    client.assert_done();
}
