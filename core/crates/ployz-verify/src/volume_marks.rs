//! A Seed request against the ZFS `ployz:*` marks on one Machine, A. Cloud's `seed` run
//! (`seedVolume`, volume-run.inngest.ts) resends `Volume.Seed` for a new Volume `into`,
//! holding the restore point `G` of `name`, until A answers a root. A copies `G` into a
//! slot of `into` and lands it as a root (`VolumeStorage::seed_volume`, seed.rs). Docker's
//! Create adopts it, a later Move cuts it into an old copy (`copy_cut`, copy.rs) once a
//! writer on B takes over, and a mirror pull re-roles that copy to a Mirror
//! (`copy_pull` and `mark_pulled`, copy.rs). A Seed request is a message: it can wait in
//! transit, before the switch lock, past its own run's row.
//!
//! The finding, before the fix: such a request from an ended seed run lands on the Mirror
//! the later run pulled. `mark_pulled` cleared `ployz:seed` and `ployz:cut` and set
//! `ployz:pulled`, so `written_here` (departure.rs) reads false, and the Seed runs
//! `zfs destroy -r` on the slot and starts copying the old restore point into it.
//! `Broken::seed_skips_floor` restores that Seed.
//!
//! The properties: a Seed from an ended seed run never destroys a copy of `into` a later
//! run made; a Seed never destroys a copy only this Machine may hold, nor a slot a receive
//! is still writing; a seed run ends done only on a root holding its restore point; at rest
//! the run's row has ended; and Inspect names only a mark `into`'s root carries. They are
//! phrased over stale Seeds where the finding is, because a live Seed replacing a pulled
//! Mirror is designed (`seed_tests.rs`
//! `a_seed_replaces_a_slot_a_mirror_pull_marked_or_that_holds_no_point`).
//!
//! The fix: Seed carries its run's gen, and `seed_volume` (machine_api/local.rs:557-573)
//! checks it against `into`'s floor under the switch lock, before the plugin's
//! `seed_volume` runs, through `check_gen` (machine_api/local_volume_copy.rs:273-283) and
//! `FloorTurn::check` (docker/spec_store.rs:552-565). The check refuses a gen below the
//! floor and leaves the floor where it is. Gens come from one identity column, so a later
//! run's `SetVolumeCopy` on A raises `into`'s floor above the seed run's gen.
//!
//! Every plugin handler holds `admit_mutation` (storage.rs) from its first read to its
//! answer, so each handler is one step. A receive task runs outside it, so a receive
//! completing and `mark_pulled` are steps of their own.
//!
//! One failure inside a handler is modelled: `finish_landing`'s `readonly=off` failing
//! after `land`'s rename, which leaves a read-only root with `ployz:seed` and
//! `ployz:promote` that the next Seed finishes. `RootCopy::parse` (inspect.rs) must read
//! that root by its marks, not as a cut.
//!
//! Out of scope: a seed copy that fails, `ployz:epoch` (no modelled copy carries it), the
//! departure, any other failure between the zfs calls of one handler, `ployz:pull` and the
//! role property on the slot parent, more than one Seed request stranded, and Busy answers.

use std::fmt::Write as _;

use stateright::{Model, Property};

/// A snapshot point.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Point {
    /// The restore point of `name` the seed run asks for, on `name`'s writer root on A.
    G,
    /// The Move's cut target (`copy_cut`, copy.rs).
    T1,
    /// What the writer on B snapshotted after the Move committed.
    T2,
    /// The newest snapshot of the old copy an earlier Volume named `into` left on A.
    Old,
    /// What that earlier Volume's writer elsewhere snapshotted after it.
    OldNext,
}

/// Which run's data a copy of `into` holds. Gens come from one identity column
/// (dashboard tables.ts `gen`), so an earlier run's gen is below the seed run's, and
/// every later run's is above it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Made {
    /// A run on an earlier Volume named `into`, gen `EARLIER`.
    Earlier,
    /// The seed run, gen `SEED`.
    Seed,
    /// A run on `into` after the seed run, gen `LATER`: the Deploy's writes and the Move.
    Later,
    /// Another Volume that already holds the name `into`.
    Other,
}

const EARLIER: u8 = 1;
const SEED: u8 = 2;
const LATER: u8 = 3;

/// The marks `written_here` reads (departure.rs `marked_written_here`). `ployz:epoch` is
/// out of scope.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
struct Marks {
    /// `ployz:seed`, set by `land` (seed.rs) just before the rename.
    seed: bool,
    /// `ployz:promote`, set by `land`, inherited by Create (storage.rs `create_volume`).
    promote: bool,
    /// `ployz:cut`, set on the root by `copy_cut` (copy.rs) before its rename.
    cut: bool,
}

impl Marks {
    fn any(self) -> bool {
        self.seed || self.promote || self.cut
    }
}

/// One dataset holding a copy of `into`: the root, or a slot's `fs`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Copy {
    /// ZFS `readonly`.
    readonly: bool,
    /// The newest snapshot, `None` for none.
    newest: Option<Point>,
    /// `ployz:pulled`, set locally by `mark_pulled` (copy.rs).
    pulled: Option<Point>,
    marks: Marks,
    made: Made,
    /// This Machine may hold the only copy of its newest point: no writer elsewhere is
    /// known to hold it. The model's ground truth, not a mark the code reads.
    alone: bool,
}

/// What A's pool holds for `into`: nothing, the root `<pool>/ployz/<into>`, or the slot
/// parent `<pool>/ployz-mirror/<into>` with or without its `fs` (storage.rs `root_dataset`,
/// `slot_parent`, `slot_fs`).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Into {
    Absent,
    Root(Copy),
    Slot { fs: Option<Copy> },
}

/// The receive task for `into` in `Receives` (transfer.rs). `status` reads it busy from
/// `spawn` until the task ends.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Task {
    Idle,
    /// The seed's `copy_snapshot` (seed.rs) of `name@G` into the slot's `fs`.
    Copying,
    /// A mirror pull's `pull_stream` then `mark_pulled` (copy.rs `copy_pull`).
    Pulling {
        target: Point,
        made: Made,
        received: bool,
    },
}

/// The seed run's row, as `seedVolume` ends it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Row {
    /// The run is polling; `awaiting` while its step `03-ask-{poll}` waits for an answer.
    Open { awaiting: bool },
    /// `endRun(.., "done", ..)` after a root answer, with whose data that root held.
    Done(Made),
    /// `failRun`: a refusal (`sendSwitch`), a copy failure, or `03-too-long`.
    Failed,
}

impl Row {
    fn open(self) -> bool {
        matches!(self, Self::Open { .. })
    }
}

