//! A seed a person's Copy step admits, on the Machine A that holds the source's Writer.
//! Cloud resends `SeedCopy` for the new Volume `into` until A answers a root, and any
//! request can arrive late. A takes each under the switch lock in two steps
//! (`LocalMachine::seed_copy`, machine/local_machine/volume_copy.rs): it takes the step in
//! its approvals record (`ConsumptionLog::take_copy`, consumption.rs), which admits a step
//! not taken yet only while its window holds, then calls the plugin. The call can end
//! before the plugin does anything: the socket refuses it, the daemon restarts before it
//! sends, the plugin's mutation refuses or times out, or a read under it fails. Otherwise
//! the plugin's bound seed runs in one step under `admit_mutation` (`VolumeStorage::
//! seed_volume` with a `CopyBinding`, seed.rs). It reads the window on its own clock
//! there, reserves the step in a file of its own before it makes a slot, and makes the slot
//! carrying the step and recording the birth. A receive runs outside both locks. The daemon
//! can restart between any two steps, which kills the receive and loses the request in
//! hand, but keeps the record, the reservation and the datasets. The step's window closes
//! once. The user can delete `into`, and another approval can then make a Volume of the
//! same name, which carries another origin, or leave an empty slot parent of it, as an
//! interrupted mirror pull does.
//!
//! The properties: once a copy began and `into` was deleted, no copy makes it again; a
//! copy never answers, lands on or destroys a copy of `into` it did not make; every root
//! the copy answers carries the step's birth; a copy starts, or starts over, only inside
//! the step's window; and a copy is refused as deleted only after one began under its
//! step, so a call that ended before the plugin acted never costs the approval.
//!
//! Out of scope: a receive that fails (its answer is the same `Failed` arm the unbound seed
//! has), a failure between the zfs calls of one handler and between the reservation and
//! the slot (it refuses a resend as deleted, which asks for a new approval and makes
//! nothing), Docker adopting the root, the later runs `volume_marks` models, and the
//! bundle's signature, which ployz-core's `authority::copy` tests cover.

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
    /// A root, with the origin it carries (`ployz:origin`).
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

/// The one request A holds under the switch lock, once taken.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Held {
    Idle,
    /// Taken. `open` is the window when A took it, and `began` what the daemon's own
    /// record said then (read only under `Broken::reserves_in_daemon`).
    Took {
        open: bool,
        began: bool,
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
    /// The step's complete slot: `Copied::land` stamps the record and renames it.
    Lands,
    /// The step's slot holding no complete point: `Stale`, cleared and copied again.
    StartsOver,
    /// No `into` and no reservation: reserved, then a new slot and receive (`copy_point`).
    Starts,
    /// No `into` and the step reserved: refused as deleted.
    Deleted,
    /// `Stale` or `Absent` with the window closed: `CopyBinding::may_start` refuses it.
    Expired,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// Cloud sends SeedCopy; the request may wait in transit for any time.
    Send,
    /// A takes a request under the switch lock: verify, `take_copy`.
    Take,
    /// The call ends before the plugin acts: refused, lost to a restart before it was sent,
    /// or refused under the plugin's mutation before any effect.
    Outage,
    /// The plugin's bound seed, one step under `admit_mutation`.
    Plugin,
    /// The receive task exits 0.
    Received,
    /// The daemon restarts.
    Restart,
    /// The step's window closes.
    WindowCloses,
    /// The user deletes `into` (Volume.Remove, under `admit_mutation`).
    Delete,
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
    open: bool,
    /// The step is in `restart-approvals.json`.
    taken: bool,
    /// The daemon's own record that a copy began (`Broken::reserves_in_daemon` only).
    began: bool,
    /// The plugin's reservation of the step.
    reserved: bool,
    into: Into,
    receiving: bool,
    held: Held,
    on_wire: u8,
    sends: u8,
    restarted: bool,
    /// A call ended before the plugin acted.
    outage: bool,
    reused: bool,
    /// A slot was made under the step.
    made: bool,
    /// `into` was deleted while it carried the birth, so a copy had begun.
    deleted: bool,
    /// A step destroyed or landed on a copy of `into` it did not make.
    harmed_other: bool,
    /// The origin of the last root the plugin answered.
    answered: Option<Origin>,
    /// A copy started, or started over, with the window closed.
    started_late: bool,
    /// A copy was refused as deleted though none began under its step.
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
    /// The daemon records that a copy began before it calls the plugin, and the plugin's
    /// `Absent` arm reads that record, as the first cut of this code did: a call that ends
    /// before the plugin acts leaves it set.
    reserves_in_daemon: bool,
    /// The plugin's `Absent` arm ignores the reservation.
    forgets_began: bool,
    /// A slot is the step's when its record is the birth or it records nothing and holds
    /// no `fs`, whatever step its parent carries.
    ignores_step: bool,
    /// `seed_progress` adopts any root or slot of `into`, whatever it carries.
    ignores_origin: bool,
    /// `take_copy` takes a new step, and `may_start` starts a copy, whatever the window.
    ignores_window: bool,
    /// The plugin starts a copy under the window as the daemon read it when it took the
    /// request, not as it stands under the mutation.
    window_at_take: bool,
    /// `Copied::land` stamps the point's own origin, not the slot's record.
    lands_point_origin: bool,
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
    state.last == Some(Action::Plugin) && state.path == Some(path)
}

