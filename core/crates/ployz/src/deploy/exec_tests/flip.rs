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
            completed: Vec::new(),
            service: api(),
            unexecuted: plan,
        }
    );
    assert_eq!(client.publications().len(), 1);
    client.assert_done();
}

#[tokio::test]
async fn a_flip_retires_the_replaced_copy_and_removes_a_draining_one_after_the_switch() {
    let machine = machine('1');
    let kept = container('a');
    let replaced = container('b');
    let draining = container('c');
    let new = container('d');
    let older = DeploymentLogId::parse("older").unwrap();
    let mut service = spec(None, None, None);
    service.mode = ployz_core::ServiceMode::Global;
    let plan = vec![
        DeployOperation::ReplaceContainer(ReplacementOperation {
            machine_id: machine,
            old_container_id: replaced,
            spec: service,
            skip_health_monitor: true,
        }),
        DeployOperation::RemoveContainer {
            machine_id: machine,
            container_id: draining,
            purpose: ployz_core::RemoveContainerPurpose::Rollout,
        },
    ];
    let client = Scripted::new(vec![
        created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Remove(machine, draining)),
    ]);
    client.pointers.lock().unwrap().insert(
        api(),
        ServingPointer {
            deploy: older.clone(),
            containers: vec![kept, replaced],
            previous: None,
            retiring: vec![draining],
        },
    );
    let mut groups = crate::deploy::planning::test_groups(&plan);
    groups.first_mut().unwrap().kept = vec![kept];

    let outcome = super::super::execute_operation_sequence(
        &crate::deploy::DeployPlan::for_execution_test_with_groups(
            plan.clone(),
            test_namespace(),
            groups,
        ),
        &run_id(),
        &client,
        &CancellationToken::new(),
        None,
    )
    .await;

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
            retiring: vec![replaced],
        }
    );
    assert_eq!(
        publication.read.as_ref().map(|read| &read.deploy),
        Some(&publication.pointer.previous.clone().unwrap())
    );
    assert_eq!(*client.removed_after.lock().unwrap(), [(draining, 1)]);
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_rollout_removal_waits_until_every_machine_routes_the_new_pointer() {
    let lagging = machine('2');
    let machine = machine('1');
    let replaced = container('b');
    let draining = container('c');
    let new = container('d');
    let plan = vec![
        DeployOperation::ReplaceContainer(ReplacementOperation {
            machine_id: machine,
            old_container_id: replaced,
            spec: spec(None, None, None),
            skip_health_monitor: true,
        }),
        DeployOperation::RemoveContainer {
            machine_id: machine,
            container_id: draining,
            purpose: ployz_core::RemoveContainerPurpose::Rollout,
        },
    ];
    let client = Scripted {
        unrouted_machine: Some(lagging),
        ..Scripted::new(vec![created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &new,
        )])
    };
    client.pointers.lock().unwrap().insert(
        api(),
        ServingPointer {
            deploy: DeploymentLogId::parse("older").unwrap(),
            containers: vec![replaced],
            previous: None,
            retiring: vec![draining],
        },
    );

    let outcome = execute_groups(&plan, crate::deploy::planning::test_groups(&plan), &client).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                completed,
                failed: FailedOperation::Operation {
                    error: ExecutionError::DependencyHealth {
                        failure: DependencyHealthFailure::NotRouted { .. },
                        ..
                    },
                    ..
                },
                ..
            } if completed.len() == 1
        ),
        "{outcome:?}"
    );
    assert_eq!(client.publications().len(), 1);
    assert!(client.removed_after.lock().unwrap().is_empty());
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

fn execute_groups<C: MachineOperations>(
    plan: &[DeployOperation],
    groups: Vec<crate::deploy::schedule::ServiceGroup>,
    client: &C,
) -> impl std::future::Future<Output = DeployOutcome<ExecutionError>> {
    let plan = crate::deploy::DeployPlan::for_execution_test_with_groups(
        plan.to_vec(),
        test_namespace(),
        groups,
    );
    async move {
        super::super::execute_operation_sequence(
            &plan,
            &run_id(),
            client,
            &CancellationToken::new(),
            None,
        )
        .await
    }
}

