use std::time::Duration;

use ployz_core::{
    AnonymousVolumes, ContainerDetails, ContainerLocator, ContainerNow, ContainerObservation,
    ContainerWant, CopyRotation, DeploymentLogId, DockerVolumeId, Drain, EnsuredContainer,
    HealthVerdict, ImagePulled, MachineId, QualifiedService, ResolvedServiceSpec, RoutesRead,
    RpcError, RpcErrorCode, RunRotation, RunTarget, SlotKey, StopOptions,
};

use super::*;
use crate::deploy::cleanup::{self, UserStarted};

struct Recording {
    fake: Scripted,
    wants: Mutex<Vec<ContainerWant>>,
    stop_deadlines: Mutex<Vec<Option<Duration>>>,
}

impl Recording {
    fn of(fake: Scripted) -> Self {
        Self {
            fake,
            wants: Mutex::new(Vec::new()),
            stop_deadlines: Mutex::new(Vec::new()),
        }
    }

    fn wants(&self) -> Vec<ContainerWant> {
        self.wants.lock().unwrap().clone()
    }
}

impl MachineOperations for Recording {
    async fn prepare_volumes(
        &self,
        machine_id: &MachineId,
        specs: &[ployz_core::ServiceStorageSpec],
    ) -> Result<ployz_core::PreparedVolumes, RpcError> {
        self.fake.prepare_volumes(machine_id, specs).await
    }

    async fn service_containers(
        &self,
        service: &QualifiedService,
    ) -> Result<Vec<ContainerObservation>, RpcError> {
        self.fake.service_containers(service).await
    }

    async fn ensure_container(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
    ) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError> {
        self.wants.lock().unwrap().push(want.clone());
        self.fake.ensure_container(machine_id, want).await
    }

    async fn inspect_container(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
    ) -> Result<ContainerDetails, RpcError> {
        self.fake.inspect_container(machine_id, container_id).await
    }

    async fn remove_volume(&self, id: &DockerVolumeId) -> Result<(), RpcError> {
        self.fake.remove_volume(id).await
    }

