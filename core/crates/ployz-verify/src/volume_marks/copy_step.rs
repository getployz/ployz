//! A seed a person's Copy step admits, on the Machine A that holds the source's Writer.
//! Cloud resends `SeedCopy` for the new Volume `into` until A answers a root, and any
//! request can arrive late. A takes each under the switch lock in three steps
//! (`LocalMachine::seed_copy`, machine/local_machine/volume_copy.rs): it takes the step in
//! its approvals record and records that a copy may begin (`ConsumptionLog::take_copy` and
//! `copy_began`, consumption.rs), runs the plugin's bound seed in one step under
//! `admit_mutation` (`VolumeStorage::seed_volume` with a `CopyBinding`, seed.rs), and takes
//! `began` back only when the plugin refused before any effect (`began_nothing`). A
//! receive runs outside both locks. The daemon can restart between any two steps, which
//! kills the receive and loses the request in hand, but keeps the record and the datasets.
//! The step's window closes once. The user can delete `into`, and another approval can
//! then make a Volume of the same name, which carries another origin.
//!
//! The properties: once a copy began and `into` was deleted, no copy makes it again; a
//! copy never answers, lands on or destroys a copy of `into` that carries another origin;
//! every root the copy answers carries the step's birth; and a copy starts, or starts over,
//! only inside the step's window.
//!
//! Out of scope: a receive that fails (its answer is the same `Failed` arm the unbound seed
//! has), a failure between the zfs calls of one handler, Docker adopting the root, the
//! later runs `volume_marks` models, and the bundle's signature, which ployz-core's
//! `authority::copy` tests cover.

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
    /// A slot: the origin its parent records (`ployz:pull-origin`), and its `fs`.
    Slot {
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

    fn other(self) -> bool {
        matches!(
            self,
            Self::Root(Origin::Other)
                | Self::Slot {
                    record: Some(Origin::Other),
                    ..
                }
        )
    }
}

/// The step in `restart-approvals.json`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Log {
    Untaken,
    Taken { began: bool },
}

/// The one request A holds under the switch lock.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Held {
    Idle,
    /// Taken; `began` is what the record said before this request.
    Took {
        began: bool,
    },
    /// The plugin answered; `refused` when it refused before any effect.
    Answered {
        began: bool,
        refused: bool,
    },
}

/// The branch the plugin's bound seed took (`seed_progress` and `seed_volume`, seed.rs).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Path {
    /// A root or slot of `into` that does not carry the birth: `SeedProgress::Taken`.
    Taken,
    /// A root carrying the birth: answered.
    AnswersRoot,
    /// A receive still running into a slot that carries the birth: `Copying`.
    Copying,
    /// A complete slot: `Copied::land` stamps the record and renames it to the root.
    Lands,
    /// A slot holding no complete point: `Stale`, cleared and copied again.
    StartsOver,
    /// No `into`: a new slot, record and receive (`copy_point`).
    Starts,
    /// No `into` after a copy began: `CopyBinding::may_begin` refuses it as deleted.
    Deleted,
    /// `Stale` or `Absent` with the window closed: `may_restart` refuses it as expired.
    Expired,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// Cloud sends SeedCopy; the request may wait in transit for any time.
    Send,
    /// A takes a request under the switch lock: verify, `take_copy`, `copy_began(true)`.
    Take,
    /// The plugin's bound seed, one step under `admit_mutation`.
    Plugin,
    /// `seed_copy` after the plugin answers: `copy_began(false)` on an early refusal.
    Settle,
    /// The receive task exits 0.
    Received,
    /// The daemon restarts.
    Restart,
    /// The step's window closes on A's floored clock.
    WindowCloses,
    /// The user deletes `into` (Volume.Remove, under `admit_mutation`).
    Delete,
    /// Another approval makes a Volume named `into`, which carries another origin.
    Reuse,
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
}

const ALL: &[Scenario] = &[Scenario::Fresh, Scenario::TakenRoot, Scenario::TakenSlot];

const MAX_SENDS: u8 = 3;

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    open: bool,
    log: Log,
    into: Into,
    receiving: bool,
    held: Held,
    on_wire: u8,
    sends: u8,
    restarted: bool,
    reused: bool,
    /// `into` was deleted while it carried the birth, so a copy had begun.
    deleted: bool,
    /// A step destroyed or landed on a copy carrying another origin.
    harmed_other: bool,
    /// The origin of the last root the plugin answered.
    answered: Option<Origin>,
    /// A copy started, or started over, with the window closed.
    started_late: bool,
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
    /// `seed_copy` records `began` only after the plugin answers a copy, not before it
    /// calls the plugin.
    began_after_plugin: bool,
    /// The plugin's `Absent` arm ignores `began` (`CopyBinding::may_begin`).
    forgets_began: bool,
    /// `seed_progress` adopts any root or slot of `into`, whatever origin it carries.
    ignores_origin: bool,
    /// `take_copy` takes a new step, and `may_restart` starts a copy, whatever the window.
    ignores_window: bool,
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
const KEEPS_OTHER: &str = "a copy never answers, lands on or destroys another origin's into";
const ANSWERS_BIRTH: &str = "every root the copy answers carries the step's birth";
const STARTS_IN_WINDOW: &str = "a copy starts, or starts over, only inside the step's window";

