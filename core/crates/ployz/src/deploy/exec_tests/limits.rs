//! An in-place limits update keeps the copy In, whether it lands or fails.

use super::flip::execute_reported;
use super::*;
use ployz_core::{
    ContainerUpdateOperation, InPlaceChange, MachineAction, OperationStatus, RpcError,
};

fn update(
    machine_id: &MachineId,
    container_id: &ContainerId,
    order: UpdateOrder,
) -> ContainerUpdateOperation {
    let mut spec = spec(None, None, None);
    spec.update.order = order;
    ContainerUpdateOperation {
        machine_id: *machine_id,
        container_id: *container_id,
        spec,
        change: InPlaceChange::Limits,
    }
}

fn touched(client: &Scripted, container_id: &ContainerId) -> bool {
    client.moves().iter().any(|applied| match applied {
        Move::To(id, _) | Move::Gone(id) => id == container_id,
    })
}

#[tokio::test]
async fn an_update_keeps_the_same_copy_in_and_starts_nothing() {
    let machine = machine('1');
    let old = container('a');
    let plan = vec![DeployOperation::UpdateContainer(update(
        &machine,
        &old,
        UpdateOrder::StartFirst,
    ))];
    let client = Scripted::new(vec![ok(Call::UpdateLimits(machine, old))]).with_copy(
        &machine,
        &old,
        CopyRotation::In,
    );

    let (outcome, warnings) =
        execute_reported(&plan, &[], &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome,
        DeployOutcome::Success {
            completed: plan,
            services: ended(&[("api", ServiceResult::Updated)]),
        }
    );
    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(client.rotation(&old), Some(CopyRotation::In));
    assert!(!touched(&client, &old), "{:?}", client.moves());
    client.assert_done();
}

#[tokio::test]
async fn a_placement_update_records_the_placement_and_keeps_the_copy_in() {
    let machine = machine('1');
    let old = container('a');
    let plan = vec![DeployOperation::UpdateContainer(ContainerUpdateOperation {
        change: InPlaceChange::Placement,
        ..update(&machine, &old, UpdateOrder::StartFirst)
    })];
    let client = Scripted::new(vec![ok(Call::UpdatePlacement(machine, old))]).with_copy(
        &machine,
        &old,
        CopyRotation::In,
    );

    let (outcome, _warnings) =
        execute_reported(&plan, &[], &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome,
        DeployOutcome::Success {
            completed: plan,
            services: ended(&[("api", ServiceResult::Updated)]),
        }
    );
    assert_eq!(client.rotation(&old), Some(CopyRotation::In));
    assert!(!touched(&client, &old), "{:?}", client.moves());
    client.assert_done();
}

/// A row that starts nothing never reads as starting: the copy serves
/// throughout, and the history shows it updating from its first frame.
#[tokio::test]
async fn an_update_row_is_updating_from_its_first_frame() {
    let machine = machine('1');
    let old = container('a');
    let plan = vec![DeployOperation::UpdateContainer(update(
        &machine,
        &old,
        UpdateOrder::StartFirst,
    ))];
    let client = Scripted::new(vec![ok(Call::UpdateLimits(machine, old))]).with_copy(
        &machine,
        &old,
        CopyRotation::In,
    );
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
    let mut statuses = Vec::new();
    let mut live = Vec::new();
    while let Ok(event) = rx.try_recv() {
        if let DeployEvent::Progress { rows, live_at, .. } = event {
            statuses.extend(rows.into_iter().map(|row| row.status));
            if let Some(at) = live_at {
                assert_eq!(statuses.last(), Some(&OperationStatus::Completed));
                live.push(at);
            }
        }
    }
    assert_eq!(live.len(), 1);
    statuses.dedup();
    assert_eq!(
        statuses,
        [
            OperationStatus::Pending,
            OperationStatus::Running {
                phase: OperationPhase::UpdatingContainer
            },
            OperationStatus::Completed,
        ]
    );
    client.assert_done();
}

/// Docker's own words travel with the failure: the Deploy neither replaces the
/// copy nor restates the cause.
#[tokio::test]
async fn a_failed_update_fails_the_operation_and_leaves_the_copy_serving() {
    let machine = machine('1');
    let old = container('a');
    let operation =
        DeployOperation::UpdateContainer(update(&machine, &old, UpdateOrder::StopFirst));
    let plan = vec![operation.clone()];
    let docker = RpcError::internal("Docker operation failed").with_causes(vec![
            "Docker responded with status code 400: Conflicting options: Nano CPUs and CPU Period cannot both be set".into(),
        ]);
    // The failure names the Server; the Machine's own message and cause survive as is.
    let named = docker
        .clone()
        .with_details(serde_json::json!({"machine_id": machine}));
    let client = Scripted::new(vec![Step(
        Call::UpdateLimits(machine, old),
        Reply::Error(docker.clone()),
    )])
    .with_copy(&machine, &old, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[], &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome,
        DeployOutcome::Failed {
            completed: Vec::new(),
            failed: FailedOperation::Operation {
                operation,
                error: ExecutionError::Machine {
                    action: MachineAction::UpdateContainerLimits,
                    error: named,
                },
            },
            unexecuted: Vec::new(),
            services: ended(&[("api", ServiceResult::Old)]),
        }
    );
    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(client.rotation(&old), Some(CopyRotation::In));
    assert!(!touched(&client, &old), "{:?}", client.moves());
    client.assert_done();
}