fn global_replacement(
    machine_id: &MachineId,
    old: &ContainerId,
    order: UpdateOrder,
) -> DeployOperation {
    let mut service = spec(None, None, None);
    service.mode = ployz_core::ServiceMode::Global;
    service.update.order = order;
    DeployOperation::ReplaceContainer(ReplacementOperation {
        machine_id: *machine_id,
        old_container_id: *old,
        spec: service,
        skip_health_monitor: true,
    })
}

#[tokio::test]
async fn a_labelled_first_deploy_pins_what_serves_and_retires_it_behind_previous() {
    let machine = machine('1');
    let old = container('a');
    let new = container('b');
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StartFirst)];
    let client = Scripted {
        observations: Some(vec![serving(&machine, &old, Some("dep-x"))]),
        ..Scripted::new(vec![created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &new,
        )])
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let labelled = DeploymentLogId::parse("dep-x").unwrap();
    let pointers: Vec<_> = [client.pins(), client.publications()]
        .concat()
        .into_iter()
        .map(|publication| publication.pointer)
        .collect();
    assert_eq!(
        pointers,
        [
            ServingPointer {
                deploy: labelled.clone(),
                containers: vec![old],
                previous: None,
                retiring: Vec::new(),
            },
            ServingPointer {
                deploy: run_id(),
                containers: vec![new],
                previous: Some(labelled),
                retiring: vec![old],
            },
        ]
    );
    client.assert_done();
}

#[tokio::test]
async fn a_lost_publish_reply_keeps_the_copies_the_row_points_at() {
    let machine = machine('1');
    let new = container('a');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let client = Scripted {
        lost_reply: true,
        ..Scripted::new(vec![created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &new,
        )])
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.publications().len(), 1);
    assert_eq!(
        client
            .pointers
            .lock()
            .unwrap()
            .get(&api())
            .map(|pointer| pointer.containers.clone()),
        Some(vec![new])
    );
    client.assert_done();
}

#[tokio::test]
async fn a_publish_with_no_reply_keeps_its_copies_while_the_row_reads_unchanged() {
    let machine = machine('1');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let client = Scripted {
        dropped_publish: true,
        ..Scripted::new(vec![created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &container('a'),
        )])
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.publications().len(), 1);
    client.assert_done();
}

#[tokio::test]
async fn a_first_flip_keeps_the_replicas_the_plan_left_running() {
    let machine = machine('1');
    let kept = container('a');
    let new = container('b');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let client = Scripted {
        observations: Some(vec![serving(&machine, &kept, None)]),
        ..Scripted::new(vec![created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &new,
        )])
    };
    let mut groups = crate::deploy::planning::test_groups(&plan);
    groups.first_mut().unwrap().kept = vec![kept];

    let outcome = execute_groups(&plan, groups, &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let pin = DeploymentLogId::parse("pin-run").unwrap();
    let pointers: Vec<_> = [client.pins(), client.publications()]
        .concat()
        .into_iter()
        .map(|publication| publication.pointer)
        .collect();
    assert_eq!(
        pointers,
        [
            ServingPointer {
                deploy: pin.clone(),
                containers: vec![kept],
                previous: None,
                retiring: Vec::new(),
            },
            ServingPointer {
                deploy: run_id(),
                containers: vec![kept, new],
                previous: Some(pin),
                retiring: Vec::new(),
            },
        ]
    );
    client.assert_done();
}

