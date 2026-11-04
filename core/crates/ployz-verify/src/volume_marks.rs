//! A Seed request against the ZFS `ployz:*` marks on one Machine, A. Cloud's `seed` run
//! (`seedVolume`, volume-run.inngest.ts) resends `Volume.Seed` for a new Volume `into`,
//! holding the restore point `G` of `name`, until A answers a root. A copies `G` into a
//! slot of `into` and lands it as a root (`VolumeStorage::seed_volume`, seed.rs). Docker's
//! Create adopts it, a later Move cuts it into an old copy (`copy_cut`, copy.rs) once a
//! writer on B takes over, and a mirror pull re-roles that copy to a Mirror
//! (`copy_pull` and `mark_pulled`, copy.rs). A Seed request is a message: it can wait in
//! transit, before the switch lock, past its own run's row.
//!
//! The finding: such a request from an ended seed run lands on the Mirror the later run
//! pulled. `mark_pulled` cleared `ployz:seed` and `ployz:cut` and set `ployz:pulled`, so
//! `written_here` (departure.rs) reads false, and the Seed runs `zfs destroy -r` on the
//! slot and starts copying the old restore point into it.
//!
//! Every plugin handler holds `admit_mutation` (storage.rs) from its first read to its
//! answer, so each handler is one step. A receive task runs outside it, so a receive
//! completing and `mark_pulled` are steps of their own.
//!
//! Out of scope: a seed copy that fails, `ployz:epoch` (no modelled copy carries it), the
//! departure, a crash between the zfs calls of one handler, `ployz:pull` and the role
//! property on the slot parent, more than one Seed request stranded, and Busy answers.

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

/// The receive task for `into` in `Receives` (transfer.rs). `running` reads true from
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
    /// A root of `into` with this guid in `ployz:seed`: answers the root.
    AnswersRoot,
    /// A root of `into` with another or no `ployz:seed`: "Volume into already exists".
    Exists,
    /// `receives.running(&into)`: answers the copy as it stands.
    Receiving,
    /// The slot's `fs` holds `G` as its newest: `land` renames it to the root.
    Lands,
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
    /// A's `Volume.Seed` (machine_api/local.rs `seed_volume`, `switch_verb`) runs one
    /// request off the wire; `answered` when the step that sent it still waits.
    Seed { answered: bool },
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
    /// Seed requests sent and not yet run by A, all alike: `{name, into, guid: G}`.
    on_wire: u8,
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

#[derive(Clone, Copy, Debug)]
struct VolumeMarks {
    scenario: Scenario,
}

impl VolumeMarks {
    /// `written_here` (departure.rs): a departure snapshot or a seed, promote, cut or
    /// epoch mark, or a newest snapshot no pull marked.
    fn written_here(fs: Copy) -> bool {
        fs.marks.any() || fs.newest.is_some_and(|newest| fs.pulled != Some(newest))
    }

