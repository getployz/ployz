use super::*;

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
                ..
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
    assert!(client.brought_in().is_empty());
    client.assert_done();
}

#[tokio::test]
async fn a_failed_start_removes_every_new_container_of_its_service_and_brings_none_in() {
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
    assert!(client.brought_in().is_empty());
    client.assert_done();
}

#[tokio::test]
async fn each_ready_start_waits_for_the_switch_before_it_completes() {
    let first = machine('1');
    let second = machine('2');
    let (one, two) = (container('a'), container('b'));
    let plan = vec![
        run(&first, spec(None, None, None), true),
        run(&second, spec(None, None, None), true),
    ];
    let client = Scripted::new(vec![
        created(Call::Run(first, ContainerKind::ServiceContainer), &one),
        created(Call::Run(second, ContainerKind::ServiceContainer), &two),
    ]);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();

    let outcome = super::super::execute_operation_sequence(
        &crate::deploy::DeployPlan::for_execution_test(plan, test_namespace()),
        &run_id(),
        &client,
        &CancellationToken::new(),
        Some(tx),
    )
    .await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let mut frames = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let DeployEvent::Progress { rows, .. } = event {
            frames.push(rows.into_iter().map(|row| row.status).collect::<Vec<_>>());
        }
    }
    let switch = frames
        .iter()
        .position(|statuses| statuses.contains(&ployz_core::OperationStatus::Completed))
        .unwrap();
    let waiting = |container_id| ployz_core::OperationStatus::Running {
        phase: ployz_core::OperationPhase::WaitingForSwitch { container_id },
    };
    assert_eq!(
        frames.get(switch - 1).map(Vec::as_slice),
        Some([waiting(one), waiting(two)].as_slice()),
        "{frames:?}"
    );
    assert_eq!(
        frames.last().unwrap(),
        &[
            ployz_core::OperationStatus::Completed,
            ployz_core::OperationStatus::Completed
        ]
    );
    client.assert_done();
}

#[tokio::test]
async fn a_newer_in_copy_fails_the_deploy_naming_its_service_and_removes_the_new_copies() {
    let machine = machine('1');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let (newer, new) = (container('a'), container('b'));
    let client = Scripted::new(vec![
        created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Discard(machine, new)),
    ])
    .with_copy_at(&api(), &machine, &newer, CopyRotation::In, Gen::new(100));

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
    let failure = crate::deploy::report::failure(&outcome, &[], Vec::new());
    let shown = crate::ui::plain(&failure);
    assert_eq!(
        shown.lines().next(),
        Some("error: api: A newer deploy is already serving app/api."),
        "{shown}"
    );
    assert!(!shown.contains("start"), "{shown}");
    assert_eq!(failure.report().code, RpcErrorCode::Conflict);
    assert_eq!(
        failure
            .diagnostic()
            .and_then(|diagnostic| diagnostic.failed_machine),
        None
    );
    assert!(client.brought_in().is_empty());
    assert_eq!(client.rotation(&newer), Some(CopyRotation::In));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_newer_in_copy_removes_the_stop_first_copy_it_keeps_from_coming_back() {
    let machine = machine('1');
    let (old, newer, new) = (container('a'), container('c'), container('d'));
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine, old), running()),
        ok(Call::Stop(machine, old)),
        created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Discard(machine, new)),
        ok(Call::Remove(machine, old)),
    ])
    .with_copy(&machine, &old, CopyRotation::In)
    .with_copy_at(&api(), &machine, &newer, CopyRotation::In, Gen::new(100));

    let (outcome, warnings) =
        execute_reported(&plan, &[machine], &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::NewerServing { .. },
                    ..
                },
                ..
            }
        ),
        "{outcome:?}"
    );
    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::Newer)]));
    let shown = crate::ui::plain(&crate::deploy::report::failure(&outcome, &[], Vec::new()));
    for said in std::iter::once(&shown).chain(&warnings) {
        assert!(!said.contains("start"), "{said}");
    }
    assert_eq!(
        client.rotation(&old),
        None,
        "the old copy can never serve again"
    );
    assert_eq!(client.rotation(&newer), Some(CopyRotation::In));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn an_old_copy_a_newer_copy_refuses_that_will_not_go_is_left_saying_why() {
    let machine = machine('1');
    let (old, newer, new) = (container('a'), container('c'), container('d'));
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine, old), running()),
        ok(Call::Stop(machine, old)),
        created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
        ok(Call::Discard(machine, new)),
        failed(Call::Remove(machine, old), "the disk is busy"),
    ])
    .with_copy(&machine, &old, CopyRotation::In)
    .with_copy_at(&api(), &machine, &newer, CopyRotation::In, Gen::new(100));

    let (_, warnings) =
        execute_reported(&plan, &[machine], &client, &CancellationToken::new()).await;

    assert_eq!(
        warnings,
        [left_running(
            "machine-1",
            "a newer deploy is already serving app/api, and removing it failed: the disk is busy"
        )]
    );
    assert_eq!(client.rotation(&old), Some(CopyRotation::Out));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_rolling_copy_a_newer_deploy_refuses_fails_its_own_step_and_the_next_never_runs() {
    let (first, second) = (machine('1'), machine('2'));
    let (old_first, old_second, newer, new) = (
        container('a'),
        container('b'),
        container('c'),
        container('d'),
    );
    let plan = vec![
        global_replacement(&first, &old_first, UpdateOrder::Rolling),
        global_replacement(&second, &old_second, UpdateOrder::Rolling),
    ];
    let client = Scripted::new(vec![
        observed(Call::Inspect(first, old_first), running()),
        ok(Call::Stop(first, old_first)),
        created(Call::Run(first, ContainerKind::ServiceContainer), &new),
        ok(Call::Discard(first, new)),
        ok(Call::Remove(first, old_first)),
    ])
    .with_copy(&first, &old_first, CopyRotation::In)
    .with_copy(&second, &old_second, CopyRotation::In)
    .with_copy_at(&api(), &first, &newer, CopyRotation::In, Gen::new(100));

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome,
        DeployOutcome::Failed {
            completed: Vec::new(),
            failed: FailedOperation::Operation {
                operation: plan.first().cloned().unwrap(),
                error: ExecutionError::NewerServing { service: api() },
            },
            unexecuted: plan.get(1..).unwrap().to_vec(),
            services: ended(&[("api", ServiceResult::Newer)]),
        }
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_rolling_copy_that_fails_health_fails_its_own_step_and_the_next_never_runs() {
    let (first, second) = (machine('1'), machine('2'));
    let (old_first, old_second, new) = (container('a'), container('b'), container('d'));
    let plan = vec![
        global_replacement(&first, &old_first, UpdateOrder::Rolling),
        global_replacement(&second, &old_second, UpdateOrder::Rolling),
    ];
    let client = Scripted {
        unhealthy_in: BTreeMap::from([(
            new,
            HealthVerdict::Failed {
                observation: unhealthy(),
                last_check: None,
                oom_killed: false,
                departing: None,
            },
        )]),
        ..Scripted::new(vec![
            observed(Call::Inspect(first, old_first), running()),
            ok(Call::Stop(first, old_first)),
            created(Call::Run(first, ContainerKind::ServiceContainer), &new),
            ok(Call::Discard(first, new)),
            ok(Call::Restart(first, old_first)),
        ])
    }
    .with_copy(&first, &old_first, CopyRotation::In)
    .with_copy(&second, &old_second, CopyRotation::In);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    operation,
                    error: ExecutionError::Health { container_id, .. },
                },
                unexecuted,
                ..
            } if Some(operation) == plan.first()
                && *container_id == new
                && unexecuted.as_slice() == plan.get(1..).unwrap()
        ),
        "{outcome:?}"
    );
    client.assert_done();
}

#[tokio::test]
async fn a_flip_retires_the_replaced_copy_and_removes_a_draining_one_after_the_switch() {
    let machine = machine('1');
    let kept = container('a');
    let replaced = container('b');
    let draining = container('c');
    let new = container('d');
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
        observed(Call::Inspect(machine, replaced), running()),
        ok(Call::Remove(machine, replaced)),
        observed(Call::Inspect(machine, draining), running()),
        ok(Call::Remove(machine, draining)),
    ])
    .with_copy(&machine, &kept, CopyRotation::In)
    .with_copy(&machine, &replaced, CopyRotation::In)
    .with_copy(&machine, &draining, CopyRotation::Out);
    let groups = crate::deploy::planning::test_groups(&plan);

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
    assert_eq!(
        client.moves(),
        [
            Move::To(new, CopyRotation::In),
            Move::To(replaced, CopyRotation::Out),
            Move::Gone(replaced),
            Move::Gone(draining),
        ],
        "the draining copy is removed only after the new copy is In"
    );
    assert_eq!(client.rotation(&kept), Some(CopyRotation::In));
    client.assert_done();
}

#[tokio::test]
async fn a_flip_keeps_a_retiring_copy_its_plan_never_saw() {
    let machine = machine('1');
    let kept = container('a');
    let replaced = container('b');
    let pending = container('c');
    let new = container('d');
    let plan = vec![global_replacement(
        &machine,
        &replaced,
        UpdateOrder::StartFirst,
    )];
    let client = Scripted::new(vec![
        created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
        observed(Call::Inspect(machine, replaced), running()),
        ok(Call::Remove(machine, replaced)),
    ])
    .with_copy(&machine, &kept, CopyRotation::In)
    .with_copy(&machine, &replaced, CopyRotation::In)
    .with_copy(&machine, &pending, CopyRotation::Out);
    let groups = crate::deploy::planning::test_groups(&plan);

    let outcome = execute_groups(&plan, groups, &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        client.rotation(&pending),
        Some(CopyRotation::Out),
        "a copy the plan never names is left as it is"
    );
    assert_eq!(client.rotation(&kept), Some(CopyRotation::In));
    assert_eq!(client.rotation(&replaced), None);
    client.assert_done();
}

#[tokio::test]
async fn a_scale_down_retires_the_copy_it_removes_before_removing_it() {
    let machine = machine('1');
    let kept = container('a');
    let removed = container('b');
    let plan = vec![DeployOperation::RemoveContainer {
        machine_id: machine,
        container_id: removed,
        purpose: ployz_core::RemoveContainerPurpose::Rollout,
    }];
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine, removed), running()),
        ok(Call::Remove(machine, removed)),
    ])
    .with_copy(&machine, &kept, CopyRotation::In)
    .with_copy(&machine, &removed, CopyRotation::In);

    let outcome = execute_groups(&plan, scale_down_groups(), &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        client.moves(),
        [Move::To(removed, CopyRotation::Out), Move::Gone(removed)]
    );
    assert_eq!(client.rotation(&kept), Some(CopyRotation::In));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_removal_whose_out_no_proxy_confirms_fails_the_deploy() {
    let machine = machine('1');
    let kept = container('a');
    let removed = container('b');
    let plan = vec![DeployOperation::RemoveContainer {
        machine_id: machine,
        container_id: removed,
        purpose: ployz_core::RemoveContainerPurpose::Rollout,
    }];
    let client = Scripted {
        withheld: BTreeMap::from([(api(), usize::MAX)]),
        ..Scripted::new(vec![observed(Call::Inspect(machine, removed), running())])
    }
    .with_copy(&machine, &kept, CopyRotation::In)
    .with_copy(&machine, &removed, CopyRotation::In);

    let (outcome, warnings) =
        execute_groups_reported(&plan, scale_down_groups(), &[machine], &client).await;

    assert!(
        !matches!(outcome, DeployOutcome::Success { .. }),
        "the next rotation change waits on this Out: {outcome:?}"
    );
    assert_eq!(client.moves(), [Move::To(removed, CopyRotation::Out)]);
    assert_eq!(
        warnings,
        [left_running(
            "machine-1",
            "proxy on machine-1 did not confirm"
        )]
    );
    client.assert_done();
}