fn fresh(model: &CopyStep) -> bool {
    model.scenario == Scenario::Fresh
}

const NEVER_REMADE: &str = "once a copy began and into was deleted, no copy makes it again";
const KEEPS_OTHER: &str =
    "a copy never answers, lands on or destroys a copy of into it did not make";
const ANSWERS_BIRTH: &str = "every root the copy answers carries the step's birth";
const STARTS_IN_WINDOW: &str = "a copy starts, or starts over, only inside the step's window";
const DELETED_ONLY_AFTER_BEGAN: &str = "a copy is refused as deleted only after one began";

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
        reached: |_, state| took(state, Path::Starts) && state.outage,
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
        name: "a copy killed by a restart starts over inside the window",
        on: fresh,
        omit: Some(Omit::Path(Path::StartsOver)),
        reached: |_, state| took(state, Path::StartsOver),
    },
    Coverage {
        name: "a late resend refuses into deleted after its copy began",
        on: fresh,
        omit: Some(Omit::Path(Path::Deleted)),
        reached: |_, state| took(state, Path::Deleted) && state.deleted,
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
        reached: |_, state| took(state, Path::Expired),
    },
];

impl CopyStep {
    /// `seed_volume` under a `CopyBinding` (seed.rs): `seed_progress`'s bound reads, then
    /// the arm it matched. Returns whether it refused before any effect.
    fn plugin(&self, state: &mut State, (open_at_take, began_at_take): (bool, bool)) -> bool {
        let checks_origin = !self.broken.ignores_origin;
        let window = if self.broken.window_at_take {
            open_at_take
        } else {
            state.open
        } || self.broken.ignores_window;
        let (path, refused) = match state.into {
            Into::Root(origin) if checks_origin && origin != Origin::Birth => (Path::Taken, true),
            Into::Root(origin) => {
                state.harmed_other |= origin == Origin::Other;
                state.answered = Some(origin);
                (Path::AnswersRoot, false)
            }
            Into::Slot { ours, record, fs } => {
                let mine = if self.broken.ignores_step {
                    record.is_none_or(|record| record == Origin::Birth)
                        && !(record.is_none() && fs.is_some())
                } else {
                    ours && record == Some(Origin::Birth)
                };
                if checks_origin && !mine {
                    (Path::Taken, true)
                } else if state.receiving {
                    (Path::Copying, false)
                } else if fs == Some(Fill::Point) {
                    let stamped = if self.broken.lands_point_origin {
                        Origin::Source
                    } else {
                        record.unwrap_or(Origin::Source)
                    };
                    state.harmed_other |= !ours;
                    state.into = Into::Root(stamped);
                    state.answered = Some(stamped);
                    (Path::Lands, false)
                } else if !window {
                    (Path::Expired, true)
                } else {
                    state.harmed_other |= !ours;
                    self.start(state);
                    (Path::StartsOver, false)
                }
            }
            Into::Absent => {
                let began = if self.broken.reserves_in_daemon {
                    began_at_take
                } else {
                    state.reserved
                };
                if began && !self.broken.forgets_began {
                    state.falsely_deleted |= !state.made;
                    (Path::Deleted, true)
                } else if !window {
                    (Path::Expired, true)
                } else {
                    state.reserved = true;
                    self.start(state);
                    (Path::Starts, false)
                }
            }
        };
        state.path = Some(path);
        refused
    }