/// The branch of `VolumeStorage::seed_volume` (seed.rs) one Seed request took.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum SeedPath {
    /// `check_gen` refuses the seed run's gen below `into`'s floor.
    Stale,
    /// A root of `into` with this guid in `ployz:seed`: answers the root.
    AnswersRoot,
    /// That root still holds `G`, so a landing stopped on it: `finish_landing` runs
    /// again, then it answers the root.
    Finishes,
    /// A root of `into` with another or no `ployz:seed`: "Volume into already exists".
    Exists,
    /// `receives.take_failure(&into)` answers `Busy`: answers the copy as it stands.
    Receiving,
    /// The slot's `fs` holds `G` as its newest: `land` renames it to the root.
    Lands,
    /// As `Lands`, but `finish_landing`'s `readonly=off` fails after the rename.
    Stops,
    /// `written_here(&fs)`: "Volume into is held in slot".
    Held,
    /// A slot exists: `zfs destroy -r <parent>`, then a new slot and a copy of `G`.
    Replaces,
    /// No slot: a new one and a copy of `G`.
    Starts,
}

/// What the Seed's `zfs destroy -r <parent>` (seed.rs) took.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Destroy {
    /// The seed run's row had ended when the request landed.
    stale: bool,
    /// The run whose data the destroyed `fs` held, `None` for no `fs`.
    made: Option<Made>,
    /// The destroyed `fs` may have held the only copy of its point.
    alone: bool,
    /// A receive task for `into` was still running.
    receiving: bool,
    /// The destroyed `fs` was a Mirror whose newest snapshot a pull marked.
    pulled: bool,
}

/// How A answered a Seed request.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Answer {
    Root(Made),
    Slot,
    Refused,
    /// An internal error: `sendSwitch` fails only the attempt, and the step retries.
    Failed,
}

/// Which copies of `into` A starts with.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Scenario {
    /// No copy of `into` on A; after the seed lands, the Deploy adopts it and a later Move
    /// takes it to B and back as a Mirror.
    Fresh,
    /// An old copy an earlier Volume named `into` left in its slot when its writer moved
    /// on, cut-marked, with that writer's newer snapshot waiting for a pull.
    OldCopy,
    /// Another Volume already holds the name `into` as a root on A.
    Taken,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// `seedVolume`'s step `03-ask-{poll}` sends `seed_volume` (volume-run.inngest.ts),
    /// once `sendSwitch`'s `requireOwner` (volume-run.server.ts) found the row its own.
    Send,
    /// The attempt fails short of an answer, a transport failure: `sendSwitch` fails only
    /// the attempt, the step retries, and the request it sent stays in transit.
    TimeOut,
    /// `PATIENCE_WAITS` ran out: `03-too-long` fails the run.
    GiveUp,
    /// A's `Volume.Seed` (machine_api/local.rs `seed_volume`) runs one request off the
    /// wire under the switch lock; `answered` when the step that sent it still waits. Under
    /// `Broken::floor_checked_only_before_lock` it runs one request `Admit` let through.
    Seed { answered: bool },
    /// A Seed request that takes `land` (seed.rs), where `finish_landing`'s `set
    /// readonly=off` fails after the rename. Offered beside each `Seed` that would land.
    SeedStops { answered: bool },
    /// Only under `Broken::floor_checked_only_before_lock`: the floor check takes one
    /// request off the wire before the switch lock, and either refuses it or lets it on
    /// toward the lock; `answered` when it is the request the step waits for.
    Admit { answered: bool },
    /// The seed's `copy_snapshot` receive completes and its task ends.
    CopyDone,
    /// The Deploy's Docker Create adopts the landed root (storage.rs `create_volume`).
    Adopt,
    /// The later Move's WriterToMirror on A (copy.rs `copy_cut`).
    Cut,
    /// The Move commits on B: B pulled `T1`, took the writer, and snapshotted `T2`.
    Commit,
    /// The later run's Mirror want on A (copy.rs `copy_pull`), target `T2`.
    Pull,
    /// The earlier run's Mirror want on A (copy.rs `copy_pull`), target `OldNext`.
    EarlierPull,
    /// A pull's receive exits 0 (copy.rs `pull_stream`).
    Received,
    /// `mark_pulled` (copy.rs), then the pull's task ends.
    MarkPulled,
}

/// The most Seed requests the run sends, and how many may outlive their attempt.
const MAX_SENDS: u8 = 3;
const MAX_TIMEOUTS: u8 = 1;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    row: Row,
    sends: u8,
    timeouts: u8,
    /// Seed requests sent and not yet run by A, all alike: `{name, into, guid: G, gen: SEED}`.
    on_wire: u8,
    /// Requests `Admit` let through that have not yet taken the switch lock.
    queued: u8,
    /// The request the step waits for is among `queued`, not `on_wire`.
    awaiting_queued: bool,
    /// A's `volume_floors` row for `into` (docker/spec_store.rs `FloorTurn::admit`).
    floor: u8,
    into: Into,
    task: Task,
    /// The Deploy adopted the landed root.
    adopted: bool,
    /// The Move committed on B.
    committed: bool,
    /// The later run sent its Mirror want to A.
    later_pulled: bool,
    /// The earlier run sent its Mirror want to A.
    earlier_pulled: bool,
    /// The branch the last Seed request took.
    seed: Option<SeedPath>,
    /// What the last Seed `destroy -r` took.
    destroyed: Option<Destroy>,
    last: Option<Action>,
}

impl State {
    fn at_rest(&self, model: &VolumeMarks) -> bool {
        let mut actions = Vec::new();
        model.actions(self, &mut actions);
        actions.is_empty()
    }
}

/// A guard the code has, taken out, to show the check catches what it prevents.
#[derive(Clone, Copy, Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag removes one independent guard"
)]
struct Broken {
    /// The root branch of `seed_volume` (seed.rs) answers any root of `into`, skipping
    /// its `ployz:seed` check against the guid.
    root_ignores_seed: bool,
    /// `seed_volume` skips the `Busy` arm of `receives.take_failure(&into)` (seed.rs).
    seed_ignores_receive: bool,
    /// `seed_volume` skips `written_here(&fs)` before its `destroy -r` (seed.rs).
    seed_ignores_written_here: bool,
    /// `mark_pulled` (copy.rs) sets `ployz:pulled` without inheriting `ployz:cut`,
    /// `epoch`, `promote` and `seed` first.
    pull_keeps_marks: bool,
    /// Volume.Seed checks no floor: the code before the fix, a Cloud that sends no gen,
    /// or a Machine that predates the field and drops it.
    seed_skips_floor: bool,
    /// Volume.Seed checks the floor before it takes the switch lock, not under it.
    floor_checked_only_before_lock: bool,
    /// The Seed's floor check raises the floor, as `FloorTurn::admit` does.
    seed_raises_floor: bool,
    /// `RootCopy::parse` (inspect.rs) reads every read-only root as a cut, whatever its
    /// marks.
    parse_reads_readonly_as_cut: bool,
}

/// How `RootCopy::parse` (inspect.rs) reads a root, by its `readonly` and marks. The
/// model drops its `seed`, which no property reads.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Parsed {
    Writable,
    Cut,
    Promote,
    Unmarked,
}

/// What a coverage check takes out of the model, to show the `sometimes` naming it is
/// reached no other way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Omit {
    /// These actions are never taken.
    Actions(&'static [Action]),
    /// No Seed request takes this branch.
    Path(SeedPath),
}

