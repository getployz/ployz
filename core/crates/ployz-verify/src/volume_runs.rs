//! A Move of one Volume from A to B, the Release that takes it over, and every late
//! `SetVolumeCopy` the dead Move can still deliver.
//!
//! DESIGN.md bet 6 promises that a late step from a dead run leaves a Volume with at
//! most one Writer. This model checks that promise, and what each run reports, over
//! every interleaving of:
//!
//! - the Move (`moveVolume` in `dashboard/src/modules/volume-run/volume-run.inngest.ts`)
//!   from its cut: cut A, pull the cut's snapshot to B, commit, start B, remove A's
//!   kept Container;
//! - Cloud ending the Move's row at any moment, which hands the Volume to a Release:
//!   back to A while warm (`releaseVolume`), forward to B once committed
//!   (`finishRelease`);
//! - the Move's driver carrying on after its row ended, as a cancelled Inngest run
//!   can, and resending any request, so an earlier copy lands late;
//! - each Machine running `SetVolumeCopy`
//!   (`ployzd/src/machine_api/local_volume_copy.rs`), with each lock and floor check
//!   of the code as its own step, and the transition picked by the real
//!   [`ployz_core::plan`];
//! - A's daemon restarting halfway through the cut's flip.
//!
//! Out of scope: Deploy lanes, operator collisions, Machines that do not answer,
//! warm rounds, and a second takeover.

use std::net::Ipv6Addr;

use ployz_core::{
    ContainerId, CopyPlan, CopyRefusal, CopyTransition, Drain, ExpectedCopy, ManagementAddress,
    SlotId, Snapshot, SnapshotGuid, Start, VolumeCopy, WantedCopy, WriterEpoch,
};
use stateright::{Checker, HasDiscoveries, Model, Property};

/// The Move's lane `gen`, and the Release's after it.
const MOVE_GEN: u8 = 1;
const RELEASE_GEN: u8 = 2;
/// A's Writer epoch before the Move, the Move's, and the Release's from `takeEpoch`.
const OLD_EPOCH: u32 = 1;
const MOVE_EPOCH: u32 = 2;
const RELEASE_EPOCH: u32 = 3;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Machine {
    A,
    B,
}

impl Machine {
    const ALL: [Self; 2] = [Self::A, Self::B];

    fn other(self) -> Self {
        match self {
            Self::A => Self::B,
            Self::B => Self::A,
        }
    }
}

/// The warm round's snapshot, which B already holds, and the cut's final `f-<epoch>`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Snap {
    Warm,
    Final,
}

/// What a Machine holds of the Volume: `VolumeCopy` without the fields no step reads.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Held {
    None,
    Root {
        readonly: bool,
        epoch: u32,
        newest: Option<Snap>,
    },
    Slot {
        newest: Option<Snap>,
    },
}

impl Held {
    fn newest(self) -> Option<Snap> {
        match self {
            Self::None => None,
            Self::Root { newest, .. } | Self::Slot { newest } => newest,
        }
    }

    fn is_writer(self) -> bool {
        matches!(
            self,
            Self::Root {
                readonly: false,
                ..
            }
        )
    }
}

