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

/// Two ingress repairs planned from one snapshot, where the second admits the pointer
/// only after the first has flipped it, as when its image pull finishes later. Both
/// repairs only add a copy, so the second must keep the first's new Ingress Proxy.
#[tokio::test]
async fn a_late_ingress_repair_keeps_the_proxy_an_earlier_repair_published() {
    use crate::deploy::planning::ingress_repairs::{ingress_intent, ingress_on, servers};
    use crate::deploy::planning::{SlotAction, plan_ingress_repairs};

    let first = crate::global_fixtures::machine('1', "edge-1");
    let second = crate::global_fixtures::machine('2', "edge-2");
    let serving = crate::global_fixtures::machine('3', "edge-3");
    let snapshot = servers(
        &[first.clone(), second.clone(), serving.clone()],
        vec![ingress_on('c', &serving)],
    );
    let plans: Vec<_> = plan_ingress_repairs(&ingress_intent(), &snapshot)
        .into_iter()
        .map(|repair| match repair.action {
            SlotAction::Create(Ok(plan)) => plan,
            action @ (SlotAction::Create(Err(_))
            | SlotAction::Start(_)
            | SlotAction::Dead(_)
            | SlotAction::Restarting(_)
            | SlotAction::Unchecked(_)) => {
                panic!("a create on each empty Server, got {action:?}")
            }
        })
        .collect();
    let [first_plan, second_plan] = plans.as_slice() else {
        panic!("two empty Servers to repair, got {plans:?}");
    };
    let ingress = QualifiedService::system_ingress();
    let earlier = ServingPointer {
        deploy: DeploymentLogId::parse("earlier").unwrap(),
        containers: vec![container('c')],
        previous: None,
        retiring: Vec::new(),
        deciding: None,
    };

    let first_client = Scripted::new(vec![created(
        Call::Run(first.id, ContainerKind::ServiceContainer),
        &container('a'),
    )]);
    first_client
        .pointers
        .lock()
        .unwrap()
        .insert(ingress.clone(), earlier);
    let outcome = super::super::execute_operation_sequence(
        first_plan,
        &run_id(),
        &first_client,
        &CancellationToken::new(),
        None,
    )
    .await;
    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let mut published = first_client
        .pointers
        .lock()
        .unwrap()
        .get(&ingress)
        .cloned()
        .expect("the first repair flipped the pointer");
    published.deploy = DeploymentLogId::parse("first-repair").unwrap();

    let second_client = Scripted::new(vec![created(
        Call::Run(second.id, ContainerKind::ServiceContainer),
        &container('b'),
    )]);
    second_client
        .pointers
        .lock()
        .unwrap()
        .insert(ingress.clone(), published);
    let outcome = super::super::execute_operation_sequence(
        second_plan,
        &run_id(),
        &second_client,
        &CancellationToken::new(),
        None,
    )
    .await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let last = second_client
        .pointers
        .lock()
        .unwrap()
        .get(&ingress)
        .cloned()
        .expect("the second repair flipped the pointer");
    let mut serving_now = last.containers.clone();
    serving_now.sort();
    assert_eq!(
        (serving_now, last.retiring),
        (
            vec![container('a'), container('b'), container('c')],
            Vec::new()
        ),
        "the second repair retired the first repair's Ingress Proxy"
    );
    first_client.assert_done();
    second_client.assert_done();
}

/// Runs the ingress repair planned for edge-1, while edge-1 and edge-2 are empty and
/// edge-3 serves proxy `c`, against `admitted`, the pointer some later Deploy wrote.
async fn repair_against(
    admitted: ServingPointer,
) -> (DeployOutcome<ExecutionError>, ServingPointer) {
    use crate::deploy::planning::ingress_repairs::{ingress_intent, ingress_on, servers};
    use crate::deploy::planning::{SlotAction, plan_ingress_repairs};

    let first = crate::global_fixtures::machine('1', "edge-1");
    let second = crate::global_fixtures::machine('2', "edge-2");
    let serving = crate::global_fixtures::machine('3', "edge-3");
    let snapshot = servers(
        &[first.clone(), second, serving.clone()],
        vec![ingress_on('c', &serving)],
    );
    let repair = plan_ingress_repairs(&ingress_intent(), &snapshot)
        .into_iter()
        .find(|repair| repair.machine_id == first.id)
        .expect("edge-1 is repaired");
    let plan = match repair.action {
        SlotAction::Create(Ok(plan)) => plan,
        action @ (SlotAction::Create(Err(_))
        | SlotAction::Start(_)
        | SlotAction::Dead(_)
        | SlotAction::Restarting(_)
        | SlotAction::Unchecked(_)) => panic!("a create on edge-1, got {action:?}"),
    };
    let ingress = QualifiedService::system_ingress();
    let client = Scripted::new(vec![created(
        Call::Run(first.id, ContainerKind::ServiceContainer),
        &container('a'),
    )]);
    client
        .pointers
        .lock()
        .unwrap()
        .insert(ingress.clone(), admitted);
    let outcome = super::super::execute_operation_sequence(
        &plan,
        &run_id(),
        &client,
        &CancellationToken::new(),
        None,
    )
    .await;
    client.assert_done();
    let last = client
        .pointers
        .lock()
        .unwrap()
        .get(&ingress)
        .cloned()
        .expect("the repair flipped the pointer");
    (outcome, last)
}

/// A regular Deploy retired `c` after the repair was planned. The repair only adds
/// its own proxy, so `c` stays retiring.
#[tokio::test]
async fn an_ingress_repair_leaves_a_proxy_a_newer_deploy_retired_retiring() {
    let (outcome, last) = repair_against(ServingPointer {
        deploy: DeploymentLogId::parse("newer").unwrap(),
        containers: vec![container('d'), container('e')],
        previous: None,
        retiring: vec![container('c')],
        deciding: None,
    })
    .await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let mut serving_now = last.containers.clone();
    serving_now.sort();
    assert_eq!(
        serving_now,
        vec![container('a'), container('d'), container('e')]
    );
    assert!(last.retiring.contains(&container('c')), "{last:?}");
}

/// A drain or a Server removal dropped `c` from the pointer after the repair was
/// planned. The repair must not route it again.
#[tokio::test]
async fn an_ingress_repair_does_not_route_a_proxy_the_pointer_dropped() {
    let (outcome, last) = repair_against(ServingPointer {
        deploy: DeploymentLogId::parse("drained").unwrap(),
        containers: vec![container('d')],
        previous: None,
        retiring: Vec::new(),
        deciding: None,
    })
    .await;

    assert!(
        matches!(outcome, DeployOutcome::Success { .. }),
        "{outcome:?}"
    );
    let mut serving_now = last.containers.clone();
    serving_now.sort();
    assert_eq!(serving_now, vec![container('a'), container('d')]);
}
