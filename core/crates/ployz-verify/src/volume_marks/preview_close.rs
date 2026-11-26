//! A preview's Close on its home Server A, against a delayed Boot, a storage arrival and a
//! daemon crash. The preview runs one Container on two Volumes: `own`, whose root carries
//! the origin the preview's birth derives, and `foreign`, whose root carries another one.
//! Cloud sends Close, and may send it again.
//!
//! `LocalMachine::close_preview` (preview_close.rs) takes the switch, then the exclusive
//! turn, and holds both through the sweep, so no create, start or copy runs inside it. Its
//! steps are separate here only because a crash can fall between them: `begin_close`
//! writes Closing (consumption.rs), `remove_namespace` removes the Containers
//! (lifecycle.rs), the plugin's Close (volume_plugin/close.rs) destroys the copy whose
//! origin is exactly the preview's and moves the other to the Trash, one per step, and
//! `finish_close` writes Closed. A daemon start runs `resume_preview_closes` for every
//! record left Closing.
//!
//! A Boot is a create or start of the preview's Container that Cloud sent before the Close
//! and that reaches A late: `create_container_admitted` opens the record, `start_checked`
//! reads it, and both refuse a preview whose Close began. Docker's Create of a Volume whose
//! root is gone makes a new, empty root, which the Container then writes. An arrival is a
//! pull or Seed into `own`: `admit_preview_arrival` refuses it under the switch once the
//! Close began. Each waits out the turn, so neither runs inside a Close.
//!
//! The finding this guards: a Close that writes only Closed, at its end, leaves the record
//! Open across a crash between its first destroy and that write. A late Boot then makes a
//! new root and writes it, and the resent Close destroys those writes
//! (`Broken::closing_last`).

use std::fmt::Write as _;

use stateright::{Model, Property};

use crate::harness::{self, Narrated, everywhere};

/// The preview's record in `restart-approvals.json`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Record {
    Open,
    Closing,
    Closed,
}

/// One of the preview's Volumes on A.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Copy {
    /// The root the preview had before its Close began.
    Kept,
    /// A root Docker's Create made after the Close began, holding writes made since.
    Fresh,
    Destroyed,
    Trashed,
}

impl Copy {
    fn present(self) -> bool {
        matches!(self, Self::Kept | Self::Fresh)
    }
}

/// The step a Close holding the turn runs next.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Step {
    Containers,
    Own,
    Foreign,
    Mark,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// Cloud's Close, or a daemon start's resume, takes the turn and writes Closing.
    Begin,
    /// The rest of the sweep, one step.
    Sweep(Step),
    /// The daemon dies; Docker keeps what it runs.
    Crash,
    /// The daemon starts again.
    Restart,
    /// The late create or start of the preview's Container.
    Boot,
    /// A late pull or Seed into `own`.
    Arrive,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    record: Record,
    container: bool,
    own: Copy,
    foreign: Copy,
    /// The Close holding the turn, and its next step.
    closing: Option<Step>,
    up: bool,
    crashes: u8,
    /// Closes Cloud has yet to send.
    sends: u8,
    /// A daemon start found the record Closing and will run its Close.
    resume: bool,
    boot: bool,
    arrival: bool,
    /// Ghosts the properties read.
    began: bool,
    late: bool,
    lost_writes: bool,
    destroyed_foreign: bool,
    resumed: bool,
    last: Option<Action>,
}

