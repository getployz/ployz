//! A Seed whose copy fails, on the Machine A that holds the source's writer. Cloud's
//! `seed` run (`seedVolume`, volume-run.inngest.ts) resends `Volume.Seed`, carrying the
//! run's gen, until A answers a root, and fails the run on an answer that shows a copy
//! failure. A reply can be lost, or arrive after Cloud's attempt timed out, and Cloud then
//! resends the same run's Seed (`sendSwitch`, volume-run.cluster.server.ts). A run whose
//! sends all went unanswered, or that ran out of patience, gives up. The operator may then
//! seed the same point again, which is a new run with a later gen.
//!
//! A runs each request under `admit_mutation` (storage.rs) and reads its receive registry
//! (`Receives`, transfer.rs) twice: once for its decision (`VolumeStorage::seed_progress`,
//! seed.rs), whose one registry read is `Receives::seed_failure`, with the arm
//! `seed_volume` takes on it, then again when it forms its reply (`VolumeStorage::seeded`),
//! whose `current_copy` (inspect.rs) shows a failure `Receives::status_of` reads, and
//! `seed_volume` records the run it showed a failure to (`Receives::answered`). The
//! receive task runs outside the mutation, so it can end between the two. Forming a reply
//! and Cloud receiving it are separate steps, since the reply can be lost between them.
//!
//! The findings. Before PR #4576, the reply showed a failure that ended after the decision
//! and left it, so the operator's next run answered it again instead of starting over
//! (`Broken::answer_leaves_failure`). The PR's first fix took the failure when a reply
//! showed it, so a reply lost on its way left the run's resend to start the copy over, and
//! Cloud never saw the failure (`Broken::answer_takes_failure`). The fix: a failure stays
//! for every Seed of the run it was answered to, and goes with the first Seed of a later
//! run, which Cloud starts only after the earlier run ended.
//!
//! The properties: no copy failure reaches Cloud in two runs, and a copy starts over past
//! a failure Cloud never received only in a run that began after the failure.
//!
//! Out of scope: the marks, the floor and the later runs (the parent `volume_marks` model),
//! a request in transit past its run, a Seed carrying no gen (it names no run, so the next
//! one after an answer starts over), a dropped child outliving the hold (its failure shows
//! while `seed_failure` still answers `Busy`), Busy answers, and a landing that fails.

use std::fmt::Write as _;

use stateright::{Model, Property};

use crate::harness::{self, Narrated, everywhere};

/// The receives the runs may spawn, numbered from 1. All but the last may fail.
const MAX_RECEIVES: u8 = 3;
/// The seed runs: the first, and the operator's runs after it.
const MAX_RUNS: u8 = 3;
/// The Seeds one run sends before it gives up.
const MAX_SENDS: u8 = 3;
/// The replies lost on their way to Cloud, over all runs.
const MAX_LOSSES: u8 = 2;

/// The seed run's row, as `seedVolume` ends it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Run {
    Polling,
    /// `endRun(.., "done", ..)` after a root answer.
    Done,
    /// `failed("03-failed", ..)` after a reply showing a copy failure.
    Failed,
    /// Every send went unanswered, or `03-too-long`: the run ended without a failure shown.
    GaveUp,
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

/// Which runs a failure was answered to (`Answered`, transfer.rs), by run number.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Answered {
    Not,
    /// The newest run it was answered to.
    To(u8),
}

/// `into`'s entry in `Receives`, for receive `n`. A landed entry reads as none, since
/// `seed_failure` answers `Ok(None)` for it and `status_of` reads it `Idle`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Receive {
    None,
    Running(u8),
    Failed(u8, Answered),
}

/// The arm of `seed_volume` (seed.rs) a request's decision took.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum SeedPath {
    /// `SeedProgress::Copying`: `seed_failure` answered `Busy`.
    Copying,
    /// `SeedProgress::Failed`: `seed_failure` read receive `n`'s failure, which the reply
    /// shows through `answering` (inspect.rs).
    Failed(u8),
    /// `SeedProgress::Copied`: `Copied::land` makes the slot's `fs` the root.
    Lands,
    /// `SeedProgress::Landed`: a resend after the root's reply was lost answers the root.
    Landed,
    /// `SeedProgress::Stale`: `Stale::clear`, then `copy_point` spawns a receive.
    Restarts,
    /// `SeedProgress::Absent`: `copy_point` makes the slot and spawns a receive.
    Starts,
}