#[tokio::test]
async fn a_rollback_that_keeps_a_retiring_copy_repoints_it_before_removing_the_other() {
    let machine = machine('1');
    let kept = container('a');
    let removed = container('b');
    let current = DeploymentLogId::parse("d1").unwrap();
    let plan = vec![DeployOperation::RemoveContainer {
        machine_id: machine,
        container_id: removed,
        purpose: ployz_core::RemoveContainerPurpose::Rollout,
    }];
    let client = Scripted::new(vec![ok(Call::Remove(machine, removed))]);
    client.pointers.lock().unwrap().insert(
        api(),
        ServingPointer {
            deploy: current.clone(),
            containers: vec![removed],
            previous: Some(DeploymentLogId::parse("d0").unwrap()),
            retiring: vec![kept],
        },
    );
    let service = ServiceName::parse("api").unwrap();
    let groups = vec![
        crate::deploy::schedule::ServiceGroup {
            service: Some(service.clone()),
            dependencies: Vec::new(),
            steps: Vec::new(),
            publish: true,
            kept: vec![kept],
        },
        crate::deploy::schedule::ServiceGroup {
            service: None,
            dependencies: vec![service],
            steps: vec![crate::deploy::schedule::Step::Serial(0)],
            publish: false,
            kept: Vec::new(),
        },
    ];

    let outcome = execute_groups(&plan, groups, &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let publications = client.publications();
    let [publication] = publications.as_slice() else {
        panic!("one pointer flip before the removal");
    };
    assert_eq!(
        publication.pointer,
        ServingPointer {
            deploy: run_id(),
            containers: vec![kept],
            previous: Some(current),
            retiring: Vec::new(),
        }
    );
    client.assert_done();
}

#[tokio::test]
async fn a_stop_first_replacement_publishes_its_new_container() {
    let machine = machine('1');
    let old = container('a');
    let new = container('b');
    let older = DeploymentLogId::parse("older").unwrap();
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine, old), running()),
        ok(Call::Stop(machine, old)),
        created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Remove(machine, old)),
    ]);
    client.pointers.lock().unwrap().insert(
        api(),
        ServingPointer {
            deploy: older.clone(),
            containers: vec![old],
            previous: None,
            retiring: Vec::new(),
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
            containers: vec![new],
            previous: Some(older),
            retiring: Vec::new(),
        }
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_stop_first_replacement_waits_for_its_dependency_like_any_start() {
    let machine = machine('1');
    let old = container('a');
    let mut plan = vec![run(&machine, spec(None, None, None), true)];
    let DeployOperation::ReplaceContainer(mut web) =
        global_replacement(&machine, &old, UpdateOrder::StopFirst)
    else {
        unreachable!()
    };
    web.spec.name = ServiceName::parse("web").unwrap();
    plan.push(DeployOperation::ReplaceContainer(web));
    let client = Scripted {
        unrouted: Some(ServiceName::parse("api").unwrap()),
        ..Scripted::new(vec![created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &container('b'),
        )])
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::DependencyHealth {
                        failure: DependencyHealthFailure::NotRouted { .. },
                        ..
                    },
                    ..
                },
                ..
            }
        ),
        "{outcome:?}"
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_dependent_that_never_gets_its_ack_removes_what_it_started_elsewhere() {
    let first = machine('1');
    let second = machine('2');
    let started = container('b');
    let plan = vec![
        run(&first, spec(None, None, None), true),
        web_run(&first),
        web_run(&second),
    ];
    let client = Scripted {
        unrouted_machine: Some(second),
        ..Scripted::new(vec![
            created(
                Call::Run(first, ContainerKind::ServiceContainer),
                &container('a'),
            ),
            created(Call::Run(first, ContainerKind::ServiceContainer), &started),
            ok(Call::Discard(first, started)),
        ])
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::DependencyHealth {
                        failure: DependencyHealthFailure::NotRouted { machine_id },
                        ..
                    },
                    ..
                },
                ..
            } if *machine_id == second
        ),
        "{outcome:?}"
    );
    assert_eq!(client.publications().len(), 1);
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_service_flips_as_soon_as_its_own_replicas_are_up() {
    let machine = machine('1');
    let plan = vec![
        run(&machine, spec(None, None, None), true),
        web_run(&machine),
    ];
    let client = Scripted::new(vec![
        Step(
            Call::Run(machine, ContainerKind::ServiceContainer),
            Reply::CreatedAfter(container('a'), std::time::Duration::from_secs(30)),
        ),
        created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &container('b'),
        ),
    ]);
    let mut groups = crate::deploy::planning::test_groups(&plan);
    groups.get_mut(1).unwrap().dependencies.clear();

    let outcome = execute_groups(&plan, groups, &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let services: Vec<_> = client
        .publications()
        .iter()
        .map(|publication| publication.service.name.to_string())
        .collect();
    assert_eq!(
        services,
        ["web", "api"],
        "web does not wait for api's slow start"
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_failed_group_removes_a_container_a_sibling_starts_afterwards() {
    let machine = machine('1');
    let stopped = container('0');
    let late = container('b');
    let plan = vec![
        stop(&machine, &stopped),
        run(&machine, spec(None, None, None), true),
    ];
    let client = Scripted::new(vec![
        failed(Call::Stop(machine, stopped), "stop failed"),
        Step(
            Call::Run(machine, ContainerKind::ServiceContainer),
            Reply::CreatedAfter(late, std::time::Duration::from_millis(5)),
        ),
        ok(Call::Discard(machine, late)),
    ]);
    let groups = vec![crate::deploy::schedule::ServiceGroup {
        service: Some(ServiceName::parse("api").unwrap()),
        dependencies: Vec::new(),
        steps: vec![crate::deploy::schedule::Step::Parallel(vec![0, 1])],
        publish: true,
        kept: Vec::new(),
    }];

    let outcome = execute_groups(&plan, groups, &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert!(client.publications().is_empty());
    client.assert_done();
}

#[tokio::test]
async fn a_hook_that_ran_stays_completed_when_its_service_fails_to_flip() {
    let machine = machine('1');
    let ran = container('f');
    let migration = hook(&machine, spec(None, None, Some(5_000)));
    let plan = vec![
        migration.clone(),
        run(&machine, spec(None, None, None), true),
    ];
    let client = Scripted::new(vec![
        created(Call::Run(machine, ContainerKind::PreDeployHook), &ran),
        observed(Call::Inspect(machine, ran), exited(0)),
        failed(
            Call::Run(machine, ContainerKind::ServiceContainer),
            "run failed",
        ),
        ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed { completed, unexecuted, .. }
                if *completed == [migration.clone()] && unexecuted.is_empty()
        ),
        "{outcome:?}"
    );
    assert!(client.publications().is_empty());
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_first_deploy_of_an_empty_service_starts_nothing_until_its_pin_is_routed() {
    let machine = machine('1');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let client = Scripted {
        observations: Some(Vec::new()),
        unrouted_pin: true,
        ..Scripted::new(Vec::new())
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                completed,
                failed: FailedOperation::Operation {
                    error: ExecutionError::DependencyHealth {
                        failure: DependencyHealthFailure::NotRouted { .. },
                        ..
                    },
                    ..
                },
                ..
            } if completed.is_empty()
        ),
        "{outcome:?}"
    );
    let pins: Vec<_> = client
        .pins()
        .into_iter()
        .map(|pin| pin.pointer.containers)
        .collect();
    assert_eq!(pins, [Vec::<ContainerId>::new()]);
    assert!(client.publications().is_empty());
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_labelled_first_deploy_starts_nothing_until_its_pin_is_routed() {
    let machine = machine('1');
    let old = container('a');
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StartFirst)];
    let client = Scripted {
        observations: Some(vec![serving(&machine, &old, Some("dep-x"))]),
        unrouted_pin: true,
        ..Scripted::new(Vec::new())
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { ref completed, .. } if completed.is_empty()),
        "{outcome:?}"
    );
    assert_eq!(client.pins().len(), 1);
    assert!(client.publications().is_empty());
    client.assert_done();
}

