//! A Seed whose copy fails, on the Machine A that holds the source's writer. Cloud's
//! `seed` run (`seedVolume`, volume-run.inngest.ts) resends `Volume.Seed` until A answers a
//! root, and fails the run on an answer that shows a copy failure. The operator may then
//! seed the same point again. A runs each request under `admit_mutation` (storage.rs), and
//! reads its receive registry (`Receives`, transfer.rs) twice: once for its decision
//! (`VolumeStorage::seed_progress`, seed.rs), whose one registry read is
//! `Receives::take_failure`, with the arm `seed_volume` takes on it, then again for its
//! answer (`VolumeStorage::seeded`), whose `current_copy` (inspect.rs) shows a failure
//! `Receives::status_of` reads. The receive task runs outside the mutation, so it can end
//! between the two.
//!
//! The finding, before the fix: the receive fails after the decision found it running or
//! spawned it, and before the answer's read. The answer shows the failure without taking it,
//! Cloud fails the run, and the operator's next Seed takes that failure and answers it again
//! instead of starting the copy over. `Broken::answer_leaves_failure` restores that answer.
//! The fix: `seeded` takes the failure its copy shows, of the target the slot's pull names.
//! A failure it shows has already ended, so its read and its take are one step here.
//!
//! The properties: no copy failure is answered twice, and at rest every copy that failed
//! was answered.
//!
//! Out of scope: the marks, the floor and the later runs (the parent `volume_marks` model),
//! a request in transit past its run, a dropped child outliving the hold (its failure shows
//! while `take_failure` still answers `Busy`, so it shows again until the child exits), Busy
//! answers, a landing that fails, and a second receive that fails.

use std::fmt::Write as _;

use stateright::{Model, Property};

use crate::harness::{self, Narrated, everywhere};

/// The receives a run may spawn, numbered from 1. Only the first may fail.
const MAX_RECEIVES: u8 = 2;
/// The seed runs: the first, and the operator's one more after it fails.
const MAX_RUNS: u8 = 2;

/// The seed run's row, as `seedVolume` ends it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Run {
    Polling,
    /// `endRun(.., "done", ..)` after a root answer.
    Done,
    /// `failed("03-failed", ..)` after an answer showing a copy failure.
    Failed,
}

/// What A's pool holds for `into`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Into {
    Absent,
    /// The slot, whose `fs` holds the point once a receive exited 0.
    Slot {
        holds_point: bool,
    },
    Root,
}

/// `into`'s entry in `Receives`, for receive `n`. A landed entry reads as none, since
/// `take_failure` answers `Ok(None)` for it and `status_of` reads it `Idle`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Receive {
    None,
    Running(u8),
    Failed(u8),
}

/// The arm of `seed_volume` (seed.rs) a request's decision took.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum SeedPath {
    /// `SeedProgress::Copying`: `take_failure` answered `Busy`.
    Copying,
    /// `SeedProgress::Failed`: `take_failure` took receive `n`'s failure, which the answer
    /// shows through `answering` (inspect.rs).
    Failed(u8),
    /// `SeedProgress::Copied`: `Copied::land` makes the slot's `fs` the root.
    Lands,
    /// `SeedProgress::Stale`: `Stale::clear`, then `copy_point` spawns a receive.
    Restarts,
    /// `SeedProgress::Absent`: `copy_point` makes the slot and spawns a receive.
    Starts,
}

/// What an answer showed Cloud.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Shown {
    Root,
    /// A slot with no failure: Cloud polls again.
    Slot,
    /// A slot whose pull shows receive `n`'s failure.
    Failure(u8),
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// A takes the run's request under `admit_mutation`: `seed_progress`, then the arm
    /// `seed_volume` takes on it.
    Decide,
    /// The request's answer: `seeded` reads the copy, the mutation ends, and `seedVolume`
    /// reads the answer.
    Answer,
    /// The running receive's task ends (`copy_snapshot`, seed.rs), failed when the
    /// receive or the send exits non-zero.
    End { failed: bool },
    /// The operator seeds the same point again after the run failed.
    Retry,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    runs: u8,
    run: Run,
    into: Into,
    receive: Receive,
    spawned: u8,
    /// The arm the last decision took.
    path: Option<SeedPath>,
    /// A decision ran and its answer has not: the handler holds the mutation.
    answering: bool,
    shown: Option<Shown>,
    /// Receives whose copy failed, one bit each.
    failed: u8,
    /// Receives whose failure an answer showed, one bit each.
    answered: u8,
    /// Receives whose failure a second answer showed.
    answered_again: u8,
    last: Option<Action>,
}

