//! A resize of one Volume, a grow or a shrink, then a Move of it from A to B, the Release
//! that takes it over, and every late `SetVolumeSize` and `SetVolumeCopy` the dead runs
//! can still deliver. Then, on three Machines, a second Move to C, a Restore, and a
//! Deploy's lane, each with every late step a dead run or a crash leaves.
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
//!   (`ployzd/src/machine/local_machine/volume_copy.rs`), with each lock and floor check
//!   of the code as its own step, and the transition picked by the real
//!   [`ployz_core::plan`];
//! - A's daemon restarting halfway through the cut's flip;
//! - each Machine refusing, on arrival, an Operator's `SetVolumeCopy` whose start would
//!   create a Container (`admit_volume_start` in
//!   `ployzd/src/machine/local_machine/admission.rs`). The gate may close at any
//!   moment of the Move, which covers a Machine that refuses from the first step. A start
//!   of a Container the Machine kept is served.
//!
//! The three-Machine scenarios (`TwoMoves`, `Restore`) add, one run at a time under the
//! row lock, with Cloud ending any row at any moment:
//!
//! - after the first Move's row ended, the `volume mirror rm` that `planMove` needs
//!   before a second Move (`deleteMirror`: forget each other slot, then the Writer's
//!   snapshots), and a second Move from the Writer to C, which snapshots the Writer
//!   before its warm pull, with its own Release. `planMove` refuses a Move while no
//!   Machine holds a root, since a Release that ended mid-promote may still make a
//!   slot the Writer;
//! - a Machine away for any stretch: nothing it was sent lands while it is away, and
//!   every run that observes it waits and fails. Cloud may forget it while it is away
//!   (`ployz server forget`); a `ployz volume restore` from the one copy Cloud still
//!   sees (`restoreVolume`) then promotes that copy. The forgotten Machine keeps
//!   writing its copy when it comes back, until it is added back and departs
//!   (`volume_plugin/departure.rs`), which snapshots its root and parks it in a slot;
//! - that Machine joining again with its parked copy, and a `volume mirror` onto it
//!   (`mirrorVolume`'s round: snapshot the restored Writer, pull it). No Writer holds the
//!   departure's snapshot, so the pull's incremental finds no base, and `pull_stream`
//!   keeps a copy `written_here` rather than replace it with a full send. Then a
//!   `volume sync --full` onto that slot, which Cloud plans whatever the slot holds:
//!   it forgets the slot, then the Writer's snapshots, then pulls afresh. Then a Move
//!   onto that slot, whose warm round sends the mirror's snapshot and pull, and the
//!   orphan sweep's Forget of the slot once the Volume left the config (`findOrphans`,
//!   `deleteMirror`; the Writer's removal is left out, since the Machine plans the
//!   slot's Forget from the slot alone). Cloud keeps a slot `written_here` from the
//!   sweep before any row. The Machine's `plan` refuses a bare Forget of a slot
//!   `written_here`; only a person's `volume mirror rm` says `discard`, so only it
//!   destroys one. With `Skew::OldCloud` every Forget is bare, as an older Cloud's is.
//!   With `Skew::OldDaemon` no Machine advertises `ployz.volume.keep-written.v1`, so
//!   none reports `written_here` or reads `discard`, its plugin destroys a slot as its
//!   daemon planned, and its pull replaces a copy it can't send an incremental onto.
//!   Cloud then plans no full sync, no mirror and no Move there but a pull from the
//!   Writer's newest snapshot (`mayReplace`), and its sweep keeps every slot there. A
//!   cut's final a Restore from an earlier copy skipped is, like a departure's, on no
//!   Writer's line, so a pull can't send an incremental onto it;
//! - a slot's Forget in two steps: the daemon plans it under the switch lock, then the
//!   plugin's `copy_destroy` plans it again on the slot it finds and destroys it. A
//!   receive started earlier runs outside the switch lock, so `Replace` and `Mark` may
//!   land between the two;
//! - a Deploy's lane at its real boundaries, sent to the Writer Cloud sees: Prepare
//!   (`prepare_volumes`) takes the switch lock, reads the forgotten record, then in a
//!   step of its own admits a plain mount (`admit_plain_mount`) from the copy and the
//!   Machine's replicated view, which may lag, and ensures the Volume; Create
//!   (`create_container`) checks placement and ensures the Volume with no plain-mount
//!   admission; and the start of a stopped kept Container mounts only a writable root
//!   (`VolumeStorage::mountpoint`). No Deploy step reads a floor;
//! - `ployzd` restarting between any floor write and the plugin step after it.
//!
//! The whole product is millions of states, so each `Scenario` checks one bounded
//! slice of it.
//!
//! With Cloud honest, the row lock and the owner checks (`requireOwner`, `commitMove`)
//! keep late promotes to one Writer, and floors and the expected copy are a second
//! fence: removing either alone keeps `never two Writers` on three Machines, and the
//! floors' own break needs the owner checks gone too, on two. Prepare's
//! `admit_plain_mount`, Prepare's and Create's forgotten-origin checks, and the switch
//! lock Prepare holds across its two reads are each the only fence for the late Deploy
//! steps they stop.
//!
//! Out of scope: operator collisions, a forged step, a Deploy that takes over a Move's
//! lane (`fenceTaken`, `fenceFollowed`), the rowless mirror sync, a demote, warm rounds
//! beyond one pull, a third takeover, and a first Deploy's late step while no copy
//! exists yet, since every start has a Writer (`first_placement` checks it). Of the resize: real capacities and floors (one
//! Machine either takes the target or not, and a floor never moves), ZFS (a bound is one
//! value per copy), the Store's record transaction (one atomic step on the lock row), a
//! second resize, a resize during or after the Move (a late `SetVolumeSize` may land
//! during it), and a Mirror made after the resize by anything but a Move's own pull.

use std::{net::Ipv6Addr, num::NonZeroU64};

use ployz_core::{
    ContainerId, CopyPlan, CopyRefusal, CopyTransition, Drain, ExpectedCopy, ManagementAddress,
    ProvisionedVolumeMaximumBytes, SlotId, SlotRole, Snapshot, SnapshotGuid, Start, VolumeCopy,
    VolumeGen, WantedCopy, WriterEpoch,
};
use ployz_decide::volume_floor;
use ployz_decide::volume_size::{Refused, SizeFacts, SizePlan, size_plan};
use stateright::{Checker, Model, Property};

/// A's Writer epoch before the first Move.
const OLD_EPOCH: u32 = 1;
/// The first Move, and the Release after it, take the next gen; a warm Release takes the
/// epoch after its Move's.
const FIRST_MOVE: MovePlan = MovePlan {
    from: Machine::A,
    to: Machine::B,
    r#gen: 1,
    epoch: 2,
    held: OLD_EPOCH,
    warm: Snap::Warm,
    last: Snap::Final,
};
/// The `volume mirror rm` between the two Moves.
const DROP_GEN: u8 = 3;
/// The second Move's gen and epoch; its Release is the gen after.
const SECOND_GEN: u8 = 4;
const SECOND_EPOCH: u32 = 4;
const RESTORE_GEN: u8 = 6;
const RESTORE_EPOCH: u32 = 6;
/// The `volume mirror` onto the old copy a Machine that joined again kept.
const MIRROR_GEN: u8 = 7;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
enum Machine {
    A,
    B,
    C,
}

impl Machine {
    const ALL: [Self; 3] = [Self::A, Self::B, Self::C];

    const fn index(self) -> usize {
        self as usize
    }
}

/// A Volume size. One resize per trace, between these two, keeps the state space small.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq, PartialOrd, Ord)]
enum Size {
    Small,
    Large,
}

impl Size {
    /// The bytes the size stands for: 2 GB and 4 GB, so a copy referencing the smaller one
    /// has a `shrink_floor` above it, and one referencing nothing has a floor below it.
    fn bytes(self) -> ProvisionedVolumeMaximumBytes {
        let bytes = match self {
            Self::Small => 2_000_000_000,
            Self::Large => 4_000_000_000,
        };
        ProvisionedVolumeMaximumBytes::new(NonZeroU64::new(bytes).expect("a size is positive"))
    }
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
    /// Its copy references the smaller size, so `ployz_core::shrink_floor` of it sits above
    /// the target: a shrink refuses `BelowFloor`.
    Copy(Machine),
}

/// The first Move's warm round snapshot, which B already holds, and its cut's final
/// `f-<epoch>`; the second Move's snapshot before its warm pull, and its cut's. In
/// `Restore`, `Warm2` is the snapshot the mirror's round takes on the restored Writer.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Snap {
    Warm,
    Final,
    Warm2,
    Final2,
    /// The `dep-<seconds>` snapshot a departure takes of a root it parks
    /// (`freeze_root`): a point no other Machine holds, so no Writer can send a pull an
    /// incremental from it.
    Departure,
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

/// Whether a Machine answers.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Presence {
    Here,
    /// Down or cut off: nothing sent to it lands, and its Containers keep running.
    Away,
    /// It learnt the Cluster forgot it and left: its roots are parked in slots and its
    /// Containers are gone.
    Departed,
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct Host {
    copy: Held,
    /// `volume_floors`: the newest lane `gen` this Machine admitted for the Volume.
    floor: u8,
    holder: Holder,
    /// `LocalMachine::lock_switch`'s lock, held by one request from its second floor
    /// check, or a Prepare from its first read, to its reply.
    switch: Option<usize>,
    /// The exclusive admission turn, held by `start_volume_holder` from its floor
    /// check to the start.
    turn: Option<usize>,
    /// The copy's ZFS `refquota`, its root's or its slot parent's (`storage.rs`
    /// `bound()`). A cut and a promote keep it; `SetVolumeSize` and a pull raise it.
    bound: Size,
    presence: Presence,
    /// `volume_forgotten`: this Machine forgot a copy of the Volume, so its data lives on
    /// another Server.
    forgotten: bool,
    /// Cloud lists the Machine as a Server: false for a C the two-Machine scenarios
    /// never have, and for a Machine Cloud forgot.
    member: bool,
    /// Which slot holds the copy: another slot at the same snapshot replaced it once
    /// this is 1 (`Replace`).
    slot: u8,
    /// The plugin marked the slot written here, as a cut or a promote does, without
    /// changing its snapshot (`Mark`).
    marked: bool,
}

