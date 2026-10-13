use super::*;

fn api() -> QualifiedService {
    QualifiedService::new(test_namespace(), ServiceName::parse("api").unwrap())
}

fn web_run(machine_id: &MachineId) -> DeployOperation {
    let mut web = spec(None, None, None);
    web.name = ServiceName::parse("web").unwrap();
    run(machine_id, web, true)
}

#[tokio::test]
async fn a_pull_failure_fails_the_deploy_with_nothing_touched() {
    let first = machine('1');
    let second = machine('2');
    let plan = vec![
        run(&first, spec(None, None, None), true),
        run(&second, spec(None, None, None), true),
    ];
    let client = Scripted {
        pull_error: Some(error("manifest unknown")),
        ..Scripted::new(Vec::new())
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                completed,
                failed: FailedOperation::Operation {
                    error: ExecutionError::Machine {
                        action: MachineAction::PullImage,
                        error,
                    },
                    ..
                },
                unexecuted,
            } if completed.is_empty() && unexecuted.len() == 1 && error.message == "manifest unknown"
        ),
        "{outcome:?}"
    );
    assert_eq!(
        client
            .pulled
            .lock()
            .unwrap()
            .iter()
            .copied()
            .collect::<Vec<_>>(),
        vec![first, second]
    );
    assert!(client.publications().is_empty());
    client.assert_done();
}

#[tokio::test]
async fn a_failed_start_removes_every_new_container_of_its_service_and_writes_no_pointer() {
    let first = machine('1');
    let second = machine('2');
    let started = container('a');
    let plan = vec![
        run(&first, spec(None, None, None), true),
        run(&second, spec(None, None, None), true),
    ];
    let client = Scripted::new(vec![
        created(Call::Run(first, ContainerKind::ServiceContainer), &started),
        failed(
            Call::Run(second, ContainerKind::ServiceContainer),
            "start failed",
        ),
        ok(Call::DiscardSlot(second, ContainerKind::ServiceContainer)),
        ok(Call::Discard(first, started)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Machine {
                        action: MachineAction::CreateContainer,
                        error,
                    },
                    ..
                },
                ..
            } if error.message == "start failed"
        ),
        "{outcome:?}"
    );
    assert!(client.publications().is_empty());
    client.assert_done();
}

#[tokio::test]
async fn a_lost_publish_reports_superseded_and_removes_the_unpointed_copies() {
    let machine = machine('1');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let new = container('a');
    let client = Scripted {
        superseded: Some(ServiceName::parse("api").unwrap()),
        ..Scripted::new(vec![
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            ok(Call::Discard(machine, new)),
        ])
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome,
        DeployOutcome::Superseded {
            completed: plan,
            service: api(),
            unexecuted: Vec::new(),
        }
    );
    assert_eq!(client.publications().len(), 1);
    client.assert_done();
}

#[tokio::test]
async fn a_flip_carries_draining_ids_and_the_replaced_copy_into_retiring() {
    let machine = machine('1');
    let kept = container('a');
    let replaced = container('b');
    let draining = container('c');
    let new = container('d');
    let older = DeploymentLogId::parse("older").unwrap();
    let mut service = spec(None, None, None);
    service.mode = ployz_core::ServiceMode::Global;
    let plan = vec![DeployOperation::ReplaceContainer(ReplacementOperation {
        machine_id: machine,
        old_container_id: replaced,
        spec: service,
        skip_health_monitor: true,
    })];
    let client = Scripted::new(vec![created(
        Call::Run(machine, ContainerKind::ServiceContainer),
        &new,
    )]);
    client.pointers.lock().unwrap().insert(
        api(),
        ServingPointer {
            deploy: older.clone(),
            containers: vec![kept, replaced],
            previous: None,
            retiring: vec![draining],
        },
    );

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let publications = client.publications();
    let [publication] = publications.as_slice() else {
        panic!("one pointer flip");
    };
    assert_eq!(
        publication.pointer,
        ServingPointer {
            deploy: run_id(),
            containers: vec![kept, new],
            previous: Some(older),
            retiring: vec![draining, replaced],
        }
    );
    assert_eq!(
        publication.read.as_ref().map(|read| &read.deploy),
        Some(&publication.pointer.previous.clone().unwrap())
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_dependent_starts_once_its_machine_acks_the_dependency() {
    let machine = machine('1');
    let plan = vec![
        run(&machine, spec(None, None, None), true),
        web_run(&machine),
    ];
    let client = Scripted::new(vec![
        created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &container('a'),
        ),
        created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &container('b'),
        ),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let services: Vec<_> = client
        .publications()
        .iter()
        .map(|publication| publication.service.name.to_string())
        .collect();
    assert_eq!(services, ["api", "web"]);
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_dependent_fails_when_its_machine_never_acks_the_dependency() {
    let machine = machine('1');
    let plan = vec![
        run(&machine, spec(None, None, None), true),
        web_run(&machine),
    ];
    let client = Scripted {
        unrouted: Some(ServiceName::parse("api").unwrap()),
        ..Scripted::new(vec![created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &container('a'),
        )])
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                completed,
                failed: FailedOperation::Operation {
                    error: ExecutionError::DependencyHealth {
                        dependency,
                        failure: DependencyHealthFailure::NotRouted { machine_id },
                    },
                    ..
                },
                unexecuted,
            } if completed.len() == 1
                && unexecuted.is_empty()
                && *dependency == api()
                && *machine_id == machine
        ),
        "{outcome:?}"
    );
    assert_eq!(client.publications().len(), 1);
    client.assert_done();
}