/// One `sometimes`: a branch the code handles, read from the state that took it, the
/// scenarios that reach it, and what to take out of the model to show nothing else does.
struct Coverage {
    name: &'static str,
    scenarios: &'static [Scenario],
    omit: Omit,
    reached: fn(&VolumeMarks, &State) -> bool,
}

/// The last step was a Seed request that took `path`.
fn took(state: &State, path: SeedPath) -> bool {
    matches!(
        state.last,
        Some(Action::Seed { .. } | Action::SeedStops { .. })
    ) && state.seed == Some(path)
}

const ALL: &[Scenario] = &[Scenario::Fresh, Scenario::OldCopy, Scenario::Taken];
const SEEDS: &[Scenario] = &[Scenario::Fresh, Scenario::OldCopy];
const FRESH: &[Scenario] = &[Scenario::Fresh];
const OLD_COPY: &[Scenario] = &[Scenario::OldCopy];
const TAKEN: &[Scenario] = &[Scenario::Taken];

/// Each branch the bounds reach, once. `every_sometimes_needs_what_it_names` takes each
/// row's `omit` out and expects its `name` unreached.
const COVERAGE: &[Coverage] = &[
    Coverage {
        name: "a Seed starts its copy in a new slot",
        scenarios: FRESH,
        omit: Omit::Path(SeedPath::Starts),
        reached: |_, state| took(state, SeedPath::Starts),
    },
    Coverage {
        name: "a resent Seed answers the copy still receiving",
        scenarios: SEEDS,
        omit: Omit::Path(SeedPath::Receiving),
        reached: |_, state| took(state, SeedPath::Receiving),
    },
    Coverage {
        name: "a Seed lands its complete copy as a root",
        scenarios: SEEDS,
        omit: Omit::Path(SeedPath::Lands),
        reached: |_, state| took(state, SeedPath::Lands),
    },
    Coverage {
        name: "Inspect reads a landing stopped before readonly=off as promote",
        scenarios: SEEDS,
        omit: Omit::Path(SeedPath::Stops),
        reached: |model, state| {
            took(state, SeedPath::Stops)
                && matches!(state.into, Into::Root(root) if model.parse(root) == Parsed::Promote)
        },
    },
    Coverage {
        name: "a resent Seed finishes a landing that stopped",
        scenarios: SEEDS,
        omit: Omit::Path(SeedPath::Finishes),
        reached: |_, state| took(state, SeedPath::Finishes),
    },
    Coverage {
        name: "a Seed answers the root it landed",
        scenarios: FRESH,
        omit: Omit::Path(SeedPath::AnswersRoot),
        reached: |_, state| took(state, SeedPath::AnswersRoot),
    },
    Coverage {
        name: "a Seed refuses a Volume into that already exists",
        scenarios: TAKEN,
        omit: Omit::Path(SeedPath::Exists),
        reached: |_, state| took(state, SeedPath::Exists),
    },
    Coverage {
        name: "a Seed refuses a slot written here",
        scenarios: OLD_COPY,
        omit: Omit::Path(SeedPath::Held),
        reached: |_, state| took(state, SeedPath::Held),
    },
    Coverage {
        name: LIVE_REPLACES_PULLED,
        scenarios: OLD_COPY,
        omit: Omit::Path(SeedPath::Replaces),
        reached: |_, state| {
            took(state, SeedPath::Replaces) && state.destroyed.is_some_and(|d| !d.stale && d.pulled)
        },
    },
    Coverage {
        name: "the floor refuses a Seed from an ended seed run",
        scenarios: FRESH,
        omit: Omit::Path(SeedPath::Stale),
        reached: |_, state| {
            took(state, SeedPath::Stale)
                && state.last == Some(Action::Seed { answered: false })
                && !state.row.open()
        },
    },
    Coverage {
        name: "the seed run ends done",
        scenarios: SEEDS,
        omit: Omit::Actions(&[Action::Seed { answered: true }]),
        reached: |_, state| {
            state.last == Some(Action::Seed { answered: true }) && matches!(state.row, Row::Done(_))
        },
    },
    Coverage {
        name: "a Seed request outlives its run's row",
        scenarios: ALL,
        omit: Omit::Actions(&[Action::TimeOut]),
        reached: |_, state| !state.row.open() && state.on_wire > 0,
    },
    Coverage {
        name: "the run gives up after its last poll",
        scenarios: FRESH,
        omit: Omit::Actions(&[Action::GiveUp]),
        reached: |_, state| state.last == Some(Action::GiveUp),
    },
    Coverage {
        name: "Create adopts the landed root",
        scenarios: FRESH,
        omit: Omit::Actions(&[Action::Adopt]),
        reached: |_, state| state.last == Some(Action::Adopt),
    },
    Coverage {
        name: "the later Move cuts the adopted root into an old copy",
        scenarios: FRESH,
        omit: Omit::Actions(&[Action::Cut]),
        reached: |_, state| state.last == Some(Action::Cut),
    },
    Coverage {
        name: "mark_pulled marks the slot a pull received",
        scenarios: SEEDS,
        omit: Omit::Actions(&[Action::MarkPulled]),
        reached: |_, state| state.last == Some(Action::MarkPulled),
    },
    Coverage {
        name: EARLIER_PULL_AFTER_SEED,
        scenarios: OLD_COPY,
        omit: Omit::Path(SeedPath::Held),
        reached: |_, state| {
            state.last == Some(Action::EarlierPull)
                && state.task != Task::Idle
                && state.seed.is_some()
        },
    },
];

#[derive(Clone, Copy, Debug)]
struct VolumeMarks {
    scenario: Scenario,
    broken: Broken,
    /// A coverage check's cut, `None` to run the whole model.
    omit: Option<Omit>,
}

impl VolumeMarks {
    /// `written_here` (departure.rs): a departure snapshot or a seed, promote, cut or
    /// epoch mark, or a newest snapshot no pull marked.
    fn written_here(fs: Copy) -> bool {
        fs.marks.any() || fs.newest.is_some_and(|newest| fs.pulled != Some(newest))
    }

    /// `RootCopy::parse` (inspect.rs): the read-only split, then `ployz:cut`, then
    /// `ployz:promote`.
    fn parse(&self, root: Copy) -> Parsed {
        match (root.readonly, root.marks.cut, root.marks.promote) {
            (false, _, _) => Parsed::Writable,
            (true, _, _) if self.broken.parse_reads_readonly_as_cut => Parsed::Cut,
            (true, true, _) => Parsed::Cut,
            (true, false, true) => Parsed::Promote,
            (true, false, false) => Parsed::Unmarked,
        }
    }