/// Every proxy routes both copies of a Service whose copies all exited. Each
/// Out waits on the route that a sibling still In keeps, so the removal sets
/// every copy Out before it waits on any.
#[tokio::test(start_paused = true)]
async fn removing_a_service_sets_every_copy_out_before_waiting_on_any() {
    let machine = machine('1');
    let first = container('a');
    let second = container('b');
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine, first), exited(1)),
        observed(Call::Inspect(machine, second), exited(1)),
        ok(Call::Remove(machine, second)),
        ok(Call::Remove(machine, first)),
    ])
    .with_copy(&machine, &first, CopyRotation::In)
    .with_copy(&machine, &second, CopyRotation::In);
    client.routes(&api(), false);
    for id in [first, second] {
        client.record(id, |copy| copy.stopped = true);
    }

    let outcome = remove_everything(&client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let moves = client.moves();
    let last_out = moves
        .iter()
        .rposition(|applied| matches!(applied, Move::To(_, CopyRotation::Out)))
        .unwrap();
    let first_gone = moves
        .iter()
        .position(|applied| matches!(applied, Move::Gone(_)))
        .unwrap();
    assert!(last_out < first_gone, "{moves:?}");
    client.assert_done();
}

/// A Pending copy keeps every proxy on the last routed copy, so its removal
/// runs to the end before any sibling goes Out, however long it takes. No
/// sibling's wait runs out on its account.
#[tokio::test(start_paused = true)]
async fn a_slow_pending_removal_completes_before_any_sibling_goes_out() {
    let machine = machine('1');
    let pending = container('a');
    let routed = container('b');
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine, pending), running()),
        Step(
            Call::Remove(machine, pending),
            Reply::OkAfter(|| tokio::time::sleep(2 * crate::deploy::cleanup::ACK_WAIT)),
        ),
        observed(Call::Inspect(machine, routed), running()),
        ok(Call::Remove(machine, routed)),
    ])
    .with_copy(&machine, &pending, CopyRotation::Pending)
    .with_copy(&machine, &routed, CopyRotation::In);
    client.routes(&api(), false);

    let outcome = remove_everything(&client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let moves = client.moves();
    assert_eq!(moves.first(), Some(&Move::Gone(pending)), "{moves:?}");
    client.assert_done();
}

#[tokio::test]
async fn a_removal_of_the_last_in_copy_is_refused() {
    let machine = machine('1');
    let kept = container('a');
    let removed = container('b');
    let plan = vec![DeployOperation::RemoveContainer {
        machine_id: machine,
        container_id: removed,
        purpose: ployz_core::RemoveContainerPurpose::Rollout,
    }];
    let client = Scripted::new(vec![observed(Call::Inspect(machine, removed), running())])
        .with_copy(&machine, &kept, CopyRotation::Pending)
        .with_copy(&machine, &removed, CopyRotation::In);
    let mut groups = scale_down_groups();
    groups.first_mut().unwrap().publish = false;

    let outcome = execute_groups(&plan, groups, &client).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Machine {
                        action: MachineAction::RemoveContainer,
                        error,
                    },
                    ..
                },
                ..
            } if error.code == RpcErrorCode::Conflict
        ),
        "{outcome:?}"
    );
    assert_eq!(client.rotation(&removed), Some(CopyRotation::In));
    client.assert_done();
}

fn scale_down_groups() -> Vec<crate::deploy::schedule::ServiceGroup> {
    let service = ServiceName::parse("api").unwrap();
    vec![
        crate::deploy::schedule::ServiceGroup {
            service: Some(service.clone()),
            dependencies: Vec::new(),
            steps: Vec::new(),
            publish: true,
        },
        crate::deploy::schedule::ServiceGroup {
            service: None,
            dependencies: vec![service],
            steps: vec![crate::deploy::schedule::Step::Serial(0)],
            publish: false,
        },
    ]
}

#[tokio::test(start_paused = true)]
async fn a_rollout_removal_waits_until_every_machine_lists_the_new_copy() {
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
        acks: BTreeMap::from([(lagging, Ack::Never)]),
        ..Scripted::new(vec![created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &new,
        )])
    }
    .with_copy(&machine, &replaced, CopyRotation::In)
    .with_copy(&machine, &draining, CopyRotation::Out);

    let outcome = execute_groups(&plan, crate::deploy::planning::test_groups(&plan), &client).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Machine {
                        action: MachineAction::ReadRoutes,
                        error,
                    },
                    ..
                },
                ..
            } if error.code == RpcErrorCode::Unavailable
        ),
        "{outcome:?}"
    );
    assert_eq!(
        serde_json::to_value(outcome.services()).unwrap(),
        serde_json::json!([{"service": "api", "result": "unconfirmed"}])
    );
    assert_eq!(client.brought_in(), [new]);
    assert_eq!(client.rotation(&replaced), Some(CopyRotation::In));
    assert_eq!(client.rotation(&draining), Some(CopyRotation::Out));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_dependent_starts_once_its_machine_acks_the_dependency() {
    let machine = machine('1');
    let plan = vec![
        run(&machine, spec(None, None, None), true),
        web_run(&machine),
    ];
    let (first, second) = (container('a'), container('b'));
    let client = Scripted {
        acks: BTreeMap::from([(machine, Ack::After(3))]),
        ..Scripted::new(vec![
            created(Call::Run(machine, ContainerKind::ServiceContainer), &first),
            created(Call::Run(machine, ContainerKind::ServiceContainer), &second),
        ])
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.brought_in(), [first, second]);
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_dependent_never_starts_when_its_machine_never_lists_the_dependency() {
    let machine = machine('1');
    let plan = vec![
        run(&machine, spec(None, None, None), true),
        web_run(&machine),
    ];
    let client = Scripted {
        acks: BTreeMap::from([(machine, Ack::Never)]),
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
                failed: FailedOperation::Operation {
                    error: ExecutionError::Machine {
                        action: MachineAction::ReadRoutes,
                        error,
                    },
                    ..
                },
                ..
            } if error.code == RpcErrorCode::Unavailable
        ),
        "{outcome:?}"
    );
    assert_eq!(
        serde_json::to_value(outcome.services()).unwrap(),
        serde_json::json!([{"service": "api", "result": "unconfirmed"}, {"service": "web", "result": "not_started"}])
    );
    assert_eq!(client.brought_in(), [container('a')]);
    client.assert_done();
}

#[tokio::test]
async fn a_failed_chain_reports_each_service_as_new_old_or_not_started() {
    let machine = machine('1');
    let mut db = spec(None, None, None);
    db.name = ServiceName::parse("db").unwrap();
    let plan = vec![
        run(&machine, spec(None, None, None), true),
        web_run(&machine),
        run(&machine, db, true),
    ];
    let client = Scripted::new(vec![
        created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &container('a'),
        ),
        failed(
            Call::Run(machine, ContainerKind::ServiceContainer),
            "start failed",
        ),
        ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
    ]);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome.services(),
        ended(&[
            ("api", ServiceResult::New),
            ("web", ServiceResult::Old),
            ("db", ServiceResult::NotStarted),
        ])
    );
    client.assert_done();
}

#[tokio::test]
async fn a_failed_deploy_keeps_the_services_it_removes() {
    use crate::deploy::{ServiceGroup, Step};

    let machine = machine('1');
    let removed = container('a');
    let volume = DockerVolumeId {
        machine_id: machine,
        name: DockerVolumeName::parse("data").unwrap(),
    };
    let operations = vec![
        run(&machine, spec(None, None, None), true),
        DeployOperation::RemoveContainer {
            machine_id: machine,
            container_id: removed,
            purpose: ployz_core::RemoveContainerPurpose::Retire,
        },
        DeployOperation::RemoveVolume { id: volume },
    ];
    let api = ServiceName::parse("api").unwrap();
    let groups = vec![
        ServiceGroup {
            service: Some(api.clone()),
            dependencies: Vec::new(),
            steps: vec![Step::Parallel(vec![0])],
            publish: true,
        },
        ServiceGroup {
            service: None,
            dependencies: vec![api],
            steps: vec![Step::Serial(1), Step::Serial(2)],
            publish: false,
        },
    ];
    let plan = crate::deploy::DeployPlan::for_execution_test_with_groups(
        operations,
        test_namespace(),
        groups,
    );
    let client = Scripted::new(vec![
        failed(
            Call::Run(machine, ContainerKind::ServiceContainer),
            "start failed",
        ),
        ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
    ]);

    let outcome = super::super::execute_operation_sequence(
        &plan,
        &run_id(),
        &client,
        &CancellationToken::new(),
        None,
    )
    .await;

    assert!(
        matches!(&outcome, DeployOutcome::Failed { unexecuted, .. } if unexecuted.len() == 2),
        "{outcome:?}"
    );
    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::Old)]));
    client.assert_done();
}

/// Runs the Deploy that removes every Service `client` holds copies of, with
/// the steps the planner gives it.
async fn remove_everything(client: &Scripted) -> DeployOutcome<ExecutionError> {
    let intent = crate::deploy::DeployIntent::apply_all(
        test_namespace(),
        [],
        crate::deploy::PlanOptions::default(),
    );
    let plan = crate::deploy::plan_deploy(&intent, &client.snapshot()).unwrap();
    super::super::execute_operation_sequence(
        &plan,
        &run_id(),
        client,
        &CancellationToken::new(),
        None,
    )
    .await
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
async fn an_unanswered_in_ask_that_landed_brings_the_copy_in() {
    let machine = machine('1');
    let new = container('a');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let client = Scripted {
        unanswered_in: BTreeSet::from([new]),
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
    assert_eq!(client.rotation(&new), Some(CopyRotation::In));
    assert_eq!(
        client.brought_in(),
        [new],
        "the retried ask finds the copy In"
    );
    client.assert_done();
}

#[tokio::test]
async fn a_cancel_before_the_stop_ends_the_service_old() {
    let machine = machine('1');
    let old = container('a');
    let plan = vec![stop(&machine, &old)];
    let cancellation = CancellationToken::new();
    let client = Scripted {
        acks: BTreeMap::from([(machine, Ack::Never)]),
        cancel_on_move: Some((
            Move::To(old, CopyRotation::Held(run_id())),
            cancellation.clone(),
        )),
        ..Scripted::new(vec![observed(Call::Inspect(machine, old), running())])
    }
    .with_copy(&machine, &old, CopyRotation::In);
    let mut groups = scale_down_groups();
    groups.truncate(1);
    groups.first_mut().unwrap().steps = vec![crate::deploy::schedule::Step::Serial(0)];
    let plan =
        crate::deploy::DeployPlan::for_execution_test_with_groups(plan, test_namespace(), groups);

    let outcome =
        super::super::execute_operation_sequence(&plan, &run_id(), &client, &cancellation, None)
            .await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        serde_json::to_value(outcome.services()).unwrap(),
        serde_json::json!([{"service": "api", "result": "old"}])
    );
    assert_eq!(
        client.rotation(&old),
        Some(CopyRotation::In),
        "the copy this run held serves again"
    );
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
    }
    .with_copy(&machine, &kept, CopyRotation::In);
    let groups = crate::deploy::planning::test_groups(&plan);

    let outcome = execute_groups(&plan, groups, &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.brought_in(), [new]);
    assert_eq!(client.rotation(&kept), Some(CopyRotation::In));
    client.assert_done();
}