#[tokio::test]
async fn a_pin_that_loses_to_another_deploy_stops_with_nothing_touched() {
    let machine = machine('1');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let winner = ServingPointer {
        deploy: DeploymentLogId::parse("winner").unwrap(),
        containers: vec![container('e')],
        previous: None,
        retiring: Vec::new(),
    };
    let client = Scripted {
        observations: Some(Vec::new()),
        rival: Some(winner.clone()),
        ..Scripted::new(Vec::new())
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome,
        DeployOutcome::Superseded {
            completed: Vec::new(),
            service: api(),
            unexecuted: plan,
        }
    );
    assert!(client.publications().is_empty());
    assert_eq!(client.pointers.lock().unwrap().get(&api()), Some(&winner));
    client.assert_done();
}

fn not_routed(outcome: &DeployOutcome<ExecutionError>) -> bool {
    matches!(
        outcome,
        DeployOutcome::Failed {
            completed,
            failed: FailedOperation::Operation {
                error: ExecutionError::DependencyHealth {
                    failure: DependencyHealthFailure::NotRouted { .. },
                    ..
                },
                ..
            },
            ..
        } if completed.is_empty()
    )
}

fn retry(
    earlier: &Scripted,
    observations: Vec<ContainerObservation>,
    unrouted_pin: bool,
    steps: Vec<Step>,
) -> Scripted {
    Scripted {
        observations: Some(observations),
        unrouted_pin,
        pointers: Mutex::new(earlier.pointers.lock().unwrap().clone()),
        ..Scripted::new(steps)
    }
}

