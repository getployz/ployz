//! A resize of one Volume, a grow or a shrink, then a Move of it from A to B, the Release
//! that takes it over, and every late `SetVolumeSize` and `SetVolumeCopy` the dead runs
//! can still deliver.
//!
//! DESIGN.md bet 6 promises that a late step from a dead run leaves a Volume with at
//! most one Writer. This model checks that promise, and what each run reports, over
//! every interleaving of:
//!
//! - the resize (`resizeVolume` in `volume-run.inngest.ts`) setting every copy to one
//!   target. A grow asks Mirrors first and the primary last, then records the target in
//!   the Store; a Machine whose Pool can't fit it refuses, and the run ends recording
//!   nothing. A shrink records the target first, then lowers the primary and then each
//!   Mirror; a Machine whose copy references too much for it refuses, and a refusal
//!   before any copy changed records the old size back. Cloud may end the row at any
//!   moment, and its `SetVolumeSize` still lands later, unfenced. The Store's size must
//!   never exceed the bound of a live copy, the one any later run may promote;
//! - the Move (`moveVolume` in `dashboard/src/modules/volume-run/volume-run.inngest.ts`)
//!   from its cut, or from one warm pull when B held no copy for the grow to ask: cut
//!   A, pull the cut's snapshot to B, commit, start B, remove A's kept Container;
//! - Cloud ending the Move's row at any moment, which hands the Volume to a Release:
//!   back to A while warm (`releaseVolume`), forward to B once committed
//!   (`finishRelease`);
//! - the Move resending any request while its row runs, and every request it sent
//!   landing at any later moment, even after its row ended. Once the row ends, the
//!   driver's next step stops at `requireOwner`, so it sends nothing new;
//! - each Machine running `SetVolumeCopy`
//!   (`ployzd/src/machine_api/local_volume_copy.rs`), with each lock and floor check
//!   of the code as its own step, and the transition picked by the real
//!   [`ployz_core::plan`];
//! - A's daemon restarting halfway through the cut's flip.
//!
//! The whole product is millions of states, so each `Scenario` checks one bounded
//! slice of it.
//!
//! Out of scope: Deploy lanes, operator collisions, Machines that do not answer,
//! warm rounds beyond that one pull, and a second takeover. Of the resize: real
//! capacities and floors (one Machine either takes the target or not, and a floor
//! never moves), ZFS (a bound is one value per copy), the Store's record transaction
//! (one atomic step on the lock row), a second resize, a resize during or after the
//! Move (a late `SetVolumeSize` may land during it), a Restore, and a Mirror made after
//! the resize by anything but the Move's own pull.

use std::net::Ipv6Addr;

use ployz_core::{
    ContainerId, CopyPlan, CopyRefusal, CopyTransition, Drain, ExpectedCopy, ManagementAddress,
    SlotId, SlotRole, Snapshot, SnapshotGuid, Start, VolumeCopy, WantedCopy, WriterEpoch,
};
use stateright::{Checker, HasDiscoveries, Model, Property};

/// The Move's lane `gen`, and the Release's after it.
const MOVE_GEN: u8 = 1;
const RELEASE_GEN: u8 = 2;
/// A's Writer epoch before the Move, the Move's, and a warm Release's from `takeEpoch`.
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

/// A Volume size. One resize per trace, between these two, keeps the state space small.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
enum Size {
    Small,
    Large,
}

/// Which way the resize moves the Volume's bound (`SizeDirection` in `resize.ts`).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Direction {
    Grow,
    Shrink,
}

impl Direction {
    /// The size every copy and the Store hold when the resize starts.
    fn from(self) -> Size {
        match self {
            Self::Grow => Size::Small,
            Self::Shrink => Size::Large,
        }
    }

    /// The size the resize asks for.
    fn to(self) -> Size {
        match self {
            Self::Grow => Size::Large,
            Self::Shrink => Size::Small,
        }
    }
}

