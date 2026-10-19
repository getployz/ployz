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
async fn a_newer_in_copy_reports_superseded_and_removes_the_new_copies() {
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
        DeployOutcome::Superseded {
            completed: Vec::new(),
            service: api(),
            unexecuted: plan,
            services: ended(&[("api", ServiceResult::Old)]),
        }
    );
    assert!(client.brought_in().is_empty());
    assert_eq!(client.rotation(&newer), Some(CopyRotation::In));
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
    let mut groups = crate::deploy::planning::test_groups(&plan);
    groups.first_mut().unwrap().kept = vec![kept];

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

    let outcome = execute_groups(&plan, scale_down_groups(&[kept]), &client).await;

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
    let mut groups = scale_down_groups(&[kept]);
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

fn scale_down_groups(kept: &[ContainerId]) -> Vec<crate::deploy::schedule::ServiceGroup> {
    let service = ServiceName::parse("api").unwrap();
    vec![
        crate::deploy::schedule::ServiceGroup {
            service: Some(service.clone()),
            dependencies: Vec::new(),
            steps: Vec::new(),
            publish: true,
            kept: kept.to_vec(),
        },
        crate::deploy::schedule::ServiceGroup {
            service: None,
            dependencies: vec![service],
            steps: vec![crate::deploy::schedule::Step::Serial(0)],
            publish: false,
            kept: Vec::new(),
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
            kept: Vec::new(),
        },
        ServiceGroup {
            service: None,
            dependencies: vec![api],
            steps: vec![Step::Serial(1), Step::Serial(2)],
            publish: false,
            kept: Vec::new(),
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
    let mut groups = scale_down_groups(&[]);
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
    let mut groups = crate::deploy::planning::test_groups(&plan);
    groups.first_mut().unwrap().kept = vec![kept];

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
            kept: vec![container('d')],
        },
        ServiceGroup {
            service: Some(ServiceName::parse("api").unwrap()),
            dependencies: vec![ServiceName::parse("db").unwrap()],
            steps: vec![Step::Parallel(vec![0])],
            publish: true,
            kept: Vec::new(),
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

/// Run `plan` with each Machine in `named` called `machine-<hex>`, and return
/// the outcome with the warnings as the report prints them.
async fn execute_reported<C: MachineOperations>(
    plan: &[DeployOperation],
    named: &[MachineId],
    client: &C,
    cancellation: &CancellationToken,
) -> (DeployOutcome<ExecutionError>, Vec<String>) {
    let plan = crate::deploy::DeployPlan::for_execution_test(plan.to_vec(), test_namespace())
        .with_machine_names(named);
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
    let outcome =
        super::super::execute_operation_sequence(&plan, &run_id(), client, cancellation, Some(tx))
            .await;
    let mut warnings = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let DeployEvent::Warning { warning } = event {
            warnings.push(warning.to_string());
        }
    }
    (outcome, warnings)
}

fn left_running(machine: &str, reason: &str) -> String {
    format!(
        "{}  left running on {machine} ({reason}). The next deploy cleans it up.",
        api()
    )
}

fn mixed_after_return(machine: &str) -> String {
    format!(
        "Once {machine} is back, a copy left running can answer beside the new copies, and a left worker still takes jobs, until the next deploy."
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

#[tokio::test(start_paused = true)]
async fn a_retirement_no_proxy_confirms_leaves_the_old_copy_running_and_says_it_can_serve_again() {
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
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
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
        [
            left_running("machine-1", "proxy on machine-1 did not confirm"),
            mixed_after_return("machine-1"),
        ]
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
            failed_unavailable(Call::Inspect(machine, old), "no route to the machine")
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
    assert_eq!(client.rotation(&two), Some(CopyRotation::In));
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
        [
            left_running("machine-1", "proxy on machine-1 did not confirm"),
            mixed_after_return("machine-1"),
        ]
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
async fn outs_wait_for_previous_unlisted() {
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
            ok(Call::Remove(first, one)),
            observed(Call::Inspect(second, two), running()),
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
            Move::Gone(one),
            Move::To(two, CopyRotation::Out),
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
        every(Move::Gone(one), &|route| !route.contains(&one)),
        "a copy is removed only once no proxy lists it"
    );
    assert!(
        every(Move::To(two, CopyRotation::Out), &|route| !route
            .contains(&one)),
        "the next Out waits until no proxy lists the previous copy"
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
        kept: Vec::new(),
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

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
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