    /// `VolumeStorage::seed_volume` (seed.rs) for one request, under `admit_mutation`.
    fn seed(state: &mut State) -> Answer {
        let (path, answer) = match state.into {
            // seed.rs: `Self::dataset(..)` finds the root; `ployz:seed` against the guid.
            // A landed root no longer holds `G`, so `finish_landing` does not run again.
            Into::Root(root) if root.marks.seed => (SeedPath::AnswersRoot, Answer::Root(root.made)),
            Into::Root(_) => (SeedPath::Exists, Answer::Refused),
            // seed.rs: `self.receives.running(&into)`.
            _ if state.task != Task::Idle => (SeedPath::Receiving, Answer::Slot),
            // seed.rs: the slot's `fs` newest is `G`, so `land` inherits `ployz:pulled`,
            // sets `ployz:seed` and `ployz:promote`, renames it to the root and
            // `finish_landing` destroys the parent and `root@G`.
            Into::Slot { fs: Some(fs) } if fs.newest == Some(Point::G) => {
                state.into = Into::Root(Copy {
                    newest: None,
                    pulled: None,
                    marks: Marks {
                        seed: true,
                        promote: true,
                        cut: false,
                    },
                    ..fs
                });
                (SeedPath::Lands, Answer::Root(fs.made))
            }
            // seed.rs: `slot_fs` exists and `written_here(&fs)`.
            Into::Slot { fs: Some(fs) } if Self::written_here(fs) => {
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
            Answer::Slot => Row::Open { awaiting: false },
            Answer::Refused => Row::Failed,
        };
    }

    /// `FloorTurn::admit` (docker/spec_store.rs) through `admit_gen`
    /// (machine_api/local_volume_copy.rs), which `set_copy` runs before the switch lock.
    fn admit(state: &mut State, generation: u8) -> bool {
        if generation < state.floor {
            return false;
        }
        state.floor = state.floor.max(generation);
        true
    }

    fn step(state: &mut State, action: Action) {
        match action {
            Action::Send => {
                state.sends += 1;
                state.on_wire += 1;
                state.row = Row::Open { awaiting: true };
            }
            Action::TimeOut => {
                state.timeouts += 1;
                state.row = Row::Open { awaiting: false };
            }
            Action::GiveUp => state.row = Row::Failed,
            Action::Seed { answered } => {
                state.on_wire -= 1;
                let answer = Self::seed(state);
                if answered {
                    Self::answer(state, answer);
                }
            }
            Action::CopyDone => {
                state.into = Into::Slot {
                    fs: Some(Copy {
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
                    fs.marks = Marks::default();
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
}

impl Model for VolumeMarks {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        let into = match self.scenario {
            Scenario::Fresh => Into::Absent,
            Scenario::OldCopy => Into::Slot {
                fs: Some(Copy {
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
        let awaiting = u8::from(state.row == Row::Open { awaiting: true });
        if awaiting == 1 && state.on_wire >= 1 {
            actions.push(Action::Seed { answered: true });
        }
        if state.on_wire > awaiting {
            actions.push(Action::Seed { answered: false });
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
                if !state.adopted && root.is_some_and(|root| root.marks.promote) {
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
        Self::step(&mut state, action);
        state.last = Some(action);
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        vec![
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
        ]
    }
}

const STALE_KEEPS_LATER: &str =
    "a Seed from an ended seed run never destroys a copy of into a later run made";
const KEEPS_ALONE: &str = "a Seed never destroys a copy of into only this Machine may hold";
const KEEPS_RECEIVING: &str = "a Seed never destroys a slot a receive is still writing";
const DONE_HOLDS_POINT: &str = "a seed run ends done only on a root holding its restore point";
const EVERY_ROW_ENDS: &str = "at rest, the seed run's row has ended";

/// `path` in the code's words: each step, and the row, the wire, A's floor and `into`'s
/// datasets once it ran.
fn story(path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    // Stateright pairs each state with the action taken from it; the row lists the
    // action that led to the state instead, so the last row shows the broken state.
    let mut led_by: Option<(State, Action)> = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = led_by.map_or_else(
            || "start".to_owned(),
            |(before, action)| reads_as(&before, action, state),
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
                "newest={:?} pulled={:?} marks=[{}] made={:?}",
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
            "{n:2}. {what}\n    row={:?} on_wire={} floor={} | into: {into} | task={:?}",
            state.row, state.on_wire, state.floor, state.task,
        );
    }
    story
}

/// `action` from `before`, read with `after` for the branch it took.
fn reads_as(before: &State, action: Action, after: &State) -> String {
    match action {
        Action::Send => {
            "Cloud: seedVolume's 03-ask step sends seed_volume {name, into, guid: G} (volume-run.inngest.ts)".to_owned()
        }
        Action::TimeOut => {
            "Cloud: the attempt fails short of an answer; sendSwitch fails only the attempt, the step retries, and its request stays in transit (volume-run.server.ts)".to_owned()
        }
        Action::GiveUp => "Cloud: PATIENCE_WAITS ran out, 03-too-long fails the run".to_owned(),
        Action::Seed { answered } => {
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
            let branch = match after.seed {
                Some(SeedPath::AnswersRoot) => {
                    "the root of into carries ployz:seed = G: answers the root"
                }
                Some(SeedPath::Exists) => {
                    "the root of into carries another ployz:seed: refuses, Volume into already exists"
                }
                Some(SeedPath::Receiving) => "receives.running(into): answers the copy as it stands",
                Some(SeedPath::Lands) => {
                    "the slot's fs holds G: land sets ployz:seed and ployz:promote and renames it to the root"
                }
                Some(SeedPath::Held) => {
                    "written_here(fs) reads true: refuses, Volume into is held in slot"
                }
                Some(SeedPath::Replaces) => {
                    "written_here(fs) reads false: zfs destroy -r the slot parent, a new slot, copy_snapshot of name@G"
                }
                Some(SeedPath::Starts) => "no slot: a new slot, copy_snapshot of name@G",
                None => "?",
            };
            format!(
                "A: Volume.Seed runs a request, {row}, under the switch lock and admit_mutation; {branch} (seed.rs seed_volume); {to}"
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
        Action::MarkPulled => {
            "A: mark_pulled inherits ployz:cut, epoch, promote and seed and sets ployz:pulled to the newest guid; the task ends (copy.rs)".to_owned()
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use stateright::{Checker, HasDiscoveries};

    use super::*;

    type Path = Vec<(State, Option<Action>)>;

    /// The first path in BFS order whose last state breaks `property` under `model`, so
    /// no shorter path breaks it, printed as a story. A visitor keeps the first: the
    /// checker's own discovery is whichever violation its last block stored. One thread
    /// keeps the path the same from run to run.
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
            story(&path)
        );
        path
    }

    fn last(path: &Path) -> State {
        path.last().expect("a path has a state").0
    }

    /// The finding. The seed run's first request outlives its attempt. A resend starts
    /// the copy, another lands it, and the row ends done. The Deploy adopts the root, the
    /// later Move cuts it into an old copy and commits on B, and B's newer snapshot is
    /// pulled back into the slot, where `mark_pulled` clears `ployz:seed` and `ployz:cut`.
    /// The stranded request then lands: `written_here` reads false, and the Seed
    /// destroys the Mirror the later run pulled. This test passes by finding that path.
    #[test]
    fn current_code_lets_a_late_seed_destroy_a_mirror_a_later_run_pulled() {
        let path = breaks(
            VolumeMarks {
                scenario: Scenario::Fresh,
            },
            STALE_KEEPS_LATER,
        );
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

    /// Every other property holds on the code as it is, in every scenario: the late
    /// Seed breaks only the one the finding names.
    #[test]
    fn current_code_breaks_only_the_late_seed_property() {
        for scenario in [Scenario::Fresh, Scenario::OldCopy, Scenario::Taken] {
            let checker = VolumeMarks { scenario }
                .checker()
                .threads(4)
                .spawn_bfs()
                .join();
            println!(
                "{scenario:?}, as is: {} states",
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
}