#[tokio::test]
async fn a_stop_first_sibling_replace_leaves_the_updated_copy_in() {
    let (first, second) = (machine('1'), machine('2'));
    let (old_first, old_second) = (container('a'), container('b'));
    let new_second = container('c');
    let plan = vec![
        DeployOperation::UpdateContainer(update(&first, &old_first, UpdateOrder::StopFirst)),
        replacement(&second, &old_second, UpdateOrder::StopFirst),
    ];
    let client = Scripted::new(vec![
        ok(Call::UpdateLimits(first, old_first)),
        observed(Call::Inspect(second, old_second), running()),
        ok(Call::Stop(second, old_second)),
        created(
            Call::Run(second, ContainerKind::ServiceContainer),
            &new_second,
        ),
        observed(Call::Inspect(second, old_second), running()),
        ok(Call::Remove(second, old_second)),
    ])
    .with_copy(&first, &old_first, CopyRotation::In)
    .with_copy(&second, &old_second, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[], &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome.services(),
        ended(&[("api", ServiceResult::New)]),
        "{outcome:?}"
    );
    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(client.brought_in(), [new_second]);
    assert_eq!(client.rotation(&old_first), Some(CopyRotation::In));
    assert!(!touched(&client, &old_first), "{:?}", client.moves());
    assert_eq!(
        client.rotation(&old_second),
        None,
        "the replaced copy retires"
    );
    client.assert_done();
}

#[tokio::test]
async fn a_failed_sibling_never_removes_the_updated_copy() {
    let (first, second) = (machine('1'), machine('2'));
    let (old_first, old_second) = (container('a'), container('b'));
    let plan = vec![
        DeployOperation::UpdateContainer(update(&first, &old_first, UpdateOrder::StartFirst)),
        replacement(&second, &old_second, UpdateOrder::StartFirst),
    ];
    let client = Scripted::new(vec![
        ok(Call::UpdateLimits(first, old_first)),
        failed(
            Call::Run(second, ContainerKind::ServiceContainer),
            "start failed",
        ),
        ok(Call::DiscardSlot(second, ContainerKind::ServiceContainer)),
    ])
    .with_copy(&first, &old_first, CopyRotation::In)
    .with_copy(&second, &old_second, CopyRotation::In);

    let (outcome, _) = execute_reported(&plan, &[], &client, &CancellationToken::new()).await;

    assert!(
        matches!(outcome, DeployOutcome::Failed { .. }),
        "{outcome:?}"
    );
    assert_eq!(client.rotation(&old_first), Some(CopyRotation::In));
    assert!(!touched(&client, &old_first), "{:?}", client.moves());
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn cancelling_an_in_place_deploy_does_not_wait_for_the_live_ack() {
    let machine = machine('1');
    let copy = container('a');
    let plan = crate::deploy::DeployPlan::for_execution_test(
        vec![DeployOperation::UpdateContainer(update(
            &machine,
            &copy,
            UpdateOrder::StartFirst,
        ))],
        test_namespace(),
    );
    let client = Scripted::new(vec![ok(Call::UpdateLimits(machine, copy))]).with_copy(
        &machine,
        &copy,
        CopyRotation::In,
    );
    let (tx, mut events) = tokio::sync::mpsc::unbounded_channel();
    let (acknowledge, commit) = tokio::sync::oneshot::channel();
    let cancel = CancellationToken::new();
    let run = run_id();
    let execution = super::super::execute_with_switch_commit(
        &plan,
        &run,
        &client,
        &cancel,
        Some(tx),
        Some(commit),
    );
    let cancel_at_live = async {
        while let Some(event) = events.recv().await {
            if matches!(
                event,
                DeployEvent::Progress {
                    live_at: Some(_),
                    ..
                }
            ) {
                cancel.cancel();
                acknowledge
                    .send(crate::deploy::SwitchDecision::Refused)
                    .unwrap();
                return;
            }
        }
        panic!("the executor never reported the switch");
    };
    let (outcome, ()) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(execution, cancel_at_live)
    })
    .await
    .expect("cancelling released the acknowledgement wait");
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
    assert_eq!(client.rotation(&copy), Some(CopyRotation::In));
    client.assert_done();
}