    async fn update_container_limits(
        &self,
        machine_id: &MachineId,
        container_id: &ContainerId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ContainerLimitsUpdated, RpcError> {
        self.fake
            .update_container_limits(machine_id, container_id, spec)
            .await
    }

    async fn runs_global(
        &self,
        machine_id: &MachineId,
        slot: &SlotKey,
        spec: &ResolvedServiceSpec,
    ) -> Result<bool, RpcError> {
        self.fake.runs_global(machine_id, slot, spec).await
    }

    async fn ensure_image(
        &self,
        machine_id: &MachineId,
        spec: &ResolvedServiceSpec,
    ) -> Result<ImagePulled, RpcError> {
        self.fake.ensure_image(machine_id, spec).await
    }

    async fn ensure_unsettled(
        &self,
        machine_id: &MachineId,
        want: ContainerWant,
        timeout: Option<Duration>,
    ) -> Result<EnsuredContainer, RpcError> {
        if matches!(want, ContainerWant::Stopped { .. }) {
            self.stop_deadlines.lock().unwrap().push(timeout);
        }
        self.wants.lock().unwrap().push(want.clone());
        self.fake.ensure_unsettled(machine_id, want, timeout).await
    }

    async fn read_routes(
        &self,
        machine_id: &MachineId,
        service: &QualifiedService,
    ) -> Result<RoutesRead, RpcError> {
        self.fake.read_routes(machine_id, service).await
    }

    async fn read_entry_routes(&self, service: &QualifiedService) -> Result<RoutesRead, RpcError> {
        self.fake.read_entry_routes(service).await
    }
}

#[derive(Clone, Copy, Debug)]
enum Command {
    Stop,
    Start,
    Restart,
    Remove,
}

struct Ran {
    answer: Result<Option<UserStarted>, RpcError>,
    wants: Vec<ContainerWant>,
    rotation: Option<CopyRotation>,
    fake: Scripted,
}

async fn run_command(command: Command, recording: Recording) -> Ran {
    let (machine, copy) = (machine('a'), container('1'));
    let none = || StopOptions {
        signal: None,
        grace_period_seconds: None,
    };
    let answer = match command {
        Command::Stop => cleanup::user_stop(
            &recording,
            &machine,
            &copy,
            none(),
            1,
            &CancellationToken::new(),
        )
        .await
        .map(|()| None),
        Command::Start => cleanup::user_start(&recording, &machine, &copy)
            .await
            .map(Some),
        Command::Restart => cleanup::user_restart(
            &recording,
            &machine,
            &copy,
            none(),
            1,
            &CancellationToken::new(),
        )
        .await
        .map(Some),
        Command::Remove => {
            cleanup::user_remove(&recording, &machine, &copy, &CancellationToken::new())
                .await
                .map(|()| None)
        }
    };
    recording.fake.assert_done();
    Ran {
        answer,
        wants: recording.wants(),
        rotation: recording.fake.rotation(&copy),
        fake: recording.fake,
    }
}

async fn row(command: Command, rotation: CopyRotation, stopped: bool, steps: Vec<Step>) -> Ran {
    let (machine, copy) = (machine('a'), container('1'));
    let fake = Scripted::new(steps).with_copy(&machine, &copy, rotation);
    fake.record(copy, |recorded| recorded.stopped = stopped);
    run_command(command, Recording::of(fake)).await
}

fn inspected() -> Step {
    observed(Call::Inspect(machine('a'), container('1')), running())
}

fn copy_call(call: fn(MachineId, ContainerId) -> Call) -> Step {
    ok(call(machine('a'), container('1')))
}

fn id() -> ContainerLocator {
    ContainerLocator::Id(container('1'))
}

fn held(run: &DeploymentLogId, expect: CopyRotation) -> ContainerWant {
    ContainerWant::Held {
        container: id(),
        run: run.clone(),
        expect: Some(expect),
    }
}

fn stopped() -> ContainerWant {
    ContainerWant::Stopped {
        container: id(),
        drain: Drain::DEFAULT,
        stop: StopOptions {
            signal: None,
            grace_period_seconds: None,
        },
    }
}

fn brought_in(run: Option<&DeploymentLogId>, expect: CopyRotation) -> ContainerWant {
    ContainerWant::Running {
        container: RunTarget::Existing(id()),
        rotation: RunRotation::In {
            run: run.cloned(),
            expect: Some(expect),
        },
    }
}

fn started_pending() -> ContainerWant {
    ContainerWant::Running {
        container: RunTarget::Existing(id()),
        rotation: RunRotation::Pending,
    }
}

fn out(expect: CopyRotation) -> ContainerWant {
    ContainerWant::Out {
        container: id(),
        allow_empty: true,
        expect: Some(expect),
    }
}

fn gone() -> ContainerWant {
    ContainerWant::Gone {
        container: id(),
        drain: Drain::DEFAULT,
        anonymous_volumes: AnonymousVolumes::Remove,
    }
}

fn held_run(rotation: &Option<CopyRotation>) -> DeploymentLogId {
    match rotation {
        Some(CopyRotation::Held(run)) => run.clone(),
        other => panic!("expected a Held copy, found {other:?}"),
    }
}

/// Named, with the rotation it keeps, and never routed by a user command.
fn not_admitted(ran: &Ran, rotation: &str) {
    let error = ran.answer.as_ref().unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
    assert!(
        error.message.contains(&container('1').to_string())
            && error.message.contains(&format!(
                "left {rotation} and not routed: no Deploy brought it In"
            )),
        "{error:?}"
    );
}

fn conflict(ran: &Ran) {
    let error = ran.answer.as_ref().unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Conflict, "{error:?}");
    assert!(
        error.message.contains("Deploy the Service again"),
        "{error:?}"
    );
}