/// A guard the code has, taken out, to show the check catches what it prevents.
#[derive(Clone, Copy, Debug, Default)]
struct Broken {
    /// The Close writes nothing until its end, and then Closed, as the first design did.
    closing_last: bool,
    /// A Boot creates and starts whatever the record says.
    boot_skips_record: bool,
    /// An arrival lands whatever the record says.
    arrival_skips_record: bool,
    /// A daemon start resumes nothing.
    no_resume: bool,
    /// The sweep destroys every Volume of the Namespace by its name.
    destroys_by_name: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Omit {
    Actions(&'static [Action]),
}

type Coverage = harness::Coverage<PreviewClose, Omit>;

const NOTHING_LATE: &str = "nothing of a preview starts or arrives once its Close began";
const NO_LOST_WRITES: &str = "a Close destroys no data written after it began";
const ONLY_ITS_OWN: &str = "a Close destroys only data carrying the preview's own origin";
const CLOSED_EMPTY: &str = "a Closed preview holds no Container and no Volume";
const ENDS_CLOSED: &str = "every run ends with the preview Closed";

/// Two, so a Close each of Cloud's sends began can die in turn.
const MAX_CRASHES: u8 = 2;

const COVERAGE: &[Coverage] = &[
    Coverage {
        name: "a delayed Boot reaches A while the preview is Closing",
        on: everywhere,
        omit: Some(Omit::Actions(&[Action::Crash])),
        reached: |_, state| state.last == Some(Action::Boot) && state.record == Record::Closing,
    },
    Coverage {
        name: "the daemon dies between the first destroy and Closed",
        on: everywhere,
        omit: Some(Omit::Actions(&[Action::Sweep(Step::Own)])),
        reached: |_, state| {
            state.last == Some(Action::Crash)
                && state.own == Copy::Destroyed
                && state.record != Record::Closed
        },
    },
    Coverage {
        name: "a daemon start resumes a Close and ends it Closed",
        on: everywhere,
        omit: Some(Omit::Actions(&[Action::Restart])),
        reached: |_, state| state.last == Some(Action::Sweep(Step::Mark)) && state.resumed,
    },
    Coverage {
        name: "a repeat Close sweeps a Closed preview again",
        on: everywhere,
        omit: None,
        reached: |_, state| state.last == Some(Action::Begin) && state.record == Record::Closed,
    },
];

#[derive(Clone, Copy, Debug)]
struct PreviewClose {
    broken: Broken,
    omit: Option<Omit>,
}

impl PreviewClose {
    fn step(&self, state: &mut State, action: Action) {
        match action {
            Action::Begin => {
                if state.resume {
                    state.resume = false;
                    state.resumed = true;
                } else {
                    state.sends -= 1;
                }
                if !self.broken.closing_last && state.record == Record::Open {
                    state.record = Record::Closing;
                }
                state.began = true;
                state.closing = Some(Step::Containers);
            }
            Action::Sweep(Step::Containers) => {
                state.container = false;
                state.closing = Some(Step::Own);
            }
            Action::Sweep(Step::Own) => {
                if state.own == Copy::Fresh {
                    state.lost_writes = true;
                }
                if state.own.present() {
                    state.own = Copy::Destroyed;
                }
                state.closing = Some(Step::Foreign);
            }
            Action::Sweep(Step::Foreign) => {
                if state.foreign.present() {
                    state.foreign = if self.broken.destroys_by_name {
                        state.destroyed_foreign = true;
                        Copy::Destroyed
                    } else {
                        Copy::Trashed
                    };
                }
                state.closing = Some(Step::Mark);
            }
            Action::Sweep(Step::Mark) => {
                state.record = Record::Closed;
                state.closing = None;
            }
            Action::Crash => {
                state.up = false;
                state.closing = None;
                state.crashes += 1;
            }
            Action::Restart => {
                state.up = true;
                state.resume = state.record == Record::Closing && !self.broken.no_resume;
            }
            Action::Boot => {
                state.boot = false;
                if !refuses(state, self.broken.boot_skips_record) {
                    state.container = true;
                    state.late |= state.began;
                    if !state.own.present() {
                        state.own = Copy::Fresh;
                    }
                }
            }
            Action::Arrive => {
                state.arrival = false;
                if !refuses(state, self.broken.arrival_skips_record) {
                    state.late |= state.began;
                    if !state.own.present() {
                        state.own = Copy::Fresh;
                    }
                }
            }
        }
    }

    fn omitted(&self, action: Action) -> bool {
        matches!(self.omit, Some(Omit::Actions(actions)) if actions.contains(&action))
    }
}

impl Model for PreviewClose {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            record: Record::Open,
            container: true,
            own: Copy::Kept,
            foreign: Copy::Kept,
            closing: None,
            up: true,
            crashes: 0,
            sends: 2,
            resume: false,
            boot: true,
            arrival: true,
            began: false,
            late: false,
            lost_writes: false,
            destroyed_foreign: false,
            resumed: false,
            last: None,
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        if let Some(step) = state.closing {
            actions.push(Action::Sweep(step));
        } else if state.up {
            if state.sends > 0 || state.resume {
                actions.push(Action::Begin);
            }
            if state.boot {
                actions.push(Action::Boot);
            }
            if state.arrival {
                actions.push(Action::Arrive);
            }
        }
        if state.up && state.crashes < MAX_CRASHES {
            actions.push(Action::Crash);
        }
        if !state.up {
            actions.push(Action::Restart);
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        if self.omitted(action) {
            return None;
        }
        let mut state = *last;
        self.step(&mut state, action);
        state.last = Some(action);
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(NOTHING_LATE, |_, state: &State| !state.late),
            Property::always(NO_LOST_WRITES, |_, state: &State| !state.lost_writes),
            Property::always(ONLY_ITS_OWN, |_, state: &State| !state.destroyed_foreign),
            Property::always(CLOSED_EMPTY, |_, state: &State| {
                state.record != Record::Closed
                    || !(state.container || state.own.present() || state.foreign.present())
            }),
            // A state with nothing left to run is the end of every path through it.
            Property::always(ENDS_CLOSED, |model: &Self, state: &State| {
                let mut next = Vec::new();
                model.actions(state, &mut next);
                !next.is_empty() || state.record == Record::Closed
            }),
        ];
        properties.extend(harness::sometimes(COVERAGE, self));
        properties
    }
}

/// Whether a late create, start or arrival is refused: the record reads past Open, unless
/// `skips` takes that check out.
fn refuses(state: &State, skips: bool) -> bool {
    !skips && state.record != Record::Open
}

/// `action` in the code's words, read with `after` for the arm it took.
fn reads_as(action: Action, after: &State) -> String {
    match action {
        Action::Begin => format!(
            "A: close_preview takes the switch and the exclusive turn; begin_close leaves the record {:?} (preview_close.rs)",
            after.record
        ),
        Action::Sweep(Step::Containers) => {
            "A: remove_namespace force-removes the preview's Containers (lifecycle.rs)".to_owned()
        }
        Action::Sweep(Step::Own) => format!(
            "A: the plugin's Close reads own's ployz:origin, the preview's, and destroys it: {:?} (close.rs)",
            after.own
        ),
        Action::Sweep(Step::Foreign) => format!(
            "A: the plugin's Close reads foreign's origin, another one: {:?} (close.rs)",
            after.foreign
        ),
        Action::Sweep(Step::Mark) => {
            "A: finish_close writes Closed and drops the preview's hook records (consumption.rs)"
                .to_owned()
        }
        Action::Crash => "the daemon dies; Docker keeps every Container it runs".to_owned(),
        Action::Restart => format!(
            "the daemon starts; resume_preview_closes {} (daemon.rs)",
            if after.resume {
                "will run the Close the record left Closing"
            } else {
                "finds nothing Closing"
            }
        ),
        Action::Boot => format!(
            "A: Cloud's late create or start of the preview's Container, under its turn: {} (create_container_admitted, start_checked)",
            if after.container {
                "it runs, on own's root"
            } else {
                "refused, the preview is closed"
            }
        ),
        Action::Arrive => format!(
            "A: a late pull or Seed into own, under the switch: {} (admit_preview_arrival)",
            if after.late && after.own == Copy::Fresh {
                "it lands"
            } else {
                "refused, or it lands before the Close"
            }
        ),
    }
}

impl Narrated for PreviewClose {
    fn story(&self, path: &[(State, Option<Action>)]) -> String {
        let mut story = format!("{:?}\n", self.broken);
        let afters = path.iter().skip(1).map(|(state, _)| state);
        for (number, ((_, action), after)) in path.iter().zip(afters).enumerate() {
            let Some(action) = action else { continue };
            let _ = write!(
                story,
                "{}. {}\n    record={:?} container={} own={:?} foreign={:?} up={}\n",
                number + 1,
                reads_as(*action, after),
                after.record,
                after.container,
                after.own,
                after.foreign,
                after.up,
            );
        }
        story
    }
}

#[cfg(test)]
mod tests {
    use stateright::Checker;