impl State {
    fn at_rest(&self, model: &SeedFailure) -> bool {
        let mut actions = Vec::new();
        model.actions(self, &mut actions);
        actions.is_empty()
    }
}

/// Receive `n`'s bit.
fn bit(n: u8) -> u8 {
    1 << (n - 1)
}

/// A guard the code has, taken out, to show the check catches what it prevents.
#[derive(Clone, Copy, Debug, Default)]
struct Broken {
    /// `seeded` shows the failure `current_copy` reads and leaves it in the registry: the
    /// answer before the fix.
    answer_leaves_failure: bool,
}

/// What a coverage check takes out of the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Omit {
    Actions(&'static [Action]),
    /// No decision takes this arm.
    Path(SeedPath),
}

type Coverage = harness::Coverage<SeedFailure, Omit>;

/// The last step was an answer to a decision that took `path`, and it showed a failure.
fn answered_failure(state: &State, path: SeedPath) -> bool {
    state.last == Some(Action::Answer)
        && state.path == Some(path)
        && matches!(state.shown, Some(Shown::Failure(_)))
}

const COVERAGE: &[Coverage] = &[
    Coverage {
        name: "an answer shows the failure its decision took",
        on: everywhere,
        omit: Some(Omit::Path(SeedPath::Failed(1))),
        reached: |_, state| answered_failure(state, SeedPath::Failed(1)),
    },
    Coverage {
        name: "an answer shows a failure that ended after its decision found the receive running",
        on: everywhere,
        omit: Some(Omit::Path(SeedPath::Copying)),
        reached: |_, state| answered_failure(state, SeedPath::Copying),
    },
    Coverage {
        name: "an answer shows a failure that ended after its decision spawned the receive",
        on: everywhere,
        omit: Some(Omit::Path(SeedPath::Starts)),
        reached: |_, state| answered_failure(state, SeedPath::Starts),
    },
    Coverage {
        name: "the next Seed starts the copy over after a failure",
        on: everywhere,
        omit: Some(Omit::Path(SeedPath::Restarts)),
        reached: |_, state| {
            state.last == Some(Action::Decide) && state.path == Some(SeedPath::Restarts)
        },
    },
    Coverage {
        name: "the seed run ends done after an earlier run failed",
        on: everywhere,
        omit: Some(Omit::Actions(&[Action::Retry])),
        reached: |_, state| state.run == Run::Done && state.runs == MAX_RUNS,
    },
    Coverage {
        name: "the seed run ends done on its first run",
        on: everywhere,
        omit: Some(Omit::Path(SeedPath::Lands)),
        reached: |_, state| state.run == Run::Done && state.runs == 1,
    },
];

const ANSWERED_ONCE: &str = "no copy failure is answered twice";
const EVERY_FAILURE_ANSWERED: &str = "at rest, every copy that failed was answered";

#[derive(Clone, Copy, Debug)]
struct SeedFailure {
    broken: Broken,
    /// A coverage check's cut, `None` to run the whole model.
    omit: Option<Omit>,
}

impl SeedFailure {
    fn spawn(state: &mut State) {
        state.spawned += 1;
        state.receive = Receive::Running(state.spawned);
    }