impl Host {
    fn here(&self) -> bool {
        self.presence == Presence::Here
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Lane {
    Resize,
    Move,
    Release,
    Drop,
    Restore,
    Deploy,
    Mirror,
    /// The orphan sweep's `delete_mirror`.
    Sweep,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Expect {
    Writer {
        epoch: u32,
    },
    Mirror {
        newest: Snap,
    },
    /// The slot exactly as a person confirmed deleting it.
    Confirmed(Confirmed),
    None,
}

/// A slot as a person confirmed deleting it (`ConfirmedCopy`): its snapshot, which slot
/// held it, and whether its Machine said it may hold writes no other Server has.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Confirmed {
    newest: Snap,
    slot: u8,
    written: bool,
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
    Snapshot {
        at: Snap,
    },
    Forget {
        said: Said,
    },
    Floor,
    /// `SetVolumeSize`, which carries no `gen` and checks no lane floor. A shrink run
    /// sends it with `allow_shrink`.
    Size {
        to: Size,
        allow_shrink: bool,
    },
    /// A Deploy's `PrepareVolumes`: `admit_plain_mount`, then the Volume ensured.
    Prepare,
    /// A Deploy's Container create: placement, the Volume ensured, the Container made.
    Create,
    /// A Deploy's start of the Container it created or kept.
    Start,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Refusal {
    /// A bare Forget met a slot that may hold writes no other Server has.
    WrittenHere,
    Stale,
    Busy,
    Mismatch,
    NoTransition,
    Behind,
    Switching,
    NoCapacity,
    Shrink,
    BelowFloor,
    /// The Machine refused an Operator's Writer start that creates a Container.
    Enforced,
    /// `admit_plain_mount`: the data is elsewhere, or only in this Machine's slot.
    NoWriter,
    /// `create_volume`: the name is held in a slot.
    InSlot,
    /// The plugin's Mount: no writable root to mount.
    Unmountable,
    /// `refuse_forgotten`: this Machine forgot its copy, and holds none now.
    NotWritable,
    Other,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Reply {
    Copy(Held),
    Bound(Size),
    PullFailed,
    Refused(Refusal),
}

/// Where one request is on its Machine.
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
    /// The daemon planned a slot's Forget under the switch lock and asked the plugin to
    /// destroy it (`copy_destroy`). A receive started earlier runs outside that lock, so
    /// the slot can change before the plugin acts on it.
    Destroying,
    /// A Deploy's Prepare read the forgotten record, under the switch lock; it reads its
    /// copy and its view next.
    PrepareRead {
        forgot: bool,
    },
    Replied(Reply),
    /// The daemon restarted under it; no reply comes.
    Lost,
}

/// What a Forget says about a slot that may hold writes no other Machine has.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Said {
    /// Nothing: a full sync's, and every Forget an older Cloud sends.
    Nothing,
    /// `discard`: a person's `volume mirror rm`.
    Discard,
    /// `keep_written`, which 2f4f30f267's full sync alone sent (`Broken::forget_opt_in`).
    Keep,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Request {
    lane: Lane,
    to: Machine,
    r#gen: u8,
    expect: Expect,
    want: Want,
    progress: Progress,
    /// Admitted past the start gate while the Machine refused created starts.
    enforced: bool,
}

/// A run's row in Cloud's `volume_run` table.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Row {
    /// Planned and not yet started: the row lock is free until Cloud starts it.
    Queued,
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
    /// The second Move's snapshot of the Writer before its warm pull (`runRounds`).
    Snapshot,
    /// The warm round's pull.
    Warm,
    Cut,
    Pull,
    /// A Move planned from a lone slot pulls that slot's snapshot to a `to` holding
    /// nothing, with no cut (`planMove`'s `final` before it refused a Move from slots).
    Final,
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

/// `deleteMirror`: forget each slot, then the Writer's snapshots.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum DropStep {
    Forget(Machine),
    Withdraw,
    Finish,
    Over,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum RestoreStep {
    Promote,
    Finish,
    Over,
}

/// `mirrorVolume`'s one round (`runRounds`): snapshot the Writer, then pull it. A full
/// sync first forgets the Mirror's copy, then the Writer's run snapshots.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum MirrorStep {
    Forget,
    Withdraw,
    Snapshot,
    Pull,
    Finish,
    Over,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum DeployStep {
    Prepare,
    Create,
    Start,
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

impl<S> Driver<S> {
    fn new(step: S, resends: u8) -> Self {
        Self {
            step,
            waiting: None,
            resends,
            busy: 1,
        }
    }
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
    views: [Option<Held>; 3],
    resume: Option<Resume>,
    /// The slots at the resumed root's snapshot whose kept Containers go.
    behind: [bool; 3],
    /// The other slots a Release from slots inspects, forgetting each at its snapshot.
    others: [bool; 3],
}

/// Where a Move goes and what it numbers its steps with.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct MovePlan {
    from: Machine,
    to: Machine,
    r#gen: u8,
    /// The epoch the Move gives `to`'s Writer.
    epoch: u32,
    /// The epoch of `from`'s Writer when the Move planned.
    held: u32,
    /// The snapshot the warm round pulls, and the cut's.
    warm: Snap,
    last: Snap,
}

impl MovePlan {
    const fn release_gen(self) -> u8 {
        self.r#gen + 1
    }

    /// `takeEpoch` for a warm Release: past every epoch the Move recorded.
    const fn release_epoch(self) -> u32 {
        self.epoch + 1
    }
}

#[derive(Clone, Debug, Hash, PartialEq, Eq)]
struct MoveRun {
    plan: MovePlan,
    row: Row,
    driver: Driver<MoveStep>,
    release: Option<Release>,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct DropRun {
    row: Row,
    driver: Driver<DropStep>,
    /// The slots it forgets, each as its person confirmed it when they asked.
    slots: [Option<Confirmed>; 3],
    writer: Machine,
    writer_epoch: u32,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct RestoreRun {
    row: Row,
    driver: Driver<RestoreStep>,
    on: Machine,
    expect: Expect,
    at: Snap,
    epoch: u32,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct MirrorRun {
    row: Row,
    driver: Driver<MirrorStep>,
    writer: Machine,
    epoch: u32,
    to: Machine,
    /// The slot `to` held when the run planned, and none once a full sync forgot it.
    expect: Expect,
    onto: Onto,
}

/// What a run after the Restore does to the one slot.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Onto {
    /// `ployz volume mirror`: a snapshot of the Writer and a pull of it.
    Mirror,
    /// `ployz volume sync --full`: the slot's Forget and the Writer's, then that round.
    Sync,
    /// A Move to the slot's Machine: its warm round, the same snapshot and pull.
    Move,
    /// The orphan sweep: the slot's Forget alone.
    Sweep,
}

impl MirrorRun {
    fn lane(self) -> Lane {
        match self.onto {
            Onto::Mirror | Onto::Sync => Lane::Mirror,
            Onto::Move => Lane::Move,
            Onto::Sweep => Lane::Sweep,
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct DeployRun {
    row: Row,
    on: Machine,
    driver: Driver<DeployStep>,
}

/// Which run Cloud starts next once the row is free.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Stage {
    /// Nothing has run yet.
    Start,
    /// The first Move, or its Release, still holds the row.
    First,
    /// The mirror rm holds the row.
    Drop,
    /// The second Move, or the Restore, has run or holds the row.
    Second,
    /// The `volume mirror` after the Restore has run or holds the row.
    Mirror,
    /// The `volume sync --full` after that mirror has run or holds the row.
    Sync,
    /// The Move onto that slot has run or holds the row.
    Move,
    /// The orphan sweep has run or holds the row.
    Sweep,
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
    hosts: [Host; 3],
    requests: Vec<Request>,
    /// What the Store records, which Deploys and the planners read; apart from the
    /// target the resize sends, so the order of set and record is what gets checked.
    size: Size,
    resize: Resize,
    cramped: Option<Cramped>,
    /// The first Move, then the second.
    moves: Vec<MoveRun>,
    drop: Option<DropRun>,
    restore: Option<RestoreRun>,
    mirror: Option<MirrorRun>,
    deploy: Option<DeployRun>,
    stage: Stage,
    crashes: u8,
    /// Deploys left to start.
    deploys: u8,
    /// Rows Cloud may still end early, on three Machines.
    ends: u8,
    /// Away stretches left to start.
    aways: u8,
    /// Each Machine has held no copy at some moment: a view lagging far enough reads it
    /// so.
    emptied: [bool; 3],
    /// A Restore promoted a copy while Cloud had forgotten a Machine.
    restored: bool,
    /// The copies a Restore displaced: each forgotten Machine's copy when the Restore
    /// promoted, kept while that Machine holds it, through any late promote of it.
    displaced: [bool; 3],
    /// A request from a run whose row had ended was refused as stale.
    stale_landed: bool,
    /// Both Machines refuse an Operator's start that creates a Container.
    enforcing: bool,
    /// A Machine refused a Writer start.
    refused_start: bool,
    /// A Container an Operator's start created while its Machine refused such starts.
    unapproved_start: bool,
    /// A Move's recheck before `commitMove` found its start refused, so it ended warm.
    rechecked: bool,
    /// The promote of B a Deploy repairing a refused start sent (`fenceFollowed`).
    repair: Option<usize>,
    /// That Deploy's own Create started the Service on B.
    repaired: bool,
    /// A pull or a full sync's Forget destroyed a slot holding a departure's point, which
    /// no other Machine holds.
    lost_written: bool,
    /// A mirror rm destroyed a slot other than the one its person confirmed, as they saw
    /// it: replaced since, or written there since.
    unconfirmed: bool,
}

/// The entry of a per-Machine array for `machine`.
fn at<T>(array: &[T; 3], machine: Machine) -> &T {
    let [a, b, c] = array;
    match machine {
        Machine::A => a,
        Machine::B => b,
        Machine::C => c,
    }
}

fn at_mut<T>(array: &mut [T; 3], machine: Machine) -> &mut T {
    let [a, b, c] = array;
    match machine {
        Machine::A => a,
        Machine::B => b,
        Machine::C => c,
    }
}

impl State {
    fn run_mut(&mut self, k: usize) -> &mut MoveRun {
        self.moves.get_mut(k).expect("an action names a Move")
    }

    fn host(&self, machine: Machine) -> &Host {
        at(&self.hosts, machine)
    }

    fn host_mut(&mut self, machine: Machine) -> &mut Host {
        at_mut(&mut self.hosts, machine)
    }

    fn first(&self) -> &MoveRun {
        self.moves.first().expect("every start has a first Move")
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

    fn send(&mut self, lane: Lane, r#gen: u8, to: Machine, expect: Expect, want: Want) -> usize {
        self.requests.push(Request {
            lane,
            to,
            r#gen,
            expect,
            want,
            progress: Progress::Sent,
            enforced: false,
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
            | Progress::Destroying
            | Progress::PrepareRead { .. }
            | Progress::Lost => None,
        }
    }

    /// The writable roots, but the one a Restore displaced on a forgotten Machine when
    /// `exempt` (bet 6's accepted window), until it departs.
    fn writers(&self, exempt: bool) -> usize {
        Machine::ALL
            .iter()
            .filter(|machine| {
                self.host(**machine).copy.is_writer() && !self.displaced(**machine, exempt)
            })
            .count()
    }

    fn running(&self, exempt: bool) -> usize {
        Machine::ALL
            .iter()
            .filter(|machine| {
                self.host(**machine).holder == Holder::Running && !self.displaced(**machine, exempt)
            })
            .count()
    }

    /// The copy an offline Restore displaced: the one a Machine Cloud forgot held when a
    /// Restore promoted another copy, for as long as it holds it. A fresh root that
    /// Machine makes once that copy is gone is not it.
    fn displaced(&self, machine: Machine, exempt: bool) -> bool {
        exempt && *at(&self.displaced, machine)
    }

    /// Whether no Writer holds `point` to send an incremental from: a departure's
    /// snapshot, or a cut's final that a Restore from an earlier copy skipped. The restored
    /// Writer's line is the copy it promoted, then the mirror round's `Warm2`.
    fn off_line(&self, point: Snap) -> bool {
        let skipped = self.restored
            && self.restore.is_some_and(|restore| {
                matches!(
                    (restore.at, point),
                    (Snap::Warm, Snap::Final)
                        | (Snap::Warm | Snap::Final | Snap::Warm2, Snap::Final2)
                )
            });
        point == Snap::Departure || skipped
    }

    fn note_restored(&mut self) {
        for machine in Machine::ALL {
            let Host { member, copy, .. } = *self.host(machine);
            if member {
                continue;
            }
            self.restored = true;
            if copy != Held::None {
                *at_mut(&mut self.displaced, machine) = true;
            }
        }
    }

    /// Whether `machine`'s fresh view lists a copy on another Machine.
    fn elsewhere(&self, machine: Machine) -> bool {
        Machine::ALL.iter().any(|other| {
            *other != machine
                && self.host(*other).copy != Held::None
                && self.host(*other).presence != Presence::Departed
        })
    }

    /// Whether a view lagging far enough could list no copy elsewhere: every other
    /// Machine held none at some moment.
    fn stale_empty(&self, machine: Machine) -> bool {
        Machine::ALL
            .iter()
            .all(|other| *other == machine || *at(&self.emptied, *other))
    }

    fn note_emptied(&mut self) {
        for machine in Machine::ALL {
            let host = self.host(machine);
            let gone = host.copy == Held::None;
            if gone || host.presence == Presence::Departed {
                *at_mut(&mut self.emptied, machine) = true;
            }
            if gone {
                *at_mut(&mut self.displaced, machine) = false;
            }
        }
    }

    /// Every index a driver or a lock holds, so a removal can shift them.
    fn held_indices(&mut self) -> Vec<&mut Option<usize>> {
        let mut held = vec![&mut self.resize.driver.waiting];
        for run in &mut self.moves {
            held.push(&mut run.driver.waiting);
            if let Some(release) = run.release.as_mut() {
                held.push(&mut release.driver.waiting);
            }
        }
        if let Some(drop) = self.drop.as_mut() {
            held.push(&mut drop.driver.waiting);
        }
        if let Some(restore) = self.restore.as_mut() {
            held.push(&mut restore.driver.waiting);
        }
        if let Some(mirror) = self.mirror.as_mut() {
            held.push(&mut mirror.driver.waiting);
        }
        if let Some(deploy) = self.deploy.as_mut() {
            held.push(&mut deploy.driver.waiting);
        }
        held.push(&mut self.repair);
        for host in &mut self.hosts {
            held.push(&mut host.switch);
            held.push(&mut host.turn);
        }
        held
    }

    /// A request whose answer no run will read leaves the state, and every index past it
    /// moves down one.
    fn forget(&mut self, index: usize) {
        self.requests.remove(index);
        for held in self.held_indices() {
            if let Some(at) = held.as_mut()
                && *at > index
            {
                *at -= 1;
            }
        }
    }

    /// Answers no driver waits on leave the state, as `forget` does for each.
    fn prune(&mut self) {
        for index in (0..self.requests.len()).rev() {
            let progress = self.request(index).progress;
            let landed = matches!(progress, Progress::Replied(_) | Progress::Lost);
            let read = self
                .held_indices()
                .into_iter()
                .any(|held| *held == Some(index));
            if landed && !read {
                self.stale_landed |= self.request(index).lane == Lane::Move
                    && progress == Progress::Replied(Reply::Refused(Refusal::Stale));
                self.forget(index);
            }
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
            && self.first().release.is_none()
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
            !matches!(host.copy, Held::Slot { .. })
                || Machine::ALL.iter().all(|other| {
                    let writer = self.host(*other);
                    !writer.copy.is_writer() || writer.bound <= host.bound
                })
        })
    }

    /// No run holds the row lock.
    fn row_free(&self) -> bool {
        let moves = self.moves.iter().all(|run| {
            !run.row.running()
                && run
                    .release
                    .as_ref()
                    .is_none_or(|release| !release.row.running())
        });
        moves
            && self.drop.is_none_or(|drop| !drop.row.running())
            && self.restore.is_none_or(|restore| !restore.row.running())
            && self.mirror.is_none_or(|mirror| !mirror.row.running())
            && self.deploy.is_none_or(|deploy| !deploy.row.running())
    }

    /// The Machines Cloud lists, each answering, or `None` while one doesn't.
    fn members(&self) -> Option<Vec<Machine>> {
        let members: Vec<Machine> = Machine::ALL
            .into_iter()
            .filter(|machine| self.host(*machine).member)
            .collect();
        members
            .iter()
            .all(|machine| self.host(*machine).here())
            .then_some(members)
    }

    /// The one writable root Cloud sees, where a Deploy places the Service.
    fn writer_seen(&self) -> Option<Machine> {
        let members = self.members()?;
        let roots: Vec<Machine> = members
            .into_iter()
            .filter(|machine| matches!(self.host(*machine).copy, Held::Root { .. }))
            .collect();
        match roots.as_slice() {
            [writer] if self.host(*writer).copy.is_writer() => Some(*writer),
            _ => None,
        }
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
    /// A Machine admits an Operator's Writer start that creates a Container.
    start_gate: bool,
    /// Prepare skips `admit_plain_mount`.
    plain_mount: bool,
    /// Prepare reads no forgotten record.
    prepare_forgotten: bool,
    /// Prepare takes no switch lock, so a Forget can land between its read of the
    /// forgotten record and its read of the copy.
    prepare_switch: bool,
    /// Create makes a fresh root on a Machine that forgot its copy.
    forgotten: bool,
    /// A pull whose Writer can't send an incremental onto its slot replaces a copy a
    /// departure parked with a full send (`pull_stream` without its `written_here`).
    pull_replaces_written: bool,
    /// `planMove` plans a Move from a lone slot while no Machine holds a root.
    move_from_slots: bool,
    /// The plugin reports no slot `written_here`, so a full sync's Forget destroys a copy
    /// a departure parked.
    forget_ignores_written: bool,
    /// 2f4f30f267's Forget: a Machine keeps a slot `written_here` only when the Forget says
    /// `keep_written`, which only that commit's full sync said, so a bare one destroys it.
    forget_opt_in: bool,
    /// Cloud plans a full sync onto a Machine that doesn't advertise
    /// `ployz.volume.keep-written.v1`.
    sync_skips_capability: bool,
    /// Cloud plans a mirror onto a copy a departure parked on a Machine that doesn't
    /// advertise `ployz.volume.keep-written.v1`.
    mirror_skips_capability: bool,
    /// Cloud plans a Move onto such a copy: 4c6dc32563 checked only `mirror`.
    move_skips_capability: bool,
    /// The orphan sweep forgets every slot it finds and says `discard`, as 4c6dc32563's
    /// did.
    sweep_discards: bool,
    /// The orphan sweep forgets a slot on a Machine that doesn't advertise the
    /// capability, as 4c6dc32563's did.
    sweep_skips_capability: bool,
    /// A mirror rm says `discard` for the slot its run finds rather than the one its
    /// person confirmed, as 2ff6f2c021's did.
    discard_unbound: bool,
    /// A confirmed Forget's expect names no `written_here`, as 2ff6f2c021's Machine read
    /// it.
    expect_ignores_written: bool,
    /// `copy_destroy` destroys the slot by name, as 161c95ffc2's did, without planning the
    /// daemon's Forget again on the slot it finds.
    destroy_unchecked: bool,
    /// Cloud refuses a pull onto a Machine without `ployz.volume.keep-written.v1` only
    /// when the slot's newest is a departure's `dep-` snapshot, as 161c95ffc2's
    /// `planFromCopies` did, so a cut's final a Restore skipped gets the pull.
    dep_prefix_only: bool,
}

/// Which side runs a release from before `ployz.volume.keep-written.v1`.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
enum Skew {
    #[default]
    None,
    /// Cloud sends a bare Forget for every run and plans without the capability.
    OldCloud,
    /// No Machine advertises the capability: each reports no slot `written_here`, ignores
    /// `discard`, and its pull replaces a copy it can't send an incremental onto.
    OldDaemon,
}

/// One bounded slice of the runs. Each is checked whole; together they cover the resize
/// with every late step it sends, the Move after it with every late step of its own, and
/// on three Machines the runs that follow it.
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
    /// On A, B and C: the first Move and its Release, the mirror rm, the second Move to
    /// C and its Release, and one Deploy at any moment the row is free.
    TwoMoves,
    /// On A, B and C: the first Move and its Release, a Machine away and forgotten, a
    /// Restore from the copy left, the forgotten Machine departing and joining again, a
    /// `volume mirror` onto its parked copy, and one Deploy at any moment the row is free.
    Restore,
}

impl Scenario {
    const fn three(self) -> bool {
        matches!(self, Self::TwoMoves | Self::Restore)
    }
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
            // No resize: the size never changes. B holds a Mirror before the Move, or
            // joins at its warm pull.
            Self::TwoMoves | Self::Restore => {
                vec![start(Grow, true, None), start(Grow, false, None)]
            }
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct VolumeRuns {
    scenario: Scenario,
    /// Only this direction's starts; `None` runs both.
    direction: Option<Direction>,
    /// Only starts where B held a Mirror before the first Move, or only where it did not.
    b_held: Option<bool>,
    /// Resends each run may make of a request still unanswered.
    resends: u8,
    /// Daemon restarts: during a cut's flip, and on three Machines between any floor
    /// write and the plugin step after it.
    crashes: u8,
    /// The Machines may begin refusing created starts at any moment after the resize.
    enforce: bool,
    /// `never two Writers` leaves out the copy an offline Restore displaced.
    exempt: bool,
    /// On three Machines: Deploys Cloud may start, and stretches a Machine may be away.
    deploys: u8,
    aways: u8,
    /// On three Machines: rows Cloud may end before their run ends them.
    ends: u8,
    skew: Skew,
    broken: Broken,
}

impl Default for VolumeRuns {
    fn default() -> Self {
        Self {
            scenario: Scenario::ResizeThenMove,
            direction: None,
            b_held: None,
            resends: 1,
            crashes: 1,
            enforce: false,
            exempt: true,
            deploys: 1,
            aways: 1,
            ends: 1,
            skew: Skew::None,
            broken: Broken::default(),
        }
    }
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// One step of request `n` on its Machine.
    Machine(usize),
    /// Request `n`'s Prepare reads a view lagging far enough to list no copy elsewhere.
    Lagging(usize),
    /// The daemon restarts during request `n`, losing every request on its Machine.
    Crash(usize),
    /// Move `k`'s next step, its resend, and Cloud ending its row.
    Move(usize),
    MoveResend(usize),
    EndMove(usize),
    /// Move `k`'s Release.
    Release(usize),
    ReleaseResend(usize),
    EndRelease(usize),
    Resize,
    ResizeResend,
    /// Cloud ends the resize's row: a cancel, a Release taking the Volume, or a failed
    /// step.
    EndResize,
    /// The Machines begin refusing created starts.
    Enforce,
    /// A Deploy follows a committed Move whose start was refused: it promotes B with no
    /// start, then creates the Service from its own approved spec.
    Repair,
    /// Cloud starts the next run the row lock lets in.
    Next,
    Drop,
    EndDrop,
    Restore,
    EndRestore,
    /// The `volume mirror` run's next step.
    Mirror,
    /// Cloud starts a Deploy while the row is free.
    StartDeploy,
    Deploy,
    DeployResend,
    EndDeploy,
    Away(Machine),
    Back(Machine),
    /// `ployz server forget` of a Machine that is away.
    Forget(Machine),
    /// A forgotten Machine is added back, and its enroll seals its roots as it leaves.
    Depart(Machine),
    /// A departed Machine joins the Cluster again, its parked copy still in its slot.
    Rejoin(Machine),
    /// Another slot at the same snapshot replaces the Machine's slot while a mirror rm
    /// someone confirmed waits to run.
    Replace(Machine),
    /// The plugin marks the Machine's slot written here, while a mirror rm someone
    /// confirmed waits to run or a Release may forget it.
    Mark(Machine),
}

// --- The real planner -------------------------------------------------------------

fn snapshot(snap: Snap) -> Snapshot {
    let (name, guid) = match snap {
        Snap::Warm => ("w-1-0", 1),
        Snap::Final => ("f-2", 2),
        Snap::Warm2 => ("w-4-0", 3),
        Snap::Final2 => ("f-4", 4),
        Snap::Departure => ("dep-1700000000", 5),
    };
    Snapshot {
        name: name.parse().expect("a valid snapshot name"),
        guid: SnapshotGuid::new(guid),
        created_unix_seconds: 1,
    }
}

fn slot_id(machine: Machine, slot: u8) -> SlotId {
    let first = match machine {
        Machine::A => 10,
        Machine::B => 11,
        Machine::C => 12,
    };
    SlotId::new(first + 3 * u64::from(slot))
}

/// `inspect`'s copy. A slot is `written_here` when it holds a departure's snapshot, or once
/// `Mark` stands for the cut, seed, promote and epoch marks that also set it in the
/// plugin. Broken, or on a Machine from before it, the plugin never reports it.
fn volume_copy(host: &Host, machine: Machine, reports_written: bool) -> Option<VolumeCopy> {
    match host.copy {
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
            mark: None,
            origin: None,
            placed: None,
        }),
        Held::Slot { newest } => Some(VolumeCopy::Slot {
            slot: slot_id(machine, host.slot),
            readonly: true,
            newest: newest.map(snapshot),
            resume_token: None,
            pull: None,
            role: Some(SlotRole::Mirror),
            pull_state: None,
            origin: None,
            written_here: reports_written && (host.marked || newest == Some(Snap::Departure)),
        }),
    }
}

/// The expect as the Machine reads it. A run that names no slot expects the one there now.
/// Broken, a confirmed delete names no `written_here`, as round 3's did.
fn expected(expect: Expect, machine: Machine, slot: u8, broken: Broken) -> ExpectedCopy {
    match expect {
        Expect::Writer { epoch } => ExpectedCopy::Writer {
            epoch: WriterEpoch::new(epoch),
        },
        Expect::Mirror { newest } => ExpectedCopy::Mirror {
            newest: snapshot(newest).guid,
            slot: slot_id(machine, slot),
            role: None,
            written_here: None,
        },
        Expect::Confirmed(confirmed) => ExpectedCopy::Mirror {
            newest: snapshot(confirmed.newest).guid,
            slot: slot_id(machine, confirmed.slot),
            role: None,
            written_here: (!broken.expect_ignores_written).then_some(confirmed.written),
        },
        Expect::None => ExpectedCopy::None,
    }
}

/// The want as the Machine reads it. It destroys a slot `written_here` when the Forget says
/// `discard`, or, before 2f4f30f267's opt-in was reversed, unless it says `keep_written`.
fn wanted(want: Want, broken: Broken) -> WantedCopy {
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
            holder: None,
            running: false,
        },
        Want::Snapshot { at } => WantedCopy::Snapshot {
            name: snapshot(at).name,
        },
        Want::Forget { said } => WantedCopy::Forget {
            discard: if broken.forget_opt_in {
                said != Said::Keep
            } else {
                said == Said::Discard
            },
        },
        Want::Floor => WantedCopy::Floor,
        Want::Size { .. } | Want::Prepare | Want::Create | Want::Start => {
            unreachable!("only SetVolumeCopy asks plan")
        }
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
        CopyRefusal::WrittenHere => Refusal::WrittenHere,
        CopyRefusal::Written
        | CopyRefusal::NoSource
        | CopyRefusal::Foreign
        | CopyRefusal::Held
        | CopyRefusal::Stopped
        | CopyRefusal::DockerUnavailable
        | CopyRefusal::Unrecognized => Refusal::Other,
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

/// A modelled gen as the lane gen `ployz_decide::volume_floor` reads.
fn lane(r#gen: u8) -> VolumeGen {
    VolumeGen::new(r#gen.into())
}

fn modelled(r#gen: VolumeGen) -> u8 {
    u8::try_from(r#gen.get()).expect("a modelled gen")
}

// --- One Machine ------------------------------------------------------------------

impl VolumeRuns {
    /// Whether `inspect` reports a slot's `written_here`.
    fn reports_written(&self) -> bool {
        !self.broken.forget_ignores_written && self.skew != Skew::OldDaemon
    }

    /// A slot as `inspect` reports it and a person confirms it: none for a slot with no
    /// snapshot or no slot at all.
    fn confirmed(&self, host: &Host) -> Option<Confirmed> {
        match host.copy {
            Held::Slot {
                newest: Some(newest),
            } => Some(Confirmed {
                newest,
                slot: host.slot,
                written: self.reports_written() && (host.marked || newest == Snap::Departure),
            }),
            Held::Slot { newest: None } | Held::Root { .. } | Held::None => None,
        }
    }

    /// Cloud's Forget of a slot: `discard` for a person's `mirror rm` (`deleteMirror`),
    /// bare for a Release, a full sync and the orphan sweep, which no one chose. An older
    /// Cloud sends every Forget bare; 2f4f30f267's said `keep_written` in a full sync's.
    fn forget_slot(&self, discard: bool) -> Want {
        let said = match (self.skew, self.broken.forget_opt_in, discard) {
            (Skew::OldCloud, ..) | (_, true, true) | (_, false, false) => Said::Nothing,
            (_, true, false) => Said::Keep,
            (_, false, true) => Said::Discard,
        };
        Want::Forget { said }
    }

    /// `FloorTurn::admit`, through `ployz_decide::volume_floor::admit`. Without floors a
    /// refused gen passes and leaves the floor.
    fn admit(&self, host: &mut Host, r#gen: u8) -> bool {
        match volume_floor::admit(lane(host.floor), lane(r#gen)) {
            Ok(raised) => {
                host.floor = modelled(raised);
                true
            }
            Err(_) => self.broken.floors,
        }
    }

    fn can_step(&self, state: &State, index: usize) -> bool {
        let request = state.request(index);
        let host = state.host(request.to);
        if !host.here() {
            return false;
        }
        match request.progress {
            // `set_volume_size` queues `SetVolumeSize` on the switch lock, and
            // `prepare_volumes` takes it before its admission turn.
            Progress::Sent if matches!(request.want, Want::Size { .. }) => host.switch.is_none(),
            Progress::Sent if request.want == Want::Prepare && !self.broken.prepare_switch => {
                host.switch.is_none()
            }
            Progress::Sent
            | Progress::CutDrain
            | Progress::StartRun
            | Progress::Destroying
            | Progress::PrepareRead { .. } => true,
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
                if request.want == Want::Prepare {
                    // `forgot_here`, before `admit_plain_mount` reads the copy.
                    let host = state.host_mut(to);
                    if !self.broken.prepare_switch {
                        host.switch = Some(index);
                    }
                    let forgot = host.forgotten && !self.broken.prepare_forgotten;
                    return state.set_progress(index, Progress::PrepareRead { forgot });
                }
                if matches!(request.want, Want::Create | Want::Start) {
                    let reply = self.deploy_step(state, index, false);
                    return state.reply(index, reply);
                }
                if matches!(
                    request.want,
                    Want::Writer {
                        start: Some(StartKind::Create),
                        ..
                    }
                ) && state.enforcing
                {
                    if !self.broken.start_gate {
                        state.refused_start = true;
                        state.reply(index, Reply::Refused(Refusal::Enforced));
                        return;
                    }
                    if let Some(request) = state.requests.get_mut(index) {
                        request.enforced = true;
                    }
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
                    newest: Some(cut_target(request)),
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
                state.unapproved_start |= state.request(index).enforced;
                let host = state.host_mut(to);
                host.holder = Holder::Running;
                let copy = host.copy;
                state.reply(index, Reply::Copy(copy));
            }
            Progress::PrepareRead { .. } => {
                let reply = self.deploy_step(state, index, false);
                state.reply(index, reply);
            }
            Progress::Destroying => self.destroy(state, index),
            Progress::Replied(_) | Progress::Lost => {}
        }
    }

    /// A Deploy's step on its Machine, whole but for Prepare's first read: no step reads
    /// a floor. Prepare admits a plain mount only on this Machine's writable root, or on
    /// nothing at all while its view lists no copy elsewhere; `lagging` reads that view
    /// as stale as it may be. Prepare and Create then ensure the Volume: `create_volume`
    /// refuses a name held in a slot and makes a fresh root for a name held nowhere.
    fn deploy_step(&self, state: &mut State, index: usize, lagging: bool) -> Reply {
        let request = state.request(index);
        let to = request.to;
        let held = state.host(to).copy;
        let forgotten = held == Held::None && state.host(to).forgotten;
        match request.want {
            Want::Prepare => {
                let Progress::PrepareRead { forgot } = request.progress else {
                    unreachable!("Prepare admits after it reads the forgotten record")
                };
                let forgotten = held == Held::None && forgot;
                // `admit_plain_mount`, which reads this Machine's copy before its view.
                let admitted = self.broken.plain_mount
                    || match held {
                        Held::None => !forgotten && (lagging || !state.elsewhere(to)),
                        Held::Root { readonly, .. } => !readonly,
                        Held::Slot { .. } => false,
                    };
                if !admitted {
                    return Reply::Refused(Refusal::NoWriter);
                }
                match Self::ensure(state, to) {
                    Ok(copy) => Reply::Copy(copy),
                    Err(refused) => Reply::Refused(refused),
                }
            }
            // `refuse_forgotten`, before the Volume is ensured.
            Want::Create if forgotten && !self.broken.forgotten => {
                Reply::Refused(Refusal::NotWritable)
            }
            Want::Create => match Self::ensure(state, to) {
                Ok(copy) => {
                    let host = state.host_mut(to);
                    if host.holder == Holder::Absent {
                        host.holder = Holder::Stopped;
                    }
                    Reply::Copy(copy)
                }
                Err(refused) => Reply::Refused(refused),
            },
            Want::Start => {
                let host = state.host_mut(to);
                match host.holder {
                    Holder::Running => Reply::Copy(held),
                    Holder::Stopped if held.is_writer() => {
                        host.holder = Holder::Running;
                        Reply::Copy(held)
                    }
                    Holder::Stopped | Holder::Absent => Reply::Refused(Refusal::Unmountable),
                }
            }
            Want::Writer { .. }
            | Want::Mirror { .. }
            | Want::Snapshot { .. }
            | Want::Forget { .. }
            | Want::Floor
            | Want::Size { .. } => unreachable!("a Deploy sends only its own steps"),
        }
    }

    /// `ensure_volume_source` and the plugin's `create_volume`: a root is adopted
    /// whatever it holds, a slot refuses, and nothing at all becomes a fresh root at the
    /// Store's size.
    fn ensure(state: &mut State, to: Machine) -> Result<Held, Refusal> {
        let size = state.size;
        let host = state.host_mut(to);
        match host.copy {
            Held::Root { .. } => Ok(host.copy),
            Held::Slot { .. } => Err(Refusal::InSlot),
            Held::None => {
                host.copy = Held::Root {
                    readonly: false,
                    epoch: 0,
                    newest: None,
                };
                host.bound = size;
                Ok(host.copy)
            }
        }
    }

    /// `set_size_admitted` (`ployzd/src/volume_plugin/size.rs`), whole under the switch
    /// lock. `ployz_decide::volume_size::size_plan` decides it, from the bound, the target
    /// and the bytes the copy references, which `shrink_floor` (read once, at the start of
    /// the request) holds above the target for a cramped copy. The Pool's room is the
    /// model's `fits`, as `grow_bound` reads it.
    fn set_size(state: &mut State, to: Machine, size: Size, allow_shrink: bool) -> Reply {
        let fits = state.fits(to, size);
        let referenced_bytes = if state.below_floor(to) {
            Size::Small.bytes().get()
        } else {
            0
        };
        let host = state.host_mut(to);
        let switching = match host.copy {
            Held::None => return Reply::Refused(Refusal::Other),
            Held::Root { readonly, .. } => readonly,
            Held::Slot { .. } => false,
        };
        let plan = size_plan(SizeFacts {
            bound: host.bound.bytes(),
            wanted: size.bytes(),
            allow_shrink,
            switching,
            referenced_bytes,
            copy_behind: false,
        });
        match plan {
            SizePlan::Answer(_) => Reply::Bound(size),
            SizePlan::Refuse(Refused::Shrink) => Reply::Refused(Refusal::Shrink),
            SizePlan::Refuse(Refused::Switching) => Reply::Refused(Refusal::Switching),
            SizePlan::Refuse(Refused::BelowFloor { .. }) => Reply::Refused(Refusal::BelowFloor),
            SizePlan::Shrink => {
                host.bound = size;
                Reply::Bound(size)
            }
            SizePlan::Grow { .. } if !fits => Reply::Refused(Refusal::NoCapacity),
            SizePlan::Grow { .. } => {
                host.bound = size;
                Reply::Bound(size)
            }
            SizePlan::RaiseCopy => unreachable!("a bound is one value per copy"),
        }
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

    /// `set_copy_planned` past its floor check: inspect, `plan`, and run the transition.
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
            | Want::Snapshot { .. }
            | Want::Forget { .. }
            | Want::Floor
            | Want::Size { .. }
            | Want::Prepare
            | Want::Create
            | Want::Start => None,
        };
        let plan = ployz_core::plan(
            volume_copy(&host, to, self.reports_written()).as_ref(),
            start == Some(StartState::Running),
            &expected(request.expect, to, host.slot, self.broken),
            &wanted(request.want, self.broken),
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
                if request.lane == Lane::Restore {
                    state.note_restored();
                }
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
                if request.lane == Lane::Restore {
                    state.note_restored();
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
            (CopyTransition::WriterToSnapshot, Want::Snapshot { at }) => {
                if let Held::Root {
                    readonly, epoch, ..
                } = host.copy
                {
                    state.host_mut(to).copy = Held::Root {
                        readonly,
                        epoch,
                        newest: Some(at),
                    };
                }
                state.reply(index, Reply::Copy(state.host(to).copy));
            }
            // `copy_forget`: a root keeps its data and loses its snapshots.
            (CopyTransition::WriterToForget, Want::Forget { .. }) => {
                if let Held::Root {
                    readonly, epoch, ..
                } = host.copy
                {
                    state.host_mut(to).copy = Held::Root {
                        readonly,
                        epoch,
                        newest: None,
                    };
                }
                state.reply(index, Reply::Copy(state.host(to).copy));
            }
            // The daemon's Forget of a slot: `copy_destroy` on the plugin, next.
            (
                CopyTransition::MirrorToForget | CopyTransition::SeedToForget,
                Want::Forget { .. },
            ) => state.set_progress(index, Progress::Destroying),
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
                // The pull lands in one step when the source answers and holds the target.
                let source = state.host(from);
                if !source.here() || source.copy.newest() != Some(target) {
                    return state.reply(index, Reply::PullFailed);
                }
                // `pull_stream`: the incremental starts from the slot's newest snapshot, which
                // must be on the Writer's line. A departure's is on none, and a cut's final a
                // Restore from before it skipped is on no Writer's since, so the full send
                // would replace the slot's copy. `written_here` keeps it, as the departure's
                // snapshot or the cut's own mark sets it: the pull fails.
                if let Some(point) = host.copy.newest()
                    && state.off_line(point)
                {
                    if !self.broken.pull_replaces_written && self.skew != Skew::OldDaemon {
                        return state.reply(index, Reply::PullFailed);
                    }
                    if Machine::ALL.iter().all(|machine| {
                        *machine == to || state.host(*machine).copy.newest() != Some(point)
                    }) {
                        state.lost_written = true;
                    }
                }
                let copy = Held::Slot {
                    newest: Some(target),
                };
                state.host_mut(to).copy = copy;
                state.reply(index, Reply::Copy(copy));
            }
            (transition, want) => unreachable!("the planner paired {transition:?} with {want:?}"),
        }
    }

    /// `copy_destroy` (`ployzd/src/volume_plugin/copy.rs`), under the plugin's mutation
    /// once no receive runs: it plans the daemon's Forget again on the slot it finds and
    /// destroys it only if the same Forget still deletes it. A daemon from before that
    /// check sends no expect, so its plugin destroys the slot as the daemon planned. The
    /// slot parent goes, its origin recorded first.
    fn destroy(&self, state: &mut State, index: usize) {
        let request = state.request(index);
        let to = request.to;
        let host = state.host(to).clone();
        if !self.broken.destroy_unchecked && self.skew != Skew::OldDaemon {
            let plan = ployz_core::plan(
                volume_copy(&host, to, self.reports_written()).as_ref(),
                false,
                &expected(request.expect, to, host.slot, self.broken),
                &wanted(request.want, self.broken),
            );
            match plan {
                CopyPlan::Transition(
                    CopyTransition::MirrorToForget | CopyTransition::SeedToForget,
                ) => {}
                CopyPlan::AtWant => return state.reply(index, Reply::Copy(host.copy)),
                CopyPlan::Refuse(reason) => {
                    return state.reply(index, Reply::Refused(refusal(reason)));
                }
                CopyPlan::Transition(_) => {
                    return state.reply(index, Reply::Refused(Refusal::NoTransition));
                }
            }
        }
        if request.lane == Lane::Drop
            && state.drop.and_then(|drop| *at(&drop.slots, to)) != self.confirmed(&host)
        {
            state.unconfirmed = true;
        }
        if request.lane != Lane::Drop
            && host.copy.newest() == Some(Snap::Departure)
            && Machine::ALL.iter().all(|machine| {
                *machine == to || state.host(*machine).copy.newest() != Some(Snap::Departure)
            })
        {
            state.lost_written = true;
        }
        state.host_mut(to).forgotten = true;
        state.host_mut(to).copy = Held::None;
        state.reply(index, Reply::Copy(Held::None));
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

    /// Whether a restart may land during request `index`. Two Machines restart only
    /// mid-flip; three restart between any floor write and the plugin step after it.
    fn crashes_at(&self, state: &State, index: usize) -> bool {
        let request = state.request(index);
        if state.crashes >= self.crashes || !state.host(request.to).here() {
            return false;
        }
        match request.progress {
            Progress::CutFlip => self.flips(state, index),
            Progress::Admitted
            | Progress::CutDrain
            | Progress::CutStop
            | Progress::StartAdmit
            | Progress::StartRun => self.scenario.three(),
            Progress::Sent
            | Progress::Destroying
            | Progress::PrepareRead { .. }
            | Progress::Replied(_)
            | Progress::Lost => false,
        }
    }

    /// The daemon restarts: every request on the Machine is lost with its locks. A cut
    /// caught mid-`zfs` leaves its root read-only at its snapshot.
    fn crash(state: &mut State, index: usize) {
        let request = state.request(index);
        let to = request.to;
        let host = state.host_mut(to);
        if request.progress == Progress::CutFlip
            && let Held::Root { epoch, .. } = host.copy
        {
            host.copy = Held::Root {
                readonly: true,
                epoch,
                newest: Some(cut_target(request)),
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

/// The snapshot a cut flips its root to.
fn cut_target(request: Request) -> Snap {
    match request.want {
        Want::Mirror { target, .. } => target,
        Want::Writer { .. }
        | Want::Snapshot { .. }
        | Want::Forget { .. }
        | Want::Floor
        | Want::Size { .. }
        | Want::Prepare
        | Want::Create
        | Want::Start => unreachable!("only a cut flips"),
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
        | Progress::StartRun
        | Progress::Destroying
        | Progress::PrepareRead { .. } => true,
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
                    0,
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

    /// `fenceFollowed` on a committed Move whose forward Release failed on a refused start:
    /// B, already writable at the committed epoch, needs no promote; otherwise it becomes
    /// the Writer at the snapshot the commit recorded, with no start. Then the Deploy's
    /// Create, which carries its own approved spec, starts the Service on B's Writer.
    fn repair_step(state: &mut State) {
        let plan = state.first().plan;
        let to = plan.to;
        let copy = state.host(to).copy;
        let writes =
            matches!(copy, Held::Root { readonly: false, epoch, .. } if epoch == plan.epoch);
        if state.repair.is_none() && !writes {
            if copy.newest().is_none() {
                state.repaired = true;
                return;
            }
            let sent = state.send(
                Lane::Deploy,
                plan.release_gen(),
                to,
                expect_of(copy),
                Want::Writer {
                    at: plan.last,
                    epoch: plan.epoch,
                    start: None,
                },
            );
            state.repair = Some(sent);
            return;
        }
        if let Some(sent) = state.repair.take() {
            state.forget(sent);
        }
        if state.host(to).copy.is_writer() && state.host(to).holder != Holder::Running {
            state.host_mut(to).holder = Holder::Running;
        }
        state.repaired = true;
    }

    /// `failAndRelease` and the cancel handler: end Move `k`'s running row and request
    /// the Release, back to `from` while warm and forward to `to` once committed.
    fn end_move(&self, state: &mut State, k: usize) {
        let run = &mut state.run_mut(k);
        if !run.row.running() {
            return;
        }
        let forward = run.row == Row::Committed;
        run.row = Row::Failed;
        let first = Machine::ALL
            .into_iter()
            .find(|machine| state.host(*machine).member)
            .unwrap_or(Machine::A);
        state.run_mut(k).release = Some(Release {
            row: Row::Warm,
            forward,
            driver: Driver::new(
                if forward {
                    ReleaseStep::Observe
                } else {
                    ReleaseStep::Floor(first)
                },
                self.resends,
            ),
            views: [None; 3],
            resume: None,
            behind: [false; 3],
            others: [false; 3],
        });
    }

    fn move_step(&self, state: &mut State, k: usize) {
        let MoveRun {
            plan, row, driver, ..
        } = state.run_mut(k).clone();
        let MovePlan { from, to, .. } = plan;
        let next = |state: &mut State, step: MoveStep| {
            state.run_mut(k).driver.step = step;
            state.run_mut(k).driver.waiting = None;
        };
        // Every Move step that talks to a Machine or the row checks ownership first.
        if !row.running() && !self.broken.owner_checks {
            return next(state, MoveStep::Over);
        }
        let wait = |state: &mut State, sent: usize| state.run_mut(k).driver.waiting = Some(sent);
        let reply = state.replied(driver.waiting);
        match (driver.step, driver.waiting, reply) {
            (MoveStep::Snapshot, None, _) => {
                let sent = state.send(
                    Lane::Move,
                    plan.r#gen,
                    from,
                    Expect::Writer { epoch: plan.held },
                    Want::Snapshot { at: plan.warm },
                );
                wait(state, sent);
            }
            (MoveStep::Warm, None, _) => {
                let sent = state.send(
                    Lane::Move,
                    plan.r#gen,
                    to,
                    Expect::None,
                    Want::Mirror {
                        from: Some(from),
                        target: plan.warm,
                    },
                );
                wait(state, sent);
            }
            (MoveStep::Cut, None, _) => {
                let sent = state.send(
                    Lane::Move,
                    plan.r#gen,
                    from,
                    Expect::Writer { epoch: plan.held },
                    Want::Mirror {
                        from: None,
                        target: plan.last,
                    },
                );
                wait(state, sent);
            }
            (MoveStep::Pull, None, _) => {
                let sent = state.send(
                    Lane::Move,
                    plan.r#gen,
                    to,
                    Expect::Mirror { newest: plan.warm },
                    Want::Mirror {
                        from: Some(from),
                        target: plan.last,
                    },
                );
                wait(state, sent);
            }
            (MoveStep::Final, None, _) => {
                let sent = state.send(
                    Lane::Move,
                    plan.r#gen,
                    to,
                    Expect::None,
                    Want::Mirror {
                        from: Some(from),
                        target: plan.last,
                    },
                );
                wait(state, sent);
            }
            (MoveStep::Commit, _, _) => {
                // `commitAndStart` asks `to` to check its start just before `commitMove`; a
                // refusal ends the row while warm, so its Release starts the Service on `from`.
                if row == Row::Warm && state.enforcing {
                    state.rechecked = true;
                    self.end_move(state, k);
                    return next(state, MoveStep::Over);
                }
                // `commitMove`: only a row still running commits.
                if row.running() || self.broken.owner_checks {
                    if row == Row::Warm {
                        state.run_mut(k).row = Row::Committed;
                    }
                    next(state, MoveStep::Start);
                } else {
                    next(state, MoveStep::Over);
                }
            }
            (MoveStep::Start, None, _) => {
                // `startWriter`: a `to` already running the Service is left alone.
                if state.host(to).holder == Holder::Running {
                    next(state, MoveStep::Remove);
                } else if state.host(from).holder == Holder::Stopped {
                    let sent = state.send(
                        Lane::Move,
                        plan.r#gen,
                        to,
                        Expect::Mirror { newest: plan.last },
                        Want::Writer {
                            at: plan.last,
                            epoch: plan.epoch,
                            start: Some(StartKind::Create),
                        },
                    );
                    wait(state, sent);
                } else {
                    self.end_move(state, k);
                    next(state, MoveStep::Over);
                }
            }
            (MoveStep::Remove, _, _) => {
                // `removeKept` checks the run still owns its row, then removes `from`'s
                // stopped Container.
                if state.run_mut(k).row.running() {
                    let host = state.host_mut(from);
                    if host.holder == Holder::Stopped {
                        host.holder = Holder::Absent;
                    }
                    next(state, MoveStep::Finish);
                } else {
                    next(state, MoveStep::Over);
                }
            }
            (MoveStep::Finish, _, _) => {
                if state.run_mut(k).row.running() {
                    state.run_mut(k).row = Row::Done;
                }
                next(state, MoveStep::Over);
            }
            (
                MoveStep::Snapshot
                | MoveStep::Warm
                | MoveStep::Cut
                | MoveStep::Pull
                | MoveStep::Final
                | MoveStep::Start,
                Some(_),
                Some(reply),
            ) => {
                let advanced = match (driver.step, reply) {
                    (MoveStep::Snapshot, Reply::Copy(Held::Root { newest, .. }))
                        if newest == Some(plan.warm) =>
                    {
                        Some(MoveStep::Warm)
                    }
                    (MoveStep::Warm, Reply::Copy(Held::Slot { newest }))
                        if newest == Some(plan.warm) =>
                    {
                        Some(MoveStep::Cut)
                    }
                    (
                        MoveStep::Cut | MoveStep::Pull | MoveStep::Final,
                        Reply::Copy(Held::Slot { newest }),
                    ) if newest == Some(plan.last) => Some(if driver.step == MoveStep::Cut {
                        MoveStep::Pull
                    } else {
                        MoveStep::Commit
                    }),
                    (MoveStep::Start, Reply::Copy(_)) => Some(MoveStep::Remove),
                    _ => None,
                };
                match (advanced, reply) {
                    (Some(step), _) => next(state, step),
                    // A newer lane fenced this run: it touches nothing more.
                    (None, Reply::Refused(Refusal::Stale)) => next(state, MoveStep::Over),
                    (None, Reply::Refused(Refusal::Busy)) if driver.busy > 0 => {
                        state.run_mut(k).driver.busy -= 1;
                        state.run_mut(k).driver.waiting = None;
                    }
                    (
                        None,
                        Reply::Refused(_) | Reply::PullFailed | Reply::Copy(_) | Reply::Bound(_),
                    ) => {
                        self.end_move(state, k);
                        next(state, MoveStep::Over);
                    }
                }
            }
            // Its patience with a Machine that doesn't answer runs out (`settle`).
            (_, Some(index), None) if stalled(state, index) => {
                self.end_move(state, k);
                next(state, MoveStep::Over);
            }
            (
                MoveStep::Snapshot
                | MoveStep::Warm
                | MoveStep::Cut
                | MoveStep::Pull
                | MoveStep::Final
                | MoveStep::Start,
                Some(_),
                None,
            )
            | (MoveStep::Over, _, _) => {}
        }
    }

    fn release_step(&self, state: &mut State, k: usize) {
        let Some(release) = state.run_mut(k).release.clone() else {
            return;
        };
        let plan = state.run_mut(k).plan;
        let r#gen = plan.release_gen();
        let driver = release.driver;
        let reply = state.replied(driver.waiting);
        let set = |state: &mut State, update: &dyn Fn(&mut Release)| {
            if let Some(release) = state.run_mut(k).release.as_mut() {
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
                if release.row.running() {
                    release.row = Row::Failed;
                }
                release.driver.step = ReleaseStep::Over;
                release.driver.waiting = None;
            });
        };
        // A Release whose row Cloud ended sends nothing more.
        if !release.row.running() {
            return set(state, &|release| {
                release.driver.step = ReleaseStep::Over;
                release.driver.waiting = None;
            });
        }
        if let Some(index) = driver.waiting
            && reply.is_none()
            && stalled(state, index)
        {
            return fail(state);
        }
        let resume = release.resume;
        match (driver.step, driver.waiting) {
            (ReleaseStep::Floor(machine), None) => {
                let sent = state.send(Lane::Release, r#gen, machine, Expect::None, Want::Floor);
                wait(state, sent);
            }
            (ReleaseStep::Floor(machine), Some(_)) => {
                let Some(Reply::Copy(held)) = reply else {
                    return;
                };
                set(state, &|release| {
                    if let Some(view) = release.views.get_mut(machine.index()) {
                        *view = Some(held);
                    }
                });
                let after = Machine::ALL
                    .into_iter()
                    .find(|other| *other > machine && state.host(*other).member);
                next(state, after.map_or(ReleaseStep::Plan, ReleaseStep::Floor));
            }
            (ReleaseStep::Plan, _) => self.plan_release(state, k, &release),
            (ReleaseStep::Inspect(machine), _) => {
                let Some(plan) = resume else {
                    return fail(state);
                };
                if !state.host(machine).here() {
                    return fail(state);
                }
                let after = next_other(&release, Some(machine), plan.on);
                match state.host(machine).copy {
                    Held::Root { .. } => fail(state),
                    Held::Slot {
                        newest: Some(newest),
                    } if newest == plan.at => next(state, ReleaseStep::Forget(machine)),
                    Held::Slot { .. } | Held::None => next(state, after),
                }
            }
            (ReleaseStep::Forget(machine), None) => {
                let Some(plan) = resume else {
                    return fail(state);
                };
                let sent = state.send(
                    Lane::Release,
                    r#gen,
                    machine,
                    Expect::Mirror { newest: plan.at },
                    self.forget_slot(false),
                );
                wait(state, sent);
            }
            (ReleaseStep::Resume(machine), None) => {
                let Some(resume) = resume else {
                    return fail(state);
                };
                let sent = state.send(
                    Lane::Release,
                    r#gen,
                    machine,
                    resume.expect,
                    Want::Writer {
                        at: resume.at,
                        epoch: plan.release_epoch(),
                        start: resume.start,
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
                        ReleaseStep::Forget(machine) => resume.map_or(ReleaseStep::Over, |plan| {
                            next_other(&release, Some(machine), plan.on)
                        }),
                        ReleaseStep::Resume(_) => first_behind(&release, None),
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
                // No one chose this delete: the Machine keeps a slot that may hold writes no
                // other Server has, and the Release restarts the Service all the same.
                Some(Reply::Refused(Refusal::WrittenHere))
                    if let (ReleaseStep::Forget(machine), Some(plan)) = (driver.step, resume) =>
                {
                    next(state, next_other(&release, Some(machine), plan.on));
                }
                Some(Reply::Refused(_) | Reply::PullFailed | Reply::Bound(_)) => fail(state),
            },
            (ReleaseStep::RemoveBehind(machine), _) => {
                let host = state.host_mut(machine);
                if host.holder != Holder::Absent {
                    host.holder = Holder::Absent;
                }
                next(state, first_behind(&release, Some(machine)));
            }
            (ReleaseStep::Observe, _) => {
                if state.host(plan.to).here() && state.host(plan.to).copy.newest().is_some() {
                    next(state, ReleaseStep::StartForward);
                } else {
                    fail(state);
                }
            }
            (ReleaseStep::StartForward, None) => {
                // `commitAndStart` on the inherited commit, then `startWriter` to `to`.
                let copy = state.host(plan.to).copy;
                let Some(newest) = copy.newest() else {
                    return fail(state);
                };
                // A committed Move's Release inherits its snapshot and epoch (`endAndRelease`),
                // and `takeEpoch` keeps the epoch. A `to` already writing at that epoch keeps its
                // own newest (`committedWriter`).
                let epoch = plan.epoch;
                let writes = matches!(copy, Held::Root { readonly: false, epoch: held, .. } if held == epoch);
                let at = if writes { newest } else { plan.last };
                let start = if state.host(plan.to).holder == Holder::Running {
                    return next(state, ReleaseStep::RemoveForward);
                } else if state.host(plan.from).holder == Holder::Stopped {
                    Some(StartKind::Create)
                } else if state.host(plan.to).holder == Holder::Stopped {
                    None
                } else {
                    return fail(state);
                };
                let sent = state.send(
                    Lane::Release,
                    r#gen,
                    plan.to,
                    expect_of(copy),
                    Want::Writer { at, epoch, start },
                );
                wait(state, sent);
            }
            (ReleaseStep::RemoveForward, _) => {
                let host = state.host_mut(plan.from);
                if host.holder == Holder::Stopped {
                    host.holder = Holder::Absent;
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
    fn plan_release(&self, state: &mut State, k: usize, release: &Release) {
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
        let slots = |at: Option<Snap>, but: Machine| {
            let mut found = [false; 3];
            for (machine, held) in &views {
                if *machine != but
                    && let Held::Slot { newest } = held
                    && (at.is_none() || *newest == at)
                {
                    *at_mut(&mut found, *machine) = true;
                }
            }
            found
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
            ] => Some((
                Resume {
                    on: *machine,
                    expect: Expect::Writer { epoch: *epoch },
                    at: *at,
                    start: kept(state, *machine),
                },
                if *readonly {
                    [false; 3]
                } else {
                    slots(Some(*at), *machine)
                },
                [false; 3],
            )),
            [] => views
                .iter()
                .find(|(machine, held)| {
                    matches!(held, Held::Slot { newest: Some(_) })
                        && kept(state, *machine).is_some()
                })
                .and_then(|(owner, held)| {
                    let at = held.newest()?;
                    Some((
                        Resume {
                            on: *owner,
                            expect: Expect::Mirror { newest: at },
                            at,
                            start: Some(StartKind::Kept),
                        },
                        [false; 3],
                        slots(None, *owner),
                    ))
                }),
            _ => None,
        };
        let Some(release) = state.run_mut(k).release.as_mut() else {
            return;
        };
        match plan {
            Some((resume, behind, others)) => {
                release.resume = Some(resume);
                release.behind = behind;
                release.others = others;
                release.driver.step = next_other(release, None, resume.on);
            }
            None => {
                release.row = Row::Failed;
                release.driver.step = ReleaseStep::Over;
            }
        }
    }

    fn drop_step(&self, state: &mut State) {
        let Some(drop) = state.drop else {
            return;
        };
        let set = |state: &mut State, step: DropStep| {
            if let Some(drop) = state.drop.as_mut() {
                drop.driver.step = step;
                drop.driver.waiting = None;
            }
        };
        let fail = |state: &mut State| {
            if let Some(drop) = state.drop.as_mut() {
                if drop.row.running() {
                    drop.row = Row::Failed;
                }
                drop.driver.step = DropStep::Over;
                drop.driver.waiting = None;
            }
        };
        if !drop.row.running() {
            return set(state, DropStep::Over);
        }
        let reply = state.replied(drop.driver.waiting);
        if let Some(index) = drop.driver.waiting
            && reply.is_none()
        {
            if stalled(state, index) {
                fail(state);
            } else if let Some(drop) = state.drop.as_mut() {
                drop.driver.waiting = None;
            }
            return;
        }
        let after = |machine: Option<Machine>| {
            Machine::ALL
                .into_iter()
                .find(|other| Some(*other) > machine && at(&drop.slots, *other).is_some())
                .map_or(DropStep::Withdraw, DropStep::Forget)
        };
        match (drop.driver.step, reply) {
            (DropStep::Forget(machine), None) => {
                let Some(confirmed) = *at(&drop.slots, machine) else {
                    return fail(state);
                };
                let now = self.confirmed(state.host(machine));
                // `stillConfirmed`: a copy replaced or written there since its person confirmed
                // it ends the run before anything is deleted. Broken, the run names the copy
                // it finds, as round 3's did.
                let expect = if self.broken.discard_unbound {
                    match now {
                        Some(now) => Expect::Mirror { newest: now.newest },
                        None => return fail(state),
                    }
                } else if now == Some(confirmed) {
                    Expect::Confirmed(confirmed)
                } else {
                    return fail(state);
                };
                let sent = state.send(
                    Lane::Drop,
                    DROP_GEN,
                    machine,
                    expect,
                    self.forget_slot(true),
                );
                if let Some(drop) = state.drop.as_mut() {
                    drop.driver.waiting = Some(sent);
                }
            }
            (DropStep::Forget(machine), Some(Reply::Copy(_))) => set(state, after(Some(machine))),
            (DropStep::Withdraw, None) => {
                let sent = state.send(
                    Lane::Drop,
                    DROP_GEN,
                    drop.writer,
                    Expect::Writer {
                        epoch: drop.writer_epoch,
                    },
                    Want::Forget {
                        said: Said::Nothing,
                    },
                );
                if let Some(drop) = state.drop.as_mut() {
                    drop.driver.waiting = Some(sent);
                }
            }
            // The slots are gone: a Writer that refuses is named as behind.
            (DropStep::Withdraw, Some(_)) => set(state, DropStep::Finish),
            (DropStep::Forget(_), Some(_)) => fail(state),
            (DropStep::Finish, _) => {
                if let Some(drop) = state.drop.as_mut() {
                    drop.row = Row::Done;
                    drop.driver.step = DropStep::Over;
                }
            }
            (DropStep::Over, _) => {}
        }
    }

    fn restore_step(state: &mut State) {
        let Some(restore) = state.restore else {
            return;
        };
        let end = |state: &mut State, row: Row| {
            if let Some(restore) = state.restore.as_mut() {
                if restore.row.running() {
                    restore.row = row;
                }
                restore.driver.step = RestoreStep::Over;
                restore.driver.waiting = None;
            }
        };
        if !restore.row.running() {
            return end(state, Row::Failed);
        }
        let reply = state.replied(restore.driver.waiting);
        if let Some(index) = restore.driver.waiting
            && reply.is_none()
        {
            if stalled(state, index) {
                end(state, Row::Failed);
            } else if let Some(restore) = state.restore.as_mut() {
                restore.driver.waiting = None;
            }
            return;
        }
        match (restore.driver.step, reply) {
            (RestoreStep::Promote, None) => {
                let sent = state.send(
                    Lane::Restore,
                    RESTORE_GEN,
                    restore.on,
                    restore.expect,
                    Want::Writer {
                        at: restore.at,
                        epoch: restore.epoch,
                        start: None,
                    },
                );
                if let Some(restore) = state.restore.as_mut() {
                    restore.driver.waiting = Some(sent);
                }
            }
            (RestoreStep::Promote, Some(Reply::Copy(_))) => {
                if let Some(restore) = state.restore.as_mut() {
                    restore.driver.step = RestoreStep::Finish;
                    restore.driver.waiting = None;
                }
            }
            (RestoreStep::Promote, Some(_)) => end(state, Row::Failed),
            (RestoreStep::Finish, _) => end(state, Row::Done),
            (RestoreStep::Over, _) => {}
        }
    }

    /// `mirrorVolume`: one round, a snapshot of the Writer and a pull of it onto `to`,
    /// after a full sync's two Forgets. A Move's warm round sends the same two; the orphan
    /// sweep sends the slot's Forget alone.
    fn mirror_step(&self, state: &mut State) {
        let Some(mirror) = state.mirror else {
            return;
        };
        let end = |state: &mut State, row: Row| {
            if let Some(mirror) = state.mirror.as_mut() {
                if mirror.row.running() {
                    mirror.row = row;
                }
                mirror.driver.step = MirrorStep::Over;
                mirror.driver.waiting = None;
            }
        };
        if !mirror.row.running() {
            return end(state, Row::Failed);
        }
        let reply = state.replied(mirror.driver.waiting);
        if let Some(index) = mirror.driver.waiting
            && reply.is_none()
        {
            if stalled(state, index) {
                end(state, Row::Failed);
            } else if let Some(mirror) = state.mirror.as_mut() {
                mirror.driver.waiting = None;
            }
            return;
        }
        let next = |state: &mut State, step: MirrorStep| {
            if let Some(mirror) = state.mirror.as_mut() {
                mirror.driver.step = step;
                mirror.driver.waiting = None;
            }
        };
        match (mirror.driver.step, reply) {
            (MirrorStep::Forget, None) => {
                let sent = state.send(
                    mirror.lane(),
                    MIRROR_GEN,
                    mirror.to,
                    mirror.expect,
                    self.forget_slot(mirror.onto == Onto::Sweep && self.broken.sweep_discards),
                );
                if let Some(mirror) = state.mirror.as_mut() {
                    mirror.driver.waiting = Some(sent);
                }
            }
            (MirrorStep::Forget, Some(Reply::Copy(_))) if mirror.onto == Onto::Sweep => {
                next(state, MirrorStep::Finish);
            }
            (MirrorStep::Forget, Some(Reply::Copy(_))) => {
                next(state, MirrorStep::Withdraw);
                if let Some(mirror) = state.mirror.as_mut() {
                    mirror.expect = Expect::None;
                }
            }
            (MirrorStep::Withdraw, None) => {
                let sent = state.send(
                    mirror.lane(),
                    MIRROR_GEN,
                    mirror.writer,
                    Expect::Writer {
                        epoch: mirror.epoch,
                    },
                    Want::Forget {
                        said: Said::Nothing,
                    },
                );
                if let Some(mirror) = state.mirror.as_mut() {
                    mirror.driver.waiting = Some(sent);
                }
            }
            (MirrorStep::Withdraw, Some(Reply::Copy(_))) => next(state, MirrorStep::Snapshot),
            (MirrorStep::Snapshot, None) => {
                let sent = state.send(
                    mirror.lane(),
                    MIRROR_GEN,
                    mirror.writer,
                    Expect::Writer {
                        epoch: mirror.epoch,
                    },
                    Want::Snapshot { at: Snap::Warm2 },
                );
                if let Some(mirror) = state.mirror.as_mut() {
                    mirror.driver.waiting = Some(sent);
                }
            }
            (MirrorStep::Snapshot, Some(Reply::Copy(_))) => next(state, MirrorStep::Pull),
            (MirrorStep::Pull, None) => {
                let sent = state.send(
                    mirror.lane(),
                    MIRROR_GEN,
                    mirror.to,
                    mirror.expect,
                    Want::Mirror {
                        from: Some(mirror.writer),
                        target: Snap::Warm2,
                    },
                );
                if let Some(mirror) = state.mirror.as_mut() {
                    mirror.driver.waiting = Some(sent);
                }
            }
            (MirrorStep::Pull, Some(Reply::Copy(_))) => next(state, MirrorStep::Finish),
            (
                MirrorStep::Forget | MirrorStep::Withdraw | MirrorStep::Snapshot | MirrorStep::Pull,
                Some(_),
            ) => end(state, Row::Failed),
            (MirrorStep::Finish, _) => end(state, Row::Done),
            (MirrorStep::Over, _) => {}
        }
    }

    fn deploy_driver(state: &mut State) {
        let Some(deploy) = state.deploy else {
            return;
        };
        let end = |state: &mut State, row: Row| {
            if let Some(deploy) = state.deploy.as_mut() {
                if deploy.row.running() {
                    deploy.row = row;
                }
                deploy.driver.step = DeployStep::Over;
                deploy.driver.waiting = None;
            }
        };
        if !deploy.row.running() {
            return end(state, Row::Failed);
        }
        let reply = state.replied(deploy.driver.waiting);
        if let Some(index) = deploy.driver.waiting
            && reply.is_none()
        {
            if stalled(state, index) {
                end(state, Row::Failed);
            } else if let Some(deploy) = state.deploy.as_mut() {
                deploy.driver.waiting = None;
            }
            return;
        }
        let step = deploy.driver.step;
        let want = match step {
            DeployStep::Prepare => Want::Prepare,
            DeployStep::Create => Want::Create,
            DeployStep::Start => Want::Start,
            DeployStep::Finish => return end(state, Row::Done),
            DeployStep::Over => return,
        };
        match reply {
            None => {
                let sent = state.send(Lane::Deploy, 0, deploy.on, Expect::None, want);
                if let Some(deploy) = state.deploy.as_mut() {
                    deploy.driver.waiting = Some(sent);
                }
            }
            Some(Reply::Copy(_)) => {
                if let Some(deploy) = state.deploy.as_mut() {
                    deploy.driver.step = match step {
                        DeployStep::Prepare => DeployStep::Create,
                        DeployStep::Create => DeployStep::Start,
                        DeployStep::Start | DeployStep::Finish | DeployStep::Over => {
                            DeployStep::Finish
                        }
                    };
                    deploy.driver.waiting = None;
                }
            }
            Some(_) => end(state, Row::Failed),
        }
    }

    /// The run Cloud starts next once the row is free, as its planner would plan it, or
    /// `None` when the planner refuses or nothing is left.
    fn next_run(&self, state: &State) -> Option<State> {
        if !self.scenario.three() || !state.row_free() {
            return None;
        }
        let members = state.members()?;
        let mut next = state.clone();
        let roots: Vec<Machine> = members
            .iter()
            .copied()
            .filter(|machine| matches!(state.host(*machine).copy, Held::Root { .. }))
            .collect();
        let slots: Vec<Machine> = members
            .iter()
            .copied()
            .filter(|machine| matches!(state.host(*machine).copy, Held::Slot { .. }))
            .collect();
        match (self.scenario, state.stage) {
            (_, Stage::Start) => {
                let first = next.run_mut(0);
                first.row = Row::Warm;
                first.driver.step = if state.host(Machine::B).copy == Held::None {
                    MoveStep::Warm
                } else {
                    MoveStep::Cut
                };
                next.stage = Stage::First;
            }
            // `planMove` refuses a Move while no Machine holds a root, so a Release makes
            // one Writer first. Broken, it moves from the lone slot onto C.
            (Scenario::TwoMoves, Stage::First | Stage::Drop)
                if self.broken.move_from_slots && roots.is_empty() =>
            {
                let [source] = slots.as_slice() else {
                    return None;
                };
                if *source == Machine::C || state.host(*source).holder == Holder::Absent {
                    return None;
                }
                let plan = MovePlan {
                    from: *source,
                    to: Machine::C,
                    r#gen: SECOND_GEN,
                    epoch: SECOND_EPOCH,
                    // A Move from a slot sends no step that expects a Writer.
                    held: 0,
                    warm: Snap::Warm2,
                    last: state.host(*source).copy.newest()?,
                };
                next.moves.push(MoveRun {
                    plan,
                    row: Row::Warm,
                    driver: Driver::new(MoveStep::Final, self.resends),
                    release: None,
                });
                next.stage = Stage::Second;
            }
            // `planMove` refuses beside another slot, so `volume mirror rm` goes first.
            (Scenario::TwoMoves, Stage::First) if !slots.is_empty() => {
                let [writer] = roots.as_slice() else {
                    return None;
                };
                let Held::Root {
                    readonly: false,
                    epoch,
                    ..
                } = state.host(*writer).copy
                else {
                    return None;
                };
                let mut forget = [None; 3];
                for slot in &slots {
                    *at_mut(&mut forget, *slot) = Some(self.confirmed(state.host(*slot))?);
                }
                let first = Machine::ALL
                    .into_iter()
                    .find(|machine| at(&forget, *machine).is_some())?;
                next.drop = Some(DropRun {
                    row: Row::Warm,
                    driver: Driver::new(DropStep::Forget(first), 0),
                    slots: forget,
                    writer: *writer,
                    writer_epoch: epoch,
                });
                next.stage = Stage::Drop;
            }
            (Scenario::TwoMoves, Stage::First | Stage::Drop) => {
                let ([writer], []) = (roots.as_slice(), slots.as_slice()) else {
                    return None;
                };
                let Held::Root {
                    readonly: false,
                    epoch,
                    ..
                } = state.host(*writer).copy
                else {
                    return None;
                };
                // `findHolder`: a Move needs the Service it moves.
                if *writer == Machine::C || state.host(*writer).holder == Holder::Absent {
                    return None;
                }
                let plan = MovePlan {
                    from: *writer,
                    to: Machine::C,
                    r#gen: SECOND_GEN,
                    epoch: SECOND_EPOCH,
                    held: epoch,
                    warm: Snap::Warm2,
                    last: Snap::Final2,
                };
                next.moves.push(MoveRun {
                    plan,
                    row: Row::Warm,
                    driver: Driver::new(MoveStep::Snapshot, self.resends),
                    release: None,
                });
                next.stage = Stage::Second;
            }
            // `planRestore`: the copy must be the only one Cloud sees, and not a Writer.
            (Scenario::Restore, Stage::First) => {
                let held: Vec<Machine> = members
                    .iter()
                    .copied()
                    .filter(|machine| state.host(*machine).copy != Held::None)
                    .collect();
                let [on] = held.as_slice() else {
                    return None;
                };
                let copy = state.host(*on).copy;
                let at = copy.newest()?;
                if copy.is_writer() {
                    return None;
                }
                next.restore = Some(RestoreRun {
                    row: Row::Warm,
                    driver: Driver::new(RestoreStep::Promote, 0),
                    on: *on,
                    expect: expect_of(copy),
                    at,
                    epoch: match copy {
                        Held::Root { epoch, .. } => epoch,
                        Held::Slot { .. } | Held::None => RESTORE_EPOCH,
                    },
                });
                next.stage = Stage::Second;
            }
            // `planCopies`' mirror: the one Writer, and no slot but on the Machine it
            // mirrors to, so a Machine that joined again with its old copy takes the pull.
            // Then its full sync onto that one slot, which Cloud plans whatever the slot
            // holds: only the Machine refuses a parked copy. Then a Move onto that slot,
            // and the orphan sweep once the Volume left the config. Every pull onto a slot
            // passes `planFromCopies`' one check, so a Machine without
            // `ployz.volume.keep-written.v1` takes no mirror and no Move onto a copy a
            // departure parked, and no full sync. The sweep keeps a slot its Machine reports
            // `written_here`, and every slot on a Machine without the capability.
            (Scenario::Restore, Stage::Second | Stage::Mirror | Stage::Sync | Stage::Move) => {
                let ([writer], [to]) = (roots.as_slice(), slots.as_slice()) else {
                    return None;
                };
                let Held::Root {
                    readonly: false,
                    epoch,
                    ..
                } = state.host(*writer).copy
                else {
                    return None;
                };
                let (onto, stage) = match state.stage {
                    Stage::Second => (Onto::Mirror, Stage::Mirror),
                    Stage::Mirror => (Onto::Sync, Stage::Sync),
                    Stage::Sync => (Onto::Move, Stage::Move),
                    Stage::Move => (Onto::Sweep, Stage::Sweep),
                    Stage::Start | Stage::First | Stage::Drop | Stage::Sweep => return None,
                };
                next.stage = stage;
                next.mirror = None;
                let newest = state.host(*to).copy.newest();
                let parked = newest == Some(Snap::Departure);
                // `mayReplace`: only a pull from the Writer's newest snapshot surely adds to
                // the slot, whatever the slot's snapshot is called.
                let replaces = if self.broken.dep_prefix_only {
                    parked
                } else {
                    newest.is_some() && newest != state.host(*writer).copy.newest()
                };
                let capable = self.skew != Skew::OldDaemon;
                let planned = match onto {
                    Onto::Mirror => capable || !replaces || self.broken.mirror_skips_capability,
                    Onto::Move => capable || !replaces || self.broken.move_skips_capability,
                    Onto::Sync => capable || self.broken.sync_skips_capability,
                    Onto::Sweep => {
                        self.broken.sweep_discards
                            || match self.skew {
                                Skew::None => !(parked && self.reports_written()),
                                Skew::OldCloud => true,
                                Skew::OldDaemon => self.broken.sweep_skips_capability,
                            }
                    }
                };
                if !planned {
                    return Some(next);
                }
                next.mirror = Some(MirrorRun {
                    row: Row::Warm,
                    driver: Driver::new(
                        match onto {
                            Onto::Sync | Onto::Sweep => MirrorStep::Forget,
                            Onto::Mirror | Onto::Move => MirrorStep::Snapshot,
                        },
                        0,
                    ),
                    writer: *writer,
                    epoch,
                    to: *to,
                    expect: expect_of(state.host(*to).copy),
                    onto,
                });
            }
            _ => return None,
        }
        Some(next)
    }

    /// A Deploy Cloud places on the Writer it sees: the start of the Container it kept
    /// there, or a Prepare, a Create and a start.
    fn start_deploy(state: &State) -> Option<State> {
        if state.deploys == 0 || !state.row_free() {
            return None;
        }
        let on = state.writer_seen()?;
        let mut next = state.clone();
        next.deploys -= 1;
        let step = if state.host(on).holder == Holder::Stopped {
            DeployStep::Start
        } else {
            DeployStep::Prepare
        };
        next.deploy = Some(DeployRun {
            row: Row::Warm,
            on,
            driver: Driver::new(step, 0),
        });
        Some(next)
    }
}

/// The first Machine after `after` a Release from slots inspects, else its resume.
fn next_other(release: &Release, after: Option<Machine>, on: Machine) -> ReleaseStep {
    Machine::ALL
        .into_iter()
        .find(|machine| Some(*machine) > after && *at(&release.others, *machine))
        .map_or(ReleaseStep::Resume(on), ReleaseStep::Inspect)
}

/// The first Machine after `after` whose kept Container the Release removes, else its
/// finish.
fn first_behind(release: &Release, after: Option<Machine>) -> ReleaseStep {
    Machine::ALL
        .into_iter()
        .find(|machine| Some(*machine) > after && *at(&release.behind, *machine))
        .map_or(ReleaseStep::Finish, ReleaseStep::RemoveBehind)
}

/// The request `index` is unanswered on a Machine that is not here: the driver's
/// patience runs out.
fn stalled(state: &State, index: usize) -> bool {
    let request = state.request(index);
    state_in_flight(request.progress) && !state.host(request.to).here()
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
        | Progress::StartRun
        | Progress::Destroying
        | Progress::PrepareRead { .. } => driver.resends > 0,
    }
}

fn steps<S>(state: &State, driver: Driver<S>) -> bool {
    driver.waiting.is_none()
        || state.replied(driver.waiting).is_some()
        || driver.waiting.is_some_and(|index| stalled(state, index))
}

/// A driver that retries a step whose reply a restart lost, as an Inngest step does.
fn retries<S>(state: &State, driver: Driver<S>) -> bool {
    let lost = driver
        .waiting
        .is_some_and(|index| state.request(index).progress == Progress::Lost);
    lost || steps(state, driver)
}

/// The resend: the driver stops waiting on its request, which may still land.
fn resend<S>(state: &State, driver: &mut Driver<S>) {
    if let Some(index) = driver.waiting
        && state.request(index).progress != Progress::Lost
    {
        driver.resends -= 1;
    }
    driver.waiting = None;
}

impl VolumeRuns {
    fn host_at(copy: Held, holder: Holder, bound: Size, member: bool) -> Host {
        Host {
            copy,
            floor: 0,
            holder,
            switch: None,
            turn: None,
            bound,
            presence: Presence::Here,
            forgotten: false,
            member,
            slot: 0,
            marked: false,
        }
    }

    fn initial(&self, start: Origin) -> State {
        let three = self.scenario.three();
        let from = start.direction.from();
        let b = if start.b_held {
            Held::Slot {
                newest: Some(Snap::Warm),
            }
        } else {
            Held::None
        };
        let mut state = State {
            hosts: [
                Self::host_at(
                    Held::Root {
                        readonly: false,
                        epoch: OLD_EPOCH,
                        newest: Some(Snap::Warm),
                    },
                    Holder::Running,
                    from,
                    true,
                ),
                Self::host_at(b, Holder::Absent, from, true),
                Self::host_at(Held::None, Holder::Absent, from, three),
            ],
            requests: Vec::new(),
            size: from,
            resize: Resize {
                row: Row::Warm,
                driver: Driver {
                    step: self.walk(start.direction)[0],
                    waiting: None,
                    resends: match self.scenario {
                        Scenario::Resize => self.resends,
                        Scenario::ResizeThenMove
                        | Scenario::LateResize
                        | Scenario::TwoMoves
                        | Scenario::Restore => 0,
                    },
                    busy: 1,
                },
                direction: start.direction,
                held: false,
                refused: false,
                cut_short: false,
            },
            cramped: start.cramped,
            moves: vec![MoveRun {
                plan: FIRST_MOVE,
                row: Row::Warm,
                driver: Driver::new(
                    if start.b_held {
                        MoveStep::Cut
                    } else {
                        MoveStep::Warm
                    },
                    self.resends,
                ),
                release: None,
            }],
            drop: None,
            restore: None,
            mirror: None,
            deploy: None,
            stage: Stage::First,
            crashes: 0,
            deploys: if three { self.deploys } else { 0 },
            aways: if three { self.aways } else { 0 },
            ends: self.ends,
            emptied: [false; 3],
            restored: false,
            displaced: [false; 3],
            stale_landed: false,
            enforcing: false,
            refused_start: false,
            rechecked: false,
            repair: None,
            repaired: false,
            unapproved_start: false,
            lost_written: false,
            unconfirmed: false,
        };
        if three {
            // No resize on three Machines: the size never changes. A Deploy may come before
            // the first Move.
            state.resize.row = Row::Failed;
            state.resize.driver.step = ResizeStep::Over;
            state.stage = Stage::Start;
            state.run_mut(0).row = Row::Queued;
            state.run_mut(0).driver.step = MoveStep::Over;
        }
        state.note_emptied();
        state
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
            .filter(|start| self.b_held.is_none_or(|only| only == start.b_held))
            .map(|start| self.initial(start))
            .collect()
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        for (index, request) in state.requests.iter().enumerate() {
            if self.can_step(state, index) {
                actions.push(Action::Machine(index));
                if matches!(request.progress, Progress::PrepareRead { .. })
                    && state.host(request.to).copy == Held::None
                    && state.elsewhere(request.to)
                    && state.stale_empty(request.to)
                {
                    actions.push(Action::Lagging(index));
                }
            }
            if self.crashes_at(state, index) {
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
                Scenario::ResizeThenMove | Scenario::TwoMoves | Scenario::Restore => false,
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
        if self.enforce && !state.enforcing && !at_rest_but_enforce(self, state) {
            actions.push(Action::Enforce);
        }
        if repairs(state) {
            actions.push(Action::Repair);
        }
        let three = self.scenario.three();
        for (k, run) in state.moves.iter().enumerate() {
            // A Move whose row ended stops whenever its step next runs or retries, leaving
            // what it sent in flight.
            let stopped = !run.row.running() && !self.broken.owner_checks;
            if run.driver.step != MoveStep::Over && (stopped || steps(state, run.driver)) {
                actions.push(Action::Move(k));
            }
            if run.driver.step != MoveStep::Over
                && (run.row.running() || self.broken.owner_checks)
                && resendable(state, run.driver)
            {
                actions.push(Action::MoveResend(k));
            }
            if run.row.running() && (!three || state.ends > 0) {
                actions.push(Action::EndMove(k));
            }
            if let Some(release) = &run.release
                && release.driver.step != ReleaseStep::Over
            {
                if !release.row.running() || steps(state, release.driver) {
                    actions.push(Action::Release(k));
                }
                if release.row.running() && resendable(state, release.driver) {
                    actions.push(Action::ReleaseResend(k));
                }
                if three && release.row.running() && state.ends > 0 {
                    actions.push(Action::EndRelease(k));
                }
            }
        }
        if !three {
            return;
        }
        if let Some(drop) = state.drop
            && drop.driver.step != DropStep::Over
        {
            if !drop.row.running() || retries(state, drop.driver) {
                actions.push(Action::Drop);
            }
            if drop.row.running() && state.ends > 0 {
                actions.push(Action::EndDrop);
            }
            for machine in Machine::ALL {
                let host = state.host(machine);
                if !drop.row.running()
                    || at(&drop.slots, machine).is_none()
                    || !matches!(host.copy, Held::Slot { .. })
                {
                    continue;
                }
                if host.slot == 0 {
                    actions.push(Action::Replace(machine));
                }
                if !host.marked && self.reports_written() {
                    actions.push(Action::Mark(machine));
                }
            }
        }
        // A slot a running Release may forget can be marked written there first.
        if let Some(release) = state.moves.iter().find_map(|run| run.release.as_ref())
            && release.row.running()
            && self.reports_written()
        {
            for machine in Machine::ALL {
                let host = state.host(machine);
                if !host.marked && matches!(host.copy, Held::Slot { newest: Some(_) }) {
                    actions.push(Action::Mark(machine));
                }
            }
        }
        if let Some(restore) = state.restore
            && restore.driver.step != RestoreStep::Over
        {
            if !restore.row.running() || retries(state, restore.driver) {
                actions.push(Action::Restore);
            }
            if restore.row.running() && state.ends > 0 {
                actions.push(Action::EndRestore);
            }
        }
        if let Some(mirror) = state.mirror
            && mirror.driver.step != MirrorStep::Over
            && (!mirror.row.running() || retries(state, mirror.driver))
        {
            actions.push(Action::Mirror);
        }
        if let Some(deploy) = state.deploy
            && deploy.driver.step != DeployStep::Over
        {
            if !deploy.row.running() || retries(state, deploy.driver) {
                actions.push(Action::Deploy);
            }
            if deploy.row.running() && resendable(state, deploy.driver) {
                actions.push(Action::DeployResend);
            }
            if deploy.row.running() && state.ends > 0 {
                actions.push(Action::EndDeploy);
            }
        }
        if self.next_run(state).is_some() {
            actions.push(Action::Next);
        }
        if Self::start_deploy(state).is_some() {
            actions.push(Action::StartDeploy);
        }
        for machine in Machine::ALL {
            let host = state.host(machine);
            match host.presence {
                Presence::Here if state.aways > 0 && host.copy != Held::None => {
                    actions.push(Action::Away(machine));
                }
                Presence::Here if !host.member => actions.push(Action::Depart(machine)),
                Presence::Away => {
                    actions.push(Action::Back(machine));
                    if host.member && self.scenario == Scenario::Restore {
                        actions.push(Action::Forget(machine));
                    }
                }
                // Only once the Restore is over and nothing sent to it is in flight: a
                // late step reaching a Machine that departed and joined again is out of
                // scope, as the model's departure keeps a pulled slot the real one
                // destroys.
                Presence::Departed
                    if self.scenario == Scenario::Restore
                        && state.stage == Stage::Second
                        && state.row_free()
                        && !state.requests.iter().any(|request| {
                            request.to == machine && state_in_flight(request.progress)
                        }) =>
                {
                    actions.push(Action::Rejoin(machine));
                }
                Presence::Here | Presence::Departed => {}
            }
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = last.clone();
        if self.scenario.three()
            && matches!(
                action,
                Action::EndMove(_)
                    | Action::EndRelease(_)
                    | Action::EndDrop
                    | Action::EndRestore
                    | Action::EndDeploy
            )
        {
            state.ends -= 1;
        }
        match action {
            Action::Machine(index) => self.step_machine(&mut state, index),
            Action::Lagging(index) => {
                let reply = self.deploy_step(&mut state, index, true);
                state.reply(index, reply);
            }
            Action::Crash(index) => Self::crash(&mut state, index),
            Action::Move(k) => self.move_step(&mut state, k),
            Action::MoveResend(k) => {
                let mut driver = state.run_mut(k).driver;
                resend(&state, &mut driver);
                state.run_mut(k).driver = driver;
            }
            Action::EndMove(k) => self.end_move(&mut state, k),
            Action::Release(k) => self.release_step(&mut state, k),
            Action::ReleaseResend(k) => {
                if let Some(mut driver) = state
                    .run_mut(k)
                    .release
                    .as_ref()
                    .map(|release| release.driver)
                {
                    resend(&state, &mut driver);
                    if let Some(release) = state.run_mut(k).release.as_mut() {
                        release.driver = driver;
                    }
                }
            }
            Action::EndRelease(k) => {
                if let Some(release) = state.run_mut(k).release.as_mut() {
                    release.row = Row::Failed;
                }
            }
            Action::Resize => self.resize_step(&mut state),
            Action::ResizeResend => {
                let mut driver = state.resize.driver;
                resend(&state, &mut driver);
                state.resize.driver = driver;
            }
            Action::EndResize => {
                state.resize.row = Row::Failed;
                state.resize.cut_short = true;
            }
            Action::Enforce => state.enforcing = true,
            Action::Repair => Self::repair_step(&mut state),
            Action::Next => state = self.next_run(&state)?,
            Action::Drop => self.drop_step(&mut state),
            Action::Replace(machine) => state.host_mut(machine).slot = 1,
            Action::Mark(machine) => state.host_mut(machine).marked = true,
            Action::EndDrop => {
                if let Some(drop) = state.drop.as_mut() {
                    drop.row = Row::Failed;
                }
            }
            Action::Restore => Self::restore_step(&mut state),
            Action::EndRestore => {
                if let Some(restore) = state.restore.as_mut() {
                    restore.row = Row::Failed;
                }
            }
            Action::Mirror => self.mirror_step(&mut state),
            Action::StartDeploy => state = Self::start_deploy(&state)?,
            Action::Deploy => Self::deploy_driver(&mut state),
            Action::DeployResend => {
                if let Some(mut driver) = state.deploy.map(|deploy| deploy.driver) {
                    resend(&state, &mut driver);
                    if let Some(deploy) = state.deploy.as_mut() {
                        deploy.driver = driver;
                    }
                }
            }
            Action::EndDeploy => {
                if let Some(deploy) = state.deploy.as_mut() {
                    deploy.row = Row::Failed;
                }
            }
            Action::Away(machine) => {
                state.aways -= 1;
                state.host_mut(machine).presence = Presence::Away;
            }
            Action::Back(machine) => state.host_mut(machine).presence = Presence::Here,
            Action::Forget(machine) => state.host_mut(machine).member = false,
            // `departure.rs` snapshots every root and seals it into a slot; the Machine
            // leaves the Cluster.
            Action::Depart(machine) => {
                let host = state.host_mut(machine);
                if let Held::Root { .. } = host.copy {
                    host.copy = Held::Slot {
                        newest: Some(Snap::Departure),
                    };
                }
                host.holder = Holder::Absent;
                host.presence = Presence::Departed;
                host.switch = None;
                host.turn = None;
            }
            Action::Rejoin(machine) => {
                let host = state.host_mut(machine);
                host.presence = Presence::Here;
                host.member = true;
            }
        }
        if self.scenario.three() {
            state.prune();
            state.note_emptied();
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(NEVER_TWO_WRITERS, |model: &Self, state: &State| {
                state.writers(model.exempt) <= 1
            }),
            Property::always(ONE_SERVICE, |model: &Self, state: &State| {
                state.running(model.exempt) <= 1
            }),
            Property::always(SERVICE_ON_WRITER, |_, state: &State| {
                Machine::ALL.iter().all(|machine| {
                    let host = state.host(*machine);
                    host.holder != Holder::Running || host.copy.is_writer()
                })
            }),
            Property::always(NOTHING_STUCK, |model: &Self, state: &State| {
                !at_rest(model, state) || all_over(model, state)
            }),
            Property::always(ENFORCED_STARTS_NOTHING, |_, state: &State| {
                !state.unapproved_start
            }),
            Property::always(STORE_FITS, |_, state: &State| state.store_fits()),
            Property::always(KEEPS_WRITTEN, |_, state: &State| !state.lost_written),
            Property::always(CONFIRMED_ONLY, |_, state: &State| !state.unconfirmed),
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
        if !self.scenario.three() {
            properties.push(Property::always(
                DONE_HOLDS,
                |model: &Self, state: &State| !at_rest(model, state) || done_holds(state),
            ));
            // A Release whose start a Machine refused can't finish.
            properties.push(Property::always(
                RELEASE_WINS,
                |model: &Self, state: &State| {
                    !at_rest(model, state)
                        || state.refused_start
                        || state
                            .first()
                            .release
                            .as_ref()
                            .is_none_or(|release| release.row == Row::Done)
                },
            ));
        }
        properties.extend(self.reached());
        if self.enforce {
            properties.extend(enforced_reached());
        }
        properties
    }
}

/// Every run's driver has stopped.
fn all_over(model: &VolumeRuns, state: &State) -> bool {
    state.resize.driver.step == ResizeStep::Over
        && (model.scenario == Scenario::Resize
            || state.moves.iter().all(|run| {
                run.driver.step == MoveStep::Over
                    && run
                        .release
                        .as_ref()
                        .is_none_or(|release| release.driver.step == ReleaseStep::Over)
            }))
        && state
            .drop
            .is_none_or(|drop| drop.driver.step == DropStep::Over)
        && state
            .restore
            .is_none_or(|restore| restore.driver.step == RestoreStep::Over)
        && state
            .mirror
            .is_none_or(|mirror| mirror.driver.step == MirrorStep::Over)
        && state
            .deploy
            .is_none_or(|deploy| deploy.driver.step == DeployStep::Over)
}

/// What refusing created starts mid-Move must reach: each such start a Move or its Release
/// sends refused, with the Volume's copies left as they were and no Service started.
fn enforced_reached() -> Vec<Property<VolumeRuns>> {
    vec![
        Property::sometimes(
            "a Release back to A starts its kept Container while Machines refuse created starts",
            |_, state: &State| {
                state.enforcing
                    && state.host(Machine::A).holder == Holder::Running
                    && state
                        .first()
                        .release
                        .as_ref()
                        .is_some_and(|release| !release.forward && release.row == Row::Done)
            },
        ),
        Property::sometimes(
            "a Move B would refuse to start releases back to A before its commit",
            |_, state: &State| {
                state.rechecked
                    && state.host(Machine::A).holder == Holder::Running
                    && state
                        .first()
                        .release
                        .as_ref()
                        .is_some_and(|release| !release.forward && release.row == Row::Done)
            },
        ),
        Property::sometimes(
            "a Deploy repairs a refused start: B writes and runs the Service",
            |_, state: &State| {
                state.repaired
                    && state.host(Machine::B).copy.is_writer()
                    && state.host(Machine::B).holder == Holder::Running
            },
        ),
        Property::sometimes(
            "a Machine refuses a committed Move's start and its Release forward",
            |_, state: &State| {
                state.refused_start
                    && state.running(false) == 0
                    && state
                        .first()
                        .release
                        .as_ref()
                        .is_some_and(|release| release.forward && release.row == Row::Failed)
            },
        ),
    ]
}

impl VolumeRuns {
    /// What each scenario must reach, so no property passes because the model can't get
    /// there.
    #[expect(clippy::too_many_lines, reason = "one list of rows per scenario")]
    fn reached(&self) -> Vec<Property<Self>> {
        let move_ends = || {
            vec![
                Property::<Self>::sometimes("a Move finishes", |_, state: &State| {
                    state.first().row == Row::Done
                }),
                Property::sometimes("a Release back to A finishes", |_, state: &State| {
                    state
                        .first()
                        .release
                        .as_ref()
                        .is_some_and(|release| !release.forward && release.row == Row::Done)
                }),
                Property::sometimes("a Release forward to B finishes", |_, state: &State| {
                    state
                        .first()
                        .release
                        .as_ref()
                        .is_some_and(|release| release.forward && release.row == Row::Done)
                }),
                Property::sometimes(
                    "a dead Move's late request is refused as stale",
                    |_, state: &State| {
                        !state.first().row.running()
                            && (state.stale_landed
                                || state.requests.iter().any(|request| {
                                    request.lane == Lane::Move
                                        && request.progress
                                            == Progress::Replied(Reply::Refused(Refusal::Stale))
                                }))
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
                            && state.host(Machine::A).copy.is_writer()
                            && matches!(state.host(Machine::B).copy, Held::Slot { .. })
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
                            && state.host(Machine::B).bound == Size::Large
                    },
                ),
                Property::sometimes(
                    "a shrink finishes with both copies lowered",
                    |_, state: &State| {
                        shrink(state)
                            && state.resize.row == Row::Done
                            && state.host(Machine::A).bound == Size::Small
                            && state.host(Machine::B).bound == Size::Small
                    },
                ),
                Property::sometimes(
                    "a shrink the Mirror refused ends with the Store at the new size, the primary lowered and the Mirror still larger",
                    |_, state: &State| {
                        shrink(state)
                            && state.resize.refused
                            && state.resize.driver.step == ResizeStep::Over
                            && state.size == Size::Small
                            && state.host(Machine::A).bound == Size::Small
                            && state.host(Machine::B).bound == Size::Large
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
                            && state.host(Machine::A).bound == Size::Large
                            && state.host(Machine::B).bound == Size::Large
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
                            && state.host(Machine::A).bound == Size::Large
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
                            && state.host(Machine::A).copy.is_writer()
                            && state
                                .first()
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
                            && state.first().row == Row::Done
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
                            && state.first().row == Row::Done
                            && state.host(Machine::B).copy.is_writer()
                            && state.host(Machine::B).bound == Size::Small
                    },
                ));
                reached.push(Property::sometimes(
                    "a Move after a shrink the Mirror refused promotes the larger copy over the smaller Store",
                    |_, state: &State| {
                        shrink(state)
                            && state.resize.refused
                            && state.first().row == Row::Done
                            && state.host(Machine::B).copy.is_writer()
                            && state.host(Machine::B).bound == Size::Large
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
                            && state.first().row == Row::Done
                            && state.host(Machine::B).copy.is_writer()
                            && state.host(Machine::B).bound == Size::Small
                    },
                ));
                reached
            }
            Scenario::TwoMoves => {
                let mut reached = move_ends();
                reached.extend(second_move_ends());
                reached
            }
            Scenario::Restore => {
                let mut reached = move_ends();
                reached.extend(restore_ends(self.skew));
                reached
            }
        }
    }
}

fn second(state: &State) -> Option<&MoveRun> {
    state.moves.get(1)
}

fn second_move_ends() -> Vec<Property<VolumeRuns>> {
    vec![
        Property::sometimes("a mirror rm forgets the old copy", |_, state: &State| {
            state.drop.is_some_and(|drop| drop.row == Row::Done)
        }),
        Property::sometimes(
            "a second Move finishes on C with its Service running",
            |_, state: &State| {
                second(state).is_some_and(|run| run.row == Row::Done)
                    && state.host(Machine::C).copy.is_writer()
                    && state.host(Machine::C).holder == Holder::Running
            },
        ),
        Property::sometimes(
            "the second Move's Release goes back to its Writer",
            |_, state: &State| {
                second(state).is_some_and(|run| {
                    run.release
                        .as_ref()
                        .is_some_and(|release| !release.forward && release.row == Row::Done)
                })
            },
        ),
        Property::sometimes(
            "the second Move's Release goes forward to C",
            |_, state: &State| {
                second(state).is_some_and(|run| {
                    run.release
                        .as_ref()
                        .is_some_and(|release| release.forward && release.row == Row::Done)
                })
            },
        ),
        Property::sometimes(
            "a second Move starts from A after the first one's Release came back",
            |_, state: &State| second(state).is_some_and(|run| run.plan.from == Machine::A),
        ),
        Property::sometimes(
            "a Deploy prepares, creates and starts on the Writer",
            |_, state: &State| {
                state.deploy.is_some_and(|deploy| {
                    deploy.row == Row::Done && state.host(deploy.on).holder == Holder::Running
                })
            },
        ),
        Property::sometimes(
            "a dead Deploy's Prepare lands after its Machine lost the Writer and is refused",
            |_, state: &State| {
                state.requests.iter().any(|request| {
                    request.want == Want::Prepare
                        && request.progress == Progress::Replied(Reply::Refused(Refusal::NoWriter))
                }) || state.deploy.is_some_and(|deploy| {
                    deploy.row == Row::Failed && !state.host(deploy.on).copy.is_writer()
                })
            },
        ),
        Property::sometimes(
            "a Machine away mid-Move ends it, and it comes back",
            |_, state: &State| {
                state.aways == 0
                    && Machine::ALL
                        .iter()
                        .all(|machine| state.host(*machine).here())
                    && state
                        .requests
                        .iter()
                        .any(|request| request.progress == Progress::Sent)
                    && state.first().release.is_some()
            },
        ),
    ]
}

/// Some Machine still holds the departure's point.
fn holds_departed(state: &State) -> bool {
    Machine::ALL
        .iter()
        .any(|machine| state.host(*machine).copy.newest() == Some(Snap::Departure))
}

fn restore_ends(skew: Skew) -> Vec<Property<VolumeRuns>> {
    let mut ends = vec![
        Property::sometimes(
            "a Restore promotes the copy left once Cloud forgot the Writer",
            |_, state: &State| {
                state.restored
                    && state
                        .restore
                        .is_some_and(|restore| restore.row == Row::Done)
            },
        ),
        Property::sometimes(
            "the forgotten old copy comes back writable beside the restored Writer",
            |_, state: &State| {
                state.restored
                    && Machine::ALL.iter().any(|machine| {
                        let host = state.host(*machine);
                        !host.member && host.here() && host.copy.is_writer()
                    })
            },
        ),
        Property::sometimes(
            "the forgotten old copy departs and a Deploy runs the Service on the restored Writer",
            |_, state: &State| {
                state.restored
                    && Machine::ALL
                        .iter()
                        .any(|machine| state.host(*machine).presence == Presence::Departed)
                    && state.deploy.is_some_and(|deploy| {
                        deploy.row == Row::Done && state.host(deploy.on).holder == Holder::Running
                    })
            },
        ),
    ];
    ends.push(Property::sometimes(
        "the orphan sweep keeps the old copy a departed Machine brought back",
        |_, state: &State| {
            state.stage == Stage::Sweep
                && state
                    .mirror
                    .is_none_or(|mirror| mirror.driver.step == MirrorStep::Over)
                && holds_departed(state)
        },
    ));
    if skew == Skew::OldDaemon {
        ends.push(Property::sometimes(
            "Cloud plans no mirror, no full sync, no Move and no sweep onto the old copy a departed Machine from before keep-written brought back",
            |_, state: &State| {
                state.stage == Stage::Sweep && state.mirror.is_none() && holds_departed(state)
            },
        ));
        return ends;
    }
    ends.push(Property::sometimes(
        "a mirror onto the old copy a departed Machine brought back fails and keeps it",
        |_, state: &State| {
            state.mirror.is_some_and(|mirror| {
                mirror.onto == Onto::Mirror
                    && mirror.row == Row::Failed
                    && mirror.driver.step == MirrorStep::Over
                    && state.host(mirror.to).copy.newest() == Some(Snap::Departure)
                    && state.host(mirror.writer).copy.newest() == Some(Snap::Warm2)
            })
        },
    ));
    ends.push(Property::sometimes(
        "a full sync onto that old copy is refused and keeps it",
        |_, state: &State| {
            state.mirror.is_some_and(|mirror| {
                mirror.onto == Onto::Sync
                    && mirror.row == Row::Failed
                    && mirror.driver.step == MirrorStep::Over
                    && state.host(mirror.to).copy.newest() == Some(Snap::Departure)
            })
        },
    ));
    ends.push(Property::sometimes(
        "a Move onto that old copy fails and keeps it",
        |_, state: &State| {
            state.mirror.is_some_and(|mirror| {
                mirror.onto == Onto::Move
                    && mirror.row == Row::Failed
                    && mirror.driver.step == MirrorStep::Over
                    && state.host(mirror.to).copy.newest() == Some(Snap::Departure)
            })
        },
    ));
    ends
}

const NEVER_TWO_WRITERS: &str = "never two Writers";
const ONE_SERVICE: &str = "the Service runs on one Machine at most";
const SERVICE_ON_WRITER: &str = "a running Service sits on its Machine's Writer";
const NOTHING_STUCK: &str = "every run ends";
const DONE_HOLDS: &str = "a run that finished done still holds at rest";
const RELEASE_WINS: &str = "a Release finishes whatever the dead Move still sends";
const ENFORCED_STARTS_NOTHING: &str =
    "a Machine creates no Container an Operator's Writer start names";
const STORE_FITS: &str = "the Store's size fits every copy";
const KEEPS_WRITTEN: &str = "only a person's mirror rm destroys a point only its Machine holds";
const CONFIRMED_ONLY: &str =
    "a mirror rm destroys only the copy its person confirmed, as they saw it";
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
    matches!(state.host(Machine::A).copy, Held::Slot { .. })
        && state
            .requests
            .iter()
            .any(|request| request.lane == Lane::Resize && request.progress == Progress::Sent)
}

/// A Deploy may follow the first Move's commitment once its forward Release failed on a
/// refused start, and then reads the promote it sent once answered.
fn repairs(state: &State) -> bool {
    let Some(first) = state.moves.first() else {
        return false;
    };
    let failed = first.release.as_ref().is_some_and(|release| {
        release.forward && release.row == Row::Failed && release.driver.step == ReleaseStep::Over
    });
    failed
        && state.refused_start
        && !state.repaired
        && state
            .repair
            .is_none_or(|sent| state.replied(Some(sent)).is_some())
}

fn at_rest(model: &VolumeRuns, state: &State) -> bool {
    let mut actions = Vec::new();
    model.actions(state, &mut actions);
    actions.is_empty()
}

/// Nothing but closing the start gate is left, so closing it would change nothing.
fn at_rest_but_enforce(model: &VolumeRuns, state: &State) -> bool {
    at_rest(
        &VolumeRuns {
            enforce: false,
            ..*model
        },
        state,
    )
}

/// The newest run that reported done left its Writer serving on the Machine it named.
fn done_holds(state: &State) -> bool {
    let serving = |machine: Machine, started: bool| {
        let host = state.host(machine);
        host.copy.is_writer()
            && state.writers(false) == 1
            && (!started || host.holder == Holder::Running)
    };
    let first = state.first();
    match &first.release {
        Some(release) if release.row == Row::Done => match (release.forward, release.resume) {
            (true, _) => serving(Machine::B, false),
            (false, Some(resume)) => serving(resume.on, resume.start.is_some()),
            (false, None) => false,
        },
        Some(_) => true,
        None => first.row != Row::Done || serving(Machine::B, true),
    }
}

mod tests {
    use super::*;
    use crate::harness::{check, discoveries};

    /// The start gate closing at any moment of the Move: no Operator start creates a
    /// Container, and the
    /// refused Release is the only run that may stop short.
    #[test]
    fn volume_runs_hold_when_machines_refuse_created_starts() {
        let checker = check(VolumeRuns {
            enforce: true,
            ..VolumeRuns::default()
        });
        println!("enforced: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    #[test]
    fn a_machine_refuses_every_operator_start_that_creates_a_container() {
        let checker = discoveries(
            VolumeRuns {
                direction: Some(Direction::Grow),
                enforce: true,
                broken: Broken {
                    start_gate: true,
                    ..Broken::default()
                },
                ..VolumeRuns::default()
            },
            &[ENFORCED_STARTS_NOTHING],
        );
        let path = checker.assert_any_discovery(ENFORCED_STARTS_NOTHING);
        println!("{ENFORCED_STARTS_NOTHING} breaks with the gate out:\n{path}");
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
        let checker = discoveries(
            VolumeRuns {
                scenario,
                direction,
                broken,
                ..VolumeRuns::default()
            },
            properties,
        );
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

    /// A slice on three Machines: a restart, a Deploy, a Machine away and one row Cloud
    /// ends early. A request a restart lost is sent again; no other is resent.
    fn three(scenario: Scenario) -> VolumeRuns {
        VolumeRuns {
            scenario,
            resends: 0,
            ..VolumeRuns::default()
        }
    }

    /// Three Machines, every run one at a time under the row lock, Cloud ending a row at
    /// any moment, every late step, a restart anywhere, a Machine away, and one Deploy.
    #[test]
    fn three_machine_runs_hold() {
        for model in [
            three(Scenario::TwoMoves),
            VolumeRuns {
                ends: 2,
                aways: 0,
                ..three(Scenario::TwoMoves)
            },
            three(Scenario::Restore),
        ] {
            let checker = check(model);
            println!(
                "{:?} ends {} aways {}: {} states",
                model.scenario,
                model.ends,
                model.aways,
                checker.unique_state_count()
            );
            checker.assert_properties();
        }
    }

    fn breaks_three(broken: Broken) {
        let checker = discoveries(
            VolumeRuns {
                broken,
                ..three(Scenario::TwoMoves)
            },
            &[NEVER_TWO_WRITERS],
        );
        let path = checker.assert_any_discovery(NEVER_TWO_WRITERS);
        println!("{NEVER_TWO_WRITERS} breaks with {broken:?}:\n{path}");
    }

    #[test]
    fn prepare_admits_only_a_plain_mount_on_the_writer() {
        breaks_three(Broken {
            plain_mount: true,
            ..Broken::default()
        });
    }

    /// A dead Deploy's Prepare that lands after a mirror rm forgot its Machine's copy,
    /// reading a view that lags, makes a fresh root beside the Writer unless it reads
    /// the forgotten record.
    #[test]
    fn prepare_makes_no_fresh_root_of_data_its_machine_forgot() {
        breaks_three(Broken {
            prepare_forgotten: true,
            ..Broken::default()
        });
    }

    /// A Forget that lands between Prepare's read of the forgotten record and its read of
    /// the copy leaves Prepare with neither, unless Prepare holds the switch lock the
    /// Forget takes.
    #[test]
    fn prepare_reads_the_record_and_the_copy_under_one_switch_lock() {
        breaks_three(Broken {
            prepare_switch: true,
            ..Broken::default()
        });
    }

    /// A dead Deploy's Create that lands after a Move took the Volume off its Machine and
    /// a mirror rm forgot the copy it left there makes a fresh root beside the Writer the
    /// Move made, unless the Machine refuses a root of the origin it forgot.
    #[test]
    fn a_machine_makes_no_fresh_root_of_data_it_forgot() {
        breaks_three(Broken {
            forgotten: true,
            ..Broken::default()
        });
    }

    /// A Release that ends while its promote of A's slot is on the wire leaves A a slot
    /// that may still become the Writer. A Move from that slot onto C commits C, and the
    /// promote landing late makes A a second Writer, unless `planMove` refuses a Move
    /// while no Machine holds a root.
    #[test]
    fn no_move_starts_from_slots_alone() {
        let checker = discoveries(
            VolumeRuns {
                ends: 2,
                aways: 0,
                broken: Broken {
                    move_from_slots: true,
                    ..Broken::default()
                },
                ..three(Scenario::TwoMoves)
            },
            &[NEVER_TWO_WRITERS],
        );
        let path = checker.assert_any_discovery(NEVER_TWO_WRITERS);
        println!("{NEVER_TWO_WRITERS} breaks with a Move from slots:\n{path}");
    }

    /// The path that breaks "a mirror rm destroys only the copy its person confirmed" with
    /// `broken`, on three Machines whose second Move first removes the Mirror.
    fn breaks_confirmed(broken: Broken, story: &str) {
        let checker = discoveries(
            VolumeRuns {
                broken,
                ..three(Scenario::TwoMoves)
            },
            &[CONFIRMED_ONLY],
        );
        let path = checker.assert_any_discovery(CONFIRMED_ONLY);
        println!("{CONFIRMED_ONLY} breaks {story}:\n{path}");
    }

    /// A person confirms the slot `mirror rm` shows them; the run may wait behind another,
    /// and the slot may be replaced or written there before it runs. Naming the slot the
    /// run finds then, as 2ff6f2c021 did, deletes one the person never saw.
    #[test]
    fn a_mirror_rm_deletes_only_the_copy_its_person_confirmed() {
        breaks_confirmed(
            Broken {
                discard_unbound: true,
                ..Broken::default()
            },
            "with the run's own look in place of the confirm",
        );
    }

    /// Cloud checks the confirmed slot before it sends the Forget, and the slot can be
    /// written there after that look. Only the Machine's check of `written_here` keeps it.
    #[test]
    fn a_machine_keeps_a_confirmed_slot_written_there_since() {
        breaks_confirmed(
            Broken {
                expect_ignores_written: true,
                ..Broken::default()
            },
            "with a Forget whose expect ignores written_here",
        );
    }

    /// The daemon plans a confirmed slot's Forget under the switch lock, but a receive it
    /// started earlier runs outside that lock and can land before the plugin destroys the
    /// slot. Only the plugin's own plan of the Forget, on the slot it finds, keeps it.
    #[test]
    fn the_plugin_destroys_a_slot_only_as_the_daemon_planned_its_forget() {
        breaks_confirmed(
            Broken {
                destroy_unchecked: true,
                ..Broken::default()
            },
            "with a plugin that destroys the slot by name",
        );
    }

    /// The path that breaks "only a person's mirror rm destroys a point only its Machine
    /// holds" on `Restore` with `skew` and `broken`.
    fn breaks_written(skew: Skew, broken: Broken, story: &str) {
        let checker = discoveries(
            VolumeRuns {
                skew,
                broken,
                ..three(Scenario::Restore)
            },
            &[KEEPS_WRITTEN],
        );
        let path = checker.assert_any_discovery(KEEPS_WRITTEN);
        println!("{KEEPS_WRITTEN} breaks {story}:\n{path}");
    }

    /// A Machine that departed and joined again holds a departure's point no Writer
    /// holds, so a mirror onto it finds no base for an incremental. A full send in its
    /// place destroys that point, unless `pull_stream` keeps a slot `written_here`.
    #[test]
    fn a_pull_keeps_a_departed_copy_its_writer_cannot_send_onto() {
        breaks_written(
            Skew::None,
            Broken {
                pull_replaces_written: true,
                ..Broken::default()
            },
            "with a full send over a departed copy",
        );
    }

    /// A full sync forgets the Mirror's copy before it pulls afresh, and Cloud plans it
    /// onto any one slot. Only the Machine's `written_here` keeps a departed copy from
    /// that Forget: without it, the sync destroys the departure's point.
    #[test]
    fn a_full_sync_keeps_a_departed_copy() {
        breaks_written(
            Skew::None,
            Broken {
                forget_ignores_written: true,
                ..Broken::default()
            },
            "with a full sync over a departed copy",
        );
    }

    /// An older Cloud and an older daemon each beside the code as it is: the departed copy
    /// survives, by the Machine's refusal of a bare Forget and by Cloud's refusal to plan
    /// onto a Machine without `ployz.volume.keep-written.v1`.
    #[test]
    fn a_departed_copy_survives_a_cloud_or_a_daemon_from_before_keep_written() {
        for skew in [Skew::OldCloud, Skew::OldDaemon] {
            let checker = check(VolumeRuns {
                skew,
                ..three(Scenario::Restore)
            });
            println!("{skew:?}: {} states", checker.unique_state_count());
            checker.assert_properties();
        }
    }

    /// An older Cloud sends its full sync the same bare Forget as its `mirror rm`. Under
    /// 2f4f30f267's opt-in, where a Machine kept a slot `written_here` only for a Forget
    /// that said `keep_written`, that bare Forget destroys the departed copy.
    #[test]
    fn a_bare_forget_from_an_older_cloud_keeps_a_departed_copy() {
        breaks_written(
            Skew::OldCloud,
            Broken {
                forget_opt_in: true,
                ..Broken::default()
            },
            "with an older Cloud's bare Forget under the opt-in",
        );
    }

    /// A Machine from before keep-written reports no `written_here` and ignores what a
    /// Forget says, so a full sync Cloud plans there destroys the departed copy.
    #[test]
    fn cloud_plans_no_full_sync_onto_a_machine_from_before_keep_written() {
        breaks_written(
            Skew::OldDaemon,
            Broken {
                sync_skips_capability: true,
                ..Broken::default()
            },
            "with a full sync onto an older daemon",
        );
    }

    /// That Machine's pull also replaces a copy its Writer can't send an incremental onto,
    /// so a mirror Cloud plans onto the departed copy there destroys it.
    #[test]
    fn cloud_plans_no_mirror_onto_a_departed_copy_on_a_machine_from_before_keep_written() {
        breaks_written(
            Skew::OldDaemon,
            Broken {
                mirror_skips_capability: true,
                ..Broken::default()
            },
            "with a mirror onto an older daemon's departed copy",
        );
    }

    /// A Move's warm round pulls onto its target's slot as a mirror's does, so a Move
    /// Cloud plans onto an older daemon's departed copy destroys it, unless the one check
    /// every pull passes covers a Move too.
    #[test]
    fn cloud_plans_no_move_onto_a_departed_copy_on_a_machine_from_before_keep_written() {
        breaks_written(
            Skew::OldDaemon,
            Broken {
                move_skips_capability: true,
                ..Broken::default()
            },
            "with a Move onto an older daemon's departed copy",
        );
    }

    /// A Machine cut away before its final reached the mirror, and a Restore promoted the
    /// mirror's earlier copy. Back after it departed, the Machine's slot holds a cut's
    /// final no Writer has, under a name that isn't a departure's. A Machine from before
    /// keep-written replaces it with a full send, so Cloud plans a pull there only from the
    /// Writer's newest snapshot, whatever the slot's snapshot is called.
    #[test]
    fn cloud_plans_no_pull_onto_a_cut_a_restore_skipped_on_a_machine_from_before_keep_written() {
        breaks_written(
            Skew::OldDaemon,
            Broken {
                dep_prefix_only: true,
                ..Broken::default()
            },
            "with a mirror onto an older daemon's cut a Restore skipped",
        );
    }

    /// No one chose the orphan sweep's deletes, so a sweep that says `discard` destroys
    /// the departed copy a Machine kept from every other Forget.
    #[test]
    fn the_orphan_sweep_keeps_a_departed_copy() {
        breaks_written(
            Skew::None,
            Broken {
                sweep_discards: true,
                ..Broken::default()
            },
            "with an orphan sweep that says discard",
        );
    }

    /// A Machine from before keep-written deletes whatever slot a Forget names, so the
    /// sweep keeps every slot there.
    #[test]
    fn the_orphan_sweep_keeps_every_slot_on_a_machine_from_before_keep_written() {
        breaks_written(
            Skew::OldDaemon,
            Broken {
                sweep_skips_capability: true,
                ..Broken::default()
            },
            "with an orphan sweep onto an older daemon",
        );
    }

    /// The old Writer Cloud forgot while it was away comes back writable beside the copy
    /// a Restore promoted, until it departs: bet 6's accepted window, and the only copy
    /// `never two Writers` leaves out.
    #[test]
    fn a_restored_copy_and_the_returning_old_one_are_the_accepted_window() {
        let checker = discoveries(
            VolumeRuns {
                exempt: false,
                ..three(Scenario::Restore)
            },
            &[NEVER_TWO_WRITERS],
        );
        let path = checker.assert_any_discovery(NEVER_TWO_WRITERS);
        println!("{NEVER_TWO_WRITERS} without the window:\n{path}");
    }
}