    use super::*;
    use crate::harness::{breaks, check, took_action};

    fn fixed() -> PreviewClose {
        PreviewClose {
            broken: Broken::default(),
            omit: None,
        }
    }

    fn broken(broken: Broken) -> PreviewClose {
        PreviewClose { broken, ..fixed() }
    }

    #[test]
    fn a_close_ends_closed_and_nothing_of_the_preview_outlives_it() {
        let checker = check(fixed());
        println!("{} states", checker.unique_state_count());
        checker.assert_properties();
    }

    #[test]
    fn every_preview_close_sometimes_needs_what_it_names() {
        for cut in harness::assert_omissions(COVERAGE, &fixed(), |model, omit| PreviewClose {
            omit: Some(omit),
            ..*model
        }) {
            println!("{cut}");
        }
    }

    /// The first design: the Close writes only Closed, at its end. The daemon dies after the
    /// first destroy, a late Boot reads the record Open, Docker makes own a new root and the
    /// Container writes it, and the resent Close destroys those writes.
    #[test]
    fn a_close_writes_closing_before_it_destroys_anything() {
        let model = broken(Broken {
            closing_last: true,
            ..Broken::default()
        });
        let path = breaks(model, NO_LOST_WRITES);
        assert!(
            took_action(&path, Action::Crash),
            "the daemon died mid-sweep"
        );
        assert!(
            took_action(&path, Action::Boot),
            "a late Boot made the new root"
        );
    }

