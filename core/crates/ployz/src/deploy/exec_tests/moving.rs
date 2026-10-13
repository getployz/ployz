//! Placement convergence's one move: what each failure leaves behind.

use super::*;

const FROM: char = '1';
const TO: char = '2';

async fn move_with(client: &Scripted) -> Result<ContainerId, MoveContainerError> {
    move_container(
        client,
        &test_namespace(),
        &spec(Some(0), None, None),
        &machine(TO),
        (&machine(FROM), &container('a')),
        &CancellationToken::new(),
    )
    .await
}

fn served() -> Vec<Step> {
    vec![created(
        Call::Run(machine(TO), ContainerKind::ServiceContainer),
        &container('b'),
    )]
}

#[tokio::test]
async fn a_move_is_done_once_the_old_container_is_removed() {
    let mut steps = served();
    steps.extend([
        ok(Call::Stop(machine(FROM), container('a'))),
        ok(Call::Discard(machine(FROM), container('a'))),
    ]);
    let client = Scripted::new(steps);
    assert_eq!(move_with(&client).await.unwrap(), container('b'));
    client.assert_done();
}

#[tokio::test]
async fn an_old_container_that_would_not_stop_still_serves() {
    let mut steps = served();
    steps.push(failed(
        Call::Stop(machine(FROM), container('a')),
        "stop failed",
    ));
    let client = Scripted::new(steps);
    assert!(matches!(
        move_with(&client).await,
        Err(MoveContainerError::OldNotRemoved {
            old_stopped: false,
            ..
        })
    ));
    client.assert_done();
}

#[tokio::test]
async fn an_old_container_that_stopped_but_would_not_go_is_told_apart() {
    let mut steps = served();
    steps.extend([
        ok(Call::Stop(machine(FROM), container('a'))),
        failed(
            Call::Discard(machine(FROM), container('a')),
            "remove failed",
        ),
    ]);
    let client = Scripted::new(steps);
    let Err(MoveContainerError::OldNotRemoved { error, old_stopped }) = move_with(&client).await
    else {
        panic!("the old Container stayed");
    };
    assert!(old_stopped);
    assert!(error.to_string().contains("remove failed"), "{error}");
    client.assert_done();
}

#[tokio::test]
async fn a_replacement_is_claimed_removed_only_when_its_removal_was_acknowledged() {
    for (removal, removed) in [
        (
            ok(Call::DiscardSlot(
                machine(TO),
                ContainerKind::ServiceContainer,
            )),
            true,
        ),
        (
            failed(
                Call::DiscardSlot(machine(TO), ContainerKind::ServiceContainer),
                "remove failed",
            ),
            false,
        ),
    ] {
        let client = Scripted::new(vec![
            failed(
                Call::Run(machine(TO), ContainerKind::ServiceContainer),
                "start failed",
            ),
            removal,
        ]);
        let Err(MoveContainerError::NotServing {
            replacement_removed,
            ..
        }) = move_with(&client).await
        else {
            panic!("the new Container never served");
        };
        assert_eq!(replacement_removed, removed);
        client.assert_done();
    }
}

#[tokio::test]
async fn a_run_whose_reply_was_lost_is_resent_to_the_same_slot() {
    let mut steps = vec![failed_unavailable(
        Call::Run(machine(TO), ContainerKind::ServiceContainer),
        "connection reset",
    )];
    steps.extend(served());
    steps.extend([
        ok(Call::Stop(machine(FROM), container('a'))),
        ok(Call::Discard(machine(FROM), container('a'))),
    ]);
    let client = Scripted::new(steps);
    assert_eq!(move_with(&client).await.unwrap(), container('b'));
    let slots = client.slots.lock().unwrap();
    let [first, second] = slots.as_slice() else {
        panic!("expected two creations, got {slots:?}");
    };
    assert_eq!(first, second);
    client.assert_done();
}

#[tokio::test]
async fn a_failed_run_removes_its_slot() {
    let client = Scripted::new(vec![
        failed(
            Call::Run(machine(TO), ContainerKind::ServiceContainer),
            "image not found",
        ),
        ok(Call::DiscardSlot(
            machine(TO),
            ContainerKind::ServiceContainer,
        )),
    ]);
    let Err(MoveContainerError::NotServing {
        replacement_removed,
        ..
    }) = move_with(&client).await
    else {
        panic!("nothing served");
    };
    assert!(replacement_removed);
    client.assert_done();
}

#[tokio::test]
async fn a_cancelled_global_move_removes_its_replacement() {
    let mut global = spec(Some(0), None, None);
    global.mode = ployz_core::ServiceMode::Global;
    let client = Scripted::new(vec![
        Step(
            Call::Run(machine(TO), ContainerKind::ServiceContainer),
            Reply::Pending,
        ),
        ok(Call::DiscardSlot(
            machine(TO),
            ContainerKind::ServiceContainer,
        )),
    ]);
    let cancellation = CancellationToken::new();
    let (namespace, to, from, old) = (test_namespace(), machine(TO), machine(FROM), container('a'));
    let moving = move_container(
        &client,
        &namespace,
        &global,
        &to,
        (&from, &old),
        &cancellation,
    );
    tokio::pin!(moving);
    tokio::select! {
        moved = &mut moving => panic!("the move returned before cancel: {moved:?}"),
        () = tokio::task::yield_now() => {}
    }
    cancellation.cancel();
    let Err(MoveContainerError::NotServing {
        error: ExecutionError::Cancelled,
        replacement_removed,
    }) = moving.await
    else {
        panic!("a cancelled move serves nothing");
    };
    assert!(replacement_removed);
    client.assert_done();
}