#[tokio::test(start_paused = true)]
async fn user_stop_start_restart_remove() {
    let theirs = DeploymentLogId::parse("stopped-before").unwrap();

    for rotation in [CopyRotation::In, CopyRotation::Pending] {
        let ran = row(
            Command::Stop,
            rotation.clone(),
            false,
            vec![inspected(), copy_call(Call::Stop)],
        )
        .await;
        ran.answer.unwrap();
        let run = held_run(&ran.rotation);
        assert_ne!(run, theirs);
        assert_eq!(ran.wants, [held(&run, rotation), stopped()]);
    }

    let ran = row(
        Command::Stop,
        CopyRotation::Held(theirs.clone()),
        false,
        vec![inspected(), copy_call(Call::Stop)],
    )
    .await;
    ran.answer.unwrap();
    assert_eq!(ran.rotation, Some(CopyRotation::Held(theirs.clone())));
    assert_eq!(
        ran.wants,
        [held(&theirs, CopyRotation::Held(theirs.clone())), stopped()]
    );

    let ran = row(
        Command::Stop,
        CopyRotation::Out,
        false,
        vec![inspected(), copy_call(Call::Stop)],
    )
    .await;
    ran.answer.unwrap();
    assert_eq!(ran.rotation, Some(CopyRotation::Out));
    assert_eq!(ran.wants, [stopped()]);

    let mut fake = Scripted::new(vec![inspected()]).with_copy(
        &machine('a'),
        &container('1'),
        CopyRotation::In,
    );
    fake.acks.insert(machine('b'), Ack::Never);
    let ran = run_command(Command::Stop, Recording::of(fake)).await;
    let error = ran.answer.as_ref().unwrap_err();
    assert_eq!(error.code, RpcErrorCode::Unavailable, "{error:?}");
    assert!(error.message.contains("left running and Held"), "{error:?}");
    assert!(
        error.message.contains(&machine('b').to_string()),
        "{error:?}"
    );
    let run = held_run(&ran.rotation);
    assert_eq!(ran.wants, [held(&run, CopyRotation::In)]);

    let ran = row(
        Command::Start,
        CopyRotation::Held(theirs.clone()),
        true,
        vec![inspected(), copy_call(Call::Restart)],
    )
    .await;
    assert_eq!(ran.answer.unwrap(), Some(UserStarted::In));
    assert_eq!(ran.rotation, Some(CopyRotation::In));
    assert_eq!(
        ran.wants,
        [brought_in(
            Some(&theirs),
            CopyRotation::Held(theirs.clone())
        )]
    );

    for command in [Command::Start, Command::Restart] {
        let steps = match command {
            Command::Restart => vec![inspected(), copy_call(Call::Stop), copy_call(Call::Restart)],
            Command::Start | Command::Stop | Command::Remove => {
                vec![inspected(), copy_call(Call::Restart)]
            }
        };
        let ran = row(command, CopyRotation::Pending, true, steps).await;
        not_admitted(&ran, "Pending");
        assert_eq!(ran.rotation, Some(CopyRotation::Pending));
        assert_eq!(ran.wants.last(), Some(&started_pending()), "{command:?}");
    }

    let ran = row(Command::Start, CopyRotation::In, true, vec![inspected()]).await;
    assert_eq!(ran.answer.unwrap(), Some(UserStarted::In));
    assert_eq!(ran.wants, [brought_in(None, CopyRotation::In)]);

    let mut fake = Scripted::new(vec![inspected()]).with_copy(
        &machine('a'),
        &container('1'),
        CopyRotation::Held(theirs.clone()),
    );
    let verdict = HealthVerdict::TimedOut {
        deadline_millis: Some(1_000),
        last_check: None,
        oom_killed: false,
    };
    fake.unhealthy_in.insert(container('1'), verdict.clone());
    let ran = run_command(Command::Start, Recording::of(fake)).await;
    assert_eq!(ran.answer.unwrap(), Some(UserStarted::NotIn(Some(verdict))));
    assert_eq!(ran.rotation, Some(CopyRotation::Held(theirs.clone())));

    for command in [Command::Start, Command::Restart] {
        let ran = row(command, CopyRotation::Out, true, vec![inspected()]).await;
        conflict(&ran);
        assert!(ran.wants.is_empty());
        assert_eq!(ran.rotation, Some(CopyRotation::Out));
    }

    let ran = row(
        Command::Restart,
        CopyRotation::In,
        false,
        vec![inspected(), copy_call(Call::Stop), copy_call(Call::Restart)],
    )
    .await;
    assert_eq!(ran.answer.unwrap(), Some(UserStarted::In));
    assert_eq!(ran.rotation, Some(CopyRotation::In));
    let [held_want, stopped_want, in_want] = ran.wants.as_slice() else {
        panic!("a restart sends three wants: {:?}", ran.wants);
    };
    let ContainerWant::Held { run, .. } = held_want else {
        panic!("a restart holds first: {held_want:?}");
    };
    assert_eq!(held_want, &held(run, CopyRotation::In));
    assert_eq!(stopped_want, &stopped());
    assert_eq!(
        in_want,
        &brought_in(Some(run), CopyRotation::Held(run.clone()))
    );
    assert_eq!(
        ran.fake.moves(),
        [
            Move::To(container('1'), CopyRotation::Held(run.clone())),
            Move::To(container('1'), CopyRotation::In),
        ]
    );

    let ran = row(
        Command::Restart,
        CopyRotation::Held(theirs.clone()),
        false,
        vec![inspected()],
    )
    .await;
    assert_eq!(ran.answer.unwrap(), Some(UserStarted::In));
    assert_eq!(
        ran.wants,
        [brought_in(
            Some(&theirs),
            CopyRotation::Held(theirs.clone())
        )]
    );

    let ran = row(
        Command::Restart,
        CopyRotation::Pending,
        false,
        vec![inspected(), copy_call(Call::Stop), copy_call(Call::Restart)],
    )
    .await;
    not_admitted(&ran, "Pending");
    assert_eq!(ran.rotation, Some(CopyRotation::Pending));
    assert_eq!(ran.wants, [stopped(), started_pending()]);

    for (rotation, was_stopped) in [
        (CopyRotation::In, true),
        (CopyRotation::Held(theirs.clone()), false),
    ] {
        let ran = row(
            Command::Remove,
            rotation.clone(),
            was_stopped,
            vec![inspected(), copy_call(Call::Remove)],
        )
        .await;
        ran.answer.unwrap();
        assert_eq!(ran.rotation, None);
        assert_eq!(ran.wants, [out(rotation), gone()]);
        assert_eq!(
            ran.fake.moves(),
            [
                Move::To(container('1'), CopyRotation::Out),
                Move::Gone(container('1')),
            ]
        );
    }

    for rotation in [CopyRotation::Pending, CopyRotation::Out] {
        let ran = row(
            Command::Remove,
            rotation,
            false,
            vec![inspected(), copy_call(Call::Remove)],
        )
        .await;
        ran.answer.unwrap();
        assert_eq!(ran.rotation, None);
        assert_eq!(ran.wants, [gone()]);
    }

    let fake = Scripted::new(vec![Step(
        Call::Inspect(machine('a'), container('1')),
        Reply::Error(RpcError {
            code: RpcErrorCode::NotFound,
            message: "no such container".into(),
            details: serde_json::Value::Null,
            cause: Vec::new(),
        }),
    )]);
    let ran = run_command(Command::Remove, Recording::of(fake)).await;
    ran.answer.unwrap();
    assert!(ran.wants.is_empty());

    for (command, call, want) in [
        (Command::Stop, Call::Stop as fn(_, _) -> _, stopped()),
        (Command::Start, Call::Restart, started_pending()),
        (Command::Remove, Call::Remove, gone()),
    ] {
        let fake = Scripted::new(vec![
            observed_ingress(Call::Inspect(machine('a'), container('1')), running()),
            copy_call(call),
        ]);
        let ran = run_command(command, Recording::of(fake)).await;
        ran.answer.unwrap();
        assert_eq!(ran.wants, [want]);
        assert!(
            ran.fake
                .moves()
                .iter()
                .all(|applied| !matches!(applied, Move::To(..)))
        );
    }
}

