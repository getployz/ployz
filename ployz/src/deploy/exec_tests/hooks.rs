use super::*;

#[tokio::test]
async fn hook_exit_zero_runs_suffix_nonzero_and_inspect_failure_retain_the_hook() {
    for reply in [
        Reply::Observed(exited(7), None),
        Reply::Error(error("inspect")),
    ] {
        let machine = machine('1');
        let hook_id = container('a');
        let suffix = container('b');
        let operations = vec![
            hook(&machine, spec(None, None, Some(5_000))),
            stop(&machine, &suffix),
        ];
        let plan = plan(operations);
        let client = Scripted::new(vec![
            created(
                Call::Create(machine, ContainerKind::PreDeployHook),
                &hook_id,
            ),
            ok(Call::Start(machine, hook_id)),
            Step(Call::Inspect(machine, hook_id), reply),
        ]);
        let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;
        assert_eq!(outcome.unexecuted, vec![stop(&machine, &suffix)]);
        client.assert_done();
    }

    let machine = machine('2');
    let hook_id = container('c');
    let suffix = container('d');
    let plan = plan(vec![
        hook(&machine, spec(None, None, Some(5_000))),
        stop(&machine, &suffix),
    ]);
    let client = Scripted::new(vec![
        created(
            Call::Create(machine, ContainerKind::PreDeployHook),
            &hook_id,
        ),
        ok(Call::Start(machine, hook_id)),
        observed(Call::Inspect(machine, hook_id), exited(0)),
        ok(Call::Stop(machine, suffix)),
    ]);
    assert!(
        execute_with(&plan, &client, &CancellationToken::new())
            .await
            .failed
            .is_none()
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn hook_timeout_and_cancellation_attempt_stop_and_retain_the_container() {
    let machine = machine('1');
    let hook_id = container('a');
    let plan = plan(vec![hook(&machine, spec(None, None, Some(10)))]);
    let client = Scripted::new(vec![
        created(
            Call::Create(machine, ContainerKind::PreDeployHook),
            &hook_id,
        ),
        ok(Call::Start(machine, hook_id)),
        Step(Call::Inspect(machine, hook_id), Reply::Pending),
        ok(Call::StopWithGrace(machine, hook_id, 0)),
    ]);
    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;
    assert!(matches!(
        outcome.failed,
        Some(FailedOperation::Operation {
            error: ExecutionError::Hook {
                failure: HookFailure::TimedOut { stop_error: None },
                ..
            },
            ..
        })
    ));
    client.assert_done();

    let cancellation = CancellationToken::new();
    let client = Scripted::new(vec![
        created(
            Call::Create(machine, ContainerKind::PreDeployHook),
            &hook_id,
        ),
        ok(Call::Start(machine, hook_id)),
        Step(Call::Inspect(machine, hook_id), Reply::Pending),
        failed(Call::StopWithGrace(machine, hook_id, 0), "stop failed"),
    ]);
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        tokio::task::yield_now().await;
        cancel.cancel();
    });
    let outcome = execute_with(&plan, &client, &cancellation).await;
    assert!(matches!(
        outcome.failed,
        Some(FailedOperation::Operation {
            error: ExecutionError::Hook {
                failure: HookFailure::Cancelled {
                    stop_error: Some(_)
                },
                ..
            },
            ..
        })
    ));
    client.assert_done();
}

#[tokio::test]
async fn executing_the_same_plan_twice_runs_a_fresh_hook_each_time() {
    let machine = machine('1');
    let first = container('a');
    let second = container('b');
    let plan = plan(vec![hook(&machine, spec(None, None, Some(5_000)))]);
    let client = Scripted::new(vec![
        created(Call::Create(machine, ContainerKind::PreDeployHook), &first),
        ok(Call::Start(machine, first)),
        observed(Call::Inspect(machine, first), exited(0)),
        created(Call::Create(machine, ContainerKind::PreDeployHook), &second),
        ok(Call::Start(machine, second)),
        observed(Call::Inspect(machine, second), exited(0)),
    ]);

    assert!(
        execute_with(&plan, &client, &CancellationToken::new())
            .await
            .failed
            .is_none()
    );
    assert!(
        execute_with(&plan, &client, &CancellationToken::new())
            .await
            .failed
            .is_none()
    );
    client.assert_done();
}