    /// Volume.Seed for one request: the floor check under the switch lock
    /// (machine_api/local.rs `seed_volume`), then `VolumeStorage::seed_volume` (seed.rs)
    /// under `admit_mutation`. With `stops`, a landing stops at `finish_landing`'s
    /// `readonly=off`.
    fn seed(&self, state: &mut State, stops: bool) -> Answer {
        let broken = self.broken;
        let checks = !broken.seed_skips_floor && !broken.floor_checked_only_before_lock;
        if checks && !self.check(state, SEED) {
            state.seed = Some(SeedPath::Stale);
            return Answer::Refused;
        }
        let (path, answer) = match state.into {
            // seed.rs: `Self::dataset(..)` finds the root; `ployz:seed` against the guid.
            // A root still holding `G` stopped landing, so `finish_landing` runs again:
            // `readonly=off`, then `destroy root@G`. A landed root no longer holds `G`.
            Into::Root(root) if root.marks.seed && root.newest == Some(Point::G) => {
                state.into = Into::Root(Copy {
                    readonly: false,
                    newest: None,
                    ..root
                });
                (SeedPath::Finishes, Answer::Root(root.made))
            }
            Into::Root(root) if root.marks.seed || broken.root_ignores_seed => {
                (SeedPath::AnswersRoot, Answer::Root(root.made))
            }
            Into::Root(_) => (SeedPath::Exists, Answer::Refused),
            // seed.rs: the `Busy` arm of `self.receives.take_failure(&into)`.
            _ if state.task != Task::Idle && !broken.seed_ignores_receive => {
                (SeedPath::Receiving, Answer::Slot)
            }
            // seed.rs: the slot's `fs` newest is `G`, so `land` inherits `ployz:pulled`,
            // sets `ployz:seed` and `ployz:promote` and renames it to the root, and
            // `finish_landing` sets `readonly=off`, destroys the parent and `root@G`.
            // Stopped at `readonly=off`, the root stays read-only and still holds `G`, and
            // the request answers an internal error.
            Into::Slot { fs: Some(fs) } if fs.newest == Some(Point::G) => {
                let marks = Marks {
                    seed: true,
                    promote: true,
                    cut: false,
                };
                if stops {
                    state.into = Into::Root(Copy {
                        readonly: true,
                        pulled: None,
                        marks,
                        ..fs
                    });
                    (SeedPath::Stops, Answer::Failed)
                } else {
                    state.into = Into::Root(Copy {
                        readonly: false,
                        newest: None,
                        pulled: None,
                        marks,
                        ..fs
                    });
                    (SeedPath::Lands, Answer::Root(fs.made))
                }
            }
            // seed.rs: `slot_fs` exists and `written_here(&fs)`.
            Into::Slot { fs: Some(fs) }
                if Self::written_here(fs) && !broken.seed_ignores_written_here =>
            {
                (SeedPath::Held, Answer::Refused)
            }
            // seed.rs: `if slot { zfs destroy -r <parent> }`, `create_bounded_slot`, set
            // `ployz:pull`, `receives.spawn(copy_snapshot)`.
            Into::Slot { fs } => {
                state.destroyed = Some(Destroy {
                    stale: !state.row.open(),
                    made: fs.map(|fs| fs.made),
                    alone: fs.is_some_and(|fs| fs.alone),
                    receiving: state.task != Task::Idle,
                    pulled: fs.is_some_and(|fs| fs.newest.is_some() && fs.pulled == fs.newest),
                });
                state.into = Into::Slot { fs: None };
                state.task = Task::Copying;
                (SeedPath::Replaces, Answer::Slot)
            }
            // seed.rs: no slot: `ensure_commitment`, `ensure_mirror_root`, then as above.
            Into::Absent => {
                state.into = Into::Slot { fs: None };
                state.task = Task::Copying;
                (SeedPath::Starts, Answer::Slot)
            }
        };
        state.seed = Some(path);
        answer
    }

    /// `seedVolume` (volume-run.inngest.ts) reads the answer to the step that waits.
    fn answer(state: &mut State, answer: Answer) {
        state.row = match answer {
            Answer::Root(made) => Row::Done(made),
            Answer::Slot | Answer::Failed => Row::Open { awaiting: false },
            Answer::Refused => Row::Failed,
        };
    }

    /// `FloorTurn::admit` (docker/spec_store.rs:525-548) through `admit_gen`
    /// (machine_api/local_volume_copy.rs:258-268), which `set_copy` runs before the switch
    /// lock (:47) and `set_copy_admitted` again under it (:88).
    fn admit(state: &mut State, generation: u8) -> bool {
        if generation < state.floor {
            return false;
        }
        state.floor = state.floor.max(generation);
        true
    }

    /// `FloorTurn::check` (docker/spec_store.rs:552-565) through `check_gen`
    /// (machine_api/local_volume_copy.rs:273-283): refuses a gen below the floor, so an
    /// equal one passes, and leaves the floor where it is.
    fn check(&self, state: &mut State, generation: u8) -> bool {
        if self.broken.seed_raises_floor {
            return Self::admit(state, generation);
        }
        generation >= state.floor
    }

    fn step(&self, state: &mut State, action: Action) {
        match action {
            Action::Send => {
                state.sends += 1;
                state.on_wire += 1;
                state.row = Row::Open { awaiting: true };
                state.awaiting_queued = false;
            }
            Action::TimeOut => {
                state.timeouts += 1;
                state.row = Row::Open { awaiting: false };
            }
            Action::GiveUp => state.row = Row::Failed,
            Action::Seed { answered } | Action::SeedStops { answered } => {
                if self.broken.floor_checked_only_before_lock {
                    state.queued -= 1;
                } else {
                    state.on_wire -= 1;
                }
                let answer = self.seed(state, matches!(action, Action::SeedStops { .. }));
                if answered {
                    Self::answer(state, answer);
                }
            }
            Action::Admit { answered } => {
                state.on_wire -= 1;
                if self.check(state, SEED) {
                    state.queued += 1;
                    state.awaiting_queued |= answered;
                } else {
                    state.seed = Some(SeedPath::Stale);
                    if answered {
                        Self::answer(state, Answer::Refused);
                    }
                }
            }
            Action::CopyDone => {
                state.into = Into::Slot {
                    fs: Some(Copy {
                        readonly: true,
                        newest: Some(Point::G),
                        pulled: None,
                        marks: Marks::default(),
                        made: Made::Seed,
                        alone: false,
                    }),
                };
                state.task = Task::Idle;
            }
            Action::Adopt => {
                if let Into::Root(root) = &mut state.into {
                    root.marks.promote = false;
                }
                state.adopted = true;
            }
            Action::Cut => {
                Self::admit(state, LATER);
                if let Into::Root(root) = state.into {
                    state.into = Into::Slot {
                        fs: Some(Copy {
                            readonly: true,
                            newest: Some(Point::T1),
                            pulled: None,
                            marks: Marks {
                                cut: true,
                                ..root.marks
                            },
                            made: Made::Later,
                            alone: true,
                        }),
                    };
                }
            }
            Action::Commit => state.committed = true,
            Action::Pull => {
                state.later_pulled = true;
                if Self::admit(state, LATER) {
                    state.task = Task::Pulling {
                        target: Point::T2,
                        made: Made::Later,
                        received: false,
                    };
                }
            }
            Action::EarlierPull => {
                state.earlier_pulled = true;
                if Self::admit(state, EARLIER) {
                    state.task = Task::Pulling {
                        target: Point::OldNext,
                        made: Made::Earlier,
                        received: false,
                    };
                }
            }
            Action::Received => {
                if let (
                    Task::Pulling {
                        target,
                        made,
                        received,
                    },
                    Into::Slot { fs: Some(fs) },
                ) = (&mut state.task, &mut state.into)
                {
                    fs.newest = Some(*target);
                    fs.made = *made;
                    fs.alone = false;
                    *received = true;
                }
            }
            Action::MarkPulled => {
                if let Into::Slot { fs: Some(fs) } = &mut state.into {
                    if !self.broken.pull_keeps_marks {
                        fs.marks = Marks::default();
                    }
                    fs.pulled = fs.newest;
                }
                state.task = Task::Idle;
            }
        }
    }

