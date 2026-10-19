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
}

impl Recording {
    fn of(fake: Scripted) -> Self {
        Self {
            fake,
            wants: Mutex::new(Vec::new()),
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
        Command::Stop => cleanup::user_stop(&recording, &machine, &copy, none(), 1)
            .await
            .map(|()| None),
        Command::Start => cleanup::user_start(&recording, &machine, &copy)
            .await
            .map(Some),
        Command::Restart => cleanup::user_restart(&recording, &machine, &copy, none())
            .await
            .map(Some),
        Command::Remove => cleanup::user_remove(&recording, &machine, &copy)
            .await
            .map(|()| None),
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

    let ran = row(
        Command::Start,
        CopyRotation::Pending,
        true,
        vec![inspected(), copy_call(Call::Restart)],
    )
    .await;
    assert_eq!(ran.answer.unwrap(), Some(UserStarted::In));
    assert_eq!(ran.rotation, Some(CopyRotation::In));
    assert_eq!(ran.wants, [brought_in(None, CopyRotation::Pending)]);

    let ran = row(Command::Start, CopyRotation::In, true, vec![inspected()]).await;
    assert_eq!(ran.answer.unwrap(), Some(UserStarted::In));
    assert_eq!(ran.wants, [brought_in(None, CopyRotation::In)]);

    let mut fake = Scripted::new(vec![inspected()]).with_copy(
        &machine('a'),
        &container('1'),
        CopyRotation::Pending,
    );
    let verdict = HealthVerdict::TimedOut {
        deadline_millis: Some(1_000),
        last_check: None,
    };
    fake.unhealthy_in.insert(container('1'), verdict.clone());
    let ran = run_command(Command::Start, Recording::of(fake)).await;
    assert_eq!(ran.answer.unwrap(), Some(UserStarted::NotIn(Some(verdict))));
    assert_eq!(ran.rotation, Some(CopyRotation::Pending));

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
    assert_eq!(ran.answer.unwrap(), Some(UserStarted::Running));
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