/// What a reply shows.
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
    /// Cloud sends the run's Seed, and A takes it under `admit_mutation`: `seed_progress`,
    /// then the arm `seed_volume` takes on it.
    Decide,
    /// A forms the reply: `seeded` reads the copy, `seed_volume` records the run a failure
    /// it shows was answered to, and the mutation ends.
    Answer,
    /// `seedVolume` reads the reply (volume-run.inngest.ts).
    Deliver,
    /// The reply is lost, or arrives after the attempt timed out (`sendSwitch`).
    Lose,
    /// The running receive's task ends (`copy_snapshot`, seed.rs), failed when the
    /// receive or the send exits non-zero.
    End { failed: bool },
    /// The operator seeds the same point again after the run ended without a root.
    Retry,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    runs: u8,
    run: Run,
    /// The Seeds the current run sent.
    sends: u8,
    losses: u8,
    into: Into,
    receive: Receive,
    spawned: u8,
    /// The arm the last decision took.
    path: Option<SeedPath>,
    /// A decision ran and its reply is not formed: the handler holds the mutation.
    answering: bool,
    /// A formed reply on its way to Cloud.
    in_flight: Option<Shown>,
    /// The run current when receive `n` failed, at index `n - 1`, 0 if it has not.
    failed_in: [u8; MAX_RECEIVES as usize],
    /// The run that received receive `n`'s failure, at index `n - 1`, 0 if none has.
    delivered_to: [u8; MAX_RECEIVES as usize],
    /// Receives whose failure a lost reply showed, one bit each.
    lost: u8,
    /// A failure reached Cloud in a second run.
    delivered_twice: bool,
    /// A copy started over past a failure Cloud never received, in the run it failed in.
    hidden: bool,
    last: Option<Action>,
}

/// Receive `n`'s entry in `runs`, or nothing for a receive the model never spawns.
fn of(runs: &mut [u8; MAX_RECEIVES as usize], n: u8) -> Option<&mut u8> {
    runs.get_mut(usize::from(n - 1))
}

/// Receive `n`'s bit.
fn bit(n: u8) -> u8 {
    1 << (n - 1)
}

/// A guard the code has, taken out, to show the check catches what it prevents.
#[derive(Clone, Copy, Debug, Default)]
struct Broken {
    /// The code before PR #4576: the decision takes a failure it reads, and the reply
    /// shows a failure that ended after the decision and leaves it.
    answer_leaves_failure: bool,
    /// PR #4576's first fix: the decision takes a failure it reads, and the reply takes
    /// the failure it shows.
    answer_takes_failure: bool,
    /// `Answered::superseded_by` reads the run a failure was answered to as later, so the
    /// run's own resend clears it.
    supersedes_its_own_run: bool,
    /// `seed_volume` records no run for a failure its reply showed.
    answer_records_no_run: bool,
}

impl Broken {
    /// The decision takes every failure it reads, as `take_failure` does.
    fn decision_takes(self) -> bool {
        self.answer_leaves_failure || self.answer_takes_failure
    }
}

/// What a coverage check takes out of the model.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Omit {
    Actions(&'static [Action]),
    /// No decision takes this arm.
    Path(SeedPath),
}

type Coverage = harness::Coverage<SeedFailure, Omit>;

/// The last step formed a reply to a decision that took `path`, and it showed a failure.
fn answered_failure(state: &State, path: SeedPath) -> bool {
    state.last == Some(Action::Answer)
        && state.path == Some(path)
        && matches!(state.in_flight, Some(Shown::Failure(_)))
}