/// Each branch the bounds reach, once. `every_sometimes_needs_what_it_names` takes each
/// row's `omit` out and expects its `name` unreached.
const COVERAGE: &[Coverage] = &[
    Coverage {
        name: "a copy starts in a new slot recording the birth",
        on: fresh,
        omit: Some(Omit::Path(Path::Starts)),
        reached: |_, state| took(state, Path::Starts),
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
        name: "a copy refuses into another approval made",
        on: everywhere,
        omit: Some(Omit::Path(Path::Taken)),
        reached: |_, state| took(state, Path::Taken) && state.into.other(),
    },
    Coverage {
        name: "a copy that never began is refused after the window",
        on: fresh,
        omit: Some(Omit::Path(Path::Expired)),
        reached: |_, state| took(state, Path::Expired),
    },
    Coverage {
        name: "a refusal before any effect takes began back",
        on: everywhere,
        omit: Some(Omit::Actions(&[Action::Settle])),
        reached: |_, state| {
            state.last == Some(Action::Settle) && state.log == Log::Taken { began: false }
        },
    },
];

impl CopyStep {
    /// `seed_volume` under a `CopyBinding` (seed.rs): `seed_progress`'s bound reads, then
    /// the arm it matched. Returns whether it refused before any effect.
    fn plugin(&self, state: &mut State, began: bool) -> bool {
        let checks_origin = !self.broken.ignores_origin;
        let window = state.open || self.broken.ignores_window;
        let (path, refused) = match state.into {
            Into::Root(origin) if checks_origin && origin != Origin::Birth => (Path::Taken, true),
            Into::Root(origin) => {
                state.harmed_other |= origin == Origin::Other;
                state.answered = Some(origin);
                (Path::AnswersRoot, false)
            }
            Into::Slot { record, fs } => {
                let foreign = record.is_some_and(|record| record != Origin::Birth)
                    || (record.is_none() && fs.is_some());
                if checks_origin && foreign {
                    (Path::Taken, true)
                } else if state.receiving {
                    // `take_failure` answers Busy; a slot not bound to the birth is Taken.
                    if record.is_some() || !checks_origin {
                        (Path::Copying, false)
                    } else {
                        (Path::Taken, true)
                    }
                } else if fs == Some(Fill::Point) {
                    let stamped = if self.broken.lands_point_origin {
                        Origin::Source
                    } else {
                        record.unwrap_or(Origin::Source)
                    };
                    state.harmed_other |= record == Some(Origin::Other);
                    state.into = Into::Root(stamped);
                    state.answered = Some(stamped);
                    (Path::Lands, false)
                } else if !window {
                    (Path::Expired, true)
                } else {
                    state.harmed_other |= record == Some(Origin::Other);
                    self.start(state);
                    (Path::StartsOver, false)
                }
            }
            Into::Absent if began && !self.broken.forgets_began => (Path::Deleted, true),
            Into::Absent if !window => (Path::Expired, true),
            Into::Absent => {
                self.start(state);
                (Path::Starts, false)
            }
        };
        state.path = Some(path);
        refused
    }

    /// `copy_point`: a slot whose parent records the birth, and a receive into its `fs`.
    fn start(&self, state: &mut State) {
        state.started_late |= !state.open;
        state.into = Into::Slot {
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
                let began = match state.log {
                    Log::Taken { began } => began,
                    Log::Untaken if !state.open && !self.broken.ignores_window => return,
                    Log::Untaken => false,
                };
                if !began && !self.broken.began_after_plugin {
                    state.log = Log::Taken { began: true };
                } else {
                    state.log = Log::Taken { began };
                }
                state.held = Held::Took { began };
            }
            Action::Plugin => {
                let Held::Took { began } = state.held else {
                    return;
                };
                let refused = self.plugin(state, began);
                state.held = Held::Answered { began, refused };
            }
            Action::Settle => {
                let Held::Answered { began, refused } = state.held else {
                    return;
                };
                // Recorded before the plugin, `began` is taken back on an early refusal;
                // under `began_after_plugin` it is first recorded here.
                if !began {
                    state.log = Log::Taken { began: !refused };
                }
                state.held = Held::Idle;
            }
            Action::Received => {
                state.receiving = false;
                if let Into::Slot { record, .. } = state.into {
                    state.into = Into::Slot {
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
                record: Some(Origin::Other),
                fs: Some(Fill::Partial),
            },
        };
        vec![State {
            open: true,
            log: Log::Untaken,
            into,
            receiving: false,
            held: Held::Idle,
            on_wire: 0,
            sends: 0,
            restarted: false,
            reused: false,
            deleted: false,
            harmed_other: false,
            answered: None,
            started_late: false,
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
            Held::Took { .. } => actions.push(Action::Plugin),
            Held::Answered { .. } => actions.push(Action::Settle),
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
            "A takes a request under the switch lock: take_copy, then copy_began(true) when it had not begun (volume_copy.rs seed_copy); record {:?}",
            after.log
        ),
        Action::Plugin => format!(
            "the plugin's bound seed (seed.rs seed_volume) takes {:?}",
            after.path
        ),
        Action::Settle => format!(
            "seed_copy settles began after the answer; record {:?}",
            after.log
        ),
        Action::Received => "the receive exits 0: fs holds the point".to_owned(),
        Action::Restart => "the daemon restarts: the receive and the held request end".to_owned(),
        Action::WindowCloses => "the step's window closes".to_owned(),
        Action::Delete => "the user deletes into".to_owned(),
        Action::Reuse => "another approval makes a Volume named into".to_owned(),
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

    /// Recorded only after the plugin answers, `began` is lost to a restart while the
    /// plugin works: the user deletes the new Volume, and a resend still inside the window
    /// makes it again.
    #[test]
    fn a_copy_records_began_before_the_plugin_may_begin() {
        let path = breaks(
            broken(
                Scenario::Fresh,
                Broken {
                    began_after_plugin: true,
                    ..Broken::default()
                },
            ),
            NEVER_REMADE,
        );
        assert!(last(&path).restarted, "a restart lost began");
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