    /// `copy_point`: a slot whose parent carries the step and records the birth, and a
    /// receive into its `fs`.
    fn start(&self, state: &mut State) {
        state.started_late |= !state.open;
        state.made = true;
        state.into = Into::Slot {
            ours: true,
            record: Some(Origin::Birth),
            fs: Some(Fill::Partial),
        };
        state.receiving = true;
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
                let began = state.began;
                if self.broken.reserves_in_daemon {
                    state.began = true;
                }
                state.held = Held::Took {
                    open: state.open,
                    began,
                };
            }
            Action::Outage => {
                state.outage = true;
                state.held = Held::Idle;
            }
            Action::Plugin => {
                let Held::Took { open, began } = state.held else {
                    return;
                };
                let refused = self.plugin(state, (open, began));
                // The first cut took its record back only on a refusal the plugin gave.
                if self.broken.reserves_in_daemon && refused && !began {
                    state.began = false;
                }
                state.held = Held::Idle;
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
                state.held = Held::Idle;
                state.receiving = false;
                state.restarted_after_receipt |= matches!(
                    state.into,
                    Into::Slot {
                        fs: Some(Fill::Point),
                        ..
                    }
                );
            }
            Action::WindowCloses => state.open = false,
            Action::Delete => {
                state.deleted |= state.into.carries_birth();
                state.into = Into::Absent;
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
            taken: false,
            began: false,
            reserved: false,
            into,
            receiving: false,
            held: Held::Idle,
            on_wire: 0,
            sends: 0,
            restarted: false,
            outage: false,
            reused: false,
            made: false,
            deleted: false,
            harmed_other: false,
            answered: None,
            started_late: false,
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
        match state.held {
            Held::Idle if state.on_wire > 0 => actions.push(Action::Take),
            Held::Idle => {}
            Held::Took { .. } => {
                actions.push(Action::Plugin);
                if !state.outage {
                    actions.push(Action::Outage);
                }
            }
        }
        if state.receiving {
            actions.push(Action::Received);
        }
        if !state.restarted {
            actions.push(Action::Restart);
        }
        if state.open {
            actions.push(Action::WindowCloses);
        }
        // Volume.Remove refuses a Volume a receive is writing.
        if state.into != Into::Absent && !state.receiving && !state.deleted {
            actions.push(Action::Delete);
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
            Property::always(NEVER_REMADE, |_, state: &State| {
                !(state.deleted && state.into.carries_birth())
            }),
            Property::always(KEEPS_OTHER, |_, state: &State| !state.harmed_other),
            Property::always(ANSWERS_BIRTH, |_, state: &State| {
                state.answered.is_none_or(|origin| origin == Origin::Birth)
            }),
            Property::always(STARTS_IN_WINDOW, |_, state: &State| !state.started_late),
            Property::always(DELETED_ONLY_AFTER_BEGAN, |_, state: &State| {
                !state.falsely_deleted
            }),
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
            "A takes a request under the switch lock: take_copy (volume_copy.rs seed_copy); taken={} began={}",
            after.taken, after.began
        ),
        Action::Outage => {
            "the call ends before the plugin acts: refused, lost to a restart, or refused under its mutation".to_owned()
        }
        Action::Plugin => format!(
            "the plugin's bound seed (seed.rs seed_volume) takes {:?}; reserved={}",
            after.path, after.reserved
        ),
        Action::Received => "the receive exits 0: fs holds the point".to_owned(),
        Action::Restart => "the daemon restarts: the receive and the held request end".to_owned(),
        Action::WindowCloses => "the step's window closes".to_owned(),
        Action::Delete => "the user deletes into".to_owned(),
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
                "{}. {}\n    open={} into={:?} receiving={} held={:?} on_wire={}\n",
                number + 1,
                reads_as(*action, after),
                after.open,
                after.into,
                after.receiving,
                after.held,
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

    /// Recorded by the daemon before it calls the plugin, a copy's start outlives a call
    /// that ended before the plugin acted, and the resend inside the window is refused as
    /// deleted though nothing was made.
    #[test]
    fn a_copy_reserves_its_step_only_where_the_slot_is_made() {
        let path = breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    reserves_in_daemon: true,
                    ..Broken::default()
                },
            ),
            DELETED_ONLY_AFTER_BEGAN,
        );
        assert!(last(&path).outage, "a call ended before the plugin acted");
    }

    #[test]
    fn a_copy_refuses_its_deleted_volume() {
        breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    forgets_began: true,
                    ..Broken::default()
                },
            ),
            NEVER_REMADE,
        );
    }

    /// An empty slot parent passes for the step's own when the step it carries is not
    /// read: a resend clears another's parent, and after a delete makes the Volume again.
    #[test]
    fn a_copy_restarts_only_a_slot_carrying_its_step() {
        breaks(
            broken(
                Scenario::EmptySlot,
                Broken {
                    ignores_step: true,
                    ..Broken::default()
                },
            ),
            KEEPS_OTHER,
        );
        breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    ignores_step: true,
                    ..Broken::default()
                },
            ),
            NEVER_REMADE,
        );
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

    #[test]
    fn a_copy_starts_only_inside_its_window() {
        breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    ignores_window: true,
                    ..Broken::default()
                },
            ),
            STARTS_IN_WINDOW,
        );
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
