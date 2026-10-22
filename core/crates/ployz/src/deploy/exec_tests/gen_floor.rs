use super::*;

fn new_gen(client: &Scripted, container_id: &ContainerId) -> Gen {
    client
        .copies
        .lock()
        .unwrap()
        .get(container_id)
        .unwrap()
        .r#gen
}

#[tokio::test]
async fn a_new_copy_outranks_an_in_copy_only_the_store_still_lists() {
    let (live, gone) = (machine('1'), machine('3'));
    let (offline, new) = (container('a'), container('b'));
    let plan = vec![run(&live, spec(None, None, None), true)];
    let client = Scripted {
        down: BTreeSet::from([gone]),
        store: Store::Copies,
        ..Scripted::new(vec![created(
            Call::Run(live, ContainerKind::ServiceContainer),
            &new,
        )])
    }
    .with_copy_at(&api(), &gone, &offline, CopyRotation::In, Gen::new(2));

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(new_gen(&client, &new), Gen::new(3));
    assert_eq!(client.brought_in(), [new]);
    client.assert_done();
}

#[tokio::test]
async fn a_new_copy_outranks_an_offline_copy_at_the_newest_gen_the_live_answers_show() {
    let (live, gone) = (machine('1'), machine('3'));
    let (visible, offline, new) = (container('a'), container('b'), container('c'));
    let plan = vec![run(&live, spec(None, None, None), true)];
    let client = Scripted {
        observations: Some(vec![observation(&live, &visible, running())]),
        down: BTreeSet::from([gone]),
        store: Store::Copies,
        ..Scripted::new(vec![created(
            Call::Run(live, ContainerKind::ServiceContainer),
            &new,
        )])
    }
    .with_copy_at(&api(), &live, &visible, CopyRotation::In, Gen::new(1))
    .with_copy_at(&api(), &gone, &offline, CopyRotation::In, Gen::new(2));

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        new_gen(&client, &new),
        Gen::new(3),
        "an equal gen would leave the offline copy In beside it once its Machine returns"
    );
    client.assert_done();
}

#[tokio::test]
async fn a_failed_store_read_leaves_the_gen_to_the_live_answers() {
    let live = machine('1');
    let new = container('a');
    let plan = vec![run(&live, spec(None, None, None), true)];
    let client = Scripted {
        store: Store::Fails,
        ..Scripted::new(vec![created(
            Call::Run(live, ContainerKind::ServiceContainer),
            &new,
        )])
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(new_gen(&client, &new), Gen::FIRST);
    client.assert_done();
}

#[tokio::test]
async fn a_failed_store_read_still_meets_the_refusal_an_offline_copy_raises() {
    let (live, gone) = (machine('1'), machine('3'));
    let (offline, new) = (container('a'), container('b'));
    let plan = vec![run(&live, spec(None, None, None), true)];
    let client = Scripted {
        down: BTreeSet::from([gone]),
        store: Store::Fails,
        ..Scripted::new(vec![
            created(Call::Run(live, ContainerKind::ServiceContainer), &new),
            ok(Call::Discard(live, new)),
        ])
    }
    .with_copy_at(&api(), &gone, &offline, CopyRotation::In, Gen::new(2));

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome,
        DeployOutcome::Failed {
            completed: Vec::new(),
            failed: FailedOperation::Operation {
                operation: plan.first().cloned().unwrap(),
                error: ExecutionError::NewerServing { service: api() },
            },
            unexecuted: Vec::new(),
            services: ended(&[("api", ServiceResult::Newer)]),
        }
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_silent_store_leaves_the_gen_to_the_live_answers() {
    let live = machine('1');
    let new = container('a');
    let plan = vec![run(&live, spec(None, None, None), true)];
    let client = Scripted {
        store: Store::Silent,
        ..Scripted::new(vec![created(
            Call::Run(live, ContainerKind::ServiceContainer),
            &new,
        )])
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(new_gen(&client, &new), Gen::FIRST);
    client.assert_done();
}

#[tokio::test]
async fn one_store_read_serves_every_service_of_a_deploy() {
    let live = machine('1');
    let (api_copy, worker_copy) = (container('a'), container('b'));
    let mut worker = spec(None, None, None);
    worker.name = ServiceName::parse("worker").unwrap();
    let plan = vec![
        run(&live, spec(None, None, None), true),
        run(&live, worker, true),
    ];
    let client = Scripted {
        store: Store::Copies,
        ..Scripted::new(vec![
            created(Call::Run(live, ContainerKind::ServiceContainer), &api_copy),
            created(
                Call::Run(live, ContainerKind::ServiceContainer),
                &worker_copy,
            ),
        ])
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(*client.store_reads.lock().unwrap(), 1);
    client.assert_done();
}

#[tokio::test]
async fn another_service_or_namespace_in_the_store_leaves_the_gen_alone() {
    let (live, other) = (machine('1'), machine('2'));
    let (worker, foreign, new) = (container('a'), container('b'), container('c'));
    let plan = vec![run(&live, spec(None, None, None), true)];
    let client = Scripted {
        store: Store::Copies,
        ..Scripted::new(vec![created(
            Call::Run(live, ContainerKind::ServiceContainer),
            &new,
        )])
    }
    .with_copy_at(
        &QualifiedService::new(test_namespace(), ServiceName::parse("worker").unwrap()),
        &other,
        &worker,
        CopyRotation::In,
        Gen::new(7),
    )
    .with_copy_at(
        &QualifiedService::new(
            Namespace::parse("other-ns").unwrap(),
            ServiceName::parse("api").unwrap(),
        ),
        &other,
        &foreign,
        CopyRotation::In,
        Gen::new(9),
    );

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(new_gen(&client, &new), Gen::FIRST);
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn cancelling_ends_a_wait_on_a_silent_store_and_starts_nothing() {
    let live = machine('1');
    let plan = vec![run(&live, spec(None, None, None), true)];
    let client = Scripted {
        store: Store::Silent,
        ..Scripted::new(Vec::new())
    };
    let cancellation = CancellationToken::new();
    let cancel = cancellation.clone();
    tokio::spawn(async move {
        tokio::time::sleep(std::time::Duration::from_secs(1)).await;
        cancel.cancel();
    });
    let started = tokio::time::Instant::now();

    let outcome = execute_with(&plan, &client, &cancellation).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Cancelled,
                    ..
                },
                ..
            }
        ),
        "{outcome:?}"
    );
    assert!(
        started.elapsed() < STORED_GENS_WAIT,
        "{:?}",
        started.elapsed()
    );
    client.assert_done();
}