    #[test]
    fn a_late_boot_reads_the_record() {
        let path = breaks(
            broken(Broken {
                boot_skips_record: true,
                ..Broken::default()
            }),
            NOTHING_LATE,
        );
        assert!(took_action(&path, Action::Boot));
    }

    #[test]
    fn a_late_arrival_reads_the_record() {
        let path = breaks(
            broken(Broken {
                arrival_skips_record: true,
                ..Broken::default()
            }),
            NOTHING_LATE,
        );
        assert!(took_action(&path, Action::Arrive));
    }

    /// With no resume, a Close the daemon died in stays Closing once Cloud sent its last.
    #[test]
    fn a_daemon_start_resumes_a_close_left_closing() {
        let path = breaks(
            broken(Broken {
                no_resume: true,
                ..Broken::default()
            }),
            ENDS_CLOSED,
        );
        assert!(took_action(&path, Action::Crash));
    }

    #[test]
    fn a_close_destroys_by_origin_not_by_name() {
        breaks(
            broken(Broken {
                destroys_by_name: true,
                ..Broken::default()
            }),
            ONLY_ITS_OWN,
        );
    }

    /// Every guard taken out breaks only what it names.
    #[test]
    fn each_guard_keeps_only_what_it_names() {
        for (flags, kept) in [
            (
                Broken {
                    destroys_by_name: true,
                    ..Broken::default()
                },
                &[NOTHING_LATE, NO_LOST_WRITES, CLOSED_EMPTY, ENDS_CLOSED][..],
            ),
            (
                Broken {
                    no_resume: true,
                    ..Broken::default()
                },
                &[NOTHING_LATE, NO_LOST_WRITES, ONLY_ITS_OWN, CLOSED_EMPTY][..],
            ),
        ] {
            let checker = check(broken(flags));
            for property in kept {
                assert!(
                    checker.discovery(property).is_none(),
                    "{flags:?} breaks {property}"
                );
            }
        }
    }
}