#[tokio::test]
async fn a_stop_first_replacement_publishes_its_new_container() {
    let machine = machine('1');
    let old = container('a');
    let new = container('b');
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine, old), running()),
        ok(Call::Stop(machine, old)),
        created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
        observed(Call::Inspect(machine, old), running()),
        ok(Call::Remove(machine, old)),
    ])
    .with_copy(&machine, &old, CopyRotation::In);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.brought_in(), [new]);
    assert_eq!(client.rotation(&old), None);
    client.assert_done();
}

#[tokio::test]
async fn a_stop_first_replacement_that_cannot_inspect_its_old_copy_leaves_it_in() {
    let machine = machine('1');
    let old = container('a');
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted::new(vec![failed(
        Call::Inspect(machine, old),
        "the machine is away",
    )])
    .with_copy(&machine, &old, CopyRotation::In);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Machine {
                        action: MachineAction::InspectContainer,
                        error,
                    },
                    ..
                },
                ..
            } if error.message == "the machine is away"
        ),
        "{outcome:?}"
    );
    assert!(
        client.moves().is_empty(),
        "the old copy is not held before the stop can run"
    );
    assert_eq!(client.rotation(&old), Some(CopyRotation::In));
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
        acks: BTreeMap::from([(machine, Ack::Never)]),
        ..Scripted::new(vec![created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &container('b'),
        )])
    }
    .with_copy_at(
        &QualifiedService::new(test_namespace(), ServiceName::parse("web").unwrap()),
        &machine,
        &old,
        CopyRotation::In,
        Gen::FIRST,
    );

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Machine {
                        action: MachineAction::ReadRoutes,
                        error,
                    },
                    ..
                },
                ..
            } if error.code == RpcErrorCode::Unavailable
        ),
        "{outcome:?}"
    );
    assert_eq!(
        serde_json::to_value(outcome.services()).unwrap(),
        serde_json::json!([{"service": "api", "result": "unconfirmed"}, {"service": "web", "result": "not_started"}])
    );
    assert_eq!(
        client.rotation(&old),
        Some(CopyRotation::In),
        "web's old copy is untouched"
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_hook_does_not_run_until_its_changed_dependency_is_routed() {
    let machine = machine('1');
    let mut migration = spec(None, None, Some(5_000));
    migration.name = ServiceName::parse("web").unwrap();
    let plan = vec![
        run(&machine, spec(None, None, None), true),
        hook(&machine, migration),
        web_run(&machine),
    ];
    let client = Scripted {
        acks: BTreeMap::from([(machine, Ack::Never)]),
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
                failed: FailedOperation::Operation {
                    error: ExecutionError::Machine {
                        action: MachineAction::ReadRoutes,
                        error,
                    },
                    ..
                },
                ..
            } if error.code == RpcErrorCode::Unavailable
        ),
        "{outcome:?}"
    );
    assert_eq!(
        serde_json::to_value(outcome.services()).unwrap(),
        serde_json::json!([{"service": "api", "result": "unconfirmed"}, {"service": "web", "result": "not_started"}])
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_dependency_one_machine_never_lists_starts_none_of_its_dependents() {
    let first = machine('1');
    let second = machine('2');
    let plan = vec![
        run(&first, spec(None, None, None), true),
        web_run(&first),
        web_run(&second),
    ];
    let client = Scripted {
        acks: BTreeMap::from([(second, Ack::Missing)]),
        ..Scripted::new(vec![created(
            Call::Run(first, ContainerKind::ServiceContainer),
            &container('a'),
        )])
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Machine {
                        action: MachineAction::ReadRoutes,
                        error,
                    },
                    ..
                },
                ..
            } if error.code == RpcErrorCode::Unavailable
        ),
        "{outcome:?}"
    );
    assert_eq!(
        serde_json::to_value(outcome.services()).unwrap(),
        serde_json::json!([{"service": "api", "result": "unconfirmed"}, {"service": "web", "result": "not_started"}])
    );
    assert_eq!(client.brought_in(), [container('a')]);
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
            Reply::CreatedAfter(container('a'), || {
                tokio::time::sleep(std::time::Duration::from_secs(30))
            }),
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
    assert_eq!(
        client.brought_in(),
        [container('b'), container('a')],
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
        observed(Call::Inspect(machine, stopped), running()),
        failed(Call::Stop(machine, stopped), "stop failed"),
        Step(
            Call::Run(machine, ContainerKind::ServiceContainer),
            Reply::CreatedAfter(late, || {
                tokio::time::sleep(std::time::Duration::from_millis(5))
            }),
        ),
        ok(Call::Discard(machine, late)),
    ]);
    let groups = vec![crate::deploy::schedule::ServiceGroup {
        service: Some(ServiceName::parse("api").unwrap()),
        dependencies: Vec::new(),
        steps: vec![crate::deploy::schedule::Step::Parallel(vec![0, 1])],
        publish: true,
    }];

    let outcome = execute_groups(&plan, groups, &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert!(client.brought_in().is_empty());
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
    assert!(client.brought_in().is_empty());
    client.assert_done();
}

#[tokio::test]
async fn a_failed_pull_names_the_service_whose_policy_pulled() {
    let machine = machine('1');
    let mut cached = spec(None, None, None);
    cached.container.pull_policy = ployz_core::PullPolicy::Missing;
    let mut fresh = cached.clone();
    fresh.name = ServiceName::parse("other").unwrap();
    fresh.container.pull_policy = ployz_core::PullPolicy::Always;
    let pulling = run(&machine, fresh, true);
    let plan = vec![run(&machine, cached, true), pulling.clone()];
    let client = Scripted {
        pull_error: Some(error("manifest unknown")),
        ..Scripted::new(Vec::new())
    };

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation { operation, .. },
                ..
            } if *operation == pulling
        ),
        "{outcome:?}"
    );
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
    assert_eq!(
        client.brought_in(),
        [container('a'), container('b'), container('c')]
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_deploy_with_an_unchanged_service_records_its_outcome() {
    use crate::deploy::{ServiceGroup, Step};

    let machine = machine('1');
    let operations = vec![run(&machine, spec(None, None, None), true)];
    let groups = vec![
        ServiceGroup {
            service: Some(ServiceName::parse("db").unwrap()),
            dependencies: Vec::new(),
            steps: Vec::new(),
            publish: false,
        },
        ServiceGroup {
            service: Some(ServiceName::parse("api").unwrap()),
            dependencies: vec![ServiceName::parse("db").unwrap()],
            steps: vec![Step::Parallel(vec![0])],
            publish: true,
        },
    ];
    let plan = crate::deploy::DeployPlan::for_execution_test_with_groups(
        operations,
        test_namespace(),
        groups,
    );
    let client = Scripted::new(vec![created(
        Call::Run(machine, ContainerKind::ServiceContainer),
        &container('a'),
    )]);

    let outcome = super::super::execute_operation_sequence(
        &plan,
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
    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::New)]));
    let recorded = ployz_core::config::project_runtime_outcome(
        plan.preview(),
        serde_json::json!({ "version": 1, "outcome": outcome }),
    );
    assert!(recorded.is_ok(), "{recorded:?}");
    client.assert_done();
}

pub(super) async fn execute_reported<C: MachineOperations>(
    plan: &[DeployOperation],
    named: &[MachineId],
    client: &C,
    cancellation: &CancellationToken,
) -> (DeployOutcome<ExecutionError>, Vec<String>) {
    let plan = crate::deploy::DeployPlan::for_execution_test(plan.to_vec(), test_namespace())
        .with_machine_names(named);
    reported(&plan, client, cancellation).await
}

async fn execute_groups_reported<C: MachineOperations>(
    plan: &[DeployOperation],
    groups: Vec<crate::deploy::schedule::ServiceGroup>,
    named: &[MachineId],
    client: &C,
) -> (DeployOutcome<ExecutionError>, Vec<String>) {
    let plan = crate::deploy::DeployPlan::for_execution_test_with_groups(
        plan.to_vec(),
        test_namespace(),
        groups,
    )
    .with_machine_names(named);
    reported(&plan, client, &CancellationToken::new()).await
}

async fn reported<C: MachineOperations>(
    plan: &crate::deploy::DeployPlan,
    client: &C,
    cancellation: &CancellationToken,
) -> (DeployOutcome<ExecutionError>, Vec<String>) {
    let (outcome, warnings) = reported_warnings(plan, client, cancellation).await;
    let warnings = warnings.iter().map(ToString::to_string).collect();
    (outcome, warnings)
}

pub(super) async fn reported_warnings<C: MachineOperations>(
    plan: &crate::deploy::DeployPlan,
    client: &C,
    cancellation: &CancellationToken,
) -> (DeployOutcome<ExecutionError>, Vec<DeployWarning>) {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let outcome =
        super::super::execute_operation_sequence(plan, &run_id(), client, cancellation, Some(tx))
            .await;
    let mut warnings = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let DeployEvent::Warning { warning } = event {
            warnings.push(warning);
        }
    }
    (outcome, warnings)
}

pub(super) fn left_running(machine: &str, reason: &str) -> String {
    format!(
        "{} left running on {machine} ({reason}). The next deploy cleans it up.",
        api()
    )
}

fn mixed_after_return(machine: &str) -> String {
    format!(
        "If {machine} restarts and confirms the new copies within a few minutes of starting, it takes its replaced copies out of traffic. Otherwise they keep answering beside the new copies. Either way they keep running, and a left worker still takes jobs, until the next deploy."
    )
}

#[tokio::test(start_paused = true)]
async fn a_new_copy_no_proxy_confirms_names_the_machines_that_did_not_confirm() {
    let (machine, lagging) = (machine('1'), machine('2'));
    let new = container('a');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let client = Scripted {
        acks: BTreeMap::from([(lagging, Ack::Never)]),
        ..Scripted::new(vec![created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &new,
        )])
    };

    let (outcome, _) = execute_reported(
        &plan,
        &[machine, lagging],
        &client,
        &CancellationToken::new(),
    )
    .await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Machine {
                        action: MachineAction::ReadRoutes,
                        error,
                    },
                    ..
                },
                ..
            } if error.message == "proxy on machine-2 did not confirm"
        ),
        "{outcome:?}"
    );
    client.assert_done();
}

/// An Up Server the entry couldn't dial, such as one whose daemon is stopped, keeps
/// its proxy serving its last routes, and nothing there writes its ack (#3663).
#[tokio::test(start_paused = true)]
async fn an_undialed_servers_proxy_does_not_hold_the_switch_and_its_routes_keep_the_old_copy() {
    let (machine, undialed) = (machine('1'), machine('2'));
    let (old, new) = (container('a'), container('b'));
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StartFirst)];
    let client = Scripted {
        acks: BTreeMap::from([(undialed, Ack::Never)]),
        undialed: BTreeMap::from([(undialed, "machine-2")]),
        ..Scripted::new(vec![
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            observed(Call::Inspect(machine, old), running()),
        ])
    }
    .with_copy(&machine, &old, CopyRotation::In);
    let began = tokio::time::Instant::now();

    let (outcome, warnings) = execute_reported(
        &plan,
        &[machine, undialed],
        &client,
        &CancellationToken::new(),
    )
    .await;

    assert!(
        began.elapsed() < cleanup::ACK_WAIT,
        "the switch ends once every other proxy confirms"
    );
    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        outcome.services(),
        ended(&[("api", ServiceResult::Unconfirmed)])
    );
    assert_eq!(
        client.moves(),
        [Move::To(new, CopyRotation::In)],
        "the old copy machine-2's proxy may still route stays In"
    );
    assert_eq!(
        warnings,
        [
            "Could not fully observe containers on Server machine-2: tcp connect error".to_owned(),
            left_running("machine-1", "proxy on machine-2 did not confirm"),
            mixed_after_return("machine-2"),
        ]
    );
    client.assert_done();
}