const COVERAGE: &[Coverage] = &[
    Coverage {
        name: "a reply shows the failure its decision read",
        on: everywhere,
        omit: Some(Omit::Path(SeedPath::Failed(1))),
        reached: |_, state| answered_failure(state, SeedPath::Failed(1)),
    },
    Coverage {
        name: "a reply shows a failure that ended after its decision found the receive running",
        on: everywhere,
        omit: Some(Omit::Path(SeedPath::Copying)),
        reached: |_, state| answered_failure(state, SeedPath::Copying),
    },
    Coverage {
        name: "a reply shows a failure that ended after its decision spawned the receive",
        on: everywhere,
        omit: Some(Omit::Path(SeedPath::Starts)),
        reached: |_, state| answered_failure(state, SeedPath::Starts),
    },
    Coverage {
        name: "a resend after a lost reply shows the failure again",
        on: everywhere,
        omit: Some(Omit::Actions(&[Action::Lose])),
        reached: |_, state| {
            state.last == Some(Action::Answer)
                && matches!(state.in_flight, Some(Shown::Failure(n)) if state.lost & bit(n) != 0)
        },
    },
    Coverage {
        name: "a run gives up after its last reply was lost",
        on: everywhere,
        omit: Some(Omit::Actions(&[Action::Lose])),
        reached: |_, state| state.last == Some(Action::Lose) && state.run == Run::GaveUp,
    },
    Coverage {
        name: "a later run starts the copy over after a failure",
        on: everywhere,
        omit: Some(Omit::Path(SeedPath::Restarts)),
        reached: |_, state| {
            state.last == Some(Action::Decide) && state.path == Some(SeedPath::Restarts)
        },
    },
    Coverage {
        name: "the seed run ends done after an earlier run ended without a root",
        on: everywhere,
        omit: Some(Omit::Actions(&[Action::Retry])),
        reached: |_, state| state.run == Run::Done && state.runs > 1,
    },
    Coverage {
        name: "a resend after the root's reply was lost answers the root",
        on: everywhere,
        omit: Some(Omit::Path(SeedPath::Landed)),
        reached: |_, state| {
            state.last == Some(Action::Deliver)
                && state.path == Some(SeedPath::Landed)
                && state.run == Run::Done
        },
    },
    Coverage {
        name: "the seed run ends done on its first run",
        on: everywhere,
        omit: Some(Omit::Path(SeedPath::Lands)),
        reached: |_, state| state.run == Run::Done && state.runs == 1,
    },
];

