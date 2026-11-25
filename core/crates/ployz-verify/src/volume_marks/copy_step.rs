//! A seed a person's Copy step admits, on the Machine A that holds the source's Writer.
//! Cloud resends `SeedCopy` for the new Volume `into` until A answers a root, and any
//! request can arrive late. A takes each under the switch lock in two steps
//! (`LocalMachine::seed_copy`, machine/local_machine/volume_copy.rs): it takes the step in
//! its approvals record (`ConsumptionLog::take_copy`, consumption.rs), which admits a step
//! not taken yet only while its window holds and tells the plugin whether the window was
//! open (`StartWindow`), then calls the plugin. The call can end before the plugin does
//! anything: the socket refuses it, the daemon restarts before it sends, or the plugin's
//! mutation refuses. Otherwise the plugin's bound seed runs under `admit_mutation`
//! (`VolumeStorage::seed_volume` with a `CopyBinding`, seed.rs): its reads, then for a new
//! copy the slot ZFS makes carrying the step and recording the birth (`create_marked_slot`)
//! and the receive's spawn, or for a copy a restart killed the clear of the old receive
//! inside the slot it keeps (`Stale::restart`) and the spawn. A new copy reads the window
//! at the top of its arm and right before each of its two effects; a copy started over in
//! the slot its step made reads none, since that slot is the destination the window
//! already admitted. Any effect can fail or be the last before a restart.
//! A receive runs outside both locks. The user deletes `into` under the same mutation, and
//! the delete moves it to the Trash (`VolumeStorage::discard`, trash.rs): the trash marks,
//! the rename, then the step's tombstone (`VolumeStorage::bury`) once the rename committed,
//! with a restart possible between any two. A seed into an absent `into` buries the steps
//! the Trash holds before it reads its own (`bury_trash`). The person can restore the entry
//! where nothing of `into` is (`restore_trash`), never a slot still carrying its step, and the
//! sweep can destroy it (`sweep_trash`), burying first.
//! The daemon and the plugin can restart between any two steps, which kills the receive
//! and loses the request in hand, but keeps the record, the tombstones and the datasets.
//! The step's window closes once, and the plugin's wall clock may step back once after.
//! Another approval can then make a Volume of the same name, which carries another origin,
//! or leave an empty slot parent of it, as an interrupted mirror pull does.
//!
//! The properties: once `into` was deleted carrying the birth, no copy makes it again; a
//! copy never answers, lands on or destroys a copy of `into` it did not make; every root
//! the copy answers carries the step's birth; a new copy makes its slot and starts its
//! receive only inside the step's window on the plugin's clock; a call the daemon found
//! past the window starts no new copy; a copy is refused as deleted only after `into`
//! was deleted, so no call that ended or failed partway costs the approval; and `into`
//! never holds two roots.
//!
//! Out of scope: a receive that fails (its answer is the same `Failed` arm the unbound seed
//! has), the zfs calls inside a landing (the `volume_marks` model checks their order), a
//! wall clock that steps back while a call the daemon found open waits for the plugin
//! (a monotonic clock both processes share would bound it; nothing here needs one), Docker
//! adopting the root, the later runs `volume_marks` models, and the bundle's signature,
//! which ployz-core's `authority::copy` tests cover.

use std::fmt::Write as _;

use stateright::{Model, Property};

use crate::harness::{self, Narrated, everywhere};

/// Where a copy of `into` began: the step's birth, the source point's own origin, or
/// another approval's Volume of the same name.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Origin {
    Birth,
    Source,
    Other,
}

/// How much of the point a slot's `fs` holds.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Fill {
    /// A receive started and has not finished, or was killed.
    Partial,
    /// The receive finished: `fs` holds the point.
    Point,
}

/// `into`'s datasets on A.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Into {
    Absent,
    /// A slot: whether its parent carries the step (`ployz:copy-step`), the origin it
    /// records (`ployz:pull-origin`), and its `fs`.
    Slot {
        ours: bool,
        record: Option<Origin>,
        fs: Option<Fill>,
    },
    /// A root, with the origin it carries (`ployz:origin`). Only the step's landing makes
    /// one carrying the birth, and that root carries the step too.
    Root(Origin),
}

impl Into {
    /// The copy of `into` carries the step's birth.
    fn carries_birth(self) -> bool {
        matches!(
            self,
            Self::Root(Origin::Birth)
                | Self::Slot {
                    record: Some(Origin::Birth),
                    ..
                }
        )
    }

    /// A dataset of `into` carries the step's digest, so destroying it buries the step.
    fn carries_step(self) -> bool {
        matches!(
            self,
            Self::Root(Origin::Birth) | Self::Slot { ours: true, .. }
        )
    }

    /// A copy of `into` the step did not make.
    fn other(self) -> bool {
        matches!(
            self,
            Self::Root(Origin::Other) | Self::Slot { ours: false, .. }
        )
    }

    const EMPTY_OTHER: Self = Self::Slot {
        ours: false,
        record: None,
        fs: None,
    };
}