/// A scale-down's surplus copy is one the undialed Server's proxy may still route.
#[tokio::test(start_paused = true)]
async fn an_undialed_servers_proxy_keeps_a_surplus_copy_the_rollout_would_remove() {
    let (machine, undialed) = (machine('1'), machine('2'));
    let (replaced, surplus, new) = (container('a'), container('b'), container('c'));
    let plan = vec![
        global_replacement(&machine, &replaced, UpdateOrder::StartFirst),
        DeployOperation::RemoveContainer {
            machine_id: machine,
            container_id: surplus,
            purpose: ployz_core::RemoveContainerPurpose::Rollout,
        },
    ];
    let client = Scripted {
        acks: BTreeMap::from([(undialed, Ack::Never)]),
        undialed: BTreeMap::from([(undialed, "machine-2")]),
        ..Scripted::new(vec![
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            observed(Call::Inspect(machine, replaced), running()),
            observed(Call::Inspect(machine, surplus), running()),
        ])
    }
    .with_copy(&machine, &replaced, CopyRotation::In)
    .with_copy(&machine, &surplus, CopyRotation::In);
    let groups = crate::deploy::planning::test_groups(&plan);
    let began = tokio::time::Instant::now();

    let (outcome, warnings) =
        execute_groups_reported(&plan, groups, &[machine, undialed], &client).await;

    assert!(began.elapsed() < cleanup::ACK_WAIT);
    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.moves(), [Move::To(new, CopyRotation::In)]);
    assert_eq!(
        warnings
            .iter()
            .filter(|warning| warning.contains("left running on machine-1"))
            .count(),
        2,
        "{warnings:?}"
    );
    client.assert_done();
}

/// A copy an earlier deploy sent Out may still be on the undialed Server's stale
/// proxy, which never drops it.
#[tokio::test(start_paused = true)]
async fn an_undialed_servers_proxy_keeps_a_surplus_copy_already_out() {
    let (machine, undialed) = (machine('1'), machine('2'));
    let (replaced, surplus, new) = (container('a'), container('b'), container('c'));
    let plan = vec![
        global_replacement(&machine, &replaced, UpdateOrder::StartFirst),
        DeployOperation::RemoveContainer {
            machine_id: machine,
            container_id: surplus,
            purpose: ployz_core::RemoveContainerPurpose::Rollout,
        },
    ];
    let client = Scripted {
        acks: BTreeMap::from([(undialed, Ack::Never)]),
        undialed: BTreeMap::from([(undialed, "machine-2")]),
        ..Scripted::new(vec![
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            observed(Call::Inspect(machine, replaced), running()),
            observed(Call::Inspect(machine, surplus), running()),
        ])
    }
    .with_copy(&machine, &replaced, CopyRotation::In)
    .with_copy(&machine, &surplus, CopyRotation::Out);
    let groups = crate::deploy::planning::test_groups(&plan);
    let began = tokio::time::Instant::now();

    let (outcome, _) = execute_groups_reported(&plan, groups, &[machine, undialed], &client).await;

    assert!(began.elapsed() < cleanup::ACK_WAIT);
    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.moves(), [Move::To(new, CopyRotation::In)]);
    client.assert_done();
}

/// The copy an earlier gapped deploy left beside its newer one (#3676). This deploy
/// keeps the Service as it is and only removes the surplus, which the undialed
/// Server's proxy still routes in place of the newer copy it never acked.
fn left_by_a_gapped_deploy(gap: Scripted, machine: &MachineId) -> Scripted {
    let (left, newer) = (container('a'), container('b'));
    let client = gap
        .with_copy(machine, &left, CopyRotation::In)
        .with_copy_at(&api(), machine, &newer, CopyRotation::In, Gen::new(2));
    client.record(newer, |copy| copy.in_before = false);
    client
}

fn the_left_copys_removal(machine: &MachineId) -> Vec<DeployOperation> {
    vec![DeployOperation::RemoveContainer {
        machine_id: *machine,
        container_id: container('a'),
        purpose: ployz_core::RemoveContainerPurpose::Rollout,
    }]
}

#[tokio::test(start_paused = true)]
async fn an_undialed_servers_proxy_keeps_a_copy_an_earlier_gapped_deploy_left() {
    let (machine, undialed) = (machine('1'), machine('2'));
    let plan = the_left_copys_removal(&machine);
    let client = left_by_a_gapped_deploy(
        Scripted {
            acks: BTreeMap::from([(undialed, Ack::Never)]),
            undialed: BTreeMap::from([(undialed, "machine-2")]),
            ..Scripted::new(vec![observed(
                Call::Inspect(machine, container('a')),
                running(),
            )])
        },
        &machine,
    );
    let began = tokio::time::Instant::now();

    let (outcome, warnings) =
        execute_groups_reported(&plan, scale_down_groups(), &[machine, undialed], &client).await;

    assert!(began.elapsed() < cleanup::ACK_WAIT);
    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        client.moves(),
        [],
        "the copy machine-2's proxy routes stays In"
    );
    assert_eq!(
        warnings,
        [
            "Could not fully observe containers on Server machine-2: tcp connect error".to_owned(),
            left_running("machine-1", "proxy on machine-2 did not confirm"),
            mixed_after_return("machine-2"),
        ]
    );
    client.assert_done();
}

/// A copy every proxy already dropped, the undialed Server's included, is removed.
#[tokio::test(start_paused = true)]
async fn an_undialed_servers_proxy_that_already_dropped_a_copy_lets_it_go() {
    let (machine, undialed) = (machine('1'), machine('2'));
    let (kept, dropped) = (container('a'), container('b'));
    let plan = vec![DeployOperation::RemoveContainer {
        machine_id: machine,
        container_id: dropped,
        purpose: ployz_core::RemoveContainerPurpose::Rollout,
    }];
    let client = Scripted {
        acks: BTreeMap::from([(undialed, Ack::Never)]),
        undialed: BTreeMap::from([(undialed, "machine-2")]),
        ..Scripted::new(vec![
            observed(Call::Inspect(machine, dropped), running()),
            ok(Call::Remove(machine, dropped)),
        ])
    }
    .with_copy(&machine, &kept, CopyRotation::In)
    .with_copy(&machine, &dropped, CopyRotation::Out);

    let (outcome, _) =
        execute_groups_reported(&plan, scale_down_groups(), &[machine, undialed], &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.moves(), [Move::Gone(dropped)]);
    client.assert_done();
}

/// The owner's route read reports the undialed Server Down, so its proxy no longer
/// counts and the left copy goes.
#[tokio::test(start_paused = true)]
async fn an_undialed_server_the_owner_reads_down_does_not_keep_a_left_copy() {
    let (machine, undialed) = (machine('1'), machine('2'));
    let plan = the_left_copys_removal(&machine);
    let client = left_by_a_gapped_deploy(
        Scripted {
            undialed: BTreeMap::from([(undialed, "machine-2")]),
            down: BTreeSet::from([undialed]),
            ..Scripted::new(vec![
                observed(Call::Inspect(machine, container('a')), running()),
                ok(Call::Remove(machine, container('a'))),
            ])
        },
        &machine,
    );

    let (outcome, _) =
        execute_groups_reported(&plan, scale_down_groups(), &[machine, undialed], &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        client.moves(),
        [
            Move::To(container('a'), CopyRotation::Out),
            Move::Gone(container('a'))
        ]
    );
    client.assert_done();
}

/// A Suspect Server's proxy still turns Down or acks, so the removal goes on as before.
#[tokio::test(start_paused = true)]
async fn a_suspect_servers_proxy_still_refuses_a_copy_an_earlier_deploy_left() {
    let (machine, suspect) = (machine('1'), machine('2'));
    let plan = the_left_copys_removal(&machine);
    let client = left_by_a_gapped_deploy(
        Scripted {
            acks: BTreeMap::from([(suspect, Ack::Never)]),
            suspect: BTreeMap::from([(suspect, "machine-2")]),
            ..Scripted::new(vec![observed(
                Call::Inspect(machine, container('a')),
                running(),
            )])
        },
        &machine,
    );

    let (outcome, _) =
        execute_groups_reported(&plan, scale_down_groups(), &[machine, suspect], &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.rotation(&container('a')), Some(CopyRotation::In));
    client.assert_done();
}

/// A Suspect Server's proxy still turns Down or acks, so the switch waits for it as
/// before.
#[tokio::test(start_paused = true)]
async fn a_suspect_servers_proxy_still_holds_the_switch() {
    let (machine, suspect) = (machine('1'), machine('2'));
    let (old, new) = (container('a'), container('b'));
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StartFirst)];
    let client = Scripted {
        acks: BTreeMap::from([(suspect, Ack::Never)]),
        suspect: BTreeMap::from([(suspect, "machine-2")]),
        ..Scripted::new(vec![created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &new,
        )])
    }
    .with_copy(&machine, &old, CopyRotation::In);

    let (outcome, _) = execute_reported(
        &plan,
        &[machine, suspect],
        &client,
        &CancellationToken::new(),
    )
    .await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.moves(), [Move::To(new, CopyRotation::In)]);
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn an_undialed_server_whose_proxy_confirms_with_the_others_retires_the_old_copy() {
    let (machine, undialed) = (machine('1'), machine('2'));
    let (old, new) = (container('a'), container('b'));
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StartFirst)];
    let client = Scripted {
        acks: BTreeMap::from([(machine, Ack::After(3)), (undialed, Ack::After(1))]),
        undialed: BTreeMap::from([(undialed, "machine-2")]),
        ..Scripted::new(vec![
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Remove(machine, old)),
        ])
    }
    .with_copy(&machine, &old, CopyRotation::In);

    let (outcome, _) = execute_reported(
        &plan,
        &[machine, undialed],
        &client,
        &CancellationToken::new(),
    )
    .await;

    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::New)]));
    assert_eq!(
        client.moves(),
        [
            Move::To(new, CopyRotation::In),
            Move::To(old, CopyRotation::Out),
            Move::Gone(old)
        ]
    );
    client.assert_done();
}