/// After a partial recovery a gen 3 copy runs beside a gen 4 copy that is In. The owner
/// refuses the older copy In, and the user rows of the caller table never retire a
/// copy, so the command names the copy and the rotation it is left in.
#[tokio::test(start_paused = true)]
async fn a_start_or_restart_of_a_copy_older_than_an_in_copy_reports_it_left_unrouted() {
    let theirs = DeploymentLogId::parse("stopped-before").unwrap();
    let newer = container('2');
    for (command, rotation, stopped, steps) in [
        (
            Command::Restart,
            CopyRotation::In,
            false,
            vec![inspected(), copy_call(Call::Stop)],
        ),
        (
            Command::Start,
            CopyRotation::Held(theirs.clone()),
            true,
            vec![inspected()],
        ),
    ] {
        let fake = Scripted::new(steps)
            .with_copy_at(
                &api(),
                &machine('a'),
                &container('1'),
                rotation,
                ployz_core::Gen::new(3),
            )
            .with_copy_at(
                &api(),
                &machine('b'),
                &newer,
                CopyRotation::In,
                ployz_core::Gen::new(4),
            );
        fake.record(container('1'), |recorded| recorded.stopped = stopped);
        let ran = run_command(command, Recording::of(fake)).await;
        let error = ran.answer.as_ref().unwrap_err();
        assert_eq!(error.code, RpcErrorCode::Conflict, "{command:?}: {error:?}");
        assert!(
            error.message.contains(&container('1').to_string())
                && error.message.contains("left Held and not routed"),
            "{command:?} must name the copy it left: {error:?}"
        );
        assert!(
            matches!(ran.rotation, Some(CopyRotation::Held(_))),
            "{command:?}: {:?}",
            ran.rotation
        );
        assert_eq!(ran.fake.rotation(&newer), Some(CopyRotation::In));
    }
}