/// The one Machine that can't take the target, if any, and why.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Cramped {
    /// Its Pool has no room for the larger size: a grow, and a pull's raise, refuse
    /// `NoCapacity`.
    Pool(Machine),
    /// Its copy references more than the smaller size leaves room for, so `shrink_floor`
    /// sits above the target: a shrink refuses `BelowFloor`.
    Copy(Machine),
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
    /// The copy's ZFS `refquota`, its root's or its slot parent's (`storage.rs`
    /// `bound()`). A cut and a promote keep it; `SetVolumeSize` and a pull raise it.
    bound: Size,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Lane {
    Resize,
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
    /// `SetVolumeSize`, which carries no `gen` and checks no lane floor. A shrink run
    /// sends it with `allow_shrink`.
    Size {
        to: Size,
        allow_shrink: bool,
    },
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Refusal {
    Stale,
    Busy,
    Mismatch,
    NoTransition,
    Behind,
    Switching,
    NoCapacity,
    Shrink,
    BelowFloor,
    Other,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Reply {
    Copy(Held),
    Bound(Size),
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

/// The resize's walk, `RESIZE_ORDER` in `resize.ts`. A grow sets Mirrors first (B), the
/// primary last (A), then records. A shrink records first, then sets the primary, then
/// each Mirror; a refusal before any copy changed records the old size back
/// (`undoOnRefusal`). A Machine holding no copy is not a holder and is skipped.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum ResizeStep {
    Set(Machine),
    Record,
    Unrecord,
    Finish,
    Over,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum MoveStep {
    /// The warm round's pull, run only when B held no copy when the grow ran.
    Warm,
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

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Resize {
    row: Row,
    driver: Driver<ResizeStep>,
    direction: Direction,
    /// A holder answered `holds`: `done` in `resizeVolume` has a `held`.
    held: bool,
    /// A holder refused the resize.
    refused: bool,
    /// Cloud ended the row (`EndResize`) before the driver ended it itself.
    cut_short: bool,
}

impl Resize {
    /// A refusal found no copy changed, so the run undoes its record (`undoOnRefusal`).
    fn undoes(self) -> bool {
        self.refused && !self.held
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct State {
    a: Host,
    b: Host,
    requests: Vec<Request>,
    /// What the Store records, which Deploys and the planners read; apart from the
    /// target the resize sends, so the order of set and record is what gets checked.
    size: Size,
    resize: Resize,
    cramped: Option<Cramped>,
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
            Lane::Resize => 0,
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

    /// A request whose answer no run will read leaves the state, and every index past it
    /// moves down one.
    fn forget(&mut self, index: usize) {
        self.requests.remove(index);
        let shift = |held: &mut Option<usize>| {
            if let Some(at) = held.as_mut()
                && *at > index
            {
                *at -= 1;
            }
        };
        shift(&mut self.resize.driver.waiting);
        shift(&mut self.moving.waiting);
        if let Some(release) = self.release.as_mut() {
            shift(&mut release.driver.waiting);
        }
        for host in [&mut self.a, &mut self.b] {
            shift(&mut host.switch);
            shift(&mut host.turn);
        }
    }

    /// The resize is over and no other run has sent anything yet. The answers it read and
    /// its retry counts are history no step reads again, so they leave the state; what is
    /// still on the wire stays, to land late.
    fn retire_resize(&mut self) {
        self.requests
            .retain(|request| state_in_flight(request.progress));
        self.resize.driver.resends = 0;
        self.resize.driver.busy = 0;
    }

    /// Every live copy holds the Store's size, so whichever one a Move, a Release or a
    /// Restore promotes, the Writer fits what a Deploy asks for.
    fn store_fits(&self) -> bool {
        Machine::ALL.iter().all(|machine| {
            let host = self.host(*machine);
            host.copy == Held::None || self.size <= host.bound
        })
    }

    /// Whether `machine`'s Pool can hold a copy of `size`.
    fn fits(&self, machine: Machine, size: Size) -> bool {
        size == Size::Small || self.cramped != Some(Cramped::Pool(machine))
    }

    /// Whether `machine`'s copy references too much to shrink to the smaller size.
    fn below_floor(&self, machine: Machine) -> bool {
        self.cramped == Some(Cramped::Copy(machine))
    }

    /// The Store holds the size the resize asked for.
    fn recorded(&self) -> bool {
        self.size == self.resize.direction.to()
    }

    /// The resize is over, every request it sent has landed, and no later run has sent
    /// anything: the copies hold what the resize left them.
    fn resize_settled(&self) -> bool {
        self.resize.driver.step == ResizeStep::Over
            && self.release.is_none()
            && self
                .requests
                .iter()
                .all(|request| request.lane == Lane::Resize && !state_in_flight(request.progress))
    }

    /// Every Mirror holds at least its Writer's bound, so promoting one never lands a
    /// Writer smaller than the one it replaces.
    fn mirrors_keep_up(&self) -> bool {
        Machine::ALL.iter().all(|machine| {
            let host = self.host(*machine);
            let writer = self.host(machine.other());
            !matches!(host.copy, Held::Slot { .. })
                || !writer.copy.is_writer()
                || writer.bound <= host.bound
        })
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
    /// `requireOwner` and `commitMove` let a Move whose row ended carry on: it commits,
    /// sends new requests, and resends.
    owner_checks: bool,
    /// `cut_volume_copy` skips its floor checks after the drain.
    cut_floor: bool,
    /// `start_volume_holder` skips its floor check.
    start_floor: bool,
    /// `cut_volume_copy` flips without stopping holders or its Busy re-check.
    cut_busy: bool,
    /// A refusal keeps the switch guard instead of dropping it with the reply.
    refusal_keeps_switch: bool,
    /// A grow records its target before it raises any Machine.
    record_before_raise: bool,
    /// A refused `SetVolumeSize` counts as held: the grow goes on and records.
    resize_skips_failures: bool,
    /// A grow's record step writes nothing, and the grow finishes done all the same.
    done_without_record: bool,
    /// A pull leaves the slot's bound as it was instead of raising it to the Writer's.
    pull_keeps_bound: bool,
    /// A shrink lowers every Machine first and records its target last.
    shrink_records_last: bool,
    /// A shrink lowers the Mirror before the primary.
    shrink_mirror_first: bool,
    /// A shrink refused before any copy changed keeps the size it recorded.
    shrink_keeps_refused_record: bool,
}

/// One bounded slice of the runs. Each is checked whole; together they cover the resize
/// with every late step it sends, and the Move after it with every late step of its own.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Scenario {
    /// The resize alone from every start, its row ended at any moment, its resend and
    /// every late `SetVolumeSize` landing after it; no Move follows.
    Resize,
    /// A resize that runs to its own end, then the Move and its Release.
    ResizeThenMove,
    /// A resize on both copies whose row ends while a `SetVolumeSize` is on the wire;
    /// the Move starts at once, and the late request lands anywhere in it.
    LateResize,
}

/// One start: which way the resize goes, whether B held a Mirror when it ran (else B
/// joins at the Move's warm pull), and which Machine, if any, can't take the target.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Origin {
    direction: Direction,
    b_held: bool,
    cramped: Option<Cramped>,
}

fn start(direction: Direction, b_held: bool, cramped: Option<Cramped>) -> Origin {
    Origin {
        direction,
        b_held,
        cramped,
    }
}

impl Scenario {
    fn starts(self) -> Vec<Origin> {
        use Direction::{Grow, Shrink};
        use Machine::{A, B};
        match self {
            Self::Resize => vec![
                start(Grow, true, None),
                start(Grow, true, Some(Cramped::Pool(A))),
                start(Grow, true, Some(Cramped::Pool(B))),
                start(Grow, false, None),
                start(Grow, false, Some(Cramped::Pool(A))),
                start(Grow, false, Some(Cramped::Pool(B))),
                start(Shrink, true, None),
                start(Shrink, true, Some(Cramped::Copy(A))),
                start(Shrink, true, Some(Cramped::Copy(B))),
                start(Shrink, false, None),
                start(Shrink, false, Some(Cramped::Copy(A))),
            ],
            // A resize refused by the first holder it asks changes nothing, so the Move
            // after it is the Move with no resize. A shrink's floor is asked by no pull,
            // so a B that joins at the Move is the same with or without one.
            Self::ResizeThenMove => vec![
                start(Grow, true, None),
                start(Grow, true, Some(Cramped::Pool(A))),
                start(Grow, false, None),
                start(Grow, false, Some(Cramped::Pool(B))),
                start(Shrink, true, None),
                start(Shrink, true, Some(Cramped::Copy(B))),
                start(Shrink, false, None),
            ],
            Self::LateResize => vec![start(Grow, true, None), start(Shrink, true, None)],
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct VolumeRuns {
    scenario: Scenario,
    /// Only this direction's starts; `None` runs both.
    direction: Option<Direction>,
    /// Resends each run may make of a request still unanswered.
    resends: u8,
    /// Daemon restarts during a cut's flip.
    crashes: u8,
    broken: Broken,
}

impl Default for VolumeRuns {
    fn default() -> Self {
        Self {
            scenario: Scenario::ResizeThenMove,
            direction: None,
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
    Resize,
    ResizeResend,
    /// Cloud ends the resize's row: a cancel, a Release taking the Volume, or a failed
    /// step.
    EndResize,
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
            role: SlotRole::Mirror,
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
            role: None,
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
        Want::Size { .. } => unreachable!("SetVolumeSize never asks plan"),
    }
}

fn refusal(reason: CopyRefusal) -> Refusal {
    match reason {
        CopyRefusal::Stale => Refusal::Stale,
        CopyRefusal::Busy => Refusal::Busy,
        CopyRefusal::Mismatch => Refusal::Mismatch,
        CopyRefusal::NoTransition => Refusal::NoTransition,
        CopyRefusal::Behind => Refusal::Behind,
        CopyRefusal::NoCapacity => Refusal::NoCapacity,
        CopyRefusal::Written | CopyRefusal::NoSource => Refusal::Other,
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
            // `switch_verb` queues `SetVolumeSize` on the switch lock.
            Progress::Sent if matches!(request.want, Want::Size { .. }) => host.switch.is_none(),
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
                if let Want::Size {
                    to: size,
                    allow_shrink,
                } = request.want
                {
                    let reply = Self::set_size(state, to, size, allow_shrink);
                    if state.resize.driver.waiting == Some(index) {
                        state.reply(index, reply);
                    } else {
                        state.forget(index);
                    }
                    return;
                }
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

    /// `set_size_admitted` (`ployzd/src/volume_plugin/size.rs`), whole under the switch
    /// lock: a bound rises, or falls when the request allows it and the copy's
    /// `shrink_floor` (read once, at the start of the request) sits below the target;
    /// an equal one answers with no write, and a read-only root is mid-cut.
    fn set_size(state: &mut State, to: Machine, size: Size, allow_shrink: bool) -> Reply {
        let fits = state.fits(to, size);
        let below_floor = state.below_floor(to);
        let host = state.host_mut(to);
        let switching = match host.copy {
            Held::None => return Reply::Refused(Refusal::Other),
            Held::Root { readonly, .. } => readonly,
            Held::Slot { .. } => false,
        };
        if size < host.bound {
            if !allow_shrink {
                return Reply::Refused(Refusal::Shrink);
            }
            if switching {
                return Reply::Refused(Refusal::Switching);
            }
            if below_floor {
                return Reply::Refused(Refusal::BelowFloor);
            }
            host.bound = size;
            return Reply::Bound(size);
        }
        if size == host.bound {
            return Reply::Bound(size);
        }
        if switching {
            return Reply::Refused(Refusal::Switching);
        }
        if !fits {
            return Reply::Refused(Refusal::NoCapacity);
        }
        host.bound = size;
        Reply::Bound(size)
    }

    /// `copy_pull` before its receive: a new slot is made at the Writer's bound
    /// (`fetch_bound`), and an existing one is raised to it (`raise_slot_bound`); a Pool
    /// that can't fit it refuses.
    fn raise_slot(&self, state: &mut State, to: Machine, from: Machine) -> Result<(), Reply> {
        let writer = state.host(from).bound;
        let host = state.host(to);
        let fresh = host.copy == Held::None;
        if !fresh && writer <= host.bound {
            return Ok(());
        }
        if !state.fits(to, writer) {
            return Err(Reply::Refused(Refusal::NoCapacity));
        }
        if !self.broken.pull_keeps_bound {
            state.host_mut(to).bound = writer;
        }
        Ok(())
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
            Want::Writer { start: None, .. }
            | Want::Mirror { .. }
            | Want::Forget
            | Want::Floor
            | Want::Size { .. } => None,
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
                if let Err(refused) = self.raise_slot(state, to, from) {
                    return state.reply(index, refused);
                }
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
    /// `planResize`: the holders in `RESIZE_ORDER[direction]` order, with the record
    /// placed first or last.
    fn walk(&self, direction: Direction) -> [ResizeStep; 4] {
        use ResizeStep::{Finish, Record, Set};
        let (a, b) = (Set(Machine::A), Set(Machine::B));
        match direction {
            Direction::Grow if self.broken.record_before_raise => [Record, b, a, Finish],
            Direction::Grow => [b, a, Record, Finish],
            Direction::Shrink if self.broken.shrink_records_last => [a, b, Record, Finish],
            Direction::Shrink if self.broken.shrink_mirror_first => [Record, b, a, Finish],
            Direction::Shrink => [Record, a, b, Finish],
        }
    }

    fn after(&self, direction: Direction, step: ResizeStep) -> ResizeStep {
        self.walk(direction)
            .into_iter()
            .skip_while(|each| *each != step)
            .nth(1)
            .unwrap_or(ResizeStep::Over)
    }

    fn resize_step(&self, state: &mut State) {
        let next = |state: &mut State, step: ResizeStep| {
            state.resize.driver.step = step;
            state.resize.driver.waiting = None;
            if step == ResizeStep::Over {
                state.retire_resize();
            }
        };
        if !state.resize.row.running() {
            return next(state, ResizeStep::Over);
        }
        let driver = state.resize.driver;
        let direction = state.resize.direction;
        match (driver.step, driver.waiting) {
            (ResizeStep::Set(machine), None) => {
                if state.host(machine).copy == Held::None {
                    return next(state, self.after(direction, driver.step));
                }
                let sent = state.send(
                    Lane::Resize,
                    machine,
                    Expect::None,
                    Want::Size {
                        to: direction.to(),
                        allow_shrink: direction == Direction::Shrink,
                    },
                );
                state.resize.driver.waiting = Some(sent);
            }
            // `sizeOutcome`: a bound at the target, or past it the way the resize goes,
            // holds it; `switching` is retried like `busy`; anything else refuses the
            // resize, which undoes a record no copy has changed under.
            (ResizeStep::Set(_), Some(_)) => match state.replied(driver.waiting) {
                None => {}
                Some(Reply::Bound(_) | Reply::Refused(Refusal::Shrink)) => {
                    state.resize.held = true;
                    next(state, self.after(direction, driver.step));
                }
                Some(Reply::Refused(Refusal::Switching)) if driver.busy > 0 => {
                    state.resize.driver.busy -= 1;
                    state.resize.driver.waiting = None;
                }
                Some(Reply::Refused(_) | Reply::Copy(_) | Reply::PullFailed) => {
                    state.resize.refused = true;
                    if self.broken.resize_skips_failures {
                        next(state, self.after(direction, driver.step));
                    } else if state.recorded()
                        && state.resize.undoes()
                        && !self.broken.shrink_keeps_refused_record
                    {
                        next(state, ResizeStep::Unrecord);
                    } else {
                        state.resize.row = Row::Failed;
                        next(state, ResizeStep::Over);
                    }
                }
            },
            (ResizeStep::Record, _) => {
                if !self.broken.done_without_record {
                    state.size = direction.to();
                }
                next(state, self.after(direction, driver.step));
            }
            (ResizeStep::Unrecord, _) => {
                state.size = direction.from();
                state.resize.row = Row::Failed;
                next(state, ResizeStep::Over);
            }
            (ResizeStep::Finish, _) => {
                state.resize.row = Row::Done;
                next(state, ResizeStep::Over);
            }
            (ResizeStep::Over, _) => {}
        }
    }

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
        // Every Move step that talks to a Machine or the row checks ownership first.
        if !state.row.running() && !self.broken.owner_checks {
            state.moving.step = MoveStep::Over;
            state.moving.waiting = None;
            return;
        }
        let driver = state.moving;
        let reply = state.replied(driver.waiting);
        let next = |state: &mut State, step: MoveStep| {
            state.moving.step = step;
            state.moving.waiting = None;
        };
        match (driver.step, driver.waiting, reply) {
            (MoveStep::Warm, None, _) => {
                let sent = state.send(
                    Lane::Move,
                    Machine::B,
                    Expect::None,
                    Want::Mirror {
                        from: Some(Machine::A),
                        target: Snap::Warm,
                    },
                );
                state.moving.waiting = Some(sent);
            }
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
                if state.row.running() || self.broken.owner_checks {
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
            (
                MoveStep::Warm | MoveStep::Cut | MoveStep::Pull | MoveStep::Start,
                Some(_),
                Some(reply),
            ) => {
                let advanced = match (driver.step, reply) {
                    (
                        MoveStep::Warm,
                        Reply::Copy(Held::Slot {
                            newest: Some(Snap::Warm),
                        }),
                    ) => Some(MoveStep::Cut),
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
                    (MoveStep::Warm | MoveStep::Cut | MoveStep::Pull | MoveStep::Start, _)
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
                    (
                        None,
                        Reply::Refused(_) | Reply::PullFailed | Reply::Copy(_) | Reply::Bound(_),
                    ) => {
                        self.end_move(state);
                        next(state, MoveStep::Over);
                    }
                }
            }
            (MoveStep::Warm | MoveStep::Cut | MoveStep::Pull | MoveStep::Start, Some(_), None)
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
                Some(Reply::Refused(_) | Reply::PullFailed | Reply::Bound(_)) => fail(state),
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
                // A committed Move's Release inherits the Move's epoch (`endAndRelease`), and
                // `takeEpoch` keeps it.
                let epoch = match copy {
                    Held::Root { epoch, .. } => epoch,
                    Held::Slot { .. } | Held::None => MOVE_EPOCH,
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

impl VolumeRuns {
    fn initial(&self, start: Origin) -> State {
        let from = start.direction.from();
        State {
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
                bound: from,
            },
            b: Host {
                copy: if start.b_held {
                    Held::Slot {
                        newest: Some(Snap::Warm),
                    }
                } else {
                    Held::None
                },
                floor: 0,
                holder: Holder::Absent,
                switch: None,
                turn: None,
                bound: from,
            },
            requests: Vec::new(),
            size: from,
            resize: Resize {
                row: Row::Warm,
                driver: Driver {
                    step: self.walk(start.direction)[0],
                    waiting: None,
                    resends: match self.scenario {
                        Scenario::Resize => self.resends,
                        Scenario::ResizeThenMove | Scenario::LateResize => 0,
                    },
                    busy: 1,
                },
                direction: start.direction,
                held: false,
                refused: false,
                cut_short: false,
            },
            cramped: start.cramped,
            row: Row::Warm,
            moving: Driver {
                step: if start.b_held {
                    MoveStep::Cut
                } else {
                    MoveStep::Warm
                },
                waiting: None,
                resends: self.resends,
                busy: 1,
            },
            release: None,
            crashes: 0,
        }
    }
}

impl Model for VolumeRuns {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        self.scenario
            .starts()
            .into_iter()
            .filter(|start| self.direction.is_none_or(|only| only == start.direction))
            .map(|start| self.initial(start))
            .collect()
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
        let resize = state.resize;
        if resize.driver.step != ResizeStep::Over {
            // One active run per Volume: the Move waits for this one.
            if !resize.row.running() || steps(state, resize.driver) {
                actions.push(Action::Resize);
            }
            if resize.row.running() && resendable(state, resize.driver) {
                actions.push(Action::ResizeResend);
            }
            let ends = match self.scenario {
                Scenario::Resize => true,
                Scenario::ResizeThenMove => false,
                Scenario::LateResize => {
                    resize.driver.waiting.is_some()
                        && state.replied(resize.driver.waiting).is_none()
                }
            };
            if resize.row.running() && ends {
                actions.push(Action::EndResize);
            }
            return;
        }
        if self.scenario == Scenario::Resize {
            return;
        }
        // A Move whose row ended stops whenever its step next runs or retries, leaving
        // what it sent in flight.
        let stopped = !state.row.running() && !self.broken.owner_checks;
        if state.moving.step != MoveStep::Over && (stopped || steps(state, state.moving)) {
            actions.push(Action::Move);
        }
        if state.moving.step != MoveStep::Over
            && (state.row.running() || self.broken.owner_checks)
            && resendable(state, state.moving)
        {
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
            Action::Resize => self.resize_step(&mut state),
            Action::ResizeResend => {
                if let Some(index) = state.resize.driver.waiting
                    && state.request(index).progress != Progress::Lost
                {
                    state.resize.driver.resends -= 1;
                }
                state.resize.driver.waiting = None;
            }
            Action::EndResize => {
                state.resize.row = Row::Failed;
                state.resize.cut_short = true;
            }
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
        let mut properties = vec![
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
                    || (state.resize.driver.step == ResizeStep::Over
                        && (model.scenario == Scenario::Resize
                            || state.moving.step == MoveStep::Over)
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
                        .is_none_or(|release| release.row == Row::Done)
            }),
            Property::always(STORE_FITS, |_, state: &State| state.store_fits()),
            Property::always(RESIZE_DONE_HOLDS, |_, state: &State| {
                state.resize.row != Row::Done || state.recorded()
            }),
            Property::always(REFUSED_RECORDS_NOTHING, |_, state: &State| {
                let resize = state.resize;
                !(resize.undoes() && resize.driver.step == ResizeStep::Over && !resize.cut_short)
                    || state.size == resize.direction.from()
            }),
            Property::always(MIRRORS_KEEP_UP, |_, state: &State| {
                !state.resize_settled() || state.mirrors_keep_up()
            }),
        ];
        properties.extend(self.reached());
        properties
    }
}

impl VolumeRuns {
    /// What each scenario must reach, so no property passes because the model can't get
    /// there.
    fn reached(&self) -> Vec<Property<Self>> {
        let move_ends = || {
            vec![
                Property::<Self>::sometimes("a Move finishes", |_, state: &State| {
                    state.row == Row::Done
                }),
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
        };
        let resizes = || {
            vec![
                Property::<Self>::sometimes(
                    "a resize settles with a Mirror held and nothing else sent",
                    |_, state: &State| {
                        state.resize_settled()
                            && state.a.copy.is_writer()
                            && matches!(state.b.copy, Held::Slot { .. })
                    },
                ),
                Property::sometimes("a grow finishes", |_, state: &State| {
                    grow(state) && state.resize.row == Row::Done
                }),
                Property::sometimes(
                    "a refused grow leaves a Mirror bigger than the Store",
                    |_, state: &State| {
                        grow(state)
                            && state.resize.refused
                            && state.size == Size::Small
                            && state.b.bound == Size::Large
                    },
                ),
                Property::sometimes(
                    "a shrink finishes with both copies lowered",
                    |_, state: &State| {
                        shrink(state)
                            && state.resize.row == Row::Done
                            && state.a.bound == Size::Small
                            && state.b.bound == Size::Small
                    },
                ),
                Property::sometimes(
                    "a shrink the Mirror refused ends with the Store at the new size, the primary lowered and the Mirror still larger",
                    |_, state: &State| {
                        shrink(state)
                            && state.resize.refused
                            && state.resize.driver.step == ResizeStep::Over
                            && state.size == Size::Small
                            && state.a.bound == Size::Small
                            && state.b.bound == Size::Large
                    },
                ),
            ]
        };
        match self.scenario {
            Scenario::Resize => {
                let mut reached = resizes();
                reached.push(Property::sometimes(
                    "a grow ends after its record and before it finishes",
                    |_, state: &State| {
                        grow(state) && state.resize.row == Row::Failed && state.size == Size::Large
                    },
                ));
                reached.push(Property::sometimes(
                    "a shrink the primary refused ends with the Store back at the old size and both copies unchanged",
                    |_, state: &State| {
                        shrink(state)
                            && state.resize.undoes()
                            && state.resize.driver.step == ResizeStep::Over
                            && !state.resize.cut_short
                            && state.size == Size::Large
                            && state.a.bound == Size::Large
                            && state.b.bound == Size::Large
                    },
                ));
                reached.push(Property::sometimes(
                    "a shrink the primary refused is cut short before its undo, leaving the new size recorded",
                    |_, state: &State| {
                        shrink(state)
                            && state.resize.undoes()
                            && state.resize.driver.step == ResizeStep::Over
                            && state.resize.cut_short
                            && state.size == Size::Small
                            && state.a.bound == Size::Large
                    },
                ));
                reached
            }
            Scenario::ResizeThenMove => {
                let mut reached = resizes();
                reached.extend(move_ends());
                reached.push(Property::sometimes(
                    "a Release back to A promotes a grown copy with no pull",
                    |_, state: &State| {
                        grow(state)
                            && state.size == Size::Large
                            && state.a.copy.is_writer()
                            && state
                                .release
                                .as_ref()
                                .is_some_and(|release| !release.forward && release.row == Row::Done)
                    },
                ));
                reached.push(Property::sometimes(
                    "a Move to a Machine the grow never asked starts it grown",
                    |_, state: &State| {
                        grow(state)
                            && state.size == Size::Large
                            && state.row == Row::Done
                            && state.requests.iter().any(|request| {
                                request.lane == Lane::Move
                                    && matches!(
                                        request.want,
                                        Want::Mirror {
                                            target: Snap::Warm,
                                            ..
                                        }
                                    )
                            })
                    },
                ));
                reached.push(Property::sometimes(
                    "a Move after a shrink finishes on the lowered copy",
                    |_, state: &State| {
                        shrink(state)
                            && state.row == Row::Done
                            && state.b.copy.is_writer()
                            && state.b.bound == Size::Small
                    },
                ));
                reached.push(Property::sometimes(
                    "a Move after a shrink the Mirror refused promotes the larger copy over the smaller Store",
                    |_, state: &State| {
                        shrink(state)
                            && state.resize.refused
                            && state.row == Row::Done
                            && state.b.copy.is_writer()
                            && state.b.bound == Size::Large
                            && state.size == Size::Small
                    },
                ));
                reached
            }
            Scenario::LateResize => {
                let mut reached = move_ends();
                reached.push(Property::sometimes(
                    "a late request lands before the Move sends, and the resize settles",
                    |_, state: &State| state.resize_settled() && state.requests.is_empty(),
                ));
                reached.push(Property::sometimes(
                    "a dead grow's request is still on the wire after the cut",
                    |_, state: &State| grow(state) && on_the_wire(state),
                ));
                reached.push(Property::sometimes(
                    "a dead shrink's request is still on the wire after the cut",
                    |_, state: &State| shrink(state) && on_the_wire(state),
                ));
                reached.push(Property::sometimes(
                    "a dead shrink's request lowers the Writer the Move made",
                    |_, state: &State| {
                        shrink(state)
                            && state.row == Row::Done
                            && state.b.copy.is_writer()
                            && state.b.bound == Size::Small
                    },
                ));
                reached
            }
        }
    }
}

const NEVER_TWO_WRITERS: &str = "never two Writers";
const ONE_SERVICE: &str = "the Service runs on one Machine at most";
const SERVICE_ON_WRITER: &str = "a running Service sits on its Machine's Writer";
const NOTHING_STUCK: &str = "every run ends";
const DONE_HOLDS: &str = "a run that finished done still holds at rest";
const RELEASE_WINS: &str = "a Release finishes whatever the dead Move still sends";
const STORE_FITS: &str = "the Store's size fits every copy";
const RESIZE_DONE_HOLDS: &str = "a resize that finished done recorded its target";
const REFUSED_RECORDS_NOTHING: &str =
    "a resize refused before any copy changed ends recording nothing";
const MIRRORS_KEEP_UP: &str = "a resize leaves every Mirror at least its Writer's size";

fn grow(state: &State) -> bool {
    state.resize.direction == Direction::Grow
}

fn shrink(state: &State) -> bool {
    state.resize.direction == Direction::Shrink
}

/// A dead resize's `SetVolumeSize` is still unlanded once the Move cut A.
fn on_the_wire(state: &State) -> bool {
    matches!(state.a.copy, Held::Slot { .. })
        && state
            .requests
            .iter()
            .any(|request| request.lane == Lane::Resize && request.progress == Progress::Sent)
}

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

    /// The code as it is, in every scenario: every property holds, and every `sometimes`
    /// is reached, so no property passes because the model cannot get there.
    #[test]
    fn volume_runs_hold() {
        for scenario in [
            Scenario::Resize,
            Scenario::ResizeThenMove,
            Scenario::LateResize,
        ] {
            let checker = check(VolumeRuns {
                scenario,
                ..VolumeRuns::default()
            });
            println!("{scenario:?}: {} states", checker.unique_state_count());
            checker.assert_properties();
        }
    }

    /// Each property fails once the guard that keeps it is taken out.
    fn breaks(broken: Broken, properties: &[&'static str]) {
        breaks_in(Scenario::ResizeThenMove, None, broken, properties);
    }

    fn breaks_in(
        scenario: Scenario,
        direction: Option<Direction>,
        broken: Broken,
        properties: &[&'static str],
    ) {
        let checker = VolumeRuns {
            scenario,
            direction,
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
    fn floors_and_owner_checks_keep_one_writer() {
        let broken = Broken {
            floors: true,
            owner_checks: true,
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

    #[test]
    fn a_grow_records_only_after_every_copy_holds_it() {
        breaks_in(
            Scenario::Resize,
            Some(Direction::Grow),
            Broken {
                record_before_raise: true,
                ..Broken::default()
            },
            &[STORE_FITS],
        );
    }

    #[test]
    fn a_done_resize_recorded_its_target() {
        for direction in [Direction::Grow, Direction::Shrink] {
            breaks_in(
                Scenario::Resize,
                Some(direction),
                Broken {
                    done_without_record: true,
                    ..Broken::default()
                },
                &[RESIZE_DONE_HOLDS],
            );
        }
    }

    #[test]
    fn a_refused_grow_records_nothing() {
        breaks_in(
            Scenario::Resize,
            Some(Direction::Grow),
            Broken {
                resize_skips_failures: true,
                ..Broken::default()
            },
            &[STORE_FITS, REFUSED_RECORDS_NOTHING],
        );
    }

    /// A shrink that goes on past a refusal can't leave the Store above a copy, since it
    /// recorded the smaller size first; what it leaves is a Mirror lowered under a
    /// primary that refused, and a record the primary's refusal should have undone.
    #[test]
    fn a_refused_shrink_stops() {
        breaks_in(
            Scenario::Resize,
            Some(Direction::Shrink),
            Broken {
                resize_skips_failures: true,
                ..Broken::default()
            },
            &[MIRRORS_KEEP_UP, REFUSED_RECORDS_NOTHING],
        );
    }

    #[test]
    fn a_shrink_records_before_any_copy_lowers() {
        breaks_in(
            Scenario::Resize,
            Some(Direction::Shrink),
            Broken {
                shrink_records_last: true,
                ..Broken::default()
            },
            &[STORE_FITS],
        );
    }

    /// The primary's floor is asked first, so a refusal changes no copy. Lowering the
    /// Mirror first leaves it under a primary that then refuses, and the record stays,
    /// since a copy did change.
    #[test]
    fn a_shrink_lowers_the_primary_before_its_mirrors() {
        breaks_in(
            Scenario::Resize,
            Some(Direction::Shrink),
            Broken {
                shrink_mirror_first: true,
                ..Broken::default()
            },
            &[MIRRORS_KEEP_UP],
        );
    }

    #[test]
    fn a_shrink_refused_before_any_copy_changed_undoes_its_record() {
        breaks_in(
            Scenario::Resize,
            Some(Direction::Shrink),
            Broken {
                shrink_keeps_refused_record: true,
                ..Broken::default()
            },
            &[REFUSED_RECORDS_NOTHING],
        );
    }

    /// A Move sends no `SetVolumeSize` of its own: its pull raises the copy it lands to
    /// the Writer's bound, so a Machine the grow never asked joins grown. With that raise
    /// taken out, the warm pull lands a copy smaller than the Store's size.
    ///
    /// Three runs promote a copy with no final pull: a Move whose start is resumed past
    /// `06-final-pull`, a Release forward, and a Restore; and a pull of a snapshot the
    /// slot already holds raises nothing. None needs a raise either, because STORE_FITS
    /// holds for every live copy in every state, not only at a promote: a resize and a
    /// Move or Release exclude each other, and a grow records only once every holder
    /// holds its target. The model reaches the Release back to A on a grown copy with no
    /// pull (`volume_runs_hold`); it has no Restore.
    #[test]
    fn a_move_needs_no_raise_of_its_own() {
        breaks(
            Broken {
                pull_keeps_bound: true,
                ..Broken::default()
            },
            &[STORE_FITS],
        );
    }
}