/// A proxy the plan's listing found behind a path dropping large packets is named with
/// that cause when it never confirms (#3521).
#[tokio::test(start_paused = true)]
async fn a_proxy_that_never_confirms_over_a_dropping_path_is_named_with_the_cause() {
    let (machine, dropping) = (machine('1'), machine('2'));
    let new = container('a');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let client = Scripted {
        acks: BTreeMap::from([(dropping, Ack::Never)]),
        ..Scripted::new(vec![created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &new,
        )])
    };
    let plan = crate::deploy::DeployPlan::for_execution_test(plan, test_namespace())
        .with_machine_names(&[machine, dropping])
        .with_dropped_large_packets(&[dropping]);

    let (outcome, _) = reported(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Machine {
                        action: MachineAction::ReadRoutes,
                        error,
                    },
                    ..
                },
                ..
            } if error.message == "proxy on machine-2 did not confirm; large packets to machine-2 \
                are dropped on its mesh path from the Server this command entered by"
        ),
        "{outcome:?}"
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_new_copy_an_incomplete_route_set_never_confirms_says_the_set_was_incomplete() {
    let machine = machine('1');
    let new = container('a');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let client = Scripted {
        incomplete: true,
        ..Scripted::new(vec![created(
            Call::Run(machine, ContainerKind::ServiceContainer),
            &new,
        )])
    };

    let (outcome, _) =
        execute_reported(&plan, &[machine], &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Machine {
                        action: MachineAction::ReadRoutes,
                        error,
                    },
                    ..
                },
                ..
            } if error.message == "the route set did not name every live Machine"
        ),
        "{outcome:?}"
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_retirement_no_proxy_confirms_leaves_the_old_copy_running() {
    let machine = machine('1');
    let (old, new) = (container('a'), container('b'));
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StartFirst)];
    let client = Scripted {
        withheld: BTreeMap::from([(api(), usize::MAX)]),
        ..Scripted::new(vec![
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            observed(Call::Inspect(machine, old), running()),
        ])
    }
    .with_copy(&machine, &old, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[machine], &client, &CancellationToken::new()).await;

    assert!(
        !matches!(outcome, DeployOutcome::Success { .. }),
        "an unconfirmed Out is no success: {outcome:?}"
    );
    assert_eq!(
        client.moves(),
        [
            Move::To(new, CopyRotation::In),
            Move::To(old, CopyRotation::Out)
        ],
        "a copy a proxy still lists is never removed"
    );
    assert_eq!(
        warnings,
        [left_running(
            "machine-1",
            "proxy on machine-1 did not confirm"
        )]
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn only_a_machine_whose_proxy_row_never_arrived_is_named_as_serving_on_return() {
    let (owner, away) = (machine('1'), machine('2'));
    let old = container('a');
    let plan = vec![DeployOperation::RemoveContainer {
        machine_id: owner,
        container_id: old,
        purpose: ployz_core::RemoveContainerPurpose::Retire,
    }];
    let client = Scripted {
        acks: BTreeMap::from([(owner, Ack::Never), (away, Ack::Missing)]),
        ..Scripted::new(vec![observed(Call::Inspect(owner, old), running())])
    }
    .with_copy(&owner, &old, CopyRotation::In);

    let (_, warnings) =
        execute_reported(&plan, &[owner, away], &client, &CancellationToken::new()).await;

    assert_eq!(
        warnings,
        [
            left_running("machine-1", "proxy on machine-1, machine-2 did not confirm"),
            mixed_after_return("machine-2"),
        ],
        "machine-1's proxy answered, so it is not away"
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn an_owner_powered_off_during_cleanup_is_named_with_the_copy_left_on_it() {
    let machine = machine('1');
    let (old, new) = (container('a'), container('b'));
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StartFirst)];
    let mut steps = vec![created(
        Call::Run(machine, ContainerKind::ServiceContainer),
        &new,
    )];
    steps.extend(
        std::iter::repeat_with(|| {
            failed_unanswered(Call::Inspect(machine, old), "no route to the machine")
        })
        .take(1000),
    );
    let client = Scripted::new(steps).with_copy(&machine, &old, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[machine], &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.rotation(&old), Some(CopyRotation::In));
    assert_eq!(client.moves(), [Move::To(new, CopyRotation::In)]);
    assert_eq!(
        warnings,
        [
            left_running("machine-1", "machine-1 did not answer"),
            mixed_after_return("machine-1"),
        ]
    );
}

#[tokio::test]
async fn a_retirement_out_refused_for_a_moved_copy_leaves_it_where_it_moved() {
    let machine = machine('1');
    let (old, new) = (container('a'), container('b'));
    let rival = DeploymentLogId::parse("rival").unwrap();
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StartFirst)];
    let client = Scripted {
        moved: Mutex::new(BTreeMap::from([(
            (old, Ask::Out),
            CopyRotation::Held(rival.clone()),
        )])),
        ..Scripted::new(vec![
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            observed(Call::Inspect(machine, old), running()),
        ])
    }
    .with_copy(&machine, &old, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[machine], &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.rotation(&old), Some(CopyRotation::Held(rival)));
    assert_eq!(client.moves(), [Move::To(new, CopyRotation::In)]);
    assert_eq!(
        warnings,
        [left_running(
            "machine-1",
            "the copy moved since it was read"
        )],
        "a RotationMismatch is no Machine being away"
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_cancel_mid_cleanup_reports_each_copy_left_running() {
    let (first, second) = (machine('1'), machine('2'));
    let (one, two) = (container('a'), container('b'));
    let (new_one, new_two) = (container('c'), container('d'));
    let plan = vec![
        global_replacement(&first, &one, UpdateOrder::StartFirst),
        global_replacement(&second, &two, UpdateOrder::StartFirst),
    ];
    let cancellation = CancellationToken::new();
    let client = Scripted {
        withheld: BTreeMap::from([(api(), usize::MAX)]),
        cancel_on_move: Some((Move::To(one, CopyRotation::Out), cancellation.clone())),
        ..Scripted::new(vec![
            created(Call::Run(first, ContainerKind::ServiceContainer), &new_one),
            created(Call::Run(second, ContainerKind::ServiceContainer), &new_two),
            observed(Call::Inspect(first, one), running()),
            observed(Call::Inspect(second, two), running()),
        ])
    }
    .with_copy(&first, &one, CopyRotation::In)
    .with_copy(&second, &two, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[first, second], &client, &cancellation).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "the new copies serve; the cut-short cleanup is reported per copy: {outcome:?}"
    );
    assert_eq!(
        warnings,
        [
            left_running("machine-1", "interrupted"),
            left_running("machine-2", "interrupted"),
        ]
    );
    assert_eq!(client.rotation(&one), Some(CopyRotation::Out));
    assert_eq!(client.rotation(&two), Some(CopyRotation::Out));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_cancel_during_a_stop_first_hold_brings_the_old_copy_back_in() {
    let machine = machine('1');
    let old = container('a');
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let cancellation = CancellationToken::new();
    let client = Scripted {
        withheld: BTreeMap::from([(api(), usize::MAX)]),
        cancel_on_move: Some((
            Move::To(old, CopyRotation::Held(run_id())),
            cancellation.clone(),
        )),
        ..Scripted::new(vec![observed(Call::Inspect(machine, old), running())])
    }
    .with_copy(&machine, &old, CopyRotation::In);

    let (outcome, warnings) = execute_reported(&plan, &[machine], &client, &cancellation).await;

    assert_eq!(
        serde_json::to_value(outcome.services()).unwrap(),
        serde_json::json!([{"service": "api", "result": "old"}])
    );
    assert_eq!(
        client.rotation(&old),
        Some(CopyRotation::In),
        "the copy this run held serves again"
    );
    assert_eq!(
        client.moves(),
        [
            Move::To(old, CopyRotation::Held(run_id())),
            Move::To(old, CopyRotation::In),
        ]
    );
    assert_eq!(warnings, [left_running("machine-1", "interrupted")]);
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn an_unconfirmed_stop_first_hold_brings_the_old_copy_back_in() {
    let machine = machine('1');
    let old = container('a');
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted {
        withheld: BTreeMap::from([(api(), usize::MAX)]),
        ..Scripted::new(vec![observed(Call::Inspect(machine, old), running())])
    }
    .with_copy(&machine, &old, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[machine], &client, &CancellationToken::new()).await;

    assert_eq!(
        serde_json::to_value(outcome.services()).unwrap(),
        serde_json::json!([{"service": "api", "result": "old"}])
    );
    assert_eq!(client.rotation(&old), Some(CopyRotation::In));
    assert_eq!(
        warnings,
        [left_running(
            "machine-1",
            "proxy on machine-1 did not confirm"
        )]
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_refused_restore_after_an_interrupted_hold_is_reported() {
    let machine = machine('1');
    let old = container('a');
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let cancellation = CancellationToken::new();
    let client = Scripted {
        withheld: BTreeMap::from([(api(), usize::MAX)]),
        cancel_on_move: Some((
            Move::To(old, CopyRotation::Held(run_id())),
            cancellation.clone(),
        )),
        moved: Mutex::new(BTreeMap::from([((old, Ask::In), CopyRotation::Out)])),
        ..Scripted::new(vec![observed(Call::Inspect(machine, old), running())])
    }
    .with_copy(&machine, &old, CopyRotation::In);

    let (_, warnings) = execute_reported(&plan, &[machine], &client, &cancellation).await;

    assert_eq!(client.rotation(&old), Some(CopyRotation::Out));
    assert_eq!(
        warnings,
        [left_running(
            "machine-1",
            "interrupted, and it could not go back In: the copy moved since it was read"
        )]
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_groups_outs_go_together_and_its_gones_follow_one_unlisting() {
    let (first, second, proxy) = (machine('1'), machine('2'), machine('3'));
    let (one, two) = (container('a'), container('b'));
    let (new_one, new_two) = (container('c'), container('d'));
    let plan = vec![
        global_replacement(&first, &one, UpdateOrder::StartFirst),
        global_replacement(&second, &two, UpdateOrder::StartFirst),
    ];
    let client = Scripted {
        acks: BTreeMap::from([(proxy, Ack::After(2))]),
        withheld: BTreeMap::from([(api(), 2)]),
        ..Scripted::new(vec![
            created(Call::Run(first, ContainerKind::ServiceContainer), &new_one),
            created(Call::Run(second, ContainerKind::ServiceContainer), &new_two),
            observed(Call::Inspect(first, one), running()),
            observed(Call::Inspect(second, two), running()),
            ok(Call::Remove(first, one)),
            ok(Call::Remove(second, two)),
        ])
    }
    .with_copy(&first, &one, CopyRotation::In)
    .with_copy(&second, &two, CopyRotation::In);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        client.moves(),
        [
            Move::To(new_one, CopyRotation::In),
            Move::To(new_two, CopyRotation::In),
            Move::To(one, CopyRotation::Out),
            Move::To(two, CopyRotation::Out),
            Move::Gone(one),
            Move::Gone(two),
        ]
    );
    let every = |applied, listed: &dyn Fn(&BTreeSet<ContainerId>) -> bool| {
        client
            .routes_at(&applied)
            .values()
            .all(|route| route.as_ref().is_some_and(listed))
    };
    assert!(
        every(Move::To(one, CopyRotation::Out), &|route| route
            .contains(&new_one)
            && route.contains(&new_two)),
        "old copies retire only once every proxy lists every new copy"
    );
    assert!(
        every(Move::Gone(one), &|route| !route.contains(&one)
            && !route.contains(&two)),
        "the group's copies are removed only once no proxy lists any of them"
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_slow_retirement_does_not_hold_up_another_services_flip() {
    let machine = machine('1');
    let (old, new, other) = (container('a'), container('b'), container('c'));
    let plan = vec![
        global_replacement(&machine, &old, UpdateOrder::StartFirst),
        web_run(&machine),
    ];
    let client = Scripted {
        withheld: BTreeMap::from([(api(), usize::MAX)]),
        ..Scripted::new(vec![
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            created(Call::Run(machine, ContainerKind::ServiceContainer), &other),
            observed(Call::Inspect(machine, old), running()),
        ])
    }
    .with_copy(&machine, &old, CopyRotation::In);
    let group = |service: &str, task| crate::deploy::schedule::ServiceGroup {
        service: Some(ServiceName::parse(service).unwrap()),
        dependencies: Vec::new(),
        steps: vec![crate::deploy::schedule::Step::Serial(task)],
        publish: true,
    };
    let started = tokio::time::Instant::now();
    let web_in = async {
        while client.rotation(&other) != Some(CopyRotation::In) {
            tokio::time::sleep(ACK_POLL).await;
        }
        started.elapsed()
    };

    let (outcome, web_in) = tokio::join!(
        execute_groups(&plan, vec![group("api", 0), group("web", 1)], &client),
        web_in
    );

    assert_eq!(
        outcome.services(),
        ended(&[
            ("api", ServiceResult::Unconfirmed),
            ("web", ServiceResult::New)
        ]),
        "api's Out was never seen through"
    );
    assert!(
        web_in < ACK_WAIT && started.elapsed() >= ACK_WAIT,
        "web went In at {web_in:?}, while api's retirement waited {:?}",
        started.elapsed()
    );
    assert_eq!(client.rotation(&old), Some(CopyRotation::Out));
    client.assert_done();
}

#[tokio::test]
async fn global_with_no_matching_machine_keeps_last_copy_serving() {
    let machine = machine('1');
    let last = container('a');
    let plan = vec![DeployOperation::RemoveContainer {
        machine_id: machine,
        container_id: last,
        purpose: ployz_core::RemoveContainerPurpose::Rollout,
    }];
    let client = Scripted::new(vec![observed(Call::Inspect(machine, last), running())]).with_copy(
        &machine,
        &last,
        CopyRotation::In,
    );

    let (outcome, warnings) =
        execute_reported(&plan, &[machine], &client, &CancellationToken::new()).await;

    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Operation {
                    error: ExecutionError::Machine {
                        action: MachineAction::RemoveContainer,
                        error,
                    },
                    ..
                },
                ..
            } if error.code == RpcErrorCode::Conflict
        ),
        "{outcome:?}"
    );
    assert!(client.moves().is_empty(), "{:?}", client.moves());
    assert_eq!(client.rotation(&last), Some(CopyRotation::In));
    assert_eq!(
        warnings,
        [left_running(
            "machine-1",
            "no other In copy is acked by every live proxy"
        )]
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_cancelled_stop_first_stops_the_new_copy_before_the_old_copy_returns() {
    let machine = machine('1');
    let (old, new) = (container('a'), container('b'));
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let cancellation = CancellationToken::new();
    let client = Scripted {
        read_latency: true,
        cancel_on_move: Some((Move::To(new, CopyRotation::In), cancellation.clone())),
        ..Scripted::new(vec![
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Stop(machine, old)),
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            ok(Call::Stop(machine, new)),
            ok(Call::Restart(machine, old)),
            ok(Call::Remove(machine, new)),
        ])
    }
    .with_copy(&machine, &old, CopyRotation::In);

    let (outcome, _) = execute_reported(&plan, &[machine], &client, &cancellation).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        client.moves(),
        [
            Move::To(old, CopyRotation::Held(run_id())),
            Move::To(new, CopyRotation::In),
            Move::To(new, CopyRotation::Held(run_id())),
            Move::To(old, CopyRotation::In),
            Move::To(new, CopyRotation::Out),
            Move::Gone(new),
        ],
        "the new copy stops before the old copy runs again, so one copy writes the Volume"
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_restored_old_copy_whose_listing_is_unconfirmed_keeps_the_new_copy_stopped() {
    let machine = machine('1');
    let (old, new) = (container('a'), container('b'));
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let cancellation = CancellationToken::new();
    let client = Scripted {
        read_latency: true,
        cancel_on_move: Some((Move::To(new, CopyRotation::In), cancellation.clone())),
        reads_fail_after: Some(Move::To(old, CopyRotation::In)),
        ..Scripted::new(vec![
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Stop(machine, old)),
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            ok(Call::Stop(machine, new)),
            ok(Call::Restart(machine, old)),
        ])
    }
    .with_copy(&machine, &old, CopyRotation::In);

    let (outcome, warnings) = execute_reported(&plan, &[machine], &client, &cancellation).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        client.moves(),
        [
            Move::To(old, CopyRotation::Held(run_id())),
            Move::To(new, CopyRotation::In),
            Move::To(new, CopyRotation::Held(run_id())),
            Move::To(old, CopyRotation::In),
        ],
        "the old copy may be writing the Volume, so the new copy never starts again"
    );
    assert_eq!(client.rotation(&new), Some(CopyRotation::Held(run_id())));
    assert_eq!(
        warnings,
        [
            left_running(
                "machine-1",
                "the Deploy failed, and it could not go back In: read routes failed: the route read failed"
            ),
            left_running(
                "machine-1",
                "the Deploy failed, and it could not go back In: its old copy may be serving"
            ),
        ]
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn an_unconfirmed_stop_first_publish_keeps_the_old_copy_stopped() {
    let (machine, lagging) = (machine('1'), machine('2'));
    let (old, new) = (container('a'), container('b'));
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted {
        acks: BTreeMap::from([(lagging, Ack::Never)]),
        ..Scripted::new(vec![
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Stop(machine, old)),
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
        ])
    }
    .with_copy(&machine, &old, CopyRotation::In);
    // The lagging proxy never listed the old copy either, so only the new
    // copy's listing waits on it.
    client.record(old, |copy| copy.in_before = false);

    let (outcome, warnings) =
        execute_reported(&plan, &[machine], &client, &CancellationToken::new()).await;

    assert_eq!(
        serde_json::to_value(outcome.services()).unwrap(),
        serde_json::json!([{"service": "api", "result": "unconfirmed"}])
    );
    assert_eq!(client.rotation(&new), Some(CopyRotation::In));
    assert_eq!(
        client.rotation(&old),
        Some(CopyRotation::Held(run_id())),
        "the new copy may serve, so the old copy must not run beside it"
    );
    assert_eq!(
        warnings,
        [left_running(
            "machine-1",
            "the Deploy failed, and it could not go back In: its new copy may be serving"
        )]
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_stop_failure_after_held_brings_the_old_copy_back_in() {
    let machine = machine('1');
    let old = container('a');
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine, old), running()),
        failed(Call::Stop(machine, old), "the docker daemon timed out"),
    ])
    .with_copy(&machine, &old, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[machine], &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.rotation(&old), Some(CopyRotation::In));
    assert_eq!(
        client.moves(),
        [
            Move::To(old, CopyRotation::Held(run_id())),
            Move::To(old, CopyRotation::In),
        ]
    );
    assert_eq!(
        warnings,
        [left_running("machine-1", "the docker daemon timed out")]
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn an_unanswered_held_ask_that_landed_goes_on_with_the_stop() {
    let machine = machine('1');
    let (old, new) = (container('a'), container('b'));
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted {
        unanswered_held: BTreeSet::from([old]),
        ..Scripted::new(vec![
            observed(Call::Inspect(machine, old), running()),
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Stop(machine, old)),
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            observed(Call::Inspect(machine, old), exited(0)),
            ok(Call::Remove(machine, old)),
        ])
    }
    .with_copy(&machine, &old, CopyRotation::In);

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::New)]));
    assert_eq!(client.rotation(&new), Some(CopyRotation::In));
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_route_read_error_after_held_brings_the_old_copy_back_in() {
    let machine = machine('1');
    let old = container('a');
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted {
        failed_route_reads: Mutex::new(1),
        ..Scripted::new(vec![observed(Call::Inspect(machine, old), running())])
    }
    .with_copy(&machine, &old, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[machine], &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.rotation(&old), Some(CopyRotation::In));
    assert_eq!(
        warnings,
        [left_running("machine-1", "the route query failed")]
    );
    client.assert_done();
}

#[tokio::test]
async fn a_failed_restore_after_publication_is_reported() {
    let machine = machine('1');
    let (old, new) = (container('a'), container('b'));
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let client = Scripted {
        unhealthy_in: BTreeMap::from([(
            new,
            HealthVerdict::Failed {
                observation: unhealthy(),
                last_check: None,
                oom_killed: false,
                departing: None,
            },
        )]),
        ..Scripted::new(vec![
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Stop(machine, old)),
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            ok(Call::Discard(machine, new)),
            failed(
                Call::Restart(machine, old),
                "the old copy could not be restored",
            ),
        ])
    }
    .with_copy(&machine, &old, CopyRotation::In);

    let (_, warnings) =
        execute_reported(&plan, &[machine], &client, &CancellationToken::new()).await;

    assert_eq!(client.rotation(&old), Some(CopyRotation::Held(run_id())));
    assert_eq!(client.rotation(&new), None);
    let [warning] = warnings.as_slice() else {
        panic!("one warning: {warnings:?}");
    };
    assert!(
        warning.contains("could not go back In")
            && warning.contains("the old copy could not be restored"),
        "{warning}"
    );
    client.assert_done();
}