async fn retry_after_an_unrouted_pin(
    plan: &[DeployOperation],
    observations: Vec<ContainerObservation>,
    start: Step,
) {
    let first = retry(
        &Scripted::new(Vec::new()),
        observations.clone(),
        true,
        Vec::new(),
    );
    let outcome = execute_with(plan, &first, &CancellationToken::new()).await;
    assert!(not_routed(&outcome), "{outcome:?}");
    assert_eq!(first.pins().len(), 1);

    let unrouted = retry(&first, observations.clone(), true, Vec::new());
    let outcome = execute_with(plan, &unrouted, &CancellationToken::new()).await;
    assert!(not_routed(&outcome), "{outcome:?}");
    assert!(unrouted.publications.lock().unwrap().is_empty());

    let routed = retry(&first, observations, false, vec![start]);
    let outcome = execute_with(plan, &routed, &CancellationToken::new()).await;
    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(routed.pins().is_empty());
    assert_eq!(routed.publications().len(), 1);
    routed.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_retry_over_an_unrouted_empty_pin_starts_nothing_until_it_is_routed() {
    let machine = machine('1');
    retry_after_an_unrouted_pin(
        &[run(&machine, spec(None, None, None), true)],
        Vec::new(),
        created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &container('b'),
        ),
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_retry_over_an_unrouted_labelled_pin_starts_nothing_until_it_is_routed() {
    let machine = machine('1');
    let old = container('a');
    retry_after_an_unrouted_pin(
        &[global_replacement(&machine, &old, UpdateOrder::StartFirst)],
        vec![serving(&machine, &old, Some("dep-x"))],
        created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &container('b'),
        ),
    )
    .await;
}

#[tokio::test(start_paused = true)]
async fn a_join_starts_once_every_dependency_it_needs_is_routed() {
    let machine = machine('1');
    let named = |service: &str| {
        let mut named = spec(None, None, None);
        named.name = ServiceName::parse(service).unwrap();
        run(&machine, named, true)
    };
    let plan = vec![named("db"), named("cache"), named("app")];
    let client = Scripted::new(
        ['a', 'b', 'c']
            .map(|id| {
                created(
                    Call::Run(machine, ContainerKind::ServiceContainer),
                    &container(id),
                )
            })
            .into(),
    );
    let group =
        |service: &str, dependencies: &[&str], task| crate::deploy::schedule::ServiceGroup {
            service: Some(ServiceName::parse(service).unwrap()),
            dependencies: dependencies
                .iter()
                .map(|dependency| ServiceName::parse(*dependency).unwrap())
                .collect(),
            steps: vec![crate::deploy::schedule::Step::Parallel(vec![task])],
            publish: true,
            kept: Vec::new(),
        };
    let groups = vec![
        group("db", &[], 0),
        group("cache", &[], 1),
        group("app", &["db", "cache"], 2),
    ];

    let outcome = execute_groups(&plan, groups, &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.publications().len(), 3);
    client.assert_done();
}