/// Ctrl-C during the unlisted wait ends the command at once. The copy it withdrew
/// goes back In rather than staying Held with nothing said.
#[tokio::test(start_paused = true)]
async fn a_cancelled_restart_or_stop_brings_the_copy_back_in() {
    for command in [Command::Restart, Command::Stop] {
        let mut fake = Scripted::new(vec![inspected()]).with_copy(
            &machine('a'),
            &container('1'),
            CopyRotation::In,
        );
        fake.acks.insert(machine('b'), Ack::Never);
        let recording = Recording::of(fake);
        let cancellation = CancellationToken::new();
        let cancel = cancellation.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(5)).await;
            cancel.cancel();
        });
        let none = || StopOptions {
            signal: None,
            grace_period_seconds: None,
        };
        let begun = tokio::time::Instant::now();
        let error = match command {
            Command::Restart => cleanup::user_restart(
                &recording,
                &machine('a'),
                &container('1'),
                none(),
                1,
                &cancellation,
            )
            .await
            .map(drop),
            Command::Stop => {
                cleanup::user_stop(
                    &recording,
                    &machine('a'),
                    &container('1'),
                    none(),
                    1,
                    &cancellation,
                )
                .await
            }
            Command::Start | Command::Remove => unreachable!(),
        }
        .unwrap_err();

        assert!(begun.elapsed() < cleanup::ACK_WAIT, "{command:?}");
        assert_eq!(
            error.code,
            RpcErrorCode::Unavailable,
            "{command:?}: {error:?}"
        );
        assert!(
            error.message.contains("cancelled") && error.message.contains("back In"),
            "{command:?}: {error:?}"
        );
        recording.fake.assert_done();
        assert_eq!(
            recording.fake.rotation(&container('1')),
            Some(CopyRotation::In)
        );
        assert!(
            !recording
                .wants()
                .iter()
                .any(|want| matches!(want, ContainerWant::Stopped { .. })),
            "{command:?} stopped a copy it never confirmed unlisted"
        );
    }
}

/// A restart queued behind others on its Machine waits out their stops too,
/// on both the rotation path and the Pending path.
#[tokio::test(start_paused = true)]
async fn a_queued_restart_waits_for_the_stops_ahead_of_it() {
    for rotation in [CopyRotation::In, CopyRotation::Pending] {
        let fake = Scripted::new(vec![
            inspected(),
            copy_call(Call::Stop),
            copy_call(Call::Restart),
        ])
        .with_copy(&machine('a'), &container('1'), rotation.clone());
        let recording = Recording::of(fake);
        let stop = StopOptions {
            signal: None,
            grace_period_seconds: None,
        };
        let restarted = cleanup::user_restart(
            &recording,
            &machine('a'),
            &container('1'),
            stop,
            3,
            &CancellationToken::new(),
        )
        .await;
        // A Pending copy restarts but is not admitted.
        assert_eq!(
            restarted.is_ok(),
            rotation == CopyRotation::In,
            "{restarted:?}"
        );
        recording.fake.assert_done();
        assert_eq!(
            *recording.stop_deadlines.lock().unwrap(),
            // Docker's default grace, once for each stop queued on the Machine.
            [crate::connect::stop_rpc_timeout(Some(10), 3)],
            "{rotation:?}"
        );
    }
}