#[tokio::test]
async fn a_failed_group_restores_a_copy_held_to_free_host_ports() {
    let machine = machine('1');
    let (old, sibling, new) = (container('a'), container('e'), container('b'));
    let plan = vec![
        global_replacement(&machine, &old, UpdateOrder::StartFirst),
        DeployOperation::StopContainer {
            machine_id: machine,
            container_id: sibling,
            purpose: ployz_core::StopContainerPurpose::FreeHostPorts,
        },
    ];
    let client = Scripted {
        unhealthy_in: BTreeMap::from([(
            new,
            HealthVerdict::Failed {
                observation: unhealthy(),
                last_check: None,
                oom_killed: false,
                departing: None,
            },
        )]),
        ..Scripted::new(vec![
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            observed(Call::Inspect(machine, sibling), running()),
            ok(Call::Stop(machine, sibling)),
            ok(Call::Discard(machine, new)),
            ok(Call::Restart(machine, sibling)),
        ])
    }
    .with_copy(&machine, &old, CopyRotation::In)
    .with_copy(&machine, &sibling, CopyRotation::In);

    let outcome = execute_groups(&plan, crate::deploy::planning::test_groups(&plan), &client).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.rotation(&sibling), Some(CopyRotation::In));
    client.assert_done();
}

#[tokio::test]
async fn new_copies_carry_the_run_label_the_dependency_wait_selects_by() {
    let machine = machine('1');
    let new = container('a');
    let plan = vec![run(&machine, spec(None, None, None), true)];
    let client = Scripted::new(vec![created(
        Call::Run(machine, ContainerKind::ServiceContainer),
        &new,
    )]);
    execute_with(&plan, &client, &CancellationToken::new()).await;
    client.assert_done();
    assert_eq!(*client.deploys.lock().unwrap(), vec![Some(run_id())]);
}