/// The effect a plugin call, its reads done, makes next. A new copy reads the window right
/// before each.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Stage {
    /// ZFS makes the slot carrying the step and recording the birth, in one create.
    Make,
    /// `Stale::restart` destroys the old receive's `fs`, keeping the parent.
    Clear,
    /// The receive starts into `fs`.
    Spawn,
}

/// The one request A holds, once taken. `open` is the daemon's verdict on the window when
/// it took the request, which the plugin hears as `StartWindow`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Held {
    Idle,
    Took {
        open: bool,
    },
    /// Inside the plugin's call, under its mutation, before `at`. `new` for a copy into an
    /// absent `into`, as against one started over.
    Plugin {
        open: bool,
        at: Stage,
        new: bool,
    },
}

/// The branch the plugin's bound seed took (`seed_progress` and `seed_volume`, seed.rs).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Path {
    /// A root that does not carry the birth, or a slot the step did not make:
    /// `SeedProgress::Taken`.
    Taken,
    /// A root carrying the birth: answered.
    AnswersRoot,
    /// A receive still running into the step's slot: `Copying`.
    Copying,
    /// The step's complete slot: `Copied::land` stamps the record and renames it, and the
    /// landing's destroy of the parent buries the step.
    Lands,
    /// The step's slot holding no complete point: `Stale`, its receive started over in it.
    StartsOver,
    /// No `into` and no tombstone: a new slot and receive.
    Starts,
    /// No `into` and the step's tombstone: refused as deleted.
    Deleted,
    /// `CopyBinding::may_start` refused a new copy: the window closed, or the daemon found
    /// it closed.
    Expired,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// Cloud sends SeedCopy; the request may wait in transit for any time.
    Send,
    /// A takes a request under the switch lock: verify, `take_copy`.
    Take,
    /// The call ends with no further effect: refused before the plugin acts, lost to a
    /// restart before it was sent, or a zfs call of the plugin's fails.
    Outage,
    /// The plugin's reads under `admit_mutation`, and the branch they choose.
    Plugin,
    /// The plugin's next effect in the call, after the window read before it for a new copy.
    Advance,
    /// The receive task exits 0.
    Received,
    /// The daemon and the plugin restart.
    Restart,
    /// The step's window closes.
    WindowCloses,
    /// The plugin's wall clock steps back across the window's end.
    ClockBack,
    /// The user deletes `into` (Volume.Remove or a forget, under `admit_mutation`):
    /// `discard` writes its trash marks, which nothing on a live copy reads.
    Delete,
    /// `discard`'s rename moves `into` to the Trash.
    Trashed,
    /// `discard` buries the step the entry carries, after the rename.
    Buried,
    /// The person restores the entry (`restore_trash`, under `admit_mutation`).
    Restore,
    /// The sweep, or an Empty, destroys the entry, burying first.
    Expire,
    /// Another approval makes a Volume named `into`, which carries another origin.
    Reuse,
    /// An interrupted mirror pull of another Volume named `into` leaves an empty slot parent.
    ReuseEmpty,
}

/// Where `into` starts.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Scenario {
    /// No `into`.
    Fresh,
    /// A root of `into` another approval made.
    TakenRoot,
    /// A slot of `into` receiving another Volume's point, killed by a restart.
    TakenSlot,
    /// An empty slot parent of `into` an interrupted mirror pull left.
    EmptySlot,
}

const ALL: &[Scenario] = &[
    Scenario::Fresh,
    Scenario::TakenRoot,
    Scenario::TakenSlot,
    Scenario::EmptySlot,
];

const MAX_SENDS: u8 = 3;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is one fact a property or coverage row reads"
)]
struct State {
    /// The window on the daemon's clock, which only moves forward.
    open: bool,
    /// The window on the plugin's wall clock.
    plugin_open: bool,
    clock_back: bool,
    /// The step is in `restart-approvals.json`.
    taken: bool,
    /// The plugin's tombstone of the step, or under `Broken::reserves_at_begin` its
    /// reservation.
    tombstone: bool,
    into: Into,
    receiving: bool,
    held: Held,
    /// A delete holds the mutation: it began and has not buried yet.
    deleting: bool,
    /// The delete's rename committed and its tombstone is not written yet.
    renamed: bool,
    /// The Trash entry the delete made, as `into` was.
    trash: Option<Into>,
    on_wire: u8,
    sends: u8,
    restarted: bool,
    outage: bool,
    reused: bool,
    /// A slot was made under the step.
    made: bool,
    /// `into` was deleted while it carried the birth.
    deleted: bool,
    /// A restart came before a delete's rename.
    delete_cut: bool,
    /// A restart came between a delete's rename and its tombstone.
    bury_cut: bool,
    /// The entry came back from the Trash.
    restored: bool,
    /// The sweep destroyed the entry.
    expired: bool,
    /// A copy made, refilled or landed `into` after it was deleted carrying the birth.
    remade: bool,
    /// A restore put a root under `into` while it held one.
    two_roots: bool,
    /// The effect a call was cut before, by an outage or a restart.
    cut: Option<Stage>,
    /// A step destroyed or landed on a copy of `into` it did not make.
    harmed_other: bool,
    /// The origin of the last root the plugin answered.
    answered: Option<Origin>,
    /// A new copy made its slot or started its receive with the window closed on the
    /// plugin's clock.
    started_late: bool,
    /// A new copy made its slot or started its receive in a call the daemon found past the
    /// window.
    started_closed: bool,
    /// A copy was refused as deleted though `into` was never deleted.
    falsely_deleted: bool,
    /// A restart came between the receive finishing and the landing.
    restarted_after_receipt: bool,
    path: Option<Path>,
    last: Option<Action>,
}