/// The copy is In, running, not stopped, and every live proxy lists it.
fn assert_serving(fake: &Scripted, command: Command) {
    let copy = container('1');
    assert_eq!(fake.rotation(&copy), Some(CopyRotation::In), "{command:?}");
    let service = {
        let copies = fake.copies.lock().unwrap();
        let recorded = copies.get(&copy).unwrap();
        assert!(!recorded.stopped, "{command:?} left the copy stopped");
        recorded.service.clone()
    };
    let routes = fake.routes(&service, false);
    assert!(!routes.is_empty(), "{command:?}");
    for (machine, routed) in routes {
        assert!(
            routed.is_some_and(|routed| routed.contains(&copy)),
            "{command:?}: the proxy on {machine} dropped the copy"
        );
    }
}

fn assert_failed_back_in(ran: &Ran, command: Command) {
    let error = ran.answer.as_ref().unwrap_err();
    assert!(error.message.contains("back In"), "{command:?}: {error:?}");
    assert!(
        !ran.wants
            .iter()
            .any(|want| matches!(want, ContainerWant::Stopped { .. })),
        "{command:?} stopped a copy it never confirmed unlisted"
    );
    assert_serving(&ran.fake, command);
}

/// The Held lands and the route read then fails: the command fails, and the
/// copy it withdrew goes back In rather than staying Held unrouted.
#[tokio::test(start_paused = true)]
async fn a_route_read_error_after_held_brings_the_copy_back_in() {
    for command in [Command::Restart, Command::Stop] {
        let fake = Scripted {
            failed_route_reads: Mutex::new(1),
            ..Scripted::new(vec![inspected()])
        }
        .with_copy(&machine('a'), &container('1'), CopyRotation::In);
        let ran = run_command(command, Recording::of(fake)).await;
        assert_failed_back_in(&ran, command);
    }
}

/// The Held ask lands unanswered and the re-read of the copy fails: whether
/// it landed is unknown, so the copy is brought back In as if it had.
#[tokio::test(start_paused = true)]
async fn an_unanswered_held_ask_whose_re_read_fails_brings_the_copy_back_in() {
    for command in [Command::Restart, Command::Stop] {
        let fake = Scripted {
            unanswered_held: BTreeSet::from([container('1')]),
            ..Scripted::new(vec![
                inspected(),
                failed(Call::Inspect(machine('a'), container('1')), "no answer"),
            ])
        }
        .with_copy(&machine('a'), &container('1'), CopyRotation::In);
        let ran = run_command(command, Recording::of(fake)).await;
        assert_failed_back_in(&ran, command);
    }
}

/// A gen 3 copy left Pending beside an In gen 4 copy is restarted, and named left
/// unrouted with the reason, as start names it.
#[tokio::test(start_paused = true)]
async fn a_restart_of_a_pending_copy_older_than_an_in_copy_reports_it_left_unrouted() {
    let newer = container('2');
    let fake = Scripted::new(vec![
        inspected(),
        copy_call(Call::Stop),
        copy_call(Call::Restart),
    ])
    .with_copy_at(
        &api(),
        &machine('a'),
        &container('1'),
        CopyRotation::Pending,
        ployz_core::Gen::new(3),
    )
    .with_copy_at(
        &api(),
        &machine('b'),
        &newer,
        CopyRotation::In,
        ployz_core::Gen::new(4),
    );
    let ran = run_command(Command::Restart, Recording::of(fake)).await;

    not_admitted(&ran, "Pending");
    assert_eq!(ran.rotation, Some(CopyRotation::Pending));
    assert!(
        !ran.fake
            .copies
            .lock()
            .unwrap()
            .get(&container('1'))
            .unwrap()
            .stopped,
        "the restart left the copy stopped"
    );
    assert_eq!(ran.fake.rotation(&newer), Some(CopyRotation::In));
}