#[tokio::test(start_paused = true)]
async fn an_unconfirmed_unlisting_leaves_every_old_copy_of_the_group_running() {
    let (first, second) = (machine('1'), machine('2'));
    let (one, two) = (container('a'), container('b'));
    let (new_one, new_two) = (container('c'), container('d'));
    let plan = vec![
        global_replacement(&first, &one, UpdateOrder::StartFirst),
        global_replacement(&second, &two, UpdateOrder::StartFirst),
    ];
    let client = Scripted {
        withheld: BTreeMap::from([(api(), usize::MAX)]),
        ..Scripted::new(vec![
            created(Call::Run(first, ContainerKind::ServiceContainer), &new_one),
            created(Call::Run(second, ContainerKind::ServiceContainer), &new_two),
            observed(Call::Inspect(first, one), running()),
            observed(Call::Inspect(second, two), running()),
        ])
    }
    .with_copy(&first, &one, CopyRotation::In)
    .with_copy(&second, &two, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[first, second], &client, &CancellationToken::new()).await;

    assert!(
        !matches!(outcome, DeployOutcome::Success { .. }),
        "an unconfirmed Out is no success: {outcome:?}"
    );
    assert_eq!(client.rotation(&one), Some(CopyRotation::Out));
    assert_eq!(
        client.rotation(&two),
        Some(CopyRotation::Out),
        "the group's Outs go together"
    );
    assert_eq!(
        warnings,
        [
            left_running("machine-1", "proxy on machine-1, machine-2 did not confirm"),
            left_running("machine-2", "proxy on machine-1, machine-2 did not confirm"),
        ]
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_failed_route_read_after_the_outs_leaves_every_old_copy_of_the_group_running() {
    let (first, second) = (machine('1'), machine('2'));
    let (one, two) = (container('a'), container('b'));
    let (new_one, new_two) = (container('c'), container('d'));
    let plan = vec![
        global_replacement(&first, &one, UpdateOrder::StartFirst),
        global_replacement(&second, &two, UpdateOrder::StartFirst),
    ];
    let client = Scripted {
        reads_fail_after: Some(Move::To(one, CopyRotation::Out)),
        ..Scripted::new(vec![
            created(Call::Run(first, ContainerKind::ServiceContainer), &new_one),
            created(Call::Run(second, ContainerKind::ServiceContainer), &new_two),
            observed(Call::Inspect(first, one), running()),
            observed(Call::Inspect(second, two), running()),
        ])
    }
    .with_copy(&first, &one, CopyRotation::In)
    .with_copy(&second, &two, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[first, second], &client, &CancellationToken::new()).await;

    assert!(
        !matches!(outcome, DeployOutcome::Success { .. }),
        "an unconfirmed Out is no success: {outcome:?}"
    );
    assert_eq!(client.rotation(&one), Some(CopyRotation::Out));
    assert_eq!(
        client.rotation(&two),
        Some(CopyRotation::Out),
        "no copy is removed while its unlisting could not be read"
    );
    assert_eq!(
        warnings,
        [
            left_running(
                "machine-1",
                "the routes could not be read through the Machine this Deploy connects to: the route read failed",
            ),
            left_running(
                "machine-2",
                "the routes could not be read through the Machine this Deploy connects to: the route read failed",
            ),
        ]
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_cut_off_owner_leaves_only_its_own_old_copy_of_the_group_running() {
    let (first, second) = (machine('1'), machine('2'));
    let (one, two) = (container('a'), container('b'));
    let (new_one, new_two) = (container('c'), container('d'));
    let plan = vec![
        global_replacement(&first, &one, UpdateOrder::StartFirst),
        global_replacement(&second, &two, UpdateOrder::StartFirst),
    ];
    let client = Scripted {
        cut_off_after: Some((Move::To(one, CopyRotation::Out), first)),
        ..Scripted::new(vec![
            created(Call::Run(first, ContainerKind::ServiceContainer), &new_one),
            created(Call::Run(second, ContainerKind::ServiceContainer), &new_two),
            observed(Call::Inspect(first, one), running()),
            observed(Call::Inspect(second, two), running()),
            Step(
                Call::Remove(first, one),
                Reply::Error(RpcError::unanswered_target(
                    RpcErrorCode::Internal,
                    "target Machine RPC timed out".into(),
                )),
            ),
            ok(Call::Remove(second, two)),
        ])
    }
    .with_copy(&first, &one, CopyRotation::In)
    .with_copy(&second, &two, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[first, second], &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "an applied Deploy that left a copy running exits partial: {outcome:?}"
    );
    assert_eq!(
        client.rotation(&two),
        None,
        "the reachable owner's old copy is retired"
    );
    assert_eq!(
        warnings,
        [
            left_running("machine-1", "machine-1 did not answer"),
            mixed_after_return("machine-1"),
        ]
    );
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_brief_unanswered_route_read_does_not_end_the_group_wait() {
    let (first, second) = (machine('1'), machine('2'));
    let (one, two) = (container('a'), container('b'));
    let (new_one, new_two) = (container('c'), container('d'));
    let plan = vec![
        global_replacement(&first, &one, UpdateOrder::StartFirst),
        global_replacement(&second, &two, UpdateOrder::StartFirst),
    ];
    let client = Scripted {
        blipped_entry_reads: Mutex::new(3),
        ..Scripted::new(vec![
            created(Call::Run(first, ContainerKind::ServiceContainer), &new_one),
            created(Call::Run(second, ContainerKind::ServiceContainer), &new_two),
            observed(Call::Inspect(first, one), running()),
            observed(Call::Inspect(second, two), running()),
            ok(Call::Remove(first, one)),
            ok(Call::Remove(second, two)),
        ])
    }
    .with_copy(&first, &one, CopyRotation::In)
    .with_copy(&second, &two, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[first, second], &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.rotation(&one), None);
    assert_eq!(client.rotation(&two), None);
    assert!(warnings.is_empty(), "{warnings:?}");
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn an_add_only_repair_on_one_machine_keeps_every_other_copy_where_it_was() {
    let (repaired, other) = (machine('1'), machine('2'));
    let (out, serving, new) = (container('a'), container('b'), container('c'));
    let mut service = spec(None, None, None);
    service.mode = ployz_core::ServiceMode::Global;
    let plan = vec![run(&repaired, service, true)];
    let client = Scripted::new(vec![created(
        Call::Run(repaired, ContainerKind::ServiceContainer),
        &new,
    )])
    .with_copy(&repaired, &out, CopyRotation::Out)
    .with_copy(&other, &serving, CopyRotation::In)
    .routing(&api());

    let outcome = execute_with(&plan, &client, &CancellationToken::new()).await;

    assert_eq!(outcome.services(), ended(&[("api", ServiceResult::New)]));
    assert_eq!(
        client.brought_in(),
        [new],
        "the repair sends In for its own copy only"
    );
    assert_eq!(client.rotation(&out), Some(CopyRotation::Out));
    assert_eq!(client.rotation(&serving), Some(CopyRotation::In));
    let listed = client.listed.lock().unwrap().clone();
    assert!(!listed.is_empty());
    assert!(
        listed
            .iter()
            .flat_map(BTreeMap::values)
            .flatten()
            .all(|route| !route.contains(&out) && route.contains(&serving)),
        "no proxy routes the Out copy or drops the other Machine's copy: {listed:?}"
    );
    for (proxy, route) in client.routes(&api(), false) {
        assert_eq!(route, Some(BTreeSet::from([serving, new])), "{proxy}");
    }
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn only_the_machine_that_did_not_answer_is_named_away() {
    let [first, second, third] = [machine('1'), machine('2'), machine('3')];
    let olds = [container('a'), container('b'), container('e')];
    let news = [container('c'), container('d'), container('f')];
    let [one, two, three] = olds;
    let plan = [first, second, third]
        .iter()
        .zip(&olds)
        .map(|(owner, old)| global_replacement(owner, old, UpdateOrder::StartFirst))
        .collect::<Vec<_>>();
    let mut steps = [first, second, third]
        .iter()
        .zip(&news)
        .map(|(owner, new)| created(Call::Run(*owner, ContainerKind::ServiceContainer), new))
        .collect::<Vec<_>>();
    steps.extend([
        observed(Call::Inspect(first, one), running()),
        observed(Call::Inspect(second, two), running()),
    ]);
    // Machine-3 is cut off, so every inspect of its copy goes unanswered.
    steps.extend(
        std::iter::repeat_with(|| {
            failed_unanswered(Call::Inspect(third, three), "no route to machine-3")
        })
        .take(1000),
    );
    let client = Scripted {
        reads_fail_after: Some(Move::To(one, CopyRotation::Out)),
        ..Scripted::new(steps)
    }
    .with_copy(&first, &one, CopyRotation::In)
    .with_copy(&second, &two, CopyRotation::In)
    .with_copy(&third, &three, CopyRotation::In);

    let (_, warnings) = execute_reported(
        &plan,
        &[first, second, third],
        &client,
        &CancellationToken::new(),
    )
    .await;

    assert_eq!(
        warnings,
        [
            left_running("machine-3", "machine-3 did not answer"),
            mixed_after_return("machine-3"),
            left_running(
                "machine-1",
                "the routes could not be read through the Machine this Deploy connects to: the route read failed",
            ),
            left_running(
                "machine-2",
                "the routes could not be read through the Machine this Deploy connects to: the route read failed",
            ),
        ],
        "the route read failed on no Machine the run can name"
    );
}

#[tokio::test(start_paused = true)]
async fn an_old_copys_owner_sees_the_new_copies_listed_before_its_out() {
    let (first, second) = (machine('1'), machine('2'));
    let (one, two) = (container('a'), container('b'));
    let (new_one, new_two) = (container('c'), container('d'));
    let plan = vec![
        global_replacement(&first, &one, UpdateOrder::StartFirst),
        global_replacement(&second, &two, UpdateOrder::StartFirst),
    ];
    let client = Scripted {
        lagging: Mutex::new(BTreeMap::from([(second, 3)])),
        ..Scripted::new(vec![
            created(Call::Run(first, ContainerKind::ServiceContainer), &new_one),
            created(Call::Run(second, ContainerKind::ServiceContainer), &new_two),
            observed(Call::Inspect(first, one), running()),
            observed(Call::Inspect(second, two), running()),
            ok(Call::Remove(first, one)),
            ok(Call::Remove(second, two)),
        ])
    }
    .with_copy(&first, &one, CopyRotation::In)
    .with_copy(&second, &two, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[first, second], &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert!(warnings.is_empty(), "{warnings:?}");
    assert_eq!(client.rotation(&two), None, "the old copy retired");
    client.assert_done();
}

/// Each ingress repair planned from one snapshot, by the Server it creates on:
/// edge-1 and edge-2 are empty and edge-3 runs proxy `c`.
fn ingress_repairs_beside_c() -> Vec<(MachineId, crate::deploy::DeployPlan)> {
    use crate::deploy::planning::ingress_repairs::{ingress_intent, ingress_on, servers};
    use crate::deploy::planning::{SlotAction, plan_ingress_repairs};

    let serving = crate::global_fixtures::machine('3', "edge-3");
    let snapshot = servers(
        &[
            crate::global_fixtures::machine('1', "edge-1"),
            crate::global_fixtures::machine('2', "edge-2"),
            serving.clone(),
        ],
        vec![ingress_on('c', &serving)],
    );
    plan_ingress_repairs(&ingress_intent(), &snapshot)
        .into_iter()
        .map(|repair| match repair.action {
            SlotAction::Create(Ok(plan)) => (repair.machine_id, *plan),
            action @ (SlotAction::Create(Err(_))
            | SlotAction::Start(_)
            | SlotAction::Dead(_)
            | SlotAction::Restarting(_)
            | SlotAction::Unchecked(_)) => {
                panic!("a create on each empty Server, got {action:?}")
            }
        })
        .collect()
}

/// A client whose only scripted call is the repair's create of `new` on
/// `repaired`, beside these Ingress Proxies, each named by its Machine's and its
/// own hex. Every one of them is listed, so a proxy taken away since stays listed.
fn ingress_client(
    repaired: &MachineId,
    new: &ContainerId,
    proxies: &[(char, char, CopyRotation)],
) -> Scripted {
    use crate::deploy::planning::ingress_repairs::ingress_on;

    let listed = proxies
        .iter()
        .map(|(owner, proxy, _)| {
            ingress_on(*proxy, &crate::global_fixtures::machine(*owner, "edge"))
        })
        .collect();
    proxies.iter().fold(
        Scripted {
            observations: Some(listed),
            ..Scripted::new(vec![created(
                Call::Run(*repaired, ContainerKind::ServiceContainer),
                new,
            )])
        },
        |client, (owner, proxy, rotation)| {
            client.with_copy_at(
                &QualifiedService::system_ingress(),
                &machine(*owner),
                &container(*proxy),
                rotation.clone(),
                Gen::FIRST,
            )
        },
    )
}

async fn run_repair(plan: &crate::deploy::DeployPlan, client: &Scripted) {
    let outcome = super::super::execute_operation_sequence(
        plan,
        &run_id(),
        client,
        &CancellationToken::new(),
        None,
    )
    .await;
    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
}

/// The proxy's rotation and whether it is stopped; none once it is gone.
fn proxy_state(client: &Scripted, proxy: &ContainerId) -> Option<(CopyRotation, bool)> {
    client
        .copies
        .lock()
        .unwrap()
        .get(proxy)
        .filter(|copy| copy.present)
        .map(|copy| (copy.rotation.clone(), copy.stopped))
}

/// The run asked none of `proxies` for anything, so each is as it was.
fn assert_untouched(client: &Scripted, proxies: &[(ContainerId, Option<(CopyRotation, bool)>)]) {
    let moves = client.moves();
    for (proxy, before) in proxies {
        assert_eq!(&proxy_state(client, proxy), before, "{proxy}");
        assert!(
            moves
                .iter()
                .all(|applied| !matches!(applied, Move::To(id, _) | Move::Gone(id) if id == proxy)),
            "the repair moved {proxy}: {moves:?}"
        );
    }
}

/// Two ingress repairs planned from one snapshot, the second run after the first
/// finished, as when its image pull finishes later. The second must leave the
/// first's new Ingress Proxy running.
#[tokio::test]
async fn a_late_ingress_repair_keeps_the_proxy_an_earlier_repair_published() {
    let repairs = ingress_repairs_beside_c();
    let [(first, first_plan), (second, second_plan)] = repairs.as_slice() else {
        panic!("two empty Servers to repair, got {repairs:?}");
    };
    let (earlier, late) = (container('a'), container('b'));
    let first_client = ingress_client(first, &earlier, &[('3', 'c', CopyRotation::Pending)]);
    run_repair(first_plan, &first_client).await;
    first_client.assert_done();
    let published = proxy_state(&first_client, &earlier);
    assert_eq!(published, Some((CopyRotation::Pending, false)));

    let second_client = ingress_client(
        second,
        &late,
        &[
            ('1', 'a', CopyRotation::Pending),
            ('3', 'c', CopyRotation::Pending),
        ],
    );
    run_repair(second_plan, &second_client).await;

    assert_untouched(
        &second_client,
        &[
            (earlier, published),
            (container('c'), Some((CopyRotation::Pending, false))),
        ],
    );
    assert_eq!(
        proxy_state(&second_client, &late),
        Some((CopyRotation::Pending, false))
    );
    second_client.assert_done();
}

/// A regular Deploy retired `c` after the repair was planned and serves `d` and
/// `e` instead. The repair only adds its own proxy, so `c` stays retired.
#[tokio::test]
async fn an_ingress_repair_leaves_a_proxy_a_newer_deploy_retired_out() {
    let repairs = ingress_repairs_beside_c();
    let (repaired, plan) = repairs.first().expect("edge-1 is repaired");
    let client = ingress_client(
        repaired,
        &container('a'),
        &[
            ('3', 'c', CopyRotation::Out),
            ('3', 'd', CopyRotation::Pending),
            ('2', 'e', CopyRotation::Pending),
        ],
    );
    client.record(container('c'), |copy| copy.stopped = true);
    let before = ['c', 'd', 'e']
        .map(container)
        .map(|proxy| (proxy, proxy_state(&client, &proxy)));

    run_repair(plan, &client).await;

    assert_untouched(&client, &before);
    assert!(client.brought_in().is_empty(), "{:?}", client.moves());
    client.assert_done();
}

/// A drain or a Server removal took `c` away after the repair was planned, though
/// a stale listing still shows it. The repair must not route or recreate it.
#[tokio::test]
async fn an_ingress_repair_does_not_bring_back_a_proxy_removed_after_it_was_planned() {
    let repairs = ingress_repairs_beside_c();
    let (repaired, plan) = repairs.first().expect("edge-1 is repaired");
    let client = ingress_client(
        repaired,
        &container('a'),
        &[
            ('3', 'c', CopyRotation::Pending),
            ('2', 'd', CopyRotation::Pending),
        ],
    );
    client.record(container('c'), |copy| copy.present = false);
    let before = ['c', 'd']
        .map(container)
        .map(|proxy| (proxy, proxy_state(&client, &proxy)));

    run_repair(plan, &client).await;

    assert_untouched(&client, &before);
    assert!(client.brought_in().is_empty(), "{:?}", client.moves());
    client.assert_done();
}

/// The old copy's restore lands and it runs In, but the Machine answers with an
/// error. It may be writing the Volume, so the new copy must stay stopped.
#[tokio::test(start_paused = true)]
async fn an_answered_error_after_the_old_copy_returns_keeps_the_new_copy_stopped() {
    let machine = machine('1');
    let (old, new) = (container('a'), container('b'));
    let plan = vec![global_replacement(&machine, &old, UpdateOrder::StopFirst)];
    let cancellation = CancellationToken::new();
    let client = Scripted {
        read_latency: true,
        cancel_on_move: Some((Move::To(new, CopyRotation::In), cancellation.clone())),
        answered_error_after_in: BTreeSet::from([old]),
        ..Scripted::new(vec![
            observed(Call::Inspect(machine, old), running()),
            ok(Call::Stop(machine, old)),
            created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
            ok(Call::Stop(machine, new)),
            ok(Call::Restart(machine, old)),
        ])
    }
    .with_copy(&machine, &old, CopyRotation::In);

    let (_, warnings) = execute_reported(&plan, &[machine], &client, &cancellation).await;

    let copies = client.copies.lock().unwrap();
    let (old_copy, new_copy) = (copies.get(&old).unwrap(), copies.get(&new).unwrap());
    assert!(!old_copy.stopped, "the old copy runs again");
    assert!(new_copy.stopped, "the new copy stays stopped: {warnings:?}");
    assert!(
        warnings
            .iter()
            .any(|line| line.contains("its old copy may be serving")),
        "{warnings:?}"
    );
}

/// Machine-3 is cut off, and this client's own deadline ends each call to it: the
/// error is unavailable but carries no unanswered mark from a router. Machine-3 is
/// still the copy's owner that did not answer, so it alone is named away.
#[tokio::test(start_paused = true)]
async fn an_owner_whose_calls_time_out_on_this_client_is_named_away() {
    let [first, second, third] = [machine('1'), machine('2'), machine('3')];
    let olds = [container('a'), container('b'), container('e')];
    let news = [container('c'), container('d'), container('f')];
    let [one, two, three] = olds;
    let plan = [first, second, third]
        .iter()
        .zip(&olds)
        .map(|(owner, old)| global_replacement(owner, old, UpdateOrder::StartFirst))
        .collect::<Vec<_>>();
    let mut steps = [first, second, third]
        .iter()
        .zip(&news)
        .map(|(owner, new)| created(Call::Run(*owner, ContainerKind::ServiceContainer), new))
        .collect::<Vec<_>>();
    steps.extend([
        observed(Call::Inspect(first, one), running()),
        observed(Call::Inspect(second, two), running()),
    ]);
    steps.extend(
        std::iter::repeat_with(|| {
            failed_unavailable(Call::Inspect(third, three), "target Machine RPC timed out")
        })
        .take(1000),
    );
    let client = Scripted {
        reads_fail_after: Some(Move::To(one, CopyRotation::Out)),
        ..Scripted::new(steps)
    }
    .with_copy(&first, &one, CopyRotation::In)
    .with_copy(&second, &two, CopyRotation::In)
    .with_copy(&third, &three, CopyRotation::In);

    let (outcome, warnings) = execute_reported(
        &plan,
        &[first, second, third],
        &client,
        &CancellationToken::new(),
    )
    .await;

    assert_eq!(
        warnings,
        [
            left_running("machine-3", "machine-3 did not answer"),
            mixed_after_return("machine-3"),
            left_running(
                "machine-1",
                "the routes could not be read through the Machine this Deploy connects to: the route read failed",
            ),
            left_running(
                "machine-2",
                "the routes could not be read through the Machine this Deploy connects to: the route read failed",
            ),
        ]
    );
    assert!(
        !matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
}

/// The old copy's owner is cut off after the switch and its calls time out on this
/// client, while the other Machine's proxy reads its routes. The Deploy applies, and
/// names the copy left running and its Machine away.
#[tokio::test(start_paused = true)]
async fn an_applied_deploy_names_the_copy_its_cut_off_owner_kept() {
    let [owner, other] = [machine('1'), machine('2')];
    let (old, new) = (container('a'), container('b'));
    let plan = vec![global_replacement(&owner, &old, UpdateOrder::StartFirst)];
    let mut steps = vec![created(
        Call::Run(owner, ContainerKind::ServiceContainer),
        &new,
    )];
    steps.extend(
        std::iter::repeat_with(|| {
            failed_unavailable(Call::Inspect(owner, old), "target Machine RPC timed out")
        })
        .take(1000),
    );
    let client = Scripted::new(steps).with_copy(&owner, &old, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[owner, other], &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.moves(), [Move::To(new, CopyRotation::In)]);
    assert_eq!(
        warnings,
        [
            left_running("machine-1", "machine-1 did not answer"),
            mixed_after_return("machine-1"),
        ]
    );
}

/// A planned removal whose owner was cut off after the switch: the copy may still
/// be In, so it is named left running and its Machine away, and the Deploy fails.
#[tokio::test(start_paused = true)]
async fn a_removal_whose_owner_does_not_answer_names_the_copy_left_running() {
    let machine = machine('1');
    let kept = container('a');
    let removed = container('b');
    let plan = vec![DeployOperation::RemoveContainer {
        machine_id: machine,
        container_id: removed,
        purpose: ployz_core::RemoveContainerPurpose::Rollout,
    }];
    let client = Scripted::new(
        std::iter::repeat_with(|| {
            failed_unavailable(
                Call::Inspect(machine, removed),
                "target Machine RPC timed out",
            )
        })
        .take(1000)
        .collect(),
    )
    .with_copy(&machine, &kept, CopyRotation::In)
    .with_copy(&machine, &removed, CopyRotation::In);

    let (outcome, warnings) =
        execute_groups_reported(&plan, scale_down_groups(), &[machine], &client).await;

    assert!(
        !matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    assert_eq!(
        warnings,
        [
            left_running("machine-1", "machine-1 did not answer"),
            mixed_after_return("machine-1"),
        ]
    );
    assert!(client.moves().is_empty(), "{:?}", client.moves());
}
