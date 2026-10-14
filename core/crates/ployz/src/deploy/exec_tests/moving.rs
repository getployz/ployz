//! Placement convergence's one move: what each failure leaves behind.

use super::*;

const FROM: char = '1';
const TO: char = '2';

async fn move_with(client: &Scripted) -> Result<ContainerId, MoveContainerError> {
    move_container(
        client,
        &test_namespace(),
        (&spec(Some(0), None, None), None),
        &machine(TO),
        (&machine(FROM), &container('a')),
        &mut true,
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
async fn a_move_that_keeps_the_old_container_never_stops_or_removes_it() {
    let client = Scripted::new(served());
    let moved = move_container(
        &client,
        &test_namespace(),
        (&spec(Some(0), None, None), None),
        &machine(TO),
        (&machine(FROM), &container('a')),
        &mut false,
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(moved.unwrap(), container('b'));
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
    let mut removes = true;
    let moving = move_container(
        &client,
        &namespace,
        (&global, None),
        &to,
        (&from, &old),
        &mut removes,
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

#[tokio::test]
async fn a_cancelled_global_move_keeps_an_adopted_replica() {
    let mut global = spec(Some(0), None, None);
    global.mode = ployz_core::ServiceMode::Global;
    let mut client = Scripted::new(vec![Step(
        Call::Run(machine(TO), ContainerKind::ServiceContainer),
        Reply::Pending,
    )]);
    client.runs_global = true;
    let cancellation = CancellationToken::new();
    let (namespace, to, from, old) = (test_namespace(), machine(TO), machine(FROM), container('a'));
    let mut removes = true;
    let moving = move_container(
        &client,
        &namespace,
        (&global, None),
        &to,
        (&from, &old),
        &mut removes,
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
    assert!(!replacement_removed);
    client.assert_done();
}

#[tokio::test(start_paused = true)]
async fn a_first_pointer_published_while_the_replacement_starts_keeps_the_old_container() {
    use crate::deploy::convergence::{AtRemoval, OldCopyGate};
    use crate::serving_publish::tests::{FakeRow, pointer};

    for published in [true, false] {
        let mut steps = vec![Step(
            Call::Run(machine(TO), ContainerKind::ServiceContainer),
            Reply::CreatedAfter(container('b'), std::time::Duration::from_secs(10)),
        )];
        if !published {
            steps.extend([
                ok(Call::Stop(machine(FROM), container('a'))),
                ok(Call::Discard(machine(FROM), container('a'))),
            ]);
        }
        let client = Scripted::new(steps);
        let row = FakeRow::default();
        let service = QualifiedService::parse("app/web").unwrap();
        let (namespace, spec, to, from, old) = (
            test_namespace(),
            spec(Some(0), None, None),
            machine(TO),
            machine(FROM),
            container('a'),
        );
        let cancellation = CancellationToken::new();
        let mut gate = OldCopyGate {
            at_removal: AtRemoval::Remove,
            reader: row.clone(),
            service: &service,
            old: &old,
        };
        let publish = async {
            tokio::time::sleep(std::time::Duration::from_secs(5)).await;
            if published {
                *row.row.lock().unwrap() = Some(pointer('a', &['a'], &[]));
            }
        };
        let (moved, ()) = tokio::join!(
            move_container(
                &client,
                &namespace,
                (&spec, None),
                &to,
                (&from, &old),
                &mut gate,
                &cancellation,
            ),
            publish,
        );
        assert_eq!(moved.unwrap(), container('b'));
        client.assert_done();
    }
}

#[tokio::test]
async fn a_kept_old_copy_is_never_stopped_and_a_removed_one_is() {
    use crate::deploy::convergence::{AtRemoval, OldCopy, OldCopyGate};
    use crate::serving_publish::tests::{FakeRow, pointer};

    let kept = pointer('c', &['a'], &[]);
    for old_copy in [OldCopy::Keep { pointer: &kept }, OldCopy::Remove] {
        let mut steps = vec![created(
            Call::Run(machine(TO), ContainerKind::ServiceContainer),
            &container('b'),
        )];
        if old_copy == OldCopy::Remove {
            steps.extend([
                ok(Call::Stop(machine(FROM), container('a'))),
                ok(Call::Discard(machine(FROM), container('a'))),
            ]);
        }
        let client = Scripted::new(steps);
        let reader = FakeRow::default();
        if old_copy != OldCopy::Remove {
            *reader.row.lock().unwrap() = Some(kept.clone());
        }
        let service = QualifiedService::parse("app/web").unwrap();
        let (namespace, spec, to, from, old) = (
            test_namespace(),
            spec(Some(0), None, None),
            machine(TO),
            machine(FROM),
            container('a'),
        );
        let cancellation = CancellationToken::new();
        let mut gate = OldCopyGate {
            at_removal: AtRemoval::of(old_copy, &old),
            reader,
            service: &service,
            old: &old,
        };
        let deploy = gate.at_removal.replacement_deploy();
        let moved = move_container(
            &client,
            &namespace,
            (&spec, deploy.as_ref()),
            &to,
            (&from, &old),
            &mut gate,
            &cancellation,
        )
        .await;
        assert_eq!(moved.unwrap(), container('b'), "{old_copy:?}");
        client.assert_done();
        let kept_deploy = (old_copy != OldCopy::Remove).then(|| kept.deploy.clone());
        assert_eq!(
            *client.deploys.lock().unwrap(),
            vec![kept_deploy],
            "a kept copy's replacement carries its pointer's deployment label"
        );
    }
}

#[tokio::test]
async fn a_pointer_whose_row_does_not_name_the_old_copy_lets_the_move_remove_it() {
    use crate::deploy::convergence::{AtRemoval, OldCopy, OldCopyGate};
    use crate::serving_publish::tests::{FakeRow, pointer};

    let unrelated = pointer('c', &['c'], &[]);
    let client = Scripted::new(vec![
        created(
            Call::Run(machine(TO), ContainerKind::ServiceContainer),
            &container('b'),
        ),
        ok(Call::Stop(machine(FROM), container('a'))),
        ok(Call::Discard(machine(FROM), container('a'))),
    ]);
    let reader = FakeRow::default();
    *reader.row.lock().unwrap() = Some(unrelated.clone());
    let service = QualifiedService::parse("app/web").unwrap();
    let (namespace, spec, to, from, old) = (
        test_namespace(),
        spec(Some(0), None, None),
        machine(TO),
        machine(FROM),
        container('a'),
    );
    let mut gate = OldCopyGate {
        at_removal: AtRemoval::of(
            OldCopy::Keep {
                pointer: &unrelated,
            },
            &old,
        ),
        reader,
        service: &service,
        old: &old,
    };
    assert_eq!(gate.at_removal, AtRemoval::Remove);
    let deploy = gate.at_removal.replacement_deploy();
    let moved = move_container(
        &client,
        &namespace,
        (&spec, deploy.as_ref()),
        &to,
        (&from, &old),
        &mut gate,
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(moved.unwrap(), container('b'));
    client.assert_done();
    assert_eq!(gate.at_removal, AtRemoval::Remove);
}

#[tokio::test]
async fn a_pointer_published_after_the_stop_keeps_the_old_container_unremoved() {
    use crate::deploy::convergence::{AtRemoval, OldCopy, OldCopyGate};
    use crate::serving_publish::PointerClient;
    use crate::serving_publish::tests::{FakeRow, pointer};

    struct PointedOnSecondRead {
        row: FakeRow,
        reads: u32,
    }

    impl PointerClient for PointedOnSecondRead {
        async fn pointer_publisher(&mut self) -> Result<ployz_core::MachineTarget, RpcError> {
            self.row.pointer_publisher().await
        }

        async fn read_pointer_at(
            &mut self,
            at: &mut ployz_core::MachineTarget,
            service: &QualifiedService,
        ) -> Result<Option<ployz_core::ServingPointer>, RpcError> {
            let read = self.row.read_pointer_at(at, service).await;
            self.reads += 1;
            if self.reads == 1 {
                *self.row.row.lock().unwrap() = Some(pointer('a', &['a'], &[]));
            }
            read
        }

        async fn publish_pointer_at(
            &mut self,
            at: &mut ployz_core::MachineTarget,
            request: ployz_core::PublishServingPointerRequest,
        ) -> Result<ployz_core::ServingPointerPublication, RpcError> {
            self.row.publish_pointer_at(at, request).await
        }
    }

    let client = Scripted::new(vec![
        created(
            Call::Run(machine(TO), ContainerKind::ServiceContainer),
            &container('b'),
        ),
        ok(Call::Stop(machine(FROM), container('a'))),
    ]);
    let service = QualifiedService::parse("app/web").unwrap();
    let (namespace, spec, to, from, old) = (
        test_namespace(),
        spec(Some(0), None, None),
        machine(TO),
        machine(FROM),
        container('a'),
    );
    let mut gate = OldCopyGate {
        at_removal: AtRemoval::of(OldCopy::Remove, &old),
        reader: PointedOnSecondRead {
            row: FakeRow::default(),
            reads: 0,
        },
        service: &service,
        old: &old,
    };
    let moved = move_container(
        &client,
        &namespace,
        (&spec, None),
        &to,
        (&from, &old),
        &mut gate,
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(moved.unwrap(), container('b'));
    client.assert_done();
    assert_eq!(
        gate.reader.reads, 2,
        "one read before the stop, one before the removal"
    );
    assert_eq!(
        gate.at_removal,
        AtRemoval::Pointed(pointer('a', &[], &[]).deploy),
        "the stopped copy is left for its pointer's retire path"
    );
}

#[tokio::test]
async fn an_unreadable_pointer_after_the_stop_reports_the_old_container_stopped() {
    use crate::deploy::convergence::{AtRemoval, OldCopy, OldCopyGate};
    use crate::serving_publish::PointerClient;
    use crate::serving_publish::tests::FakeRow;

    struct UnreadableOnSecondRead {
        row: FakeRow,
        reads: u32,
    }

    impl PointerClient for UnreadableOnSecondRead {
        async fn pointer_publisher(&mut self) -> Result<ployz_core::MachineTarget, RpcError> {
            self.row.pointer_publisher().await
        }

        async fn read_pointer_at(
            &mut self,
            at: &mut ployz_core::MachineTarget,
            service: &QualifiedService,
        ) -> Result<Option<ployz_core::ServingPointer>, RpcError> {
            self.reads += 1;
            if self.reads == 2 {
                return Err(unavailable("the publisher is away"));
            }
            self.row.read_pointer_at(at, service).await
        }

        async fn publish_pointer_at(
            &mut self,
            at: &mut ployz_core::MachineTarget,
            request: ployz_core::PublishServingPointerRequest,
        ) -> Result<ployz_core::ServingPointerPublication, RpcError> {
            self.row.publish_pointer_at(at, request).await
        }
    }

    let client = Scripted::new(vec![
        created(
            Call::Run(machine(TO), ContainerKind::ServiceContainer),
            &container('b'),
        ),
        ok(Call::Stop(machine(FROM), container('a'))),
    ]);
    let service = QualifiedService::parse("app/web").unwrap();
    let (namespace, spec, to, from, old) = (
        test_namespace(),
        spec(Some(0), None, None),
        machine(TO),
        machine(FROM),
        container('a'),
    );
    let mut gate = OldCopyGate {
        at_removal: AtRemoval::of(OldCopy::Remove, &old),
        reader: UnreadableOnSecondRead {
            row: FakeRow::default(),
            reads: 0,
        },
        service: &service,
        old: &old,
    };
    let moved = move_container(
        &client,
        &namespace,
        (&spec, None),
        &to,
        (&from, &old),
        &mut gate,
        &CancellationToken::new(),
    )
    .await;
    assert_eq!(moved.unwrap(), container('b'), "the replacement stays");
    client.assert_done();
    assert_eq!(
        gate.at_removal,
        AtRemoval::Unread {
            detail: crate::ui::row(&unavailable("the publisher is away")),
            old_stopped: true,
        },
        "the stop that already happened is reported"
    );
}
