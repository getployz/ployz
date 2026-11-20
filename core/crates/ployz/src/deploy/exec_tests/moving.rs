//! Placement convergence's one move: what each step leaves behind.

use ployz_core::{DeploymentLogId, Gen, MachineName};

use super::*;
use crate::deploy::cleanup::Copy;
use crate::deploy::{MoveEnd, MoveError};
use crate::drain::placement::{
    LastIn, MachineRef, OldCopyEnd, Relocated, Unconfirmed, confirm_promoted, relocate, retire_old,
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
    .map(|started| started.container_id)
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
    steps.push(ok(Call::DiscardSlot(
        machine(TO),
        ContainerKind::ServiceContainer,
    )));
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
        replacement_removed,
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
        let Err(MoveError {
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
    let Err(MoveError {
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
async fn an_unreadable_source_copy_starts_no_replacement() {
    let client = Scripted::new(vec![failed(
        Call::Inspect(machine(FROM), container('a')),
        "daemon unreachable",
    )]);
    let Err(MoveError {
        end: MoveEnd::NotServing { detail },
        replacement_removed,
    }) = move_with(&client).await
    else {
        panic!("the move stops at the inspect");
    };
    assert!(detail.starts_with("inspect Container failed: "), "{detail}");
    assert!(replacement_removed);
    client.assert_done();
}

/// A start that answered with its Container and then failed, or whose drain was cancelled
/// after the answer, removes what the move's own slot holds: that Container.
#[tokio::test]
async fn a_failed_start_removes_the_container_it_made() {
    for cancelled in [false, true] {
        let run = if cancelled {
            created(
                Call::Run(machine(TO), ContainerKind::ServiceContainer),
                &container('b'),
            )
        } else {
            unhealthy_run(machine(TO), &container('b'))
        };
        let mut client = moving(vec![run, discarded()]);
        let cancellation = CancellationToken::new();
        if cancelled {
            client.cancel_on = Some((
                Call::Run(machine(TO), ContainerKind::ServiceContainer),
                cancellation.clone(),
            ));
        }
        let Err(MoveError {
            end,
            replacement_removed,
        }) = move_container(
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
        assert!(replacement_removed);
        assert_eq!(client.rotation(&container('b')), None);
        client.assert_done();
    }
}

fn discarded() -> Step {
    ok(Call::DiscardSlot(
        machine(TO),
        ContainerKind::ServiceContainer,
    ))
}

/// The move's own sends of a create, by the slot each named.
fn run() -> Call {
    Call::Run(machine(TO), ContainerKind::ServiceContainer)
}

fn lost() -> RpcError {
    RpcError::unavailable("connection lost").reply_lost_from(&machine(TO))
}

/// Y, another caller's Pending copy on the destination in the slot `move:{old}`: a move of
/// the same copy by an older CLI, or a hand-written ensure.v1.
fn with_planted(client: Scripted) -> Scripted {
    client
        .with_copy(&machine(TO), &container('c'), CopyRotation::Pending)
        .with_key(&container('c'), &format!("move:{}", container('a')))
}

/// However a move fails, its cleanup takes only what its own sends made. Another caller's
/// copy in the slot `move:{old}` survives a refusal before Docker's create that carries no
/// mark (a full bridge), a cancel while the move waits to resend, a refused redial, a typed
/// refusal, a failed health check and a lost answer.
#[tokio::test(start_paused = true)]
async fn another_callers_copy_under_the_old_copys_key_survives_every_failure() {
    let full_bridge = || {
        Step(
            run(),
            Reply::Error(RpcError::invalid_argument(
                "no bridge endpoint is free on this Server",
            )),
        )
    };
    let cases: Vec<(&str, Vec<Step>, bool)> = vec![
        ("a full bridge", vec![full_bridge(), discarded()], false),
        (
            "a cancel during the resend wait",
            vec![
                Step(run(), Reply::Error(RpcError::unavailable("restarting"))),
                discarded(),
            ],
            true,
        ),
        (
            "a refused redial",
            vec![
                Step(run(), Reply::Error(crate::connect::closed_session_error())),
                discarded(),
            ],
            false,
        ),
        (
            "a typed refusal",
            vec![
                Step(
                    run(),
                    Reply::Refused(ContainerRefusalReason::ImagePullFailed, None),
                ),
                discarded(),
            ],
            false,
        ),
        (
            "a failed health check",
            vec![unhealthy_run(machine(TO), &container('b')), discarded()],
            false,
        ),
        (
            "a lost answer",
            vec![
                Step(run(), Reply::Error(lost())),
                full_bridge(),
                discarded(),
            ],
            false,
        ),
    ];
    for (case, steps, cancel_in_wait) in cases {
        let mut client = with_planted(moving(steps));
        client.redial = Some(Err(RedialFailure::Refused(RpcError::new(
            RpcErrorCode::Unauthenticated,
            "the entry refused this key",
        ))));
        let cancellation = CancellationToken::new();
        if cancel_in_wait {
            let cancel = cancellation.clone();
            tokio::spawn(async move {
                tokio::time::sleep(RESTART_POLL / 2).await;
                cancel.cancel();
            });
        }
        let moved = move_container(
            &client,
            &test_namespace(),
            &spec(Some(0), None, None),
            &machine(TO),
            (&machine(FROM), &container('a')),
            &source_image(),
            &cancellation,
        )
        .await;
        assert!(moved.is_err(), "{case}");
        assert_eq!(
            client.rotation(&container('c')),
            Some(CopyRotation::Pending),
            "{case}: another caller's copy was removed"
        );
        assert_eq!(client.rotation(&container('b')), None, "{case}");
        client.assert_done();
    }
}

/// A resend carries the slot its lost send made a Container in, so the cleanup after it
/// takes that Container too. The report still can't say none is left: the lost send's
/// request may still arrive after the cleanup.
#[tokio::test(start_paused = true)]
async fn a_resend_removes_what_its_lost_send_made() {
    let client = moving(vec![
        Step(run(), Reply::CreatedLost(container('b'), lost())),
        Step(
            run(),
            Reply::Error(RpcError::invalid_argument("no bridge endpoint is free")),
        ),
        discarded(),
    ]);
    let Err(MoveError {
        replacement_removed,
        ..
    }) = move_with(&client).await
    else {
        panic!("nothing served");
    };
    assert_eq!(client.rotation(&container('b')), None);
    assert!(!replacement_removed, "a lost send may still create");
    client.assert_done();
}

/// The cleanup's Gone is sent again while the destination restarts, so a restart there
/// doesn't leave the move's copy behind.
#[tokio::test(start_paused = true)]
async fn a_failed_moves_cleanup_waits_out_a_destination_restart() {
    let client = moving(vec![
        Step(
            run(),
            Reply::Error(RpcError::invalid_argument("no bridge endpoint is free")),
        ),
        Step(
            Call::DiscardSlot(machine(TO), ContainerKind::ServiceContainer),
            Reply::Error(RpcError::unavailable("restarting")),
        ),
        discarded(),
    ]);
    let Err(MoveError {
        replacement_removed,
        ..
    }) = move_with(&client).await
    else {
        panic!("nothing served");
    };
    assert!(replacement_removed);
    client.assert_done();
}

/// A Gone the Machine answers by reading back a Container, as one a restore brought back
/// in the slot, did not leave it empty.
#[tokio::test]
async fn a_cleanup_that_reads_back_a_container_says_it_may_still_be_there() {
    let client = moving(vec![
        Step(
            run(),
            Reply::Error(RpcError::invalid_argument("no bridge endpoint is free")),
        ),
        Step(
            Call::DiscardSlot(machine(TO), ContainerKind::ServiceContainer),
            Reply::Created(container('d')),
        ),
    ]);
    let Err(MoveError {
        replacement_removed,
        ..
    }) = move_with(&client).await
    else {
        panic!("nothing served");
    };
    assert!(!replacement_removed);
    client.assert_done();
}

/// Two drains moving the same copy at once: one fails while the other's new Container
/// waits on its health. The failed one's cleanup leaves the other's Container alone.
#[tokio::test(start_paused = true)]
async fn a_failed_move_leaves_a_concurrent_moves_copy() {
    let client = Scripted::new(vec![
        observed(Call::Inspect(machine(FROM), container('a')), running()),
        Step(
            run(),
            Reply::CreatedAfter(container('b'), || {
                tokio::time::sleep(std::time::Duration::from_secs(1))
            }),
        ),
        observed(Call::Inspect(machine(FROM), container('a')), running()),
        Step(
            run(),
            Reply::Error(RpcError::invalid_argument("no bridge endpoint is free")),
        ),
        discarded(),
    ]);
    let (served, failed) = tokio::join!(move_with(&client), move_with(&client));
    assert_eq!(served.unwrap(), container('b'));
    assert!(failed.is_err());
    assert_eq!(
        client.rotation(&container('b')),
        Some(CopyRotation::Pending)
    );
    client.assert_done();
}

/// A rerun of a drain whose earlier move brought its copy In, but never retired the old
/// one, creates nothing: it confirms that copy runs the old copy's image and is listed,
/// then retires the old copy. A copy of another image, or one failing its health check
/// though every proxy still lists it, leaves the old copy serving.
#[tokio::test]
async fn a_rerun_retires_the_old_copy_behind_the_copy_an_earlier_move_brought_in() {
    let promoted = Copy {
        machine_id: machine(TO),
        service: api(),
        container_id: container('b'),
        rotation: CopyRotation::In,
    };
    for (image, health, end) in [
        ("a", healthy(), OldCopyEnd::Stopped),
        (
            "a",
            failing(),
            OldCopyEnd::In {
                unconfirmed: Unconfirmed::Failed {
                    detail: format!(
                        "Container {} came In for it on {}, but isn't healthy",
                        container('b'),
                        name(&machine(TO))
                    ),
                },
            },
        ),
        (
            "c",
            healthy(),
            OldCopyEnd::In {
                unconfirmed: Unconfirmed::Failed {
                    detail: format!(
                        "Container {} came In for it on {}, but runs another image",
                        container('b'),
                        name(&machine(TO))
                    ),
                },
            },
        ),
    ] {
        let mut steps = vec![
            observed(Call::Inspect(machine(FROM), container('a')), running()),
            observed(Call::Inspect(machine(TO), container('b')), health),
        ];
        if end == OldCopyEnd::Stopped {
            steps.push(ok(Call::Remove(machine(FROM), container('a'))));
        }
        let mut client = Scripted::new(steps)
            .with_copy(&machine(FROM), &container('a'), CopyRotation::In)
            .with_copy(&machine(TO), &container('b'), CopyRotation::In)
            .with_key(&container('b'), &format!("move:{}", container('a')));
        client
            .image_ids
            .insert(container('a'), source_image().to_string());
        client
            .image_ids
            .insert(container('b'), format!("sha256:{}", image.repeat(64)));
        let cancellation = CancellationToken::new();
        let ended =
            match confirm_promoted(&client, &old_copy(), &promoted, &name, &cancellation).await {
                Relocated::Listed => retire_old(&client, &old_copy(), &name, &cancellation)
                    .await
                    .unwrap(),
                Relocated::Unlisted(unconfirmed) => OldCopyEnd::In { unconfirmed },
            };
        assert_eq!(ended, end);
        assert_eq!(client.rotation(&container('b')), Some(CopyRotation::In));
        client.assert_done();
    }
}

async fn cancelled_global_move(client: &Scripted) -> bool {
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
    assert!(
        !cancelled_global_move(&client).await,
        "a send was cut off while out"
    );
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
