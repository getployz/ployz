//! Placement convergence's one move: what each step leaves behind.

use ployz_core::{DeploymentLogId, Gen, MachineName};

use super::*;
use crate::deploy::cleanup::Copy;
use crate::deploy::convergence::{LastIn, RelocateError, Relocated, relocate, retire_old};
use crate::deploy::{MachineRef, OldCopyEnd, Unconfirmed};

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

/// A move first reads the source copy's gen and deploy label.
fn moving(steps: Vec<Step>) -> Scripted {
    let mut script = vec![observed(
        Call::Inspect(machine(FROM), container('a')),
        running(),
    )];
    script.extend(steps);
    Scripted::new(script)
}

fn served() -> Vec<Step> {
    vec![created(
        Call::Run(machine(TO), ContainerKind::ServiceContainer),
        &container('b'),
    )]
}

fn old_copy() -> Copy {
    Copy {
        machine_id: machine(FROM),
        service: api(),
        container_id: container('a'),
        rotation: CopyRotation::In,
    }
}

fn name(id: &MachineId) -> MachineRef {
    MachineRef {
        id: *id,
        name: MachineName::parse(format!("m{id}")).unwrap(),
    }
}

/// The whole move: the new copy brought In, then the old one retired once it is listed.
async fn relocate_with(
    client: &Scripted,
    cancellation: &CancellationToken,
) -> Result<Result<OldCopyEnd, LastIn>, RelocateError> {
    let relocated = relocate(
        client,
        &test_namespace(),
        &spec(Some(0), None, None),
        &machine(TO),
        &old_copy(),
        &name,
        cancellation,
    )
    .await?;
    Ok(match relocated {
        Relocated::Listed => retire_old(client, &old_copy(), &name, cancellation).await,
        Relocated::Unlisted(unconfirmed) => Ok(OldCopyEnd::In { unconfirmed }),
    })
}