    /// The slot's `fs` holds `point` as its newest, with no receive running.
    fn slot_holds(state: &State, point: Point) -> bool {
        state.task == Task::Idle
            && matches!(state.into, Into::Slot { fs: Some(fs) } if fs.newest == Some(point))
    }

    /// `action`, which led to `after`, is what the coverage check cut.
    fn omitted(&self, action: Action, after: &State) -> bool {
        match self.omit {
            None => false,
            Some(Omit::Actions(actions)) => actions.contains(&action),
            Some(Omit::Path(path)) => {
                matches!(action, Action::Seed { .. } | Action::SeedStops { .. })
                    && after.seed == Some(path)
            }
        }
    }
}

impl Model for VolumeMarks {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        let into = match self.scenario {
            Scenario::Fresh => Into::Absent,
            Scenario::OldCopy => Into::Slot {
                fs: Some(Copy {
                    readonly: true,
                    newest: Some(Point::Old),
                    pulled: None,
                    marks: Marks {
                        cut: true,
                        ..Marks::default()
                    },
                    made: Made::Earlier,
                    alone: true,
                }),
            },
            Scenario::Taken => Into::Root(Copy {
                readonly: false,
                newest: None,
                pulled: None,
                marks: Marks::default(),
                made: Made::Other,
                alone: true,
            }),
        };
        vec![State {
            row: Row::Open { awaiting: false },
            sends: 0,
            timeouts: 0,
            on_wire: 0,
            queued: 0,
            awaiting_queued: false,
            floor: 0,
            into,
            task: Task::Idle,
            adopted: false,
            committed: false,
            later_pulled: false,
            earlier_pulled: false,
            seed: None,
            destroyed: None,
            last: None,
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        match state.row {
            Row::Open { awaiting: false } if state.sends < MAX_SENDS => {
                actions.push(Action::Send);
            }
            Row::Open { awaiting: false } => actions.push(Action::GiveUp),
            Row::Open { awaiting: true } if state.timeouts < MAX_TIMEOUTS => {
                actions.push(Action::TimeOut);
            }
            Row::Open { .. } | Row::Done(_) | Row::Failed => {}
        }
        // The request the step waits for, in the pool it is in.
        let awaiting = state.row == Row::Open { awaiting: true };
        let on_wire_awaits = u8::from(awaiting && !state.awaiting_queued);
        if self.broken.floor_checked_only_before_lock {
            let queued_awaits = u8::from(awaiting && state.awaiting_queued);
            if on_wire_awaits == 1 {
                actions.push(Action::Admit { answered: true });
            }
            if state.on_wire > on_wire_awaits {
                actions.push(Action::Admit { answered: false });
            }
            if queued_awaits == 1 {
                actions.push(Action::Seed { answered: true });
            }
            if state.queued > queued_awaits {
                actions.push(Action::Seed { answered: false });
            }
        } else {
            if on_wire_awaits == 1 {
                actions.push(Action::Seed { answered: true });
            }
            if state.on_wire > on_wire_awaits {
                actions.push(Action::Seed { answered: false });
            }
        }
        let seeds: Vec<bool> = actions
            .iter()
            .filter_map(|action| {
                if let Action::Seed { answered } = action {
                    Some(*answered)
                } else {
                    None
                }
            })
            .collect();
        for answered in seeds {
            let mut landing = *state;
            self.step(&mut landing, Action::Seed { answered });
            if landing.seed == Some(SeedPath::Lands) {
                actions.push(Action::SeedStops { answered });
            }
        }
        match state.task {
            Task::Copying => actions.push(Action::CopyDone),
            Task::Pulling {
                received: false, ..
            } => actions.push(Action::Received),
            Task::Pulling { received: true, .. } => actions.push(Action::MarkPulled),
            Task::Idle => {}
        }
        match self.scenario {
            Scenario::Fresh => {
                let root = match state.into {
                    Into::Root(root) => Some(root),
                    Into::Absent | Into::Slot { .. } => None,
                };
                // storage.rs `create_volume`: `require_requested` refuses a read-only root.
                if !state.adopted && root.is_some_and(|root| root.marks.promote && !root.readonly) {
                    actions.push(Action::Adopt);
                }
                if state.adopted && root.is_some_and(|root| !root.marks.promote) {
                    actions.push(Action::Cut);
                }
                if !state.committed && Self::slot_holds(state, Point::T1) {
                    actions.push(Action::Commit);
                }
                if state.committed && !state.later_pulled && Self::slot_holds(state, Point::T1) {
                    actions.push(Action::Pull);
                }
            }
            Scenario::OldCopy => {
                if !state.earlier_pulled && Self::slot_holds(state, Point::Old) {
                    actions.push(Action::EarlierPull);
                }
            }
            Scenario::Taken => {}
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = *last;
        self.step(&mut state, action);
        if self.omitted(action, &state) {
            return None;
        }
        state.last = Some(action);
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(STALE_KEEPS_LATER, |_, state: &State| {
                state
                    .destroyed
                    .is_none_or(|d| !(d.stale && d.made == Some(Made::Later)))
            }),
            Property::always(KEEPS_ALONE, |_, state: &State| {
                state.destroyed.is_none_or(|d| !d.alone)
            }),
            Property::always(KEEPS_RECEIVING, |_, state: &State| {
                state.destroyed.is_none_or(|d| !d.receiving)
            }),
            Property::always(DONE_HOLDS_POINT, |_, state: &State| match state.row {
                Row::Done(made) => made == Made::Seed,
                Row::Open { .. } | Row::Failed => true,
            }),
            Property::always(EVERY_ROW_ENDS, |model: &Self, state: &State| {
                !state.at_rest(model) || !state.row.open()
            }),
            Property::always(PARSE_CLAIMS_MARKS, |model: &Self, state: &State| {
                let Into::Root(root) = state.into else {
                    return true;
                };
                match model.parse(root) {
                    Parsed::Cut => root.marks.cut,
                    Parsed::Promote => root.marks.promote && !root.marks.cut,
                    Parsed::Writable | Parsed::Unmarked => true,
                }
            }),
        ];
        properties.extend(
            COVERAGE
                .iter()
                .filter(|row| row.scenarios.contains(&self.scenario))
                .map(|row| Property::sometimes(row.name, row.reached)),
        );
        properties
    }
}

const STALE_KEEPS_LATER: &str =
    "a Seed from an ended seed run never destroys a copy of into a later run made";
const KEEPS_ALONE: &str = "a Seed never destroys a copy of into only this Machine may hold";
const KEEPS_RECEIVING: &str = "a Seed never destroys a slot a receive is still writing";
const DONE_HOLDS_POINT: &str = "a seed run ends done only on a root holding its restore point";
const EVERY_ROW_ENDS: &str = "at rest, the seed run's row has ended";
const PARSE_CLAIMS_MARKS: &str = "Inspect names only a mark into's root carries";
const LIVE_REPLACES_PULLED: &str = "a live Seed replaces a Mirror a pull received";
const EARLIER_PULL_AFTER_SEED: &str =
    "admit_gen admits the earlier run's pull after a Seed of into ran";

/// `path` in the code's words: each step, and the row, the wire, A's floor and `into`'s
/// datasets once it ran.
fn story(model: &VolumeMarks, path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    // Stateright pairs each state with the action taken from it; the row lists the
    // action that led to the state instead, so the last row shows the broken state.
    let mut led_by: Option<(State, Action)> = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = led_by.map_or_else(
            || "start".to_owned(),
            |(before, action)| reads_as(model, &before, action, state),
        );
        led_by = action.map(|action| (*state, action));
        let copy = |copy: Copy| {
            let mut marks = Vec::new();
            for (set, mark) in [
                (copy.marks.seed, "seed"),
                (copy.marks.promote, "promote"),
                (copy.marks.cut, "cut"),
            ] {
                if set {
                    marks.push(mark);
                }
            }
            format!(
                "{} newest={:?} pulled={:?} marks=[{}] made={:?}",
                if copy.readonly {
                    "readonly"
                } else {
                    "writable"
                },
                copy.newest,
                copy.pulled,
                marks.join(","),
                copy.made
            )
        };
        let into = match state.into {
            Into::Absent => "absent".to_owned(),
            Into::Root(root) => format!("root {}", copy(root)),
            Into::Slot { fs: None } => "slot, no fs".to_owned(),
            Into::Slot { fs: Some(fs) } => format!("slot fs {}", copy(fs)),
        };
        let _ = writeln!(
            story,
            "{n:2}. {what}\n    row={:?} on_wire={} queued={} floor={} | into: {into} | task={:?}",
            state.row, state.on_wire, state.queued, state.floor, state.task,
        );
    }
    story
}