    /// `seed_progress` and the arm `seed_volume` takes on it (seed.rs). The registry is
    /// read before the slot's `fs`. A run ends on the root it answers, so no decision reads
    /// one.
    fn decide(state: &mut State) -> SeedPath {
        match (state.into, state.receive) {
            (Into::Root, _) => unreachable!("a run ends on the root it answers"),
            (_, Receive::Running(_)) => SeedPath::Copying,
            (_, Receive::Failed(n)) => {
                state.receive = Receive::None;
                SeedPath::Failed(n)
            }
            (Into::Slot { holds_point: true }, Receive::None) => {
                state.into = Into::Root;
                SeedPath::Lands
            }
            (Into::Slot { holds_point: false }, Receive::None) => {
                Self::spawn(state);
                SeedPath::Restarts
            }
            (Into::Absent, Receive::None) => {
                state.into = Into::Slot { holds_point: false };
                Self::spawn(state);
                SeedPath::Starts
            }
        }
    }

    /// `seeded` (seed.rs): `current_copy` reads the copy and shows a failure `status_of`
    /// reads, which the fix takes. The `Failed` arm's own take left none to read, and
    /// `answering` shows the one it took.
    fn answer(&self, state: &mut State) -> Shown {
        if let Some(SeedPath::Failed(n)) = state.path {
            return Shown::Failure(n);
        }
        match (state.receive, state.into) {
            (Receive::Failed(n), _) => {
                if !self.broken.answer_leaves_failure {
                    state.receive = Receive::None;
                }
                Shown::Failure(n)
            }
            (Receive::None | Receive::Running(_), Into::Root) => Shown::Root,
            (Receive::None | Receive::Running(_), Into::Absent | Into::Slot { .. }) => Shown::Slot,
        }
    }

    fn step(&self, state: &mut State, action: Action) {
        match action {
            Action::Decide => {
                state.path = Some(Self::decide(state));
                state.answering = true;
            }
            Action::Answer => {
                let shown = self.answer(state);
                state.answering = false;
                state.shown = Some(shown);
                match shown {
                    Shown::Root => state.run = Run::Done,
                    Shown::Slot => {}
                    Shown::Failure(n) => {
                        if state.answered & bit(n) != 0 {
                            state.answered_again |= bit(n);
                        }
                        state.answered |= bit(n);
                        state.run = Run::Failed;
                    }
                }
            }
            Action::End { failed } => {
                let Receive::Running(n) = state.receive else {
                    unreachable!("only a running receive ends");
                };
                if failed {
                    state.receive = Receive::Failed(n);
                    state.failed |= bit(n);
                } else {
                    state.receive = Receive::None;
                    state.into = Into::Slot { holds_point: true };
                }
            }
            Action::Retry => {
                state.runs += 1;
                state.run = Run::Polling;
            }
        }
    }

    fn omitted(&self, action: Action, after: &State) -> bool {
        match self.omit {
            None => false,
            Some(Omit::Actions(actions)) => actions.contains(&action),
            Some(Omit::Path(path)) => action == Action::Decide && after.path == Some(path),
        }
    }
}

impl Model for SeedFailure {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            runs: 1,
            run: Run::Polling,
            into: Into::Absent,
            receive: Receive::None,
            spawned: 0,
            path: None,
            answering: false,
            shown: None,
            failed: 0,
            answered: 0,
            answered_again: 0,
            last: None,
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        if state.answering {
            actions.push(Action::Answer);
        } else if state.run == Run::Polling {
            actions.push(Action::Decide);
        } else if state.run == Run::Failed && state.runs < MAX_RUNS {
            actions.push(Action::Retry);
        }
        if let Receive::Running(n) = state.receive {
            actions.push(Action::End { failed: false });
            if n < MAX_RECEIVES {
                actions.push(Action::End { failed: true });
            }
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
            Property::always(ANSWERED_ONCE, |_, state: &State| state.answered_again == 0),
            Property::always(EVERY_FAILURE_ANSWERED, |model: &Self, state: &State| {
                !state.at_rest(model) || state.failed & !state.answered == 0
            }),
        ];
        properties.extend(harness::sometimes(COVERAGE, self));
        properties
    }
}