/// A guard the code has, taken out, to show the check catches what it prevents.
#[derive(Clone, Copy, Debug, Default, Hash, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag removes one independent guard"
)]
struct Broken {
    /// No destroy leaves a tombstone, so the `Absent` arm never knows `into` was deleted.
    no_tombstone: bool,
    /// The plugin reserves the step before it makes the slot and refuses an absent `into`
    /// whose step is reserved, as the cut before this one did.
    reserves_at_begin: bool,
    /// `Stale` destroys the step's whole slot, burying the step, and makes it again, as
    /// the first cuts did.
    destroys_parent_on_restart: bool,
    /// A slot is the step's when its record is the birth or it records nothing and holds
    /// no `fs`, whatever step its parent carries.
    ignores_step: bool,
    /// `seed_progress` adopts any root or slot of `into`, whatever it carries.
    ignores_origin: bool,
    /// `take_copy` takes a new step, and `may_start` starts a copy, whatever the window.
    ignores_window: bool,
    /// The plugin starts a copy under the window as the daemon read it when it took the
    /// request, not as its clock reads under the mutation.
    window_at_take: bool,
    /// The plugin reads the window once, before its reads and preparation, not right
    /// before each effect that starts a copy.
    window_before_reads: bool,
    /// The plugin reads only its own clock, not the daemon's verdict that the window
    /// closed.
    ignores_verdict: bool,
    /// `Copied::land` stamps the point's own origin, not the slot's record.
    lands_point_origin: bool,
    /// A seed into an absent `into` reads its tombstone without burying the Trash's steps.
    no_trash_bury: bool,
    /// A restore puts back a slot whose parent still carries its step.
    restores_unlanded: bool,
    /// A restore goes ahead while `into` has a copy.
    restores_over_taken: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Omit {
    Actions(&'static [Action]),
    Path(Path),
}

type Coverage = harness::Coverage<CopyStep, Omit>;

#[derive(Clone, Copy, Debug)]
struct CopyStep {
    scenario: Scenario,
    broken: Broken,
    omit: Option<Omit>,
}

fn took(state: &State, path: Path) -> bool {
    matches!(state.last, Some(Action::Plugin | Action::Advance)) && state.path == Some(path)
}

fn fresh(model: &CopyStep) -> bool {
    model.scenario == Scenario::Fresh
}

const NEVER_REMADE: &str = "once into was deleted carrying the birth, no copy makes it again";
const KEEPS_OTHER: &str =
    "a copy never answers, lands on or destroys a copy of into it did not make";
const ANSWERS_BIRTH: &str = "every root the copy answers carries the step's birth";
const STARTS_IN_WINDOW: &str = "a new copy makes its slot and starts its receive only inside the step's window on the plugin's clock";
const CLOSED_STAYS_CLOSED: &str = "a call the daemon found past the window starts no new copy";
const DELETED_ONLY_AFTER_DELETE: &str = "a copy is refused as deleted only after into was deleted";
const ONE_ROOT: &str = "into never holds two roots";

/// Each branch the bounds reach, once. `every_copy_sometimes_needs_what_it_names` takes each
/// row's `omit` out and expects its `name` unreached.
const COVERAGE: &[Coverage] = &[
    Coverage {
        name: "a copy starts in a new slot carrying the step and the birth",
        on: fresh,
        omit: Some(Omit::Path(Path::Starts)),
        reached: |_, state| took(state, Path::Starts),
    },
    Coverage {
        name: "a resend after a call that ended before the plugin begins the copy",
        on: fresh,
        omit: Some(Omit::Actions(&[Action::Outage])),
        reached: |_, state| took(state, Path::Starts) && state.outage && state.cut.is_none(),
    },
    Coverage {
        name: "a resend after a call cut before its slot was made begins the copy",
        on: fresh,
        omit: Some(Omit::Actions(&[Action::Outage, Action::Restart])),
        reached: |_, state| took(state, Path::Starts) && state.cut == Some(Stage::Make),
    },
    Coverage {
        name: "a copy lands carrying the birth",
        on: fresh,
        omit: Some(Omit::Path(Path::Lands)),
        reached: |_, state| took(state, Path::Lands) && state.answered == Some(Origin::Birth),
    },
    Coverage {
        name: "a resend after the window finishes a copy underway",
        on: fresh,
        omit: Some(Omit::Path(Path::Lands)),
        reached: |_, state| took(state, Path::Lands) && !state.open,
    },
    Coverage {
        name: "a copy lands after a restart between receipt and stamping",
        on: fresh,
        omit: Some(Omit::Actions(&[Action::Restart])),
        reached: |_, state| took(state, Path::Lands) && state.restarted_after_receipt,
    },
    Coverage {
        name: "a resend answers the copy still receiving",
        on: fresh,
        omit: Some(Omit::Path(Path::Copying)),
        reached: |_, state| took(state, Path::Copying),
    },
    Coverage {
        name: "a resend answers the root it landed",
        on: fresh,
        omit: Some(Omit::Path(Path::AnswersRoot)),
        reached: |_, state| took(state, Path::AnswersRoot),
    },
    Coverage {
        name: "a copy killed by a restart starts over in its own slot",
        on: fresh,
        omit: Some(Omit::Path(Path::StartsOver)),
        reached: |_, state| took(state, Path::StartsOver),
    },
    Coverage {
        name: "a copy killed by a restart starts over in its own slot in a call the daemon found past the window",
        on: fresh,
        omit: Some(Omit::Path(Path::StartsOver)),
        reached: |_, state| took(state, Path::StartsOver) && !state.open,
    },
    Coverage {
        name: "a copy cut after its slot was made, or while it starts over, starts over in its slot",
        on: fresh,
        omit: Some(Omit::Actions(&[Action::Outage, Action::Restart])),
        reached: |_, state| {
            took(state, Path::StartsOver) && matches!(state.cut, Some(Stage::Clear | Stage::Spawn))
        },
    },
    Coverage {
        name: "a late resend refuses into deleted",
        on: fresh,
        omit: Some(Omit::Path(Path::Deleted)),
        reached: |_, state| took(state, Path::Deleted) && state.deleted,
    },
    Coverage {
        name: "a late resend after a restart refuses into deleted",
        on: fresh,
        omit: Some(Omit::Actions(&[Action::Restart])),
        reached: |_, state| took(state, Path::Deleted) && state.restarted,
    },
    Coverage {
        name: "a delete a restart cut leaves the copy, which a resend answers",
        on: fresh,
        omit: Some(Omit::Actions(&[Action::Restart])),
        reached: |_, state| {
            state.delete_cut && (took(state, Path::AnswersRoot) || took(state, Path::Copying))
        },
    },
    Coverage {
        name: "a late resend after a restart cut a delete between its rename and its tombstone refuses into deleted",
        on: fresh,
        omit: Some(Omit::Actions(&[Action::Restart])),
        reached: |_, state| took(state, Path::Deleted) && state.bury_cut,
    },
    Coverage {
        name: "a late resend after the sweep destroyed the entry refuses into deleted",
        on: fresh,
        omit: Some(Omit::Actions(&[Action::Expire])),
        reached: |_, state| took(state, Path::Deleted) && state.expired,
    },
    Coverage {
        name: "a resend answers the root the person restored",
        on: fresh,
        omit: Some(Omit::Actions(&[Action::Restore])),
        reached: |_, state| took(state, Path::AnswersRoot) && state.restored,
    },
    Coverage {
        name: "a copy refuses a copy of into it did not make",
        on: everywhere,
        omit: Some(Omit::Path(Path::Taken)),
        reached: |_, state| took(state, Path::Taken) && state.into.other(),
    },
    Coverage {
        name: "a late resend refuses an empty slot parent left after its copy was deleted",
        on: fresh,
        omit: Some(Omit::Actions(&[Action::ReuseEmpty])),
        reached: |_, state| {
            took(state, Path::Taken) && state.deleted && state.into == Into::EMPTY_OTHER
        },
    },
    Coverage {
        name: "a copy that never began is refused after the window",
        on: fresh,
        omit: Some(Omit::Path(Path::Expired)),
        reached: |_, state| took(state, Path::Expired) && state.last == Some(Action::Plugin),
    },
    Coverage {
        name: "a window that closes between a copy's reads and its next effect starts nothing",
        on: fresh,
        omit: Some(Omit::Path(Path::Expired)),
        reached: |_, state| took(state, Path::Expired) && state.last == Some(Action::Advance),
    },
    Coverage {
        name: "a call the daemon found closed makes no slot though the clock stepped back",
        on: fresh,
        omit: Some(Omit::Actions(&[Action::ClockBack])),
        reached: |_, state| took(state, Path::Expired) && state.plugin_open,
    },
];

impl CopyStep {
    /// `CopyBinding::may_start` read right before a new copy's effect, or with `!boundary`
    /// at the top of the `Absent` arm.
    fn may_start(&self, state: &State, open: bool, boundary: bool) -> bool {
        if self.broken.ignores_window || (boundary && self.broken.window_before_reads) {
            return true;
        }
        let verdict = open || self.broken.ignores_verdict;
        let clock = if self.broken.window_at_take {
            open
        } else {
            state.plugin_open
        };
        verdict && clock
    }

    /// `seed_volume` under a `CopyBinding` (seed.rs): `seed_progress`'s bound reads, then
    /// the arm it matched, up to its first effect that starts a copy.
    fn plugin(&self, state: &mut State, open: bool) {
        let checks_origin = !self.broken.ignores_origin;
        let next = match state.into {
            Into::Root(origin) if checks_origin && origin != Origin::Birth => Err(Path::Taken),
            Into::Root(origin) => {
                state.harmed_other |= origin == Origin::Other;
                state.answered = Some(origin);
                Err(Path::AnswersRoot)
            }
            Into::Slot { ours, record, fs } => {
                let mine = if self.broken.ignores_step {
                    record.is_none_or(|record| record == Origin::Birth)
                        && !(record.is_none() && fs.is_some())
                } else {
                    ours && record == Some(Origin::Birth)
                };
                if checks_origin && !mine {
                    Err(Path::Taken)
                } else if state.receiving {
                    Err(Path::Copying)
                } else if fs == Some(Fill::Point) {
                    let stamped = if self.broken.lands_point_origin {
                        Origin::Source
                    } else {
                        record.unwrap_or(Origin::Source)
                    };
                    state.harmed_other |= !ours;
                    state.remade |= state.deleted;
                    // The landing destroys the parent, which carries the step.
                    state.tombstone |= ours && !self.broken.no_tombstone;
                    state.into = Into::Root(stamped);
                    state.answered = Some(stamped);
                    Err(Path::Lands)
                } else {
                    state.harmed_other |= !ours;
                    Ok(Held::Plugin {
                        open,
                        at: if fs.is_some() {
                            Stage::Clear
                        } else {
                            Stage::Spawn
                        },
                        new: false,
                    })
                }
            }
            Into::Absent => {
                if !self.broken.no_trash_bury && !self.broken.no_tombstone {
                    state.tombstone |= state.trash.is_some_and(Into::carries_step);
                }
                if state.tombstone && !self.broken.no_tombstone {
                    state.falsely_deleted |= !state.deleted;
                    Err(Path::Deleted)
                } else if !self.may_start(state, open, false) {
                    Err(Path::Expired)
                } else {
                    state.tombstone |= self.broken.reserves_at_begin;
                    Ok(Held::Plugin {
                        open,
                        at: Stage::Make,
                        new: true,
                    })
                }
            }
        };
        match next {
            Ok(held) => state.held = held,
            Err(path) => {
                state.path = Some(path);
                state.held = Held::Idle;
            }
        }
    }

    /// The plugin call's next effect, `at`, after the window read right before it for a new
    /// copy.
    fn advance(&self, state: &mut State, (open, at, new): (bool, Stage, bool)) {
        if new && !self.may_start(state, open, true) {
            state.path = Some(Path::Expired);
            state.held = Held::Idle;
            return;
        }
        let next = match at {
            Stage::Make => {
                state.remade |= state.deleted;
                state.started_late |= !state.plugin_open;
                state.started_closed |= !open;
                state.made = true;
                state.into = Into::Slot {
                    ours: true,
                    record: Some(Origin::Birth),
                    fs: None,
                };
                Stage::Spawn
            }
            Stage::Clear if self.broken.destroys_parent_on_restart => {
                // `destroy -r` of the parent buries the step, then the arm makes it again.
                state.tombstone |= !self.broken.no_tombstone;
                state.into = Into::Absent;
                Stage::Make
            }
            Stage::Clear => {
                if let Into::Slot { ours, record, .. } = state.into {
                    state.into = Into::Slot {
                        ours,
                        record,
                        fs: None,
                    };
                }
                Stage::Spawn
            }
            Stage::Spawn => {
                state.remade |= state.deleted;
                state.started_late |= new && !state.plugin_open;
                state.started_closed |= new && !open;
                if let Into::Slot { ours, record, .. } = state.into {
                    state.into = Into::Slot {
                        ours,
                        record,
                        fs: Some(Fill::Partial),
                    };
                }
                state.receiving = true;
                state.path = Some(if new { Path::Starts } else { Path::StartsOver });
                state.held = Held::Idle;
                return;
            }
        };
        state.held = Held::Plugin {
            open,
            at: next,
            new,
        };
    }

    /// The call in hand ends, cut before the effect it was at, if any.
    fn cut(state: &mut State) {
        if let Held::Plugin { at, .. } = state.held {
            state.cut = Some(at);
        }
        state.held = Held::Idle;
    }

    fn step(&self, state: &mut State, action: Action) {
        state.path = None;
        match action {
            Action::Send => {
                state.sends += 1;
                state.on_wire += 1;
            }
            Action::Take => {
                state.on_wire -= 1;
                if !state.taken && !state.open && !self.broken.ignores_window {
                    return;
                }
                state.taken = true;
                state.held = Held::Took { open: state.open };
            }
            Action::Outage => {
                state.outage = true;
                Self::cut(state);
            }
            Action::Plugin => {
                if let Held::Took { open } = state.held {
                    self.plugin(state, open);
                }
            }
            Action::Advance => {
                if let Held::Plugin { open, at, new } = state.held {
                    self.advance(state, (open, at, new));
                }
            }
            Action::Received => {
                state.receiving = false;
                if let Into::Slot { ours, record, .. } = state.into {
                    state.into = Into::Slot {
                        ours,
                        record,
                        fs: Some(Fill::Point),
                    };
                }
            }
            Action::Restart => {
                state.restarted = true;
                Self::cut(state);
                state.receiving = false;
                state.delete_cut |= state.deleting && !state.renamed;
                state.bury_cut |= state.renamed;
                state.deleting = false;
                state.renamed = false;
                state.restarted_after_receipt |= matches!(
                    state.into,
                    Into::Slot {
                        fs: Some(Fill::Point),
                        ..
                    }
                );
            }
            Action::WindowCloses => {
                state.open = false;
                state.plugin_open = false;
            }
            Action::ClockBack => {
                state.plugin_open = true;
                state.clock_back = true;
            }
            Action::Delete => state.deleting = true,
            Action::Trashed => {
                state.deleted |= state.into.carries_birth();
                state.trash = Some(state.into);
                state.into = Into::Absent;
                state.renamed = true;
            }
            Action::Buried => {
                state.tombstone |=
                    state.trash.is_some_and(Into::carries_step) && !self.broken.no_tombstone;
                state.deleting = false;
                state.renamed = false;
            }
            Action::Restore => {
                if let Some(entry) = state.trash.take() {
                    state.two_roots |=
                        matches!((state.into, entry), (Into::Root(_), Into::Root(_)));
                    state.into = entry;
                    state.restored = true;
                }
            }
            Action::Expire => {
                state.tombstone |=
                    state.trash.is_some_and(Into::carries_step) && !self.broken.no_tombstone;
                state.trash = None;
                state.expired = true;
            }
            Action::Reuse => {
                state.reused = true;
                state.into = Into::Root(Origin::Other);
            }
            Action::ReuseEmpty => {
                state.reused = true;
                state.into = Into::EMPTY_OTHER;
            }
        }
    }

    fn omitted(&self, action: Action, after: &State) -> bool {
        match self.omit {
            Some(Omit::Actions(actions)) => actions.contains(&action),
            Some(Omit::Path(path)) => after.path == Some(path),
            None => false,
        }
    }
}

impl Model for CopyStep {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        let into = match self.scenario {
            Scenario::Fresh => Into::Absent,
            Scenario::TakenRoot => Into::Root(Origin::Other),
            Scenario::TakenSlot => Into::Slot {
                ours: false,
                record: Some(Origin::Other),
                fs: Some(Fill::Partial),
            },
            Scenario::EmptySlot => Into::EMPTY_OTHER,
        };
        vec![State {
            open: true,
            plugin_open: true,
            clock_back: false,
            taken: false,
            tombstone: false,
            into,
            receiving: false,
            held: Held::Idle,
            deleting: false,
            renamed: false,
            trash: None,
            on_wire: 0,
            sends: 0,
            restarted: false,
            outage: false,
            reused: false,
            made: false,
            deleted: false,
            delete_cut: false,
            bury_cut: false,
            restored: false,
            expired: false,
            remade: false,
            two_roots: false,
            cut: None,
            harmed_other: false,
            answered: None,
            started_late: false,
            started_closed: false,
            falsely_deleted: false,
            restarted_after_receipt: false,
            path: None,
            last: None,
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        if state.sends < MAX_SENDS {
            actions.push(Action::Send);
        }
        let in_plugin = matches!(state.held, Held::Plugin { .. });
        match state.held {
            Held::Idle if state.on_wire > 0 => actions.push(Action::Take),
            Held::Idle => {}
            // The plugin's call waits for the mutation a delete holds.
            Held::Took { .. } if !state.deleting => actions.push(Action::Plugin),
            Held::Took { .. } => {}
            Held::Plugin { .. } => actions.push(Action::Advance),
        }
        if state.held != Held::Idle && !state.outage {
            actions.push(Action::Outage);
        }
        if state.receiving {
            actions.push(Action::Received);
        }
        if !state.restarted {
            actions.push(Action::Restart);
        }
        if state.open {
            actions.push(Action::WindowCloses);
        } else if !state.clock_back {
            actions.push(Action::ClockBack);
        }
        // Volume.Remove refuses a Volume a receive is writing, and waits for the mutation.
        if state.into != Into::Absent
            && state.trash.is_none()
            && !state.receiving
            && !state.deleted
            && !state.deleting
            && !in_plugin
        {
            actions.push(Action::Delete);
        }
        if state.deleting {
            actions.push(if state.renamed {
                Action::Buried
            } else {
                Action::Trashed
            });
        }
        // Restore and the sweep take the same mutation; a restore refuses a name with a
        // copy, and a slot whose parent still carries its step.
        if let Some(entry) = state.trash
            && !state.deleting
            && !in_plugin
        {
            let free = state.into == Into::Absent || self.broken.restores_over_taken;
            let unlanded = matches!(entry, Into::Slot { ours: true, .. });
            if free && !state.receiving && (!unlanded || self.broken.restores_unlanded) {
                actions.push(Action::Restore);
            }
            actions.push(Action::Expire);
        }
        if state.into == Into::Absent && state.deleted && !state.reused {
            actions.push(Action::Reuse);
            actions.push(Action::ReuseEmpty);
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
            Property::always(NEVER_REMADE, |_, state: &State| !state.remade),
            Property::always(KEEPS_OTHER, |_, state: &State| !state.harmed_other),
            Property::always(ANSWERS_BIRTH, |_, state: &State| {
                state.answered.is_none_or(|origin| origin == Origin::Birth)
            }),
            Property::always(STARTS_IN_WINDOW, |_, state: &State| !state.started_late),
            Property::always(CLOSED_STAYS_CLOSED, |_, state: &State| {
                !state.started_closed
            }),
            Property::always(DELETED_ONLY_AFTER_DELETE, |_, state: &State| {
                !state.falsely_deleted
            }),
            Property::always(ONE_ROOT, |_, state: &State| !state.two_roots),
        ];
        properties.extend(harness::sometimes(COVERAGE, self));
        properties
    }
}

/// `action` in the code's words.
fn reads_as(action: Action, after: &State) -> String {
    match action {
        Action::Send => "Cloud sends SeedCopy; it may wait in transit".to_owned(),
        Action::Take => format!(
            "A takes a request under the switch lock: take_copy (volume_copy.rs seed_copy); taken={} held={:?}",
            after.taken, after.held
        ),
        Action::Outage => format!(
            "the call ends: refused before the plugin acts, lost to a restart, or a zfs call fails; cut={:?}",
            after.cut
        ),
        Action::Plugin => format!(
            "the plugin's bound seed (seed.rs seed_volume) reads into: {:?} held={:?} tombstone={}",
            after.path, after.held, after.tombstone
        ),
        Action::Advance => format!(
            "the plugin reads the window and makes its next effect: {:?} held={:?}",
            after.path, after.held
        ),
        Action::Received => "the receive exits 0: fs holds the point".to_owned(),
        Action::Restart => format!(
            "the daemon and the plugin restart: the receive and the held request end; cut={:?}",
            after.cut
        ),
        Action::WindowCloses => "the step's window closes".to_owned(),
        Action::ClockBack => "the plugin's wall clock steps back inside the window".to_owned(),
        Action::Delete => "the user deletes into: discard writes the trash marks".to_owned(),
        Action::Trashed => format!(
            "discard renames into to the Trash (trash.rs discard); trash={:?}",
            after.trash
        ),
        Action::Buried => format!(
            "discard buries the step the entry carries; tombstone={}",
            after.tombstone
        ),
        Action::Restore => format!(
            "the person restores the entry (trash.rs restore_trash); into={:?}",
            after.into
        ),
        Action::Expire => format!(
            "the sweep buries and destroys the entry (trash.rs sweep_trash); tombstone={}",
            after.tombstone
        ),
        Action::Reuse => "another approval makes a Volume named into".to_owned(),
        Action::ReuseEmpty => {
            "an interrupted mirror pull leaves an empty slot parent of into".to_owned()
        }
    }
}

impl Narrated for CopyStep {
    fn story(&self, path: &[(State, Option<Action>)]) -> String {
        let mut story = format!("{:?} {:?}\n", self.scenario, self.broken);
        let afters = path.iter().skip(1).map(|(state, _)| state);
        for (number, ((_, action), after)) in path.iter().zip(afters).enumerate() {
            let Some(action) = action else { continue };
            let _ = write!(
                story,
                "{}. {}\n    open={} plugin_open={} into={:?} receiving={} on_wire={}\n",
                number + 1,
                reads_as(*action, after),
                after.open,
                after.plugin_open,
                after.into,
                after.receiving,
                after.on_wire,
            );
        }
        story
    }
}

#[cfg(test)]
mod tests {
    use stateright::Checker;

    use super::*;
    use crate::harness::{breaks, check, last};

    fn fixed(scenario: Scenario) -> CopyStep {
        CopyStep {
            scenario,
            broken: Broken::default(),
            omit: None,
        }
    }

    fn broken(scenario: Scenario, broken: Broken) -> CopyStep {
        CopyStep {
            broken,
            ..fixed(scenario)
        }
    }

    /// The step that broke the property was an effect of a new copy into an absent `into`,
    /// which the window guards, not of one started over in its own slot.
    fn a_new_copy_broke(path: &[(State, Option<Action>)]) -> bool {
        path.iter()
            .rev()
            .nth(1)
            .is_some_and(|(before, _)| matches!(before.held, Held::Plugin { new: true, .. }))
    }

    #[test]
    fn a_copy_step_holds_in_every_scenario() {
        for scenario in ALL {
            let checker = check(fixed(*scenario));
            println!("{scenario:?}: {} states", checker.unique_state_count());
            checker.assert_properties();
        }
    }

    #[test]
    fn every_copy_sometimes_needs_what_it_names() {
        for scenario in ALL {
            let model = fixed(*scenario);
            for cut in harness::assert_omissions(COVERAGE, &model, |model, omit| CopyStep {
                omit: Some(omit),
                ..*model
            }) {
                println!("{scenario:?} {cut}");
            }
        }
    }

    /// With no tombstone, nothing on A says `into` was deleted: a resend inside the window
    /// finds it absent and makes it again.
    #[test]
    fn a_copy_never_remakes_its_deleted_volume_without_the_tombstone() {
        let path = breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    no_tombstone: true,
                    ..Broken::default()
                },
            ),
            NEVER_REMADE,
        );
        assert!(
            harness::took_action(&path, Action::Trashed),
            "into was deleted"
        );
    }