const DELIVERED_ONCE: &str = "no copy failure reaches Cloud in two runs";
const NEVER_HIDDEN: &str =
    "a copy starts over past a failure Cloud never received only in a run that began after it";

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

    /// `Answered::superseded_by` (transfer.rs) for a Seed of run `run`.
    fn superseded(self, answered: Answered, run: u8) -> bool {
        match answered {
            Answered::Not => false,
            Answered::To(to) if self.broken.supersedes_its_own_run => run >= to,
            Answered::To(to) => run > to,
        }
    }

    /// Notes a copy that starts over in run `run` past a failure Cloud never received,
    /// which failed in that run.
    fn passes(state: &mut State) {
        let hidden =
            state
                .failed_in
                .iter()
                .zip(state.delivered_to)
                .any(|(&failed_in, delivered_to)| {
                    failed_in != 0 && delivered_to == 0 && state.runs <= failed_in
                });
        state.hidden |= hidden;
    }

    /// `seed_progress` and the arm `seed_volume` takes on it (seed.rs). The root is read
    /// first, then the registry, then the slot's `fs`.
    fn decide(self, state: &mut State) -> SeedPath {
        if state.into == Into::Root {
            return SeedPath::Landed;
        }
        if let Receive::Running(_) = state.receive {
            return SeedPath::Copying;
        }
        if let Receive::Failed(n, answered) = state.receive {
            if self.broken.decision_takes() {
                state.receive = Receive::None;
                return SeedPath::Failed(n);
            }
            if !self.superseded(answered, state.runs) {
                return SeedPath::Failed(n);
            }
            state.receive = Receive::None;
        }
        match state.into {
            Into::Root => unreachable!("the root was read first"),
            Into::Slot { holds_point: true } => {
                Self::passes(state);
                state.into = Into::Root;
                SeedPath::Lands
            }
            Into::Slot { holds_point: false } => {
                Self::passes(state);
                Self::spawn(state);
                SeedPath::Restarts
            }
            Into::Absent => {
                state.into = Into::Slot { holds_point: false };
                Self::spawn(state);
                SeedPath::Starts
            }
        }
    }

    /// `seeded` (seed.rs): `current_copy` reads the copy and shows a failure `status_of`
    /// reads. The `Failed` arm's `answering` shows the one its decision read. Then
    /// `seed_volume` records the run a failure it shows was answered to.
    fn answer(self, state: &mut State) -> Shown {
        let shown = match (state.path, state.receive, state.into) {
            (Some(SeedPath::Failed(n)), ..) | (_, Receive::Failed(n, _), _) => Shown::Failure(n),
            (_, Receive::None | Receive::Running(_), Into::Root) => Shown::Root,
            (_, Receive::None | Receive::Running(_), Into::Absent | Into::Slot { .. }) => {
                Shown::Slot
            }
        };
        if let Receive::Failed(n, answered) = state.receive {
            state.receive = if self.broken.answer_takes_failure {
                Receive::None
            } else if self.broken.answer_records_no_run || self.broken.answer_leaves_failure {
                Receive::Failed(n, answered)
            } else {
                let to = match answered {
                    Answered::Not => state.runs,
                    Answered::To(to) => to.max(state.runs),
                };
                Receive::Failed(n, Answered::To(to))
            };
        }
        shown
    }

    /// Ends the run without a root once it has sent all it may.
    fn out_of_sends(state: &mut State) {
        if state.sends == MAX_SENDS {
            state.run = Run::GaveUp;
        }
    }

    fn step(self, state: &mut State, action: Action) {
        match action {
            Action::Decide => {
                state.sends += 1;
                state.path = Some(self.decide(state));
                state.answering = true;
            }
            Action::Answer => {
                state.in_flight = Some(self.answer(state));
                state.answering = false;
            }
            Action::Deliver => match state.in_flight.take() {
                Some(Shown::Root) => state.run = Run::Done,
                Some(Shown::Slot) => Self::out_of_sends(state),
                Some(Shown::Failure(n)) => {
                    if let Some(to) = of(&mut state.delivered_to, n) {
                        state.delivered_twice |= *to != 0 && *to != state.runs;
                        *to = state.runs;
                    }
                    state.run = Run::Failed;
                }
                None => unreachable!("only a formed reply is delivered"),
            },
            Action::Lose => {
                if let Some(Shown::Failure(n)) = state.in_flight {
                    state.lost |= bit(n);
                }
                state.in_flight = None;
                state.losses += 1;
                Self::out_of_sends(state);
            }
            Action::End { failed } => {
                let Receive::Running(n) = state.receive else {
                    unreachable!("only a running receive ends");
                };
                if failed {
                    state.receive = Receive::Failed(n, Answered::Not);
                    if let Some(failed_in) = of(&mut state.failed_in, n) {
                        *failed_in = state.runs;
                    }
                } else {
                    state.receive = Receive::None;
                    state.into = Into::Slot { holds_point: true };
                }
            }
            Action::Retry => {
                state.runs += 1;
                state.run = Run::Polling;
                state.sends = 0;
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
            sends: 0,
            losses: 0,
            into: Into::Absent,
            receive: Receive::None,
            spawned: 0,
            path: None,
            answering: false,
            in_flight: None,
            failed_in: [0; MAX_RECEIVES as usize],
            delivered_to: [0; MAX_RECEIVES as usize],
            lost: 0,
            delivered_twice: false,
            hidden: false,
            last: None,
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        if state.answering {
            actions.push(Action::Answer);
        } else if state.in_flight.is_some() {
            actions.push(Action::Deliver);
            if state.losses < MAX_LOSSES {
                actions.push(Action::Lose);
            }
        } else if state.run == Run::Polling {
            actions.push(Action::Decide);
        } else if matches!(state.run, Run::Failed | Run::GaveUp) && state.runs < MAX_RUNS {
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
            Property::always(DELIVERED_ONCE, |_, state: &State| !state.delivered_twice),
            Property::always(NEVER_HIDDEN, |_, state: &State| !state.hidden),
        ];
        properties.extend(harness::sometimes(COVERAGE, self));
        properties
    }
}

/// `action` in the code's words, read with `after` for the arm it took.
fn reads_as(action: Action, after: &State) -> String {
    match action {
        Action::Decide => format!(
            "A: run {}'s Volume.Seed under admit_mutation; seed_progress reads seed_failure and takes {:?} (seed.rs seed_volume)",
            after.runs, after.path
        ),
        Action::Answer => format!(
            "A: seeded reads the copy and forms {:?}; seed_volume records the run it showed a failure to",
            after.in_flight
        ),
        Action::Deliver => "seedVolume reads the reply (volume-run.inngest.ts)".to_owned(),
        Action::Lose => {
            "the reply is lost or arrives after the attempt timed out (sendSwitch)".to_owned()
        }
        Action::End { failed: true } => {
            "the receive task ends failed, outside the mutation (seed.rs copy_snapshot)".to_owned()
        }
        Action::End { failed: false } => {
            "the receive task ends: fs holds the point (seed.rs copy_snapshot)".to_owned()
        }
        Action::Retry => "the operator seeds the same point again: a new run".to_owned(),
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
                "{}. {}\n    run {} {:?} sends={} | into={:?} receive={:?} failed_in={:?} delivered_to={:?}\n",
                number + 1,
                reads_as(*action, after),
                after.runs,
                after.run,
                after.sends,
                after.into,
                after.receive,
                after.failed_in,
                after.delivered_to,
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

    fn broken(broken: Broken) -> SeedFailure {
        SeedFailure { broken, ..fixed() }
    }

    #[test]
    fn a_seed_answers_each_copy_failure_to_one_run_and_hides_none() {
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

    /// The code before PR #4576. The receive fails between a decision and its reply, the
    /// reply shows the failure and leaves it in the registry, and Cloud fails the run. The
    /// operator's next run reads that same failure and is answered it again, so the copy
    /// never starts over in that call.
    #[test]
    fn the_old_reply_shows_a_failure_to_two_runs() {
        let model = broken(Broken {
            answer_leaves_failure: true,
            ..Broken::default()
        });
        let path = breaks(model, DELIVERED_ONCE);
        assert!(took_action(&path, Action::Retry), "a second run read it");
        assert!(
            path.iter().any(|(before, action)| before.answering
                && *action == Some(Action::End { failed: true })),
            "the receive failed between a decision and its reply"
        );
    }

    /// PR #4576's first fix, which the review found. The reply takes the failure it shows,
    /// the reply is lost, and the run's resend starts the copy over: Cloud never sees the
    /// failure. It keeps each failure to one run.
    #[test]
    fn a_failure_whose_reply_was_lost_is_shown_again() {
        let model = broken(Broken {
            answer_takes_failure: true,
            ..Broken::default()
        });
        let path = breaks(model, NEVER_HIDDEN);
        assert!(took_action(&path, Action::Lose), "a reply was lost");
        assert!(
            !took_action(&path, Action::Retry),
            "the run that lost it started over"
        );
        assert!(
            check(model).discovery(DELIVERED_ONCE).is_none(),
            "the first fix shows no failure to two runs"
        );
    }

    /// The run's own resend clears the failure its reply showed, as a check of `>=` would.
    #[test]
    fn a_failure_stays_for_the_resends_of_its_run() {
        let model = broken(Broken {
            supersedes_its_own_run: true,
            ..Broken::default()
        });
        let path = breaks(model, NEVER_HIDDEN);
        assert!(took_action(&path, Action::Lose), "a reply was lost");
    }

    /// A reply shows a failure that ended after its decision, and records no run for it,
    /// so the next run reads it as never answered and is shown it again.
    #[test]
    fn a_reply_records_the_run_it_showed_a_failure_to() {
        let model = broken(Broken {
            answer_records_no_run: true,
            ..Broken::default()
        });
        let path = breaks(model, DELIVERED_ONCE);
        assert!(took_action(&path, Action::Retry), "a second run read it");
    }

    /// The orders the plugin tests force on fake ZFS
    /// (`a_failed_seed_copy_is_answered_to_its_run_and_the_next_run_starts_over`,
    /// seed_tests.rs): a resend finds the receive running, it fails before that resend's
    /// reply, and the reply is lost. The run's next resend is shown the failure again under
    /// the fix, and starts the copy over under the first fix. Delivered, the operator's
    /// next run starts over under the fix and is shown it again under the old reply.
    #[test]
    fn a_failure_that_ends_during_a_resent_seed_is_shown_to_its_run_only() {
        const RESENT: &[Action] = &[
            Action::Decide,
            Action::Answer,
            Action::Deliver,
            Action::Decide,
            Action::End { failed: true },
            Action::Answer,
        ];
        let lost = [RESENT, &[Action::Lose, Action::Decide]].concat();
        let path = harness::follow(fixed(), &lost);
        assert_eq!(last(&path).path, Some(SeedPath::Failed(1)));
        let first_fix = broken(Broken {
            answer_takes_failure: true,
            ..Broken::default()
        });
        let path = harness::follow(first_fix, &lost);
        assert_eq!(last(&path).path, Some(SeedPath::Restarts));
        assert!(last(&path).hidden);

        let retried = [RESENT, &[Action::Deliver, Action::Retry, Action::Decide]].concat();
        let path = harness::follow(fixed(), &retried);
        assert_eq!(last(&path).path, Some(SeedPath::Restarts));
        let old = broken(Broken {
            answer_leaves_failure: true,
            ..Broken::default()
        });
        let path = harness::follow(
            old,
            &[&retried[..], &[Action::Answer, Action::Deliver]].concat(),
        );
        assert!(last(&path).delivered_twice);
    }
}