/// Whether the request's row was open, and who reads the answer.
fn row_and_reader(before: &State, answered: bool) -> (&'static str, &'static str) {
    let row = if before.row.open() {
        "its row open"
    } else {
        "its row ENDED"
    };
    let to = if answered {
        "the step that waits reads the answer"
    } else {
        "no step waits for the answer"
    };
    (row, to)
}

/// `action` from `before` under `model`, read with `after` for the branch it took.
fn reads_as(model: &VolumeMarks, before: &State, action: Action, after: &State) -> String {
    match action {
        Action::Send => {
            "Cloud: seedVolume's 03-ask step sends seed_volume {name, into, guid: G, gen: SEED} (volume-run.inngest.ts)".to_owned()
        }
        Action::TimeOut => {
            "Cloud: the attempt fails short of an answer; sendSwitch fails only the attempt, the step retries, and its request stays in transit (volume-run.server.ts)".to_owned()
        }
        Action::GiveUp => "Cloud: PATIENCE_WAITS ran out, 03-too-long fails the run".to_owned(),
        Action::Admit { answered } => {
            let (row, to) = row_and_reader(before, answered);
            let which = if answered {
                "the request the step waits for"
            } else {
                "a request no step waits for"
            };
            if after.queued > before.queued {
                format!(
                    "A: Volume.Seed checks the gen of {which} against into's floor before the switch lock (under Broken), {row}: admits it, and it goes on toward the lock"
                )
            } else {
                format!(
                    "A: Volume.Seed checks the gen of {which} against into's floor before the switch lock (under Broken), {row}: refuses it below the floor; {to}"
                )
            }
        }
        Action::Seed { answered } | Action::SeedStops { answered } => {
            let (row, to) = row_and_reader(before, answered);
            let broken = model.broken;
            let fs = match before.into {
                Into::Slot { fs } => fs,
                Into::Absent | Into::Root(_) => None,
            };
            let branch = match after.seed {
                Some(SeedPath::Stale) => {
                    return format!(
                        "A: Volume.Seed carries its run's gen, {row}; under the switch lock, check_gen refuses it below into's floor and leaves the floor (machine_api/local.rs seed_volume); {to}"
                    );
                }
                Some(SeedPath::AnswersRoot)
                    if !matches!(before.into, Into::Root(root) if root.marks.seed) =>
                {
                    "the root branch answers a root without ployz:seed = G, skipping its guid check (under Broken)"
                }
                Some(SeedPath::AnswersRoot) => {
                    "the root of into carries ployz:seed = G: answers the root"
                }
                Some(SeedPath::Finishes) => {
                    "the root of into carries ployz:seed = G and still holds G: finish_landing sets readonly=off and destroys root@G, then answers the root"
                }
                Some(SeedPath::Exists) => {
                    "the root of into carries another ployz:seed: refuses, Volume into already exists"
                }
                Some(SeedPath::Receiving) => "take_failure(into) answers Busy: answers the copy as it stands",
                Some(SeedPath::Lands) => {
                    "the slot's fs holds G: land sets ployz:seed and ployz:promote and renames it to the root"
                }
                Some(SeedPath::Stops) => {
                    "the slot's fs holds G: land sets ployz:seed and ployz:promote and renames it to the root, and finish_landing's readonly=off FAILS, so the root stays read-only holding G and the request answers an internal error"
                }
                Some(SeedPath::Held) => {
                    "written_here(fs) reads true: refuses, Volume into is held in slot"
                }
                Some(SeedPath::Replaces) if before.task != Task::Idle => {
                    "take_failure(into)'s Busy skipped (under Broken): zfs destroy -r the slot parent under the receive, a new slot, copy_snapshot of name@G"
                }
                Some(SeedPath::Replaces)
                    if fs.is_some_and(VolumeMarks::written_here) && broken.seed_ignores_written_here =>
                {
                    "written_here(fs) skipped (under Broken): zfs destroy -r the slot parent, a new slot, copy_snapshot of name@G"
                }
                Some(SeedPath::Replaces) if fs.is_none() => {
                    "the slot has no fs: zfs destroy -r the slot parent, a new slot, copy_snapshot of name@G"
                }
                Some(SeedPath::Replaces) => {
                    "written_here(fs) reads false: zfs destroy -r the slot parent, a new slot, copy_snapshot of name@G"
                }
                Some(SeedPath::Starts) => "no slot: a new slot, copy_snapshot of name@G",
                None => "?",
            };
            let admitted = if broken.seed_skips_floor {
                "under the switch lock, checking no floor (under Broken), and admit_mutation"
            } else if broken.floor_checked_only_before_lock {
                "under the switch lock, its floor checked only before it (under Broken), and admit_mutation"
            } else if broken.seed_raises_floor {
                "under the switch lock its floor check admits and RAISES into's floor (under Broken), then under admit_mutation"
            } else {
                "under the switch lock check_gen admits its gen and leaves the floor, then under admit_mutation"
            };
            format!(
                "A: Volume.Seed runs a request, {row}; {admitted}, {branch} (seed.rs seed_volume); {to}"
            )
        }
        Action::CopyDone => {
            "A: copy_snapshot's receive completes, fs holds G, the task ends (seed.rs)".to_owned()
        }
        Action::Adopt => {
            "A: the Deploy's Docker Create adopts the root, inheriting ployz:promote (storage.rs create_volume)".to_owned()
        }
        Action::Cut => {
            "A: the later Move's WriterToMirror admits its gen, sets ployz:cut, snapshots T1 and renames the root into the slot's fs (copy.rs copy_cut)".to_owned()
        }
        Action::Commit => "B: the Move commits; B's writer snapshots T2".to_owned(),
        Action::Pull | Action::EarlierPull => {
            let run = if action == Action::Pull {
                "the later run's"
            } else {
                "the earlier run's"
            };
            let took = if after.task == Task::Idle {
                "admit_gen refuses its gen below the floor"
            } else {
                "admit_gen admits it, the slot becomes a Mirror and its receive starts"
            };
            format!("A: {run} Mirror want: {took} (copy.rs copy_pull)")
        }
        Action::Received => {
            "A: the pull's receive exits 0; fs holds the writer's newest (copy.rs pull_stream)"
                .to_owned()
        }
        Action::MarkPulled if model.broken.pull_keeps_marks => {
            "A: mark_pulled sets ployz:pulled to the newest guid, keeping every mark (under Broken); the task ends (copy.rs)".to_owned()
        }
        Action::MarkPulled => {
            "A: mark_pulled clears (zfs inherit) ployz:cut, epoch, promote and seed and sets ployz:pulled to the newest guid; the task ends (copy.rs)".to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use stateright::{Checker, HasDiscoveries};

    use super::*;

    type Path = Vec<(State, Option<Action>)>;

    /// The code as it is.
    fn fixed(scenario: Scenario) -> VolumeMarks {
        VolumeMarks {
            scenario,
            broken: Broken::default(),
            omit: None,
        }
    }

    /// Volume.Seed with no floor check: the code before the fix, or a Seed sent or read
    /// without a gen.
    fn unfenced(scenario: Scenario) -> VolumeMarks {
        VolumeMarks {
            broken: Broken {
                seed_skips_floor: true,
                ..Broken::default()
            },
            ..fixed(scenario)
        }
    }

    fn check(model: VolumeMarks) -> impl Checker<VolumeMarks> {
        model.checker().threads(4).spawn_bfs().join()
    }

    /// The first path in BFS order whose last state breaks `property` under `model`, so
    /// no shorter path breaks it, printed as a story under the test's name. The checker's
    /// own discovery is not that path: `check_block` stores every violation it visits,
    /// each overwriting the last, and `finish_when` is read once per 1500-state block, so
    /// a visitor keeps the first. One thread keeps the path the same from run to run.
    fn breaks(model: VolumeMarks, property: &'static str) -> Path {
        let holds = model
            .properties()
            .into_iter()
            .find(|candidate| candidate.name == property)
            .expect("a model property")
            .condition;
        let first = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&first);
        model
            .checker()
            .threads(1)
            .visitor(move |path: stateright::Path<State, Action>| {
                let mut first = seen.lock().expect("the first break");
                if first.is_none() && !holds(&model, path.last_state()) {
                    *first = Some(path.into_vec());
                }
            })
            .finish_when(HasDiscoveries::AllOf([property].into()))
            .spawn_bfs()
            .join();
        let path = first
            .lock()
            .expect("the first break")
            .take()
            .expect("a path breaks the property");
        let test = std::thread::current();
        println!(
            "[{}] {property} fails under {model:?}:\n{}",
            test.name().unwrap_or("?"),
            story(&model, &path)
        );
        path
    }

    fn last(path: &Path) -> State {
        path.last().expect("a path has a state").0
    }

    /// The finding, without the floor. The seed run's first request outlives its
    /// attempt. A resend starts the copy, another lands it, and the row ends done. The
    /// Deploy adopts the root, the later Move cuts it into an old copy and commits on B,
    /// and B's newer snapshot is pulled back into the slot, where `mark_pulled` clears
    /// `ployz:seed` and `ployz:cut`. The stranded request then lands: `written_here` reads
    /// false, and the Seed destroys the Mirror the later run pulled.
    #[test]
    fn a_seed_needs_the_floor_of_into() {
        let path = breaks(unfenced(Scenario::Fresh), STALE_KEEPS_LATER);
        let broken = last(&path);
        let destroyed = broken.destroyed.expect("the Seed destroyed a slot");
        assert!(destroyed.stale, "the seed run's row had ended");
        assert_eq!(destroyed.made, Some(Made::Later));
        assert!(destroyed.pulled, "the slot was a Mirror a pull marked");
        assert!(!destroyed.alone && !destroyed.receiving);
        assert_eq!(broken.row, Row::Done(Made::Seed));
        assert_eq!(broken.task, Task::Copying, "the Seed copies the old point");
        assert!(
            path.iter()
                .any(|(_, action)| *action == Some(Action::MarkPulled)),
            "mark_pulled ran before the Seed"
        );
    }

    /// Without the floor every other property holds, in every scenario: the late Seed
    /// breaks only the one the finding names.
    #[test]
    fn without_the_floor_only_the_late_seed_property_breaks() {
        for scenario in [Scenario::Fresh, Scenario::OldCopy, Scenario::Taken] {
            let checker = check(unfenced(scenario));
            println!(
                "{scenario:?}, unfenced: {} states",
                checker.unique_state_count()
            );
            for property in [
                KEEPS_ALONE,
                KEEPS_RECEIVING,
                DONE_HOLDS_POINT,
                EVERY_ROW_ENDS,
            ] {
                assert!(
                    checker.discovery(property).is_none(),
                    "{property} fails in {scenario:?}"
                );
            }
            assert_eq!(
                checker.discovery(STALE_KEEPS_LATER).is_some(),
                scenario == Scenario::Fresh,
                "only the later Move reaches the finding"
            );
        }
    }

    /// The code holds every property in every scenario, and every `sometimes` is
    /// reached: a live Seed still replaces a Mirror a pull received, as `seed_tests.rs`
    /// `a_seed_replaces_a_slot_a_mirror_pull_marked_or_that_holds_no_point` pins, while
    /// the floor the later Move raised refuses the stranded request.
    #[test]
    fn the_floor_keeps_a_late_seed_off_a_later_copy() {
        for scenario in ALL {
            let checker = check(fixed(*scenario));
            println!(
                "{scenario:?}, fixed: {} states",
                checker.unique_state_count()
            );
            checker.assert_properties();
        }
    }

    /// A floor checked only before the switch lock admits the stranded request at floor 0
    /// (row 5 of the printed story). It is then delayed between the check and taking the
    /// lock while the Deploy adopts the root (row 11), the Move cuts it and raises the floor
    /// to 3 (row 12), and the later run pulls a Mirror back (rows 14-16). It then destroys
    /// that Mirror (row 17).
    #[test]
    fn a_seed_needs_the_floor_checked_under_the_switch_lock() {
        let path = breaks(
            VolumeMarks {
                broken: Broken {
                    floor_checked_only_before_lock: true,
                    ..Broken::default()
                },
                ..fixed(Scenario::Fresh)
            },
            STALE_KEEPS_LATER,
        );
        let broken = last(&path);
        let destroyed = broken.destroyed.expect("the Seed destroyed a slot");
        assert!(destroyed.stale && destroyed.pulled);
        assert_eq!(destroyed.made, Some(Made::Later));
        let step = |wanted: Action| {
            path.iter()
                .position(|(_, action)| *action == Some(wanted))
                .expect("the path takes the step")
        };
        assert!(
            step(Action::Admit { answered: false }) < step(Action::Cut),
            "the stranded request passed the check before the Move raised the floor"
        );
    }

    /// A Seed leaves `into`'s floor where it found it, so a request on `into` with a gen
    /// below the seed run's is still admitted after a Seed ran. A check that raised the
    /// floor would refuse it, as it would every mirror sync of `into` whose gen is below
    /// the seed run's. In every scenario, raising it keeps every safety property and leaves
    /// only that one `sometimes` unreached.
    #[test]
    fn a_seed_leaves_the_floor_of_into_where_it_found_it() {
        for scenario in ALL {
            let checker = check(VolumeMarks {
                broken: Broken {
                    seed_raises_floor: true,
                    ..Broken::default()
                },
                ..fixed(*scenario)
            });
            for property in [
                STALE_KEEPS_LATER,
                KEEPS_ALONE,
                KEEPS_RECEIVING,
                DONE_HOLDS_POINT,
                EVERY_ROW_ENDS,
            ] {
                assert!(
                    checker.discovery(property).is_none(),
                    "{property} fails in {scenario:?}"
                );
            }
            let unreached: Vec<&str> = COVERAGE
                .iter()
                .filter(|row| row.scenarios.contains(scenario))
                .map(|row| row.name)
                .filter(|name| checker.discovery(name).is_none())
                .collect();
            let expected: &[&str] = if *scenario == Scenario::OldCopy {
                &[EARLIER_PULL_AFTER_SEED]
            } else {
                &[]
            };
            assert_eq!(unreached, expected, "unreached in {scenario:?}");
        }
    }

    /// Every `sometimes` needs what it names: with the row's `omit` taken out of the
    /// fixed model, no other path reaches its predicate.
    #[test]
    fn every_sometimes_needs_what_it_names() {
        for row in COVERAGE {
            for scenario in row.scenarios {
                let whole = check(fixed(*scenario));
                assert!(
                    whole.discovery(row.name).is_some(),
                    "{} is never reached in {scenario:?}",
                    row.name
                );
                let cut = check(VolumeMarks {
                    omit: Some(row.omit),
                    ..fixed(*scenario)
                });
                assert!(
                    cut.discovery(row.name).is_none(),
                    "{} is reached in {scenario:?} without {:?}",
                    row.name,
                    row.omit
                );
            }
        }
    }

    /// A parse that reads every read-only root as a cut names a cut on the root a Seed
    /// landing left read-only, which carries `ployz:seed` and `ployz:promote` and no
    /// `ployz:cut`.
    #[test]
    fn inspect_needs_the_marks_of_a_read_only_root() {
        let model = VolumeMarks {
            broken: Broken {
                parse_reads_readonly_as_cut: true,
                ..Broken::default()
            },
            ..fixed(Scenario::Fresh)
        };
        let path = breaks(model, PARSE_CLAIMS_MARKS);
        let broken = last(&path);
        let Into::Root(root) = broken.into else {
            panic!("{:?}", broken.into);
        };
        assert!(root.readonly && root.marks.promote && !root.marks.cut);
        assert_eq!(broken.seed, Some(SeedPath::Stops));
        assert_eq!(model.parse(root), Parsed::Cut);
        assert_eq!(fixed(Scenario::Fresh).parse(root), Parsed::Promote);
    }

    /// Without the `ployz:seed` check on a root, a Seed answers a root another Volume
    /// named `into` holds, and the run ends done on data that is not its restore point.
    #[test]
    fn a_root_needs_its_seed_guid_checked() {
        let path = breaks(
            VolumeMarks {
                broken: Broken {
                    root_ignores_seed: true,
                    ..Broken::default()
                },
                ..fixed(Scenario::Taken)
            },
            DONE_HOLDS_POINT,
        );
        assert_eq!(last(&path).row, Row::Done(Made::Other));
    }

    /// Without the `Busy` arm of `receives.take_failure`, a resend while the seed's own copy receives destroys
    /// the slot under it and starts the copy over.
    #[test]
    fn a_seed_needs_to_wait_for_a_running_receive() {
        let path = breaks(
            VolumeMarks {
                broken: Broken {
                    seed_ignores_receive: true,
                    ..Broken::default()
                },
                ..fixed(Scenario::Fresh)
            },
            KEEPS_RECEIVING,
        );
        let destroyed = last(&path).destroyed.expect("the Seed destroyed a slot");
        assert!(destroyed.receiving && !destroyed.stale);
    }

    /// Without `written_here`, a live Seed destroys the cut-marked old copy an earlier
    /// Volume named `into` left, which may be the only copy of its point.
    #[test]
    fn a_seed_needs_written_here() {
        let path = breaks(
            VolumeMarks {
                broken: Broken {
                    seed_ignores_written_here: true,
                    ..Broken::default()
                },
                ..fixed(Scenario::OldCopy)
            },
            KEEPS_ALONE,
        );
        let destroyed = last(&path).destroyed.expect("the Seed destroyed a slot");
        assert_eq!(destroyed.made, Some(Made::Earlier));
        assert!(destroyed.alone && !destroyed.stale);
    }

    /// Without the mark clearing in `mark_pulled`, a pulled Mirror keeps the old copy's
    /// `ployz:cut`, `written_here` reads true, and a live Seed never replaces it, which
    /// the seed tests pin. The clearing is also what opens the finding's path: with the
    /// marks kept, the code as it is refuses the stranded request too. The fix is the
    /// floor, not a stricter `written_here`.
    #[test]
    fn a_live_seed_replaces_a_pulled_mirror_once_mark_pulled_clears_its_marks() {
        let keeps = Broken {
            pull_keeps_marks: true,
            ..Broken::default()
        };
        let checker = check(VolumeMarks {
            broken: keeps,
            ..fixed(Scenario::OldCopy)
        });
        assert!(checker.discovery(LIVE_REPLACES_PULLED).is_none());
        assert!(
            fixed(Scenario::OldCopy)
                .properties()
                .iter()
                .any(|property| property.name == LIVE_REPLACES_PULLED)
        );

        let checker = check(VolumeMarks {
            broken: Broken {
                seed_skips_floor: true,
                ..keeps
            },
            ..fixed(Scenario::Fresh)
        });
        assert!(checker.discovery(STALE_KEEPS_LATER).is_none());
    }
}