    /// Reserved before the slot exists, a step a stop cut before ZFS made the slot reads as
    /// deleted, and the resend inside the window is refused though nothing was made.
    #[test]
    fn a_copy_cut_before_its_slot_was_made_is_not_refused_as_deleted() {
        let path = breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    reserves_at_begin: true,
                    ..Broken::default()
                },
            ),
            DELETED_ONLY_AFTER_DELETE,
        );
        assert_eq!(last(&path).cut, Some(Stage::Make), "cut before the make");
    }

    /// Starting over by destroying its own slot buries the step, so a stop before the slot
    /// is made again leaves the copy refused as deleted.
    #[test]
    fn a_copy_starts_over_inside_the_slot_it_keeps() {
        let path = breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    destroys_parent_on_restart: true,
                    ..Broken::default()
                },
            ),
            DELETED_ONLY_AFTER_DELETE,
        );
        assert!(last(&path).restarted, "a restart killed the first receive");
    }

    /// An empty slot parent passes for the step's own when the step it carries is not
    /// read: a resend starts its copy over in another's parent, one there from the start
    /// or one left after the step's own Volume was deleted.
    #[test]
    fn a_copy_restarts_only_a_slot_carrying_its_step() {
        for scenario in [Scenario::EmptySlot, Scenario::Fresh] {
            let path = breaks(
                broken(
                    scenario,
                    Broken {
                        ignores_step: true,
                        ..Broken::default()
                    },
                ),
                KEEPS_OTHER,
            );
            assert_eq!(
                last(&path).deleted,
                scenario == Scenario::Fresh,
                "{scenario:?}"
            );
        }
    }

    #[test]
    fn a_copy_checks_the_origin_of_into() {
        for scenario in [Scenario::TakenRoot, Scenario::TakenSlot] {
            breaks(
                broken(
                    scenario,
                    Broken {
                        ignores_origin: true,
                        ..Broken::default()
                    },
                ),
                KEEPS_OTHER,
            );
        }
    }

    /// The window guards a new copy: with it ignored, a copy makes its slot or starts its
    /// receive after the window closed.
    #[test]
    fn a_copy_starts_only_inside_its_window() {
        let path = breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    ignores_window: true,
                    ..Broken::default()
                },
            ),
            STARTS_IN_WINDOW,
        );
        assert!(a_new_copy_broke(&path), "a new copy started late");
    }

    /// The window read when the daemon took the request is stale by the time the plugin
    /// acts: a copy taken inside the window starts after it closed.
    #[test]
    fn a_copy_reads_its_window_where_it_starts() {
        let path = breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    window_at_take: true,
                    ..Broken::default()
                },
            ),
            STARTS_IN_WINDOW,
        );
        assert!(
            harness::took_action(&path, Action::WindowCloses),
            "the window closed after the take"
        );
        assert!(a_new_copy_broke(&path), "a new copy started late");
    }

    /// Read once before the plugin's reads and preparation, the window closes before the
    /// slot is made or the receive spawned, and the copy starts late.
    #[test]
    fn a_copy_reads_its_window_right_before_each_effect() {
        let path = breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    window_before_reads: true,
                    ..Broken::default()
                },
            ),
            STARTS_IN_WINDOW,
        );
        assert!(
            harness::took_action(&path, Action::Advance),
            "the window closed inside the plugin's call"
        );
        assert!(a_new_copy_broke(&path), "a new copy started late");
    }

    /// The daemon found the window closed, so the call may only finish a copy underway;
    /// a plugin that reads only its own clock makes a new copy's slot once the clock steps
    /// back.
    #[test]
    fn a_copy_the_daemon_found_closed_starts_nothing_whatever_the_clock() {
        let path = breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    ignores_verdict: true,
                    ..Broken::default()
                },
            ),
            CLOSED_STAYS_CLOSED,
        );
        assert!(
            harness::took_action(&path, Action::ClockBack),
            "the plugin's clock stepped back"
        );
        assert!(
            a_new_copy_broke(&path),
            "a new copy started after the window"
        );
    }

    /// A delete stopped between its rename and its tombstone leaves the step unburied: a
    /// seed that reads its tombstone without burying the Trash's steps makes the deleted
    /// Volume again.
    #[test]
    fn a_copy_buries_the_trash_before_it_reads_its_tombstone() {
        let path = breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    no_trash_bury: true,
                    ..Broken::default()
                },
            ),
            NEVER_REMADE,
        );
        assert!(
            last(&path).bury_cut,
            "a restart came between rename and tombstone"
        );
    }

    /// A slot still carrying its step is a copy that never landed; put back, its seed
    /// starts it over past the window and lands the deleted Volume.
    #[test]
    fn a_restore_refuses_a_slot_that_still_carries_its_step() {
        let path = breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    restores_unlanded: true,
                    ..Broken::default()
                },
            ),
            NEVER_REMADE,
        );
        assert!(
            harness::took_action(&path, Action::Restore),
            "the slot came back"
        );
    }

    #[test]
    fn a_restore_refuses_a_name_that_has_a_copy() {
        breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    restores_over_taken: true,
                    ..Broken::default()
                },
            ),
            ONE_ROOT,
        );
    }

    #[test]
    fn a_copy_lands_with_the_birth_its_slot_records() {
        breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    lands_point_origin: true,
                    ..Broken::default()
                },
            ),
            ANSWERS_BIRTH,
        );
    }
}
