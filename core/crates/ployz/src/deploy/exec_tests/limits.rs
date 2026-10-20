//! An in-place limits update keeps the copy In; a refusal replaces it.

use super::flip::execute_reported;
use super::*;

fn update(
    machine_id: &MachineId,
    old_container_id: &ContainerId,
    order: UpdateOrder,
) -> ReplacementOperation {
    let mut spec = spec(None, None, None);
    spec.update.order = order;
    ReplacementOperation {
        machine_id: *machine_id,
        old_container_id: *old_container_id,
        spec,
        skip_health_monitor: true,
    }
}

fn fell_back(machine: &MachineId, container_id: &ContainerId, reason: &str) -> String {
    format!(
        "{}'s Container {container_id} was not updated in place on {machine} ({reason}), so the Deploy replaces it.",
        api()
    )
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
            services: ended(&[("api", ServiceResult::New)]),
        }
    );
    assert_eq!(warnings, Vec::<String>::new());
    assert_eq!(client.rotation(&old), Some(CopyRotation::In));
    assert!(!touched(&client, &old), "{:?}", client.moves());
    client.assert_done();
}

#[tokio::test]
async fn a_refused_update_replaces_the_copy_and_says_so() {
    let machine = machine('1');
    let old = container('a');
    let new = container('b');
    let plan = vec![DeployOperation::UpdateContainer(update(
        &machine,
        &old,
        UpdateOrder::StartFirst,
    ))];
    let client = Scripted::new(vec![
        Step(
            Call::UpdateLimits(machine, old),
            Reply::Error(error("the change needs a new Container")),
        ),
        created(Call::Run(machine, ContainerKind::ServiceContainer), &new),
        observed(Call::Inspect(machine, old), running()),
        ok(Call::Remove(machine, old)),
    ])
    .with_copy(&machine, &old, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[], &client, &CancellationToken::new()).await;

    assert_eq!(
        outcome,
        DeployOutcome::Success {
            completed: plan,
            services: ended(&[("api", ServiceResult::New)]),
        }
    );
    assert_eq!(
        warnings,
        [fell_back(
            &machine,
            &old,
            "the change needs a new Container"
        )]
    );
    assert_eq!(client.brought_in(), [new]);
    assert_eq!(client.rotation(&old), None, "the old copy retires");
    client.assert_done();
}

#[tokio::test]
async fn a_refused_stop_first_update_falls_back_with_the_replacement_compensation() {
    let machine = machine('1');
    let old = container('a');
    let replacement = update(&machine, &old, UpdateOrder::StopFirst);
    let plan = vec![DeployOperation::UpdateContainer(replacement.clone())];
    let client = Scripted::new(vec![
        Step(
            Call::UpdateLimits(machine, old),
            Reply::Error(error("refused")),
        ),
        observed(Call::Inspect(machine, old), running()),
        ok(Call::Stop(machine, old)),
        failed(
            Call::Run(machine, ContainerKind::ServiceContainer),
            "start failed",
        ),
        ok(Call::DiscardSlot(machine, ContainerKind::ServiceContainer)),
        ok(Call::Restart(machine, old)),
    ])
    .with_copy(&machine, &old, CopyRotation::In);

    let (outcome, warnings) =
        execute_reported(&plan, &[], &client, &CancellationToken::new()).await;

    assert_eq!(warnings, [fell_back(&machine, &old, "refused")]);
    assert!(
        matches!(
            &outcome,
            DeployOutcome::Failed {
                failed: FailedOperation::Replacement {
                    operation,
                    compensation: ReplacementCompensation::OldStopped {
                        stop_new_container: None,
                        restore_old_container: RestoreAttempt::Restored,
                    },
                    ..
                },
                ..
            } if *operation == replacement
        ),
        "{outcome:?}"
    );
    assert_eq!(client.rotation(&old), Some(CopyRotation::In));
    client.assert_done();
}

#[tokio::test]
async fn a_stop_first_sibling_replace_leaves_the_updated_copy_in() {
    let (first, second) = (machine('1'), machine('2'));
    let (old_first, old_second) = (container('a'), container('b'));
    let new_second = container('c');
    let plan = vec![
        DeployOperation::UpdateContainer(update(&first, &old_first, UpdateOrder::StopFirst)),
        DeployOperation::ReplaceContainer(update(&second, &old_second, UpdateOrder::StopFirst)),
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
        DeployOperation::ReplaceContainer(update(&second, &old_second, UpdateOrder::StartFirst)),
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