/// `action` in the code's words, read with `after` for the arm it took.
fn reads_as(action: Action, after: &State) -> String {
    match action {
        Action::Decide => format!(
            "A: Volume.Seed under admit_mutation; seed_progress reads take_failure and takes {:?} (seed.rs seed_volume)",
            after.path
        ),
        Action::Answer => format!(
            "A: seeded reads the copy and answers {:?}; seedVolume reads it (volume-run.inngest.ts)",
            after.shown
        ),
        Action::End { failed: true } => {
            "the receive task ends failed, outside the mutation (seed.rs copy_snapshot)".to_owned()
        }
        Action::End { failed: false } => {
            "the receive task ends: fs holds the point (seed.rs copy_snapshot)".to_owned()
        }
        Action::Retry => "the operator seeds the same point again".to_owned(),
    }
}

impl Narrated for SeedFailure {
    fn story(&self, path: &[(State, Option<Action>)]) -> String {
        let mut story = format!("{:?}\n", self.broken);
        let afters = path.iter().skip(1).map(|(state, _)| state);
        for (number, ((_, action), after)) in path.iter().zip(afters).enumerate() {
            let Some(action) = action else { continue };
            let _ = write!(
                story,
                "{}. {}\n    run {} {:?} | into={:?} receive={:?} answered={:#b} again={:#b}\n",
                number + 1,
                reads_as(*action, after),
                after.runs,
                after.run,
                after.into,
                after.receive,
                after.answered,
                after.answered_again,
            );
        }
        story
    }
}

#[cfg(test)]
mod tests {
    use stateright::Checker;

    use super::*;
    use crate::harness::{breaks, check, last, took_action};

    fn fixed() -> SeedFailure {
        SeedFailure {
            broken: Broken::default(),
            omit: None,
        }
    }

    #[test]
    fn a_seed_answers_each_copy_failure_once() {
        let checker = check(fixed());
        println!("{} states", checker.unique_state_count());
        checker.assert_properties();
    }

    #[test]
    fn every_seed_failure_sometimes_needs_what_it_names() {
        for cut in harness::assert_omissions(COVERAGE, &fixed(), |model, omit| SeedFailure {
            omit: Some(omit),
            ..*model
        }) {
            println!("{cut}");
        }
    }

    /// The finding. The receive fails between a decision and its answer, the answer shows
    /// the failure and leaves it in the registry, and Cloud fails the run. The operator's
    /// next Seed takes that same failure and answers it again, so the copy never starts
    /// over in that call. No failure goes unanswered: the old answer answered one twice.
    #[test]
    fn a_seed_answer_takes_the_failure_it_shows() {
        let model = SeedFailure {
            broken: Broken {
                answer_leaves_failure: true,
            },
            ..fixed()
        };
        let path = breaks(model, ANSWERED_ONCE);
        let broken = last(&path);
        assert_eq!(broken.path, Some(SeedPath::Failed(1)));
        assert!(took_action(&path, Action::Retry), "a second run read it");
        assert!(
            path.iter().any(|(before, action)| before.answering
                && *action == Some(Action::End { failed: true })),
            "the receive failed between a decision and its answer"
        );
        assert!(
            check(model).discovery(EVERY_FAILURE_ANSWERED).is_none(),
            "the old answer loses no failure"
        );
    }

    /// The order `a_failed_seed_copy_is_answered_once_and_the_next_call_starts_over`
    /// (seed_tests.rs) forces, which CI's failures took: a resend finds the receive
    /// running, and it fails before that resend's answer. The old answer leaves the
    /// failure for the operator's next Seed to answer again; the fixed one takes it, and
    /// that Seed starts the copy over.
    #[test]
    fn a_failure_that_ends_during_a_resent_seed_is_answered_by_it() {
        const RESENT: &[Action] = &[
            Action::Decide,
            Action::Answer,
            Action::Decide,
            Action::End { failed: true },
            Action::Answer,
            Action::Retry,
            Action::Decide,
        ];
        let old = SeedFailure {
            broken: Broken {
                answer_leaves_failure: true,
            },
            ..fixed()
        };
        let mut schedule = RESENT.to_vec();
        schedule.push(Action::Answer);
        let path = harness::follow(old, &schedule);
        assert_eq!(last(&path).answered_again, bit(1));

        let path = harness::follow(fixed(), RESENT);
        assert_eq!(last(&path).path, Some(SeedPath::Restarts));
    }
}