/// The Service Container that mounts the Volume on one Machine.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Holder {
    Absent,
    Stopped,
    Running,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Host {
    copy: Held,
    /// `volume_floors`: the newest lane `gen` this Machine admitted for the Volume.
    floor: u8,
    holder: Holder,
    /// `MachineService::switch_mutation`, held by one request from its second floor
    /// check to its reply.
    switch: Option<usize>,
    /// The exclusive admission turn, held by `start_volume_holder` from its floor
    /// check to the start.
    turn: Option<usize>,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Lane {
    Move,
    Release,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Expect {
    Writer { epoch: u32 },
    Mirror { newest: Snap },
    None,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum StartKind {
    Kept,
    Create,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Want {
    Writer {
        at: Snap,
        epoch: u32,
        start: Option<StartKind>,
    },
    Mirror {
        from: Option<Machine>,
        target: Snap,
    },
    Forget,
    Floor,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Refusal {
    Stale,
    Busy,
    Mismatch,
    NoTransition,
    Behind,
    Other,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Reply {
    Copy(Held),
    PullFailed,
    Refused(Refusal),
}

/// Where one `SetVolumeCopy` is on its Machine.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Progress {
    /// On the wire. A resend leaves this copy here to land whenever it lands.
    Sent,
    /// Past the first `admit_gen`, queued on the switch lock.
    Admitted,
    /// `cut_volume_copy` with a holder: next, the floor check before its drain.
    CutDrain,
    /// `stop_behind_ingress` after the drain: the floor check and the stop, under the turn.
    CutStop,
    /// The Busy re-check, the floor check and `zfs`, under the turn and the floor lock.
    CutFlip,
    /// The Writer route ran; next `run_start` takes the turn and checks the floor.
    StartAdmit,
    /// `start_volume_holder` holds the turn past its floor check.
    StartRun,
    Replied(Reply),
    /// The daemon restarted under it; no reply comes.
    Lost,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Request {
    lane: Lane,
    to: Machine,
    r#gen: u8,
    expect: Expect,
    want: Want,
    progress: Progress,
}

/// A run's row in Cloud's `volume_run` table.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Row {
    Warm,
    Committed,
    Done,
    Failed,
}

impl Row {
    fn running(self) -> bool {
        matches!(self, Self::Warm | Self::Committed)
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum MoveStep {
    Cut,
    Pull,
    Commit,
    Start,
    Remove,
    Finish,
    Over,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum ReleaseStep {
    Floor(Machine),
    Plan,
    Inspect(Machine),
    Forget(Machine),
    Resume(Machine),
    RemoveBehind(Machine),
    Observe,
    StartForward,
    RemoveForward,
    Finish,
    Over,
}

/// One Inngest run: where it is, the request it waits on, and what retries it has left.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Driver<S> {
    step: S,
    waiting: Option<usize>,
    resends: u8,
    busy: u8,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Resume {
    on: Machine,
    expect: Expect,
    at: Snap,
    start: Option<StartKind>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Release {
    row: Row,
    forward: bool,
    driver: Driver<ReleaseStep>,
    views: [Option<Held>; 2],
    resume: Option<Resume>,
    behind: Option<Machine>,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct State {
    a: Host,
    b: Host,
    requests: Vec<Request>,
    row: Row,
    moving: Driver<MoveStep>,
    release: Option<Release>,
    crashes: u8,
}

impl State {
    fn host(&self, machine: Machine) -> &Host {
        match machine {
            Machine::A => &self.a,
            Machine::B => &self.b,
        }
    }

    fn host_mut(&mut self, machine: Machine) -> &mut Host {
        match machine {
            Machine::A => &mut self.a,
            Machine::B => &mut self.b,
        }
    }

    fn request(&self, index: usize) -> Request {
        *self
            .requests
            .get(index)
            .expect("an action names a sent request")
    }

    fn set_progress(&mut self, index: usize, progress: Progress) {
        self.requests
            .get_mut(index)
            .expect("an action names a sent request")
            .progress = progress;
    }

    fn reply(&mut self, index: usize, reply: Reply) {
        self.reply_keeping(index, reply, false);
    }

    fn reply_keeping(&mut self, index: usize, reply: Reply, keep_switch: bool) {
        let to = self.request(index).to;
        self.set_progress(index, Progress::Replied(reply));
        let host = self.host_mut(to);
        if host.switch == Some(index) && !keep_switch {
            host.switch = None;
        }
        if host.turn == Some(index) {
            host.turn = None;
        }
    }

    fn send(&mut self, lane: Lane, to: Machine, expect: Expect, want: Want) -> usize {
        let r#gen = match lane {
            Lane::Move => MOVE_GEN,
            Lane::Release => RELEASE_GEN,
        };
        self.requests.push(Request {
            lane,
            to,
            r#gen,
            expect,
            want,
            progress: Progress::Sent,
        });
        self.requests.len() - 1
    }

    fn replied(&self, index: Option<usize>) -> Option<Reply> {
        match self.request(index?).progress {
            Progress::Replied(reply) => Some(reply),
            Progress::Sent
            | Progress::Admitted
            | Progress::CutDrain
            | Progress::CutStop
            | Progress::CutFlip
            | Progress::StartAdmit
            | Progress::StartRun
            | Progress::Lost => None,
        }
    }

    fn writers(&self) -> usize {
        Machine::ALL
            .iter()
            .filter(|machine| self.host(**machine).copy.is_writer())
            .count()
    }

    fn running(&self) -> usize {
        Machine::ALL
            .iter()
            .filter(|machine| self.host(**machine).holder == Holder::Running)
            .count()
    }
}

/// A guard taken out, to show the check catches what that guard prevents.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag removes one independent guard"
)]
struct Broken {
    /// Every `admit` passes: no Machine keeps a floor.
    floors: bool,
    /// `commitMove` commits a row that already ended.
    commit_gate: bool,
    /// `cut_volume_copy` skips its floor checks after the drain.
    cut_floor: bool,
    /// `start_volume_holder` skips its floor check.
    start_floor: bool,
    /// `cut_volume_copy` flips without stopping holders or its Busy re-check.
    cut_busy: bool,
    /// A refusal keeps the switch guard instead of dropping it with the reply.
    refusal_keeps_switch: bool,
}

#[derive(Clone, Copy, Debug)]
struct VolumeRuns {
    /// Resends each run may make of a request still unanswered.
    resends: u8,
    /// Daemon restarts during a cut's flip.
    crashes: u8,
    broken: Broken,
}

impl Default for VolumeRuns {
    fn default() -> Self {
        Self {
            resends: 1,
            crashes: 1,
            broken: Broken::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// One step of request `n` on its Machine.
    Machine(usize),
    /// The daemon restarts during request `n`'s flip, after `zfs` set the root read-only.
    Crash(usize),
    Move,
    MoveResend,
    /// Cloud ends the Move's row: a cancel, the stale sweep, or a failed step.
    EndMove,
    Release,
    ReleaseResend,
}

// --- The real planner -------------------------------------------------------------

fn snapshot(snap: Snap) -> Snapshot {
    let (name, guid) = match snap {
        Snap::Warm => ("w-1-0", 1),
        Snap::Final => ("f-2", 2),
    };
    Snapshot {
        name: name.parse().expect("a valid snapshot name"),
        guid: SnapshotGuid::new(guid),
        created_unix_seconds: 1,
    }
}

fn slot_id(machine: Machine) -> SlotId {
    SlotId::new(match machine {
        Machine::A => 10,
        Machine::B => 11,
    })
}

fn volume_copy(held: Held, machine: Machine) -> Option<VolumeCopy> {
    match held {
        Held::None => None,
        Held::Root {
            readonly,
            epoch,
            newest,
        } => Some(VolumeCopy::Root {
            readonly,
            newest: newest.map(snapshot),
            epoch: WriterEpoch::new(epoch),
            seed: false,
        }),
        Held::Slot { newest } => Some(VolumeCopy::Slot {
            slot: slot_id(machine),
            readonly: true,
            newest: newest.map(snapshot),
            resume_token: None,
            pull: None,
        }),
    }
}

fn expected(expect: Expect, machine: Machine) -> ExpectedCopy {
    match expect {
        Expect::Writer { epoch } => ExpectedCopy::Writer {
            epoch: WriterEpoch::new(epoch),
        },
        Expect::Mirror { newest } => ExpectedCopy::Mirror {
            newest: snapshot(newest).guid,
            slot: slot_id(machine),
        },
        Expect::None => ExpectedCopy::None,
    }
}

fn wanted(want: Want) -> WantedCopy {
    match want {
        // `plan` reads only whether a start is named; the Machine reads its kind.
        Want::Writer { at, epoch, start } => WantedCopy::Writer {
            at: snapshot(at).guid,
            epoch: WriterEpoch::new(epoch),
            start: start.map(|_| Start::Kept {
                container_id: ContainerId::parse("c".repeat(64)).expect("a valid id"),
            }),
        },
        Want::Mirror { from, target } => WantedCopy::Mirror {
            from: from.map(|machine| {
                ManagementAddress(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, machine as u16))
            }),
            target: snapshot(target).name,
            drain: Drain::NONE,
        },
        Want::Forget => WantedCopy::Forget,
        Want::Floor => WantedCopy::Floor,
    }
}

fn refusal(reason: CopyRefusal) -> Refusal {
    match reason {
        CopyRefusal::Stale => Refusal::Stale,
        CopyRefusal::Busy => Refusal::Busy,
        CopyRefusal::Mismatch => Refusal::Mismatch,
        CopyRefusal::NoTransition => Refusal::NoTransition,
        CopyRefusal::Behind => Refusal::Behind,
        CopyRefusal::NoCapacity | CopyRefusal::Written | CopyRefusal::NoSource => Refusal::Other,
    }
}

/// `start_state`: what Docker shows of the Writer's start.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum StartState {
    Running,
    Stopped,
    Gone,
}

fn start_state(holder: Holder, start: StartKind) -> StartState {
    match (holder, start) {
        (Holder::Running, StartKind::Kept | StartKind::Create) => StartState::Running,
        (Holder::Absent, StartKind::Kept) => StartState::Gone,
        (Holder::Absent, StartKind::Create)
        | (Holder::Stopped, StartKind::Kept | StartKind::Create) => StartState::Stopped,
    }
}

// --- One Machine ------------------------------------------------------------------

impl VolumeRuns {
    /// `FloorTurn::admit`: refuse a `gen` below the floor, else raise the floor to it.
    fn admit(&self, host: &mut Host, r#gen: u8) -> bool {
        if !self.broken.floors && r#gen < host.floor {
            return false;
        }
        host.floor = host.floor.max(r#gen);
        true
    }

    fn can_step(state: &State, index: usize) -> bool {
        let request = state.request(index);
        let host = state.host(request.to);
        match request.progress {
            Progress::Sent | Progress::CutDrain | Progress::StartRun => true,
            Progress::Admitted => host.switch.is_none(),
            Progress::CutStop | Progress::CutFlip | Progress::StartAdmit => host.turn.is_none(),
            Progress::Replied(_) | Progress::Lost => false,
        }
    }

    fn step_machine(&self, state: &mut State, index: usize) {
        let request = state.request(index);
        let to = request.to;
        match request.progress {
            Progress::Sent => {
                let host = state.host_mut(to);
                if request.want == Want::Floor {
                    // `floor_copy`: raise and read under the floor lock; never refuses.
                    host.floor = host.floor.max(request.r#gen);
                    let copy = host.copy;
                    state.reply(index, Reply::Copy(copy));
                } else if self.admit(host, request.r#gen) {
                    state.set_progress(index, Progress::Admitted);
                } else {
                    state.reply(index, Reply::Refused(Refusal::Stale));
                }
            }
            Progress::Admitted => {
                let host = state.host_mut(to);
                host.switch = Some(index);
                // Checked again under the switch lock.
                if !self.admit(host, request.r#gen) {
                    state.reply(index, Reply::Refused(Refusal::Stale));
                    return;
                }
                self.transition(state, index);
            }
            Progress::CutDrain => {
                if self.admit(state.host_mut(to), request.r#gen) {
                    state.set_progress(index, Progress::CutStop);
                } else {
                    state.reply(index, Reply::Refused(Refusal::Stale));
                }
            }
            Progress::CutStop => {
                let fenced = !self.broken.cut_floor;
                let host = state.host_mut(to);
                if fenced && !self.admit(host, request.r#gen) {
                    state.reply(index, Reply::Refused(Refusal::Stale));
                    return;
                }
                if host.holder == Holder::Running && !self.broken.cut_busy {
                    host.holder = Holder::Stopped;
                }
                state.set_progress(index, Progress::CutFlip);
            }
            Progress::CutFlip => {
                let fenced = !self.broken.cut_floor;
                let busy = !self.broken.cut_busy;
                let host = state.host_mut(to);
                if busy && host.holder == Holder::Running {
                    state.reply(index, Reply::Refused(Refusal::Busy));
                    return;
                }
                if fenced && !self.admit(host, request.r#gen) {
                    state.reply(index, Reply::Refused(Refusal::Stale));
                    return;
                }
                host.copy = Held::Slot {
                    newest: Some(Snap::Final),
                };
                let copy = host.copy;
                state.reply(index, Reply::Copy(copy));
            }
            Progress::StartAdmit => {
                let fenced = !self.broken.start_floor;
                let host = state.host_mut(to);
                host.turn = Some(index);
                if fenced && !self.admit(host, request.r#gen) {
                    state.reply(index, Reply::Refused(Refusal::Stale));
                    return;
                }
                state.set_progress(index, Progress::StartRun);
            }
            Progress::StartRun => {
                let host = state.host_mut(to);
                host.holder = Holder::Running;
                let copy = host.copy;
                state.reply(index, Reply::Copy(copy));
            }
            Progress::Replied(_) | Progress::Lost => {}
        }
    }

    /// `set_copy_admitted` past its floor check: inspect, `plan`, and run the transition.
    fn transition(&self, state: &mut State, index: usize) {
        let request = state.request(index);
        let to = request.to;
        let host = state.host(to).clone();
        let start = match request.want {
            Want::Writer {
                start: Some(kind), ..
            } => Some(start_state(host.holder, kind)),
            Want::Writer { start: None, .. } | Want::Mirror { .. } | Want::Forget | Want::Floor => {
                None
            }
        };
        let plan = ployz_core::plan(
            volume_copy(host.copy, to).as_ref(),
            start == Some(StartState::Running),
            &expected(request.expect, to),
            &wanted(request.want),
        );
        let transition = match plan {
            CopyPlan::AtWant => return state.reply(index, Reply::Copy(host.copy)),
            CopyPlan::Refuse(reason) => {
                return state.reply_keeping(
                    index,
                    Reply::Refused(refusal(reason)),
                    self.broken.refusal_keeps_switch,
                );
            }
            CopyPlan::Transition(_) if start == Some(StartState::Gone) => {
                return state.reply(index, Reply::Refused(Refusal::NoTransition));
            }
            CopyPlan::Transition(transition) => transition,
        };
        match (transition, request.want) {
            (CopyTransition::WriterToMirror, Want::Mirror { .. }) => {
                let next = if host.holder == Holder::Absent {
                    Progress::CutFlip
                } else {
                    Progress::CutDrain
                };
                state.set_progress(index, next);
            }
            (CopyTransition::MirrorToWriter, Want::Writer { epoch, start, .. }) => {
                state.host_mut(to).copy = Held::Root {
                    readonly: false,
                    epoch,
                    newest: host.copy.newest(),
                };
                match start {
                    Some(_) => state.set_progress(index, Progress::StartAdmit),
                    None => state.reply(index, Reply::Copy(state.host(to).copy)),
                }
            }
            (CopyTransition::WriterToWriter, Want::Writer { epoch, start, .. }) => {
                if let Held::Root {
                    readonly,
                    epoch: held,
                    newest,
                } = host.copy
                    && (readonly || held != epoch)
                {
                    state.host_mut(to).copy = Held::Root {
                        readonly: false,
                        epoch,
                        newest,
                    };
                }
                let copy = state.host(to).copy;
                // Read again: a holder can exit or go while the Writer route runs.
                match start.map(|kind| start_state(state.host(to).holder, kind)) {
                    None | Some(StartState::Running) => state.reply(index, Reply::Copy(copy)),
                    Some(StartState::Gone) => {
                        state.reply(index, Reply::Refused(Refusal::NoTransition));
                    }
                    Some(StartState::Stopped) => state.set_progress(index, Progress::StartAdmit),
                }
            }
            (
                CopyTransition::WriterToForget
                | CopyTransition::MirrorToForget
                | CopyTransition::SeedToForget,
                Want::Forget,
            ) => {
                state.host_mut(to).copy = Held::None;
                state.reply(index, Reply::Copy(Held::None));
            }
            (
                CopyTransition::MirrorToMirror | CopyTransition::NoneToMirror,
                Want::Mirror {
                    from: Some(from),
                    target,
                },
            ) => {
                // The pull lands in one step when the source holds the target.
                if state.host(from).copy.newest() == Some(target) {
                    let copy = Held::Slot {
                        newest: Some(target),
                    };
                    state.host_mut(to).copy = copy;
                    state.reply(index, Reply::Copy(copy));
                } else {
                    state.reply(index, Reply::PullFailed);
                }
            }
            (transition, want) => unreachable!("the planner paired {transition:?} with {want:?}"),
        }
    }

    /// Whether the cut's flip passes its checks and runs `zfs`, the only place a restart
    /// can leave a read-only root.
    fn flips(&self, state: &State, index: usize) -> bool {
        let request = state.request(index);
        let host = state.host(request.to);
        host.turn.is_none()
            && (self.broken.cut_busy || host.holder != Holder::Running)
            && (self.broken.floors || self.broken.cut_floor || request.r#gen >= host.floor)
    }

    /// The daemon restarts: every request on the Machine is lost with its locks. A cut
    /// caught mid-`zfs` leaves its root read-only at the final snapshot.
    fn crash(state: &mut State, index: usize) {
        let to = state.request(index).to;
        let host = state.host_mut(to);
        if let Held::Root { epoch, .. } = host.copy {
            host.copy = Held::Root {
                readonly: true,
                epoch,
                newest: Some(Snap::Final),
            };
        }
        host.switch = None;
        host.turn = None;
        for request in &mut state.requests {
            if request.to == to && state_in_flight(request.progress) {
                request.progress = Progress::Lost;
            }
        }
        state.crashes += 1;
    }
}

fn state_in_flight(progress: Progress) -> bool {
    match progress {
        Progress::Sent
        | Progress::Admitted
        | Progress::CutDrain
        | Progress::CutStop
        | Progress::CutFlip
        | Progress::StartAdmit
        | Progress::StartRun => true,
        Progress::Replied(_) | Progress::Lost => false,
    }
}

// --- Cloud and the drivers --------------------------------------------------------

fn expect_of(held: Held) -> Expect {
    match held {
        Held::Root { epoch, .. } => Expect::Writer { epoch },
        Held::Slot {
            newest: Some(newest),
        } => Expect::Mirror { newest },
        Held::Slot { newest: None } | Held::None => Expect::None,
    }
}

impl VolumeRuns {
    /// `failAndRelease` and the cancel handler: end the running row and request the
    /// Release, back to A while warm and forward to B once committed.
    fn end_move(&self, state: &mut State) {
        if !state.row.running() {
            return;
        }
        let forward = state.row == Row::Committed;
        state.row = Row::Failed;
        state.release = Some(Release {
            row: Row::Warm,
            forward,
            driver: Driver {
                step: if forward {
                    ReleaseStep::Observe
                } else {
                    ReleaseStep::Floor(Machine::A)
                },
                waiting: None,
                resends: self.resends,
                busy: 1,
            },
            views: [None, None],
            resume: None,
            behind: None,
        });
    }

    fn move_step(&self, state: &mut State) {
        let driver = state.moving;
        let reply = state.replied(driver.waiting);
        let next = |state: &mut State, step: MoveStep| {
            state.moving.step = step;
            state.moving.waiting = None;
        };
        match (driver.step, driver.waiting, reply) {
            (MoveStep::Cut, None, _) => {
                let sent = state.send(
                    Lane::Move,
                    Machine::A,
                    Expect::Writer { epoch: OLD_EPOCH },
                    Want::Mirror {
                        from: None,
                        target: Snap::Final,
                    },
                );
                state.moving.waiting = Some(sent);
            }
            (MoveStep::Pull, None, _) => {
                let sent = state.send(
                    Lane::Move,
                    Machine::B,
                    Expect::Mirror { newest: Snap::Warm },
                    Want::Mirror {
                        from: Some(Machine::A),
                        target: Snap::Final,
                    },
                );
                state.moving.waiting = Some(sent);
            }
            (MoveStep::Commit, _, _) => {
                // `commitMove`: only a row still running commits.
                if state.row.running() || self.broken.commit_gate {
                    if state.row == Row::Warm {
                        state.row = Row::Committed;
                    }
                    next(state, MoveStep::Start);
                } else {
                    next(state, MoveStep::Over);
                }
            }
            (MoveStep::Start, None, _) => {
                // `startWriter`: a B already running the Service is left alone.
                if state.b.holder == Holder::Running {
                    next(state, MoveStep::Remove);
                } else if state.a.holder == Holder::Stopped {
                    let sent = state.send(
                        Lane::Move,
                        Machine::B,
                        Expect::Mirror {
                            newest: Snap::Final,
                        },
                        Want::Writer {
                            at: Snap::Final,
                            epoch: MOVE_EPOCH,
                            start: Some(StartKind::Create),
                        },
                    );
                    state.moving.waiting = Some(sent);
                } else {
                    self.end_move(state);
                    next(state, MoveStep::Over);
                }
            }
            (MoveStep::Remove, _, _) => {
                // `removeKept` checks the run still owns its row, then removes A's
                // stopped Container.
                if state.row.running() {
                    if state.a.holder == Holder::Stopped {
                        state.a.holder = Holder::Absent;
                    }
                    next(state, MoveStep::Finish);
                } else {
                    next(state, MoveStep::Over);
                }
            }
            (MoveStep::Finish, _, _) => {
                if state.row.running() {
                    state.row = Row::Done;
                }
                next(state, MoveStep::Over);
            }
            (MoveStep::Cut | MoveStep::Pull | MoveStep::Start, Some(_), Some(reply)) => {
                let advanced = match (driver.step, reply) {
                    (
                        MoveStep::Cut,
                        Reply::Copy(Held::Slot {
                            newest: Some(Snap::Final),
                        }),
                    ) => Some(MoveStep::Pull),
                    (
                        MoveStep::Pull,
                        Reply::Copy(Held::Slot {
                            newest: Some(Snap::Final),
                        }),
                    ) => Some(MoveStep::Commit),
                    (MoveStep::Start, Reply::Copy(_)) => Some(MoveStep::Remove),
                    (MoveStep::Cut | MoveStep::Pull | MoveStep::Start, _)
                    | (
                        MoveStep::Commit | MoveStep::Remove | MoveStep::Finish | MoveStep::Over,
                        _,
                    ) => None,
                };
                match (advanced, reply) {
                    (Some(step), _) => next(state, step),
                    // A newer lane fenced this run: it touches nothing more.
                    (None, Reply::Refused(Refusal::Stale)) => next(state, MoveStep::Over),
                    (None, Reply::Refused(Refusal::Busy)) if driver.busy > 0 => {
                        state.moving.busy -= 1;
                        state.moving.waiting = None;
                    }
                    (None, Reply::Refused(_) | Reply::PullFailed | Reply::Copy(_)) => {
                        self.end_move(state);
                        next(state, MoveStep::Over);
                    }
                }
            }
            (MoveStep::Cut | MoveStep::Pull | MoveStep::Start, Some(_), None)
            | (MoveStep::Over, _, _) => {}
        }
    }

    fn release_step(&self, state: &mut State) {
        let Some(release) = state.release.clone() else {
            return;
        };
        let driver = release.driver;
        let reply = state.replied(driver.waiting);
        let set = |state: &mut State, update: &dyn Fn(&mut Release)| {
            if let Some(release) = state.release.as_mut() {
                update(release);
            }
        };
        let next = |state: &mut State, step: ReleaseStep| {
            set(state, &|release| {
                release.driver.step = step;
                release.driver.waiting = None;
            });
        };
        let wait = |state: &mut State, sent: usize| {
            set(state, &|release| release.driver.waiting = Some(sent));
        };
        let fail = |state: &mut State| {
            set(state, &|release| {
                release.row = Row::Failed;
                release.driver.step = ReleaseStep::Over;
                release.driver.waiting = None;
            });
        };
        let resume = release.resume;
        match (driver.step, driver.waiting) {
            (ReleaseStep::Floor(machine), None) => {
                let sent = state.send(Lane::Release, machine, Expect::None, Want::Floor);
                wait(state, sent);
            }
            (ReleaseStep::Floor(machine), Some(_)) => {
                let Some(Reply::Copy(held)) = reply else {
                    return;
                };
                set(state, &|release| {
                    if let Some(view) = release.views.get_mut(machine as usize) {
                        *view = Some(held);
                    }
                });
                next(
                    state,
                    match machine {
                        Machine::A => ReleaseStep::Floor(Machine::B),
                        Machine::B => ReleaseStep::Plan,
                    },
                );
            }
            (ReleaseStep::Plan, _) => self.plan_release(state, &release),
            (ReleaseStep::Inspect(machine), _) => {
                let Some(plan) = resume else {
                    return fail(state);
                };
                match state.host(machine).copy {
                    Held::Root { .. } => fail(state),
                    Held::Slot {
                        newest: Some(newest),
                    } if newest == plan.at => next(state, ReleaseStep::Forget(machine)),
                    Held::Slot { .. } | Held::None => next(state, ReleaseStep::Resume(plan.on)),
                }
            }
            (ReleaseStep::Forget(machine), None) => {
                let Some(plan) = resume else {
                    return fail(state);
                };
                let sent = state.send(
                    Lane::Release,
                    machine,
                    Expect::Mirror { newest: plan.at },
                    Want::Forget,
                );
                wait(state, sent);
            }
            (ReleaseStep::Resume(machine), None) => {
                let Some(plan) = resume else {
                    return fail(state);
                };
                let sent = state.send(
                    Lane::Release,
                    machine,
                    plan.expect,
                    Want::Writer {
                        at: plan.at,
                        epoch: RELEASE_EPOCH,
                        start: plan.start,
                    },
                );
                wait(state, sent);
            }
            (
                ReleaseStep::Forget(_) | ReleaseStep::Resume(_) | ReleaseStep::StartForward,
                Some(_),
            ) => match reply {
                None => {}
                Some(Reply::Copy(_)) => {
                    let step = match driver.step {
                        ReleaseStep::Forget(_) => {
                            resume.map_or(ReleaseStep::Over, |plan| ReleaseStep::Resume(plan.on))
                        }
                        ReleaseStep::Resume(_) => release
                            .behind
                            .map_or(ReleaseStep::Finish, ReleaseStep::RemoveBehind),
                        ReleaseStep::Floor(_)
                        | ReleaseStep::Plan
                        | ReleaseStep::Inspect(_)
                        | ReleaseStep::RemoveBehind(_)
                        | ReleaseStep::Observe
                        | ReleaseStep::StartForward
                        | ReleaseStep::RemoveForward
                        | ReleaseStep::Finish
                        | ReleaseStep::Over => ReleaseStep::RemoveForward,
                    };
                    next(state, step);
                }
                Some(Reply::Refused(Refusal::Busy)) if driver.busy > 0 => {
                    set(state, &|release| {
                        release.driver.busy -= 1;
                        release.driver.waiting = None;
                    });
                }
                Some(Reply::Refused(_) | Reply::PullFailed) => fail(state),
            },
            (ReleaseStep::RemoveBehind(machine), _) => {
                let host = state.host_mut(machine);
                if host.holder != Holder::Absent {
                    host.holder = Holder::Absent;
                }
                next(state, ReleaseStep::Finish);
            }
            (ReleaseStep::Observe, _) => {
                if state.b.copy.newest().is_some() {
                    next(state, ReleaseStep::StartForward);
                } else {
                    fail(state);
                }
            }
            (ReleaseStep::StartForward, None) => {
                // `commitAndStart` on the inherited commit, then `startWriter` to B.
                let copy = state.b.copy;
                let Some(at) = copy.newest() else {
                    return fail(state);
                };
                let epoch = match copy {
                    Held::Root { epoch, .. } => epoch,
                    Held::Slot { .. } | Held::None => RELEASE_EPOCH,
                };
                let start = if state.b.holder == Holder::Running {
                    return next(state, ReleaseStep::RemoveForward);
                } else if state.a.holder == Holder::Stopped {
                    Some(StartKind::Create)
                } else if state.b.holder == Holder::Stopped {
                    None
                } else {
                    return fail(state);
                };
                let sent = state.send(
                    Lane::Release,
                    Machine::B,
                    expect_of(copy),
                    Want::Writer { at, epoch, start },
                );
                wait(state, sent);
            }
            (ReleaseStep::RemoveForward, _) => {
                if state.a.holder == Holder::Stopped {
                    state.a.holder = Holder::Absent;
                }
                next(state, ReleaseStep::Finish);
            }
            (ReleaseStep::Finish, _) => set(state, &|release| {
                release.row = Row::Done;
                release.driver.step = ReleaseStep::Over;
            }),
            (ReleaseStep::Over, _) => {}
        }
    }

    /// `planFromCopies` for a release, on the copies the floors answered.
    fn plan_release(&self, state: &mut State, release: &Release) {
        let views: Vec<(Machine, Held)> = Machine::ALL
            .iter()
            .zip(release.views.iter())
            .filter_map(|(machine, view)| view.map(|held| (*machine, held)))
            .collect();
        let roots: Vec<(Machine, Held)> = views
            .iter()
            .copied()
            .filter(|(_, held)| matches!(held, Held::Root { .. }))
            .collect();
        let kept = |state: &State, machine: Machine| {
            (state.host(machine).holder != Holder::Absent).then_some(StartKind::Kept)
        };
        let plan = match roots.as_slice() {
            [
                (
                    machine,
                    Held::Root {
                        readonly,
                        epoch,
                        newest: Some(at),
                    },
                ),
            ] => {
                let behind = (!readonly
                    && matches!(
                        release.views.get(machine.other() as usize),
                        Some(Some(Held::Slot { .. }))
                    ))
                .then_some(machine.other());
                Some((
                    Resume {
                        on: *machine,
                        expect: Expect::Writer { epoch: *epoch },
                        at: *at,
                        start: kept(state, *machine),
                    },
                    behind,
                    ReleaseStep::Resume(*machine),
                ))
            }
            [] => views
                .iter()
                .find(|(machine, held)| {
                    matches!(held, Held::Slot { newest: Some(_) })
                        && kept(state, *machine).is_some()
                })
                .and_then(|(owner, held)| {
                    let at = held.newest()?;
                    let other = owner.other();
                    let next = if matches!(
                        release.views.get(other as usize),
                        Some(Some(Held::Slot { .. }))
                    ) {
                        ReleaseStep::Inspect(other)
                    } else {
                        ReleaseStep::Resume(*owner)
                    };
                    Some((
                        Resume {
                            on: *owner,
                            expect: Expect::Mirror { newest: at },
                            at,
                            start: Some(StartKind::Kept),
                        },
                        None,
                        next,
                    ))
                }),
            _ => None,
        };
        let Some(release) = state.release.as_mut() else {
            return;
        };
        match plan {
            Some((resume, behind, step)) => {
                release.resume = Some(resume);
                release.behind = behind;
                release.driver.step = step;
            }
            None => {
                release.row = Row::Failed;
                release.driver.step = ReleaseStep::Over;
            }
        }
    }
}

fn resendable<S>(state: &State, driver: Driver<S>) -> bool {
    let Some(index) = driver.waiting else {
        return false;
    };
    match state.request(index).progress {
        Progress::Lost => true,
        Progress::Replied(_) => false,
        Progress::Sent
        | Progress::Admitted
        | Progress::CutDrain
        | Progress::CutStop
        | Progress::CutFlip
        | Progress::StartAdmit
        | Progress::StartRun => driver.resends > 0,
    }
}

fn steps<S>(state: &State, driver: Driver<S>) -> bool {
    driver.waiting.is_none() || state.replied(driver.waiting).is_some()
}

impl Model for VolumeRuns {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            a: Host {
                copy: Held::Root {
                    readonly: false,
                    epoch: OLD_EPOCH,
                    newest: Some(Snap::Warm),
                },
                floor: 0,
                holder: Holder::Running,
                switch: None,
                turn: None,
            },
            b: Host {
                copy: Held::Slot {
                    newest: Some(Snap::Warm),
                },
                floor: 0,
                holder: Holder::Absent,
                switch: None,
                turn: None,
            },
            requests: Vec::new(),
            row: Row::Warm,
            moving: Driver {
                step: MoveStep::Cut,
                waiting: None,
                resends: self.resends,
                busy: 1,
            },
            release: None,
            crashes: 0,
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        for (index, request) in state.requests.iter().enumerate() {
            if Self::can_step(state, index) {
                actions.push(Action::Machine(index));
            }
            if request.progress == Progress::CutFlip
                && state.crashes < self.crashes
                && self.flips(state, index)
            {
                actions.push(Action::Crash(index));
            }
        }
        if state.moving.step != MoveStep::Over && steps(state, state.moving) {
            actions.push(Action::Move);
        }
        if state.moving.step != MoveStep::Over && resendable(state, state.moving) {
            actions.push(Action::MoveResend);
        }
        if state.row.running() {
            actions.push(Action::EndMove);
        }
        if let Some(release) = &state.release
            && release.driver.step != ReleaseStep::Over
        {
            if steps(state, release.driver) {
                actions.push(Action::Release);
            }
            if resendable(state, release.driver) {
                actions.push(Action::ReleaseResend);
            }
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = last.clone();
        match action {
            Action::Machine(index) => self.step_machine(&mut state, index),
            Action::Crash(index) => Self::crash(&mut state, index),
            Action::Move => self.move_step(&mut state),
            Action::MoveResend => {
                if let Some(index) = state.moving.waiting
                    && state.request(index).progress != Progress::Lost
                {
                    state.moving.resends -= 1;
                }
                state.moving.waiting = None;
            }
            Action::EndMove => self.end_move(&mut state),
            Action::Release => self.release_step(&mut state),
            Action::ReleaseResend => {
                let lost = state
                    .release
                    .as_ref()
                    .and_then(|release| release.driver.waiting)
                    .is_some_and(|index| state.request(index).progress == Progress::Lost);
                if let Some(release) = state.release.as_mut() {
                    if !lost {
                        release.driver.resends -= 1;
                    }
                    release.driver.waiting = None;
                }
            }
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
            Property::always(NEVER_TWO_WRITERS, |_, state: &State| state.writers() <= 1),
            Property::always(ONE_SERVICE, |_, state: &State| state.running() <= 1),
            Property::always(SERVICE_ON_WRITER, |_, state: &State| {
                Machine::ALL.iter().all(|machine| {
                    let host = state.host(*machine);
                    host.holder != Holder::Running || host.copy.is_writer()
                })
            }),
            Property::always(NOTHING_STUCK, |model: &Self, state: &State| {
                !at_rest(model, state)
                    || (state.moving.step == MoveStep::Over
                        && state
                            .release
                            .as_ref()
                            .is_none_or(|release| release.driver.step == ReleaseStep::Over))
            }),
            Property::always(DONE_HOLDS, |model: &Self, state: &State| {
                !at_rest(model, state) || done_holds(state)
            }),
            Property::always(RELEASE_WINS, |model: &Self, state: &State| {
                !at_rest(model, state)
                    || state
                        .release
                        .as_ref()
                        .is_none_or(|release| release.forward || release.row == Row::Done)
            }),
            Property::sometimes("a Move finishes", |_, state: &State| state.row == Row::Done),
            Property::sometimes("a Release back to A finishes", |_, state: &State| {
                state
                    .release
                    .as_ref()
                    .is_some_and(|release| !release.forward && release.row == Row::Done)
            }),
            Property::sometimes("a Release forward to B finishes", |_, state: &State| {
                state
                    .release
                    .as_ref()
                    .is_some_and(|release| release.forward && release.row == Row::Done)
            }),
            Property::sometimes(
                "a dead Move's late request is refused as stale",
                |_, state: &State| {
                    !state.row.running()
                        && state.requests.iter().any(|request| {
                            request.lane == Lane::Move
                                && request.progress
                                    == Progress::Replied(Reply::Refused(Refusal::Stale))
                        })
                },
            ),
        ]
    }
}

const NEVER_TWO_WRITERS: &str = "never two Writers";
const ONE_SERVICE: &str = "the Service runs on one Machine at most";
const SERVICE_ON_WRITER: &str = "a running Service sits on its Machine's Writer";
const NOTHING_STUCK: &str = "every run ends";
const DONE_HOLDS: &str = "a run that finished done still holds at rest";
const RELEASE_WINS: &str = "a Release back to A finishes whatever the dead Move still sends";

fn at_rest(model: &VolumeRuns, state: &State) -> bool {
    let mut actions = Vec::new();
    model.actions(state, &mut actions);
    actions.is_empty()
}

/// The newest run that reported done left its Writer serving on the Machine it named.
fn done_holds(state: &State) -> bool {
    let serving = |machine: Machine, started: bool| {
        let host = state.host(machine);
        host.copy.is_writer()
            && state.writers() == 1
            && (!started || host.holder == Holder::Running)
    };
    match &state.release {
        Some(release) if release.row == Row::Done => match (release.forward, release.resume) {
            (true, _) => serving(Machine::B, false),
            (false, Some(resume)) => serving(resume.on, resume.start.is_some()),
            (false, None) => false,
        },
        Some(_) => true,
        None => state.row != Row::Done || serving(Machine::B, true),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn check(model: VolumeRuns) -> impl Checker<VolumeRuns> {
        model.checker().threads(4).spawn_bfs().join()
    }

    /// The code as it is: every property holds, and every `sometimes` is reached, so no
    /// property passes because the model cannot get there.
    #[test]
    fn volume_runs_hold() {
        let checker = check(VolumeRuns::default());
        println!("{} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// Each property fails once the guard that keeps it is taken out.
    fn breaks(broken: Broken, properties: &[&'static str]) {
        let checker = VolumeRuns {
            broken,
            ..VolumeRuns::default()
        }
        .checker()
        .threads(4)
        .finish_when(HasDiscoveries::AllOf(properties.iter().copied().collect()))
        .spawn_bfs()
        .join();
        for property in properties {
            let path = checker.assert_any_discovery(property);
            println!("{property} breaks with {broken:?}:\n{path}");
        }
    }

    #[test]
    fn floors_let_a_release_win_over_late_steps() {
        breaks(
            Broken {
                floors: true,
                ..Broken::default()
            },
            &[RELEASE_WINS],
        );
    }

    #[test]
    fn floors_and_the_commit_gate_keep_one_writer() {
        let broken = Broken {
            floors: true,
            commit_gate: true,
            ..Broken::default()
        };
        breaks(broken, &[NEVER_TWO_WRITERS, ONE_SERVICE, DONE_HOLDS]);
    }

    #[test]
    fn the_cut_stops_the_service_before_its_flip() {
        breaks(
            Broken {
                cut_busy: true,
                ..Broken::default()
            },
            &[SERVICE_ON_WRITER],
        );
    }

    #[test]
    fn a_refusal_frees_the_switch_lock() {
        breaks(
            Broken {
                refusal_keeps_switch: true,
                ..Broken::default()
            },
            &[NOTHING_STUCK],
        );
    }
}