/// A Deploy failed after creating a gen 9 copy on b, which stayed Pending beside
/// the configured gen 8 copies. Stop, start and restart never bring that copy In,
/// so the configured copies keep serving and are never refused as older.
#[tokio::test(start_paused = true)]
async fn stop_start_restart_never_admit_a_failed_deploys_pending_copy() {
    let failed = DeploymentLogId::parse("failed-deploy").unwrap();
    let configured = [
        (machine('a'), container('1')),
        (machine('b'), container('2')),
    ];
    let stray = (machine('b'), container('9'));
    let mut fake = Scripted::new(Vec::new());
    for (machine, copy) in &configured {
        fake = fake.with_copy_at(
            &api(),
            machine,
            copy,
            CopyRotation::In,
            ployz_core::Gen::new(8),
        );
    }
    fake = fake.with_copy_at(
        &api(),
        &stray.0,
        &stray.1,
        CopyRotation::Pending,
        ployz_core::Gen::new(9),
    );
    fake.record(stray.1, |recorded| recorded.deploy = Some(failed.clone()));
    // The stray copy goes first: a Pending sibling keeps the route of the last
    // configured copy, as it would were the stops not concurrent.
    let order = [stray, configured[0], configured[1]];
    let none = || StopOptions {
        signal: None,
        grace_period_seconds: None,
    };
    let script = |calls: &[fn(MachineId, ContainerId) -> Call]| {
        order
            .iter()
            .flat_map(|(machine, copy)| {
                std::iter::once(observed(Call::Inspect(*machine, *copy), running()))
                    .chain(calls.iter().map(|call| ok(call(*machine, *copy))))
            })
            .collect::<Vec<_>>()
    };
    let cancellation = CancellationToken::new();

    *fake.steps.lock().unwrap() = script(&[Call::Stop]).into();
    for (machine, copy) in &order {
        cleanup::user_stop(&fake, machine, copy, none(), 1, &cancellation)
            .await
            .unwrap();
    }
    fake.assert_done();
    assert_eq!(
        fake.rotation(&stray.1),
        Some(CopyRotation::Held(failed.clone()))
    );

    *fake.steps.lock().unwrap() = script(&[Call::Restart]).into();
    let mut started = Vec::new();
    for (machine, copy) in &order {
        started.push(cleanup::user_start(&fake, machine, copy).await);
    }
    assert_ne!(fake.rotation(&stray.1), Some(CopyRotation::In));
    for (_, copy) in &configured {
        assert_eq!(fake.rotation(copy), Some(CopyRotation::In), "{started:?}");
    }
    let skipped = started.first().unwrap().as_ref().unwrap_err();
    assert!(
        skipped.message.contains(&stray.1.to_string()) && skipped.message.contains("not routed"),
        "{skipped:?}"
    );
    fake.assert_done();

    *fake.steps.lock().unwrap() = script(&[Call::Stop, Call::Restart]).into();
    let mut restarted = Vec::new();
    for (machine, copy) in &order {
        restarted.push(cleanup::user_restart(&fake, machine, copy, none(), 1, &cancellation).await);
    }
    fake.assert_done();
    let skipped = restarted.first().unwrap().as_ref().unwrap_err();
    assert!(
        skipped.message.contains(&stray.1.to_string()) && skipped.message.contains("not routed"),
        "{skipped:?}"
    );
    for (result, (_, copy)) in restarted.iter().skip(1).zip(&configured) {
        assert_eq!(result.as_ref().ok(), Some(&UserStarted::In), "{result:?}");
        assert_eq!(fake.rotation(copy), Some(CopyRotation::In));
    }
    assert_ne!(fake.rotation(&stray.1), Some(CopyRotation::In));
    let routes = fake.routes(&api(), false);
    for (machine, routed) in routes {
        let routed = routed.unwrap_or_default();
        assert!(
            configured.iter().all(|(_, copy)| routed.contains(copy)) && !routed.contains(&stray.1),
            "the proxy on {machine} routes {routed:?}"
        );
    }
}
