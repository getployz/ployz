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

/// Create, start and serve the new Container `b` on `TO`.
fn served() -> Vec<Step> {
    vec![
        created(
            Call::Create(machine(TO), ContainerKind::ServiceContainer),
            &container('b'),
        ),
        ok(Call::Start(machine(TO), container('b'))),
        observed(Call::Inspect(machine(TO), container('b')), running()),
        serving(container('b')),
    ]
}

#[tokio::test]
async fn a_move_is_done_once_the_old_container_is_removed() {
    let mut steps = served();
    steps.extend([
        ok(Call::Stop(machine(FROM), container('a'))),
        ok(Call::Remove(machine(FROM), container('a'))),
        failed(
            Call::Wait(vec![container('a')], ContainerObservationCondition::Dropped),
            "observation timed out",
        ),
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
        failed(Call::Remove(machine(FROM), container('a')), "remove failed"),
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
        (ok(Call::Remove(machine(TO), container('b'))), true),
        (
            failed(Call::Remove(machine(TO), container('b')), "remove failed"),
            false,
        ),
    ] {
        let client = Scripted::new(vec![
            created(
                Call::Create(machine(TO), ContainerKind::ServiceContainer),
                &container('b'),
            ),
            failed(Call::Start(machine(TO), container('b')), "start failed"),
            ok(Call::Stop(machine(TO), container('b'))),
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
async fn a_create_whose_reply_was_lost_may_have_left_a_replacement() {
    for (reply, removed) in [
        (unavailable("connection reset"), false),
        (error("image not found"), true),
    ] {
        let client = Scripted::new(vec![Step(
            Call::Create(machine(TO), ContainerKind::ServiceContainer),
            Reply::Error(reply),
        )]);
        let Err(MoveContainerError::NotServing {
            replacement_removed,
            ..
        }) = move_with(&client).await
        else {
            panic!("nothing served");
        };
        assert_eq!(replacement_removed, removed);
        client.assert_done();
    }
}
