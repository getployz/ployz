//! Placement convergence's one move: what each step leaves behind.

use ployz_core::{DeploymentLogId, Gen, MachineName};

use super::*;
use crate::connect::ConnectError;
use crate::deploy::cleanup::Copy;
use crate::deploy::{MoveEnd, MoveError};
use crate::drain::Replacement;
use crate::drain::placement::{
    LastIn, MachineRef, OldCopyEnd, Relocated, Unconfirmed, relocate, retire_old,
};

const FROM: char = '1';
const TO: char = '2';

/// The image ID the old copy runs.
fn source_image() -> ployz_core::ImageDigest {
    ployz_core::ImageDigest::parse(format!("sha256:{}", "a".repeat(64))).unwrap()
}

async fn move_with(client: &Scripted) -> Result<ContainerId, MoveError> {
    move_container(
        client,
        &test_namespace(),
        &spec(Some(0), None, None),
        &machine(TO),
        (&machine(FROM), &container('a')),
        &source_image(),
        &CancellationToken::new(),
    )
    .await
    .map(|(id, _)| id)
}

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

async fn relocate_with(
    client: &Scripted,
    cancellation: &CancellationToken,
) -> Result<Result<OldCopyEnd, LastIn>, MoveError> {
    let relocated = relocate(
        client,
        &test_namespace(),
        (&spec(Some(0), None, None), &source_image()),
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

/// A tag spec starts on the pinned route, pinned to the image its old copy runs, so the
/// tag never decides what runs there. A digest spec already names its content and goes on
/// ensure.v1 with the old copy's spec as it is, its pull policy included.
#[tokio::test]
async fn only_a_tag_spec_is_pinned_and_neither_spec_is_rewritten() {
    let digest = format!("alpine@sha256:{}", "b".repeat(64));
    for (image, pin) in [
        ("alpine:3.23.3", Some(source_image())),
        (digest.as_str(), None),
    ] {
        let mut spec = spec(Some(0), None, None);
        spec.container.image = image.to_owned();
        let client = moving(served());
        move_container(
            &client,
            &test_namespace(),
            &spec,
            &machine(TO),
            (&machine(FROM), &container('a')),
            &source_image(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        assert_eq!(*client.pins.lock().unwrap(), [(pin, spec)], "{image}");
    }
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
            oom_killed: false,
            departing: None,
        },
    );
    let Err(MoveError {
        end: MoveEnd::NotServing { .. },
        replacement,
    }) = relocate_with(&client, &CancellationToken::new()).await
    else {
        panic!("a copy that did not go In serves nothing");
    };
    assert_eq!(replacement, Replacement::Removed);
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

/// The daemon reuses the health it settled at creation only for an In under the same
/// deploy, so the In names the deploy the replacement was created for.
#[tokio::test]
async fn the_new_copy_goes_in_under_the_deploy_it_was_created_for() {
    let deploy = DeploymentLogId::parse("kept").unwrap();
    let mut steps = served();
    steps.push(ok(Call::Remove(machine(FROM), container('a'))));
    let client = moving(steps)
        .with_copy(&machine(FROM), &container('a'), CopyRotation::In)
        .with_deploy(&container('a'), &deploy);
    let end = relocate_with(&client, &CancellationToken::new())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(end, OldCopyEnd::Stopped);
    assert_eq!(
        *client.in_runs.lock().unwrap(),
        vec![(container('b'), Some(deploy))]
    );
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
            Replacement::Discarded,
        ),
        (
            failed(
                Call::DiscardSlot(machine(TO), ContainerKind::ServiceContainer),
                "remove failed",
            ),
            Replacement::Left,
        ),
    ] {
        let client = moving(vec![
            failed(
                Call::Run(machine(TO), ContainerKind::ServiceContainer),
                "start failed",
            ),
            removal,
        ]);
        let Err(MoveError { replacement, .. }) = move_with(&client).await else {
            panic!("the new Container never served");
        };
        assert_eq!(replacement, removed);
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
async fn a_move_names_its_slot_by_its_old_copy_or_by_a_global_spec() {
    for (global, key) in [
        (false, format!("move:{}", container('a'))),
        (true, crate::cluster::global_creation_key(&global_spec())),
    ] {
        let client = moving(served());
        let spec = if global {
            global_spec()
        } else {
            spec(Some(0), None, None)
        };
        move_container(
            &client,
            &test_namespace(),
            &spec,
            &machine(TO),
            (&machine(FROM), &container('a')),
            &source_image(),
            &CancellationToken::new(),
        )
        .await
        .unwrap();
        let slots = client.slots.lock().unwrap();
        let [slot] = slots.as_slice() else {
            panic!("expected one creation, got {slots:?}");
        };
        assert_eq!(slot.kind, ContainerKind::ServiceContainer);
        assert_eq!(slot.creation_key.as_str(), key);
    }
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
    let Err(MoveError { replacement, .. }) = move_with(&client).await else {
        panic!("nothing served");
    };
    assert_eq!(replacement, Replacement::Discarded);
    client.assert_done();
}

#[tokio::test]
async fn an_unreadable_source_copy_starts_no_replacement() {
    let client = Scripted::new(vec![failed(
        Call::Inspect(machine(FROM), container('a')),
        "daemon unreachable",
    )]);
    let Err(MoveError {
        end: MoveEnd::NotServing { detail },
        replacement,
    }) = move_with(&client).await
    else {
        panic!("the move stops at the inspect");
    };
    assert!(detail.starts_with("inspect Container failed: "), "{detail}");
    assert_eq!(replacement, Replacement::NoneLeft);
    client.assert_done();
}

/// A start answered without a Container, or refused because another holds the slot, left
/// nothing of this move behind. Removing by slot would take whatever holds it.
#[tokio::test]
async fn a_start_that_made_nothing_removes_nothing() {
    for (reply, occupant) in [
        (Reply::Ok, None),
        (
            Reply::Refused(ContainerRefusalReason::SlotOccupied, Some(container('c'))),
            Some(Some(container('c'))),
        ),
        (
            Reply::Refused(ContainerRefusalReason::ImageDiffers, Some(container('c'))),
            Some(Some(container('c'))),
        ),
    ] {
        let client = moving(vec![Step(
            Call::Run(machine(TO), ContainerKind::ServiceContainer),
            reply,
        )]);
        let Err(MoveError { end, replacement }) = move_with(&client).await else {
            panic!("nothing served");
        };
        match occupant {
            None => assert!(matches!(end, MoveEnd::NotServing { .. }), "{end:?}"),
            Some(occupant) => assert_eq!(end, MoveEnd::SlotOccupied { occupant }),
        }
        assert_eq!(replacement, Replacement::NoneLeft);
        client.assert_done();
    }
}

/// A start that answered with its Container and then failed, or whose drain was cancelled
/// after the answer, removes that Container by its ID and nothing else.
#[tokio::test]
async fn a_failed_start_removes_only_the_container_it_answered() {
    for cancelled in [false, true] {
        let run = if cancelled {
            created(
                Call::Run(machine(TO), ContainerKind::ServiceContainer),
                &container('b'),
            )
        } else {
            unhealthy_run(machine(TO), &container('b'))
        };
        let mut client = moving(vec![run, ok(Call::Discard(machine(TO), container('b')))]);
        let cancellation = CancellationToken::new();
        if cancelled {
            client.cancel_on = Some((
                Call::Run(machine(TO), ContainerKind::ServiceContainer),
                cancellation.clone(),
            ));
        }
        let Err(MoveError { end, replacement }) = move_container(
            &client,
            &test_namespace(),
            &spec(Some(0), None, None),
            &machine(TO),
            (&machine(FROM), &container('a')),
            &source_image(),
            &cancellation,
        )
        .await
        else {
            panic!("nothing served");
        };
        assert_eq!(cancelled, end == MoveEnd::Cancelled, "{end:?}");
        assert_eq!(replacement, Replacement::Removed);
        client.assert_done();
    }
}

/// A start's answer that is lost may have made a Container. A later refusal speaks only for
/// its own send, so the move discards its slot as any unanswered start's, and never says
/// the occupant is merely in the way.
#[tokio::test(start_paused = true)]
async fn a_refusal_after_a_lost_answer_discards_the_slot() {
    let run = || Call::Run(machine(TO), ContainerKind::ServiceContainer);
    let lost = || RpcError::unavailable("connection lost").reply_lost_from(&machine(TO));
    let refused_redial = RpcError::unauthenticated("the entry refused this client");
    for (resend, redial) in [
        (
            Reply::Refused(ContainerRefusalReason::SlotOccupied, Some(container('b'))),
            None,
        ),
        (
            Reply::Error(RpcError::internal("could not record the admission").unacted()),
            None,
        ),
        (
            Reply::Error(crate::connect::closed_session_error()),
            Some(Err(RedialFailure::Refused(refused_redial.clone()))),
        ),
    ] {
        let mut client = moving(vec![
            Step(run(), Reply::Error(lost())),
            Step(run(), resend),
            ok(Call::DiscardSlot(
                machine(TO),
                ContainerKind::ServiceContainer,
            )),
        ]);
        client.redial = redial;
        let Err(MoveError { end, replacement }) = move_with(&client).await else {
            panic!("nothing served");
        };
        assert!(matches!(end, MoveEnd::NotServing { .. }), "{end:?}");
        assert_eq!(replacement, Replacement::Discarded);
        client.assert_done();
    }
}

/// An error the Machine marked as refused before it acted made nothing, so the move
/// removes nothing: the slot may hold another caller's Container. A refusal after the
/// create carries no mark, and the Container that create made is discarded.
#[tokio::test]
async fn only_a_refusal_before_the_create_leaves_the_slot_alone() {
    let run = || Call::Run(machine(TO), ContainerKind::ServiceContainer);
    let gated = || {
        RpcError::new(
            RpcErrorCode::from(ployz_core::FORBIDDEN_CODE),
            "the spec's signer was revoked",
        )
        .with_details(serde_json::json!({ "capability": ployz_core::DEPLOY_APPROVAL_CAPABILITY }))
    };
    let before = moving(vec![Step(
        run(),
        Reply::Error(RpcError::internal("could not record the admission").unacted()),
    )]);
    let Err(MoveError { replacement, .. }) = move_with(&before).await else {
        panic!("nothing served");
    };
    assert_eq!(replacement, Replacement::NoneLeft);
    before.assert_done();

    let after = moving(vec![
        Step(run(), Reply::Error(gated())),
        ok(Call::DiscardSlot(
            machine(TO),
            ContainerKind::ServiceContainer,
        )),
    ]);
    let Err(MoveError { replacement, .. }) = move_with(&after).await else {
        panic!("nothing served");
    };
    assert_eq!(replacement, Replacement::Discarded);
    after.assert_done();
}

/// A send the entry couldn't dial never reached the destination, so it made nothing there.
/// The resend's refusal then speaks for the whole start: the occupant is named and left.
#[tokio::test(start_paused = true)]
async fn a_refusal_after_an_undialed_send_leaves_the_slot_alone() {
    let run = || Call::Run(machine(TO), ContainerKind::ServiceContainer);
    let undialed = crate::cluster::answered_by(
        &machine(TO),
        ConnectError::from(ployz_core::rpc::target_unreached_status(
            &std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "connection refused"),
        )),
    );
    let client = moving(vec![
        Step(run(), Reply::Error(undialed)),
        Step(
            run(),
            Reply::Refused(ContainerRefusalReason::SlotOccupied, Some(container('c'))),
        ),
    ]);
    let Err(MoveError { end, replacement }) = move_with(&client).await else {
        panic!("nothing served");
    };
    assert_eq!(
        end,
        MoveEnd::SlotOccupied {
            occupant: Some(container('c'))
        }
    );
    assert_eq!(replacement, Replacement::NoneLeft);
    client.assert_done();
}

/// The error a failed start reports may be an earlier send's. What is left on the Machine
/// follows the last send: one left unanswered may have made a Container, which the move
/// discards, whatever answer it reports.
#[tokio::test(start_paused = true)]
async fn a_last_send_left_unanswered_discards_the_slot_whatever_error_is_reported() {
    let run = || Call::Run(machine(TO), ContainerKind::ServiceContainer);
    let client = moving(vec![
        Step(
            run(),
            Reply::Error(RpcError::unavailable("Cluster store is not available").unacted()),
        ),
        Step(
            run(),
            Reply::ErrorAfter(
                RpcError::unanswered_target(
                    RpcErrorCode::Unavailable,
                    format!("Machine {} did not answer", machine(TO)),
                ),
                || tokio::time::sleep(RESTART_WAIT),
            ),
        ),
        ok(Call::DiscardSlot(
            machine(TO),
            ContainerKind::ServiceContainer,
        )),
    ]);
    let Err(MoveError { end, replacement }) = move_with(&client).await else {
        panic!("nothing served");
    };
    let MoveEnd::NotServing { detail } = end else {
        panic!("nothing served: {end:?}");
    };
    assert!(
        detail.contains("Cluster store is not available"),
        "{detail}"
    );
    assert_eq!(replacement, Replacement::Discarded);
    client.assert_done();
}

async fn cancelled_global_move(client: &Scripted) -> Replacement {
    let mut global = spec(Some(0), None, None);
    global.mode = ployz_core::ServiceMode::Global;
    let cancellation = CancellationToken::new();
    let (namespace, to, from, old) = (test_namespace(), machine(TO), machine(FROM), container('a'));
    let image = source_image();
    let moving = move_container(
        client,
        &namespace,
        &global,
        &to,
        (&from, &old),
        &image,
        &cancellation,
    );
    tokio::pin!(moving);
    tokio::select! {
        moved = &mut moving => panic!("the move returned before cancel: {moved:?}"),
        () = tokio::task::yield_now() => {}
    }
    cancellation.cancel();
    let Err(MoveError {
        end: MoveEnd::Cancelled,
        replacement,
    }) = moving.await
    else {
        panic!("a cancelled move serves nothing");
    };
    replacement
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
    assert_eq!(cancelled_global_move(&client).await, Replacement::Discarded);
    client.assert_done();
}

#[tokio::test]
async fn a_cancelled_global_move_keeps_an_adopted_replica() {
    let mut client = moving(vec![Step(
        Call::Run(machine(TO), ContainerKind::ServiceContainer),
        Reply::Pending,
    )]);
    client.runs_global = true;
    assert_eq!(cancelled_global_move(&client).await, Replacement::Left);
    client.assert_done();
}

/// How the old copy of a move ends when its retirement does not finish: the refusal's own
/// words, the proxies that never confirmed, or the cancel.
#[tokio::test(start_paused = true)]
async fn an_old_copy_that_does_not_retire_says_why_it_still_runs() {
    let moved = |client: &Scripted| {
        client.moved.lock().unwrap().insert(
            (container('a'), Ask::Out),
            CopyRotation::Held(DeploymentLogId::parse("other-run").unwrap()),
        );
    };
    let client = Scripted::new(vec![]).with_copy(&machine(FROM), &container('a'), CopyRotation::In);
    moved(&client);
    let refused = retire_old(&client, &old_copy(), &name, &CancellationToken::new()).await;
    assert_eq!(
        refused.unwrap(),
        OldCopyEnd::Running {
            unconfirmed: Unconfirmed::Failed {
                detail: "the copy moved since it was read".into(),
            },
        }
    );

    let mut client = Scripted::new(vec![])
        .with_copy(&machine(FROM), &container('a'), CopyRotation::In)
        .with_copy(&machine(TO), &container('b'), CopyRotation::In);
    client.acks.insert(machine('3'), Ack::Never);
    let unconfirmed = retire_old(&client, &old_copy(), &name, &CancellationToken::new()).await;
    assert_eq!(
        unconfirmed.unwrap(),
        OldCopyEnd::Running {
            unconfirmed: Unconfirmed::Proxies {
                servers: vec![name(&machine('3'))],
            },
        }
    );

    let cancellation = CancellationToken::new();
    let mut client = Scripted::new(vec![])
        .with_copy(&machine(FROM), &container('a'), CopyRotation::In)
        .with_copy(&machine(TO), &container('b'), CopyRotation::In);
    client.acks.insert(machine('3'), Ack::Never);
    client.cancel_on_move = Some((
        Move::To(container('a'), CopyRotation::Out),
        cancellation.clone(),
    ));
    let cancelled = retire_old(&client, &old_copy(), &name, &cancellation).await;
    assert_eq!(
        cancelled.unwrap(),
        OldCopyEnd::Running {
            unconfirmed: Unconfirmed::Cancelled,
        }
    );
}

#[test]
fn a_failed_move_reads_as_its_cancel_or_the_text_of_what_went_wrong() {
    let machine_failure = ExecutionError::Machine {
        action: MachineAction::CreateContainer,
        error: RpcError::unavailable("connection refused"),
    };
    let newer = ExecutionError::NewerServing {
        service: QualifiedService::parse("shop/api").unwrap(),
    };
    for (error, expected) in [
        (ExecutionError::Cancelled, MoveEnd::Cancelled),
        (
            machine_failure,
            MoveEnd::NotServing {
                detail: "create Container failed: connection refused".into(),
            },
        ),
        (
            newer,
            MoveEnd::NotServing {
                detail: "a newer deploy is already serving shop/api".into(),
            },
        ),
    ] {
        assert_eq!(MoveEnd::from(error), expected);
    }
}