#[tokio::test]
async fn a_move_brings_the_new_copy_in_and_retires_the_old_one() {
    let mut steps = served();
    steps.push(ok(Call::Remove(machine(FROM), container('a'))));
    let client = moving(steps).with_copy(&machine(FROM), &container('a'), CopyRotation::In);
    let end = relocate_with(&client, &CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(end, OldCopyEnd::Stopped);
    assert_eq!(client.rotation(&container('b')), Some(CopyRotation::In));
    assert_eq!(client.rotation(&container('a')), None);
    client.assert_done();
}

#[tokio::test]
async fn a_refused_in_discards_the_new_copy_and_keeps_the_old_one() {
    let mut steps = served();
    steps.push(ok(Call::Remove(machine(TO), container('b'))));
    let mut client = moving(steps).with_copy(&machine(FROM), &container('a'), CopyRotation::In);
    client.unhealthy_in.insert(
        container('b'),
        HealthVerdict::Failed {
            observation: unhealthy(),
            last_check: None,
        },
    );
    let Err(RelocateError::NotServing {
        replacement_removed,
        ..
    }) = relocate_with(&client, &CancellationToken::new()).await
    else {
        panic!("a copy that did not go In serves nothing");
    };
    assert!(replacement_removed);
    assert_eq!(client.rotation(&container('a')), Some(CopyRotation::In));
    assert_eq!(client.rotation(&container('b')), None);
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn an_unconfirmed_listing_leaves_both_copies_in() {
    let mut client = moving(served()).with_copy(&machine(FROM), &container('a'), CopyRotation::In);
    client.acks.insert(machine('3'), Ack::Never);
    let end = relocate_with(&client, &CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        end,
        OldCopyEnd::In {
            unconfirmed: Unconfirmed::Proxies {
                servers: vec![name(&machine('3'))],
            },
        }
    );
    assert_eq!(client.rotation(&container('a')), Some(CopyRotation::In));
    assert_eq!(client.rotation(&container('b')), Some(CopyRotation::In));
    client.assert_done();
}

#[tokio::test]
async fn the_last_copy_in_rotation_stays() {
    let client = moving(served()).with_copy(&machine(FROM), &container('a'), CopyRotation::In);
    client.moved_sibling.lock().unwrap().insert(
        (container('a'), Ask::Out),
        (container('b'), CopyRotation::Out),
    );
    let Ok(Err(LastIn)) = relocate_with(&client, &CancellationToken::new()).await else {
        panic!("Out on the last In copy is refused");
    };
    assert_eq!(client.rotation(&container('a')), Some(CopyRotation::In));
    client.assert_done();
}

#[tokio::test]
async fn a_cancel_between_in_and_out_leaves_both_copies_in() {
    let cancellation = CancellationToken::new();
    let mut client = moving(served()).with_copy(&machine(FROM), &container('a'), CopyRotation::In);
    client.cancel_on_move = Some((
        Move::To(container('b'), CopyRotation::In),
        cancellation.clone(),
    ));
    let end = relocate_with(&client, &cancellation)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        end,
        OldCopyEnd::In {
            unconfirmed: Unconfirmed::Cancelled,
        }
    );
    assert_eq!(client.rotation(&container('a')), Some(CopyRotation::In));
    assert_eq!(client.rotation(&container('b')), Some(CopyRotation::In));
    client.assert_done();
}

#[tokio::test]
async fn the_moved_copy_keeps_the_source_gen_and_deploy_label() {
    let deploy = DeploymentLogId::parse("kept").unwrap();
    let client = moving(served())
        .with_copy_at(
            &api(),
            &machine(FROM),
            &container('a'),
            CopyRotation::In,
            Gen::new(3),
        )
        .with_deploy(&container('a'), &deploy);
    assert_eq!(move_with(&client).await.unwrap(), container('b'));
    assert_eq!(
        client
            .copies
            .lock()
            .unwrap()
            .get(&container('b'))
            .map(|copy| copy.r#gen),
        Some(Gen::new(3))
    );
    assert_eq!(*client.deploys.lock().unwrap(), vec![Some(deploy)]);
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
        let client = moving(vec![
            failed(
                Call::Run(machine(TO), ContainerKind::ServiceContainer),
                "start failed",
            ),
            removal,
        ]);
        let Err(MoveContainerError {
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
    let client = moving(steps);
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
    let client = moving(vec![
        failed(
            Call::Run(machine(TO), ContainerKind::ServiceContainer),
            "image not found",
        ),
        ok(Call::DiscardSlot(
            machine(TO),
            ContainerKind::ServiceContainer,
        )),
    ]);
    let Err(MoveContainerError {
        replacement_removed,
        ..
    }) = move_with(&client).await
    else {
        panic!("nothing served");
    };
    assert!(replacement_removed);
    client.assert_done();
}

async fn cancelled_global_move(client: &Scripted) -> bool {
    let mut global = spec(Some(0), None, None);
    global.mode = ployz_core::ServiceMode::Global;
    let cancellation = CancellationToken::new();
    let (namespace, to, from, old) = (test_namespace(), machine(TO), machine(FROM), container('a'));
    let moving = move_container(
        client,
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
    let Err(MoveContainerError {
        error: ExecutionError::Cancelled,
        replacement_removed,
    }) = moving.await
    else {
        panic!("a cancelled move serves nothing");
    };
    replacement_removed
}

#[tokio::test]
async fn a_cancelled_global_move_removes_its_replacement() {
    let client = moving(vec![
        Step(
            Call::Run(machine(TO), ContainerKind::ServiceContainer),
            Reply::Pending,
        ),
        ok(Call::DiscardSlot(
            machine(TO),
            ContainerKind::ServiceContainer,
        )),
    ]);
    assert!(cancelled_global_move(&client).await);
    client.assert_done();
}

#[tokio::test]
async fn a_cancelled_global_move_keeps_an_adopted_replica() {
    let mut client = moving(vec![Step(
        Call::Run(machine(TO), ContainerKind::ServiceContainer),
        Reply::Pending,
    )]);
    client.runs_global = true;
    assert!(!cancelled_global_move(&client).await);
    client.assert_done();
}
