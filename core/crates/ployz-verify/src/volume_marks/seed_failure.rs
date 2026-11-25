//! A Seed whose copy fails, on the Machine A that holds the source's writer. Cloud's
//! `seed` run (`seedVolume`, volume-run.inngest.ts) resends `Volume.Seed`, carrying the
//! run's gen, until A answers a root, and fails the run on an answer that shows a copy
//! failure or a Stale refusal (`sendSwitch`, volume-run.cluster.server.ts). A reply can be
//! lost, or arrive after Cloud's attempt timed out, and Cloud then resends the same run's
//! Seed. A request can also wait in transit past its attempt, and reach A after its run
//! ended and later runs began. A run whose sends all went unanswered, or that ran out of
//! patience, gives up. The operator may then seed the same point again, which is a new
//! run. Postgres takes a run's gen before it admits the run (`volume_run.gen`, tables.ts),
//! so a run admitted later can carry a lower gen than one that already ended.
//!
//! A runs each request under `admit_mutation` (storage.rs). It first admits the Seed's gen
//! against the newest it admitted for `into` (`Receives::admit_seed`, transfer.rs), and
//! refuses an older one Stale. Its decision (`VolumeStorage::seed_progress`, seed.rs) reads
//! the receive registry once, through `Receives::seed_failure`, which answers a failure
//! and records the run it answered in one step. The reply then reads the copy
//! (`VolumeStorage::seeded`), and shows only the failure the decision answered
//! (`answering` and `receiving`, inspect.rs). The receive task runs outside the mutation,
//! so it can end between the decision and the reply. A task that failed can leave a `zfs`
//! child it dropped alive past the hold: the registry then shows the failure while
//! `seed_failure` still answers `Busy`, and the reply shows the copy still receiving with
//! that failure, which Cloud ends the run on without it being answered. Forming a reply and
//! Cloud receiving it are separate steps, since the reply can be lost between them.
//!
//! The findings. Before PR #4576, the reply showed a failure that ended after the decision
//! and left it, so the operator's next run answered it again instead of starting over
//! (`Broken::answer_leaves_failure`). The PR's first fix took the failure when a reply
//! showed it, so a reply lost on its way left the run's resend to start the copy over, and
//! Cloud never saw the failure (`Broken::answer_takes_failure`). Its second kept a failure
//! for the newest run a reply showed it to, which a request of an ended run, or of a run
//! with a lower gen, could claim (`Broken::no_seed_fence`), and showed a failure in a
//! reply whose decision answered Busy (`Broken::reply_reads_registry`). The fix: A admits
//! no Seed older than one it admitted, and a reply shows only the failure its decision
//! answered and recorded. The third review found that fix's reply showing a failure whose
//! dropped child never exits as a copy still receiving, so the run waited out its patience
//! (`Broken::lingering_shows_receiving`). Now that reply shows the failure as still
//! receiving, and records no run for it.
//!
//! The properties: no copy failure is answered to two runs, a reply formed while a failed
//! copy's child lives shows that failure, and a copy starts over past a failure Cloud never
//! received only for a Seed of a later run than every one that reached A before the
//! failure. A cannot fence a run whose Seed has not reached it, so a request of an ended
//! run can take a failure while the next run's first Seed is still on its way. That run
//! then starts the copy over, and is answered what its own copy does.
//!
//! Out of scope: the marks, the floor and the later runs (the parent `volume_marks` model),
//! a Seed carrying no gen (it names no run, so the next one after an answer starts over),
//! a plugin restart (it empties the registry, so no failure is left to answer), and a
//! landing that fails.

use std::fmt::Write as _;

use stateright::{Model, Property};

use crate::harness::{self, Narrated, everywhere};

/// The receives the runs may spawn, numbered from 1. All but the last may fail.
const MAX_RECEIVES: u8 = 3;
/// The seed runs: the first, and the operator's runs after it.
const MAX_RUNS: u8 = 3;
/// The Seeds one run sends before it gives up: as many as the plugin tests send one run.
const MAX_SENDS: u8 = 4;
/// The replies lost on their way to Cloud, over all runs.
const MAX_LOSSES: u8 = 2;
/// The requests that outlive their attempt, over all runs.
const MAX_STRANDS: u8 = 1;
/// The gen Postgres took for a run before every other run's: the one a run admitted after
/// them may carry.
const FIRST_GEN: u8 = 1;

/// The seed run's row, as `seedVolume` ends it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Run {
    Polling,
    /// `endRun(.., "done", ..)` after a root answer.
    Done,
    /// `failed("03-failed", ..)` after a reply showing a copy failure.
    Failed,
    /// `failed("03-stuck", ..)` after a reply showing a copy still receiving with a
    /// failure: its task failed and a child it dropped is still alive.
    Stuck,
    /// `sendSwitch` failed the run on A's Stale refusal: a later run took `into`.
    Fenced,
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

/// Which runs a failure was answered to (`Answered`, transfer.rs), by gen.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Answered {
    Not,
    /// The newest gen it was answered to.
    To(u8),
}

/// `into`'s entry in `Receives`, for receive `n`. A landed entry reads as none, since
/// `seed_failure` answers `Ok(None)` for it and `status_of` reads it `Idle`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Receive {
    None,
    Running(u8),
    /// Receive `n`'s task failed. `settled` is false while a child it dropped outlives the
    /// hold: `status_of` reads the failure, and `seed_failure` answers `Busy`. Before the
    /// hold passes both read it running, which `Running` stands for.
    Failed {
        n: u8,
        answered: Answered,
        settled: bool,
    },
}

/// The arm of `seed_volume` (seed.rs) a request's decision took.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum SeedPath {
    /// `admit_seed` refused the request's gen, below the newest A admitted.
    Fenced,
    /// `SeedProgress::Copying`: `seed_failure` answered `Busy`.
    Copying,
    /// `SeedProgress::Failed`: `seed_failure` answered receive `n`'s failure.
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
    /// A slot whose pull still receives, with receive `n`'s failure: its task failed and a
    /// child it dropped outlives the hold (`VolumeStorage::receiving`, inspect.rs).
    Lingering(u8),
    /// The Stale refusal.
    Stale,
}

/// A request A holds the mutation for: the run that sent it, and whether it outlived its
/// attempt, so no reply reaches Cloud.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Request {
    run: u8,
    late: bool,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// Cloud sends the run's Seed, and A takes it under `admit_mutation`: `admit_seed`,
    /// `seed_progress`, then the arm `seed_volume` takes on it.
    Decide,
    /// Cloud sends the run's Seed, and it waits in transit past the attempt, which times
    /// out (`sendSwitch`).
    Strand,
    /// A takes the request that waited in transit, as `Decide` does.
    Late,
    /// A forms the reply: `seeded` reads the copy, the arm shows what its decision
    /// answered, and the mutation ends.
    Answer,
    /// `seedVolume` reads the reply (volume-run.inngest.ts).
    Deliver,
    /// The reply is lost, or arrives after the attempt timed out (`sendSwitch`).
    Lose,
    /// The running receive's task ends (`copy_snapshot`, seed.rs), failed when the
    /// receive or the send exits non-zero, with a child it dropped alive past the hold
    /// when `lingers`.
    End { failed: bool, lingers: bool },
    /// The child a failed task dropped exits (`Exit::held`, transfer.rs).
    Settle,
    /// The operator seeds the same point again after the run ended without a root: a new
    /// run, carrying the gen Postgres took first when `first_gen`.
    Retry { first_gen: bool },
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    runs: u8,
    run: Run,
    /// The run that carries `FIRST_GEN`, 0 if none does.
    first_gen_run: u8,
    /// The Seeds the current run sent.
    sends: u8,
    losses: u8,
    strands: u8,
    /// The run whose request waits in transit.
    stranded: Option<u8>,
    /// The newest gen `admit_seed` admitted for `into`, 0 if none.
    admitted: u8,
    into: Into,
    receive: Receive,
    spawned: u8,
    /// The arm the last decision took.
    path: Option<SeedPath>,
    /// A decision ran and its reply is not formed: the handler holds the mutation.
    handling: Option<Request>,
    /// A formed reply on its way to Cloud.
    in_flight: Option<Shown>,
    /// The newest gen of a Seed A took, fenced or not, 0 if none.
    taken: u8,
    /// `taken` when receive `n` failed, at index `n - 1`, 0 if it has not: the newest run
    /// A knew of then.
    failed_for: [u8; MAX_RECEIVES as usize],
    /// The run that received receive `n`'s failure, at index `n - 1`, 0 if none has.
    delivered_to: [u8; MAX_RECEIVES as usize],
    /// Receives whose failure a lost reply showed, one bit each.
    lost: u8,
    /// Receives whose failure a run ended `Stuck` on, one bit each.
    stuck: u8,
    /// A failure reached Cloud in a second run.
    delivered_twice: bool,
    /// A reply formed while a failed task's child outlived the hold showed no failure.
    silent: bool,
    /// A copy started over past a failure Cloud never received, for a Seed of a run A had
    /// taken a Seed of before the failure, or of an earlier run.
    hidden: bool,
    last: Option<Action>,
}

impl State {
    /// The gen run `run` carries: `FIRST_GEN` for the run that took it, and otherwise one
    /// above every gen before it, in the order the runs began.
    fn gen_of(&self, run: u8) -> u8 {
        if run == self.first_gen_run {
            FIRST_GEN
        } else {
            run + FIRST_GEN
        }
    }
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
    /// PR #4576's second fix: the reply shows every failure the registry reads, even one
    /// its decision answered `Busy`, and records the run for it.
    reply_reads_registry: bool,
    /// PR #4576's second fix: A admits a Seed whatever its gen, as the floor check does at
    /// an equal floor.
    no_seed_fence: bool,
    /// `Answered::superseded_by` reads the run a failure was answered to as later, so the
    /// run's own resend clears it.
    supersedes_its_own_run: bool,
    /// `seed_failure` records no run for a failure it answers.
    answer_records_no_run: bool,
    /// PR #4576's third reply: a reply to a Seed answered Busy shows the copy still
    /// receiving with no failure, though a child its failed task dropped outlives the hold.
    lingering_shows_receiving: bool,
}

impl Broken {
    /// The decision takes every failure it reads, as `take_failure` does.
    fn decision_takes(self) -> bool {
        self.answer_leaves_failure || self.answer_takes_failure
    }

    /// The reply shows a failure `status_of` reads, not only the one its decision
    /// answered.
    fn reply_reads(self) -> bool {
        self.answer_leaves_failure || self.answer_takes_failure || self.reply_reads_registry
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

const RETRIES: &[Action] = &[
    Action::Retry { first_gen: false },
    Action::Retry { first_gen: true },
];

const COVERAGE: &[Coverage] = &[
    Coverage {
        name: "a reply shows the failure its decision read",
        on: everywhere,
        omit: Some(Omit::Path(SeedPath::Failed(1))),
        reached: |_, state| {
            state.last == Some(Action::Answer)
                && state.path == Some(SeedPath::Failed(1))
                && state.in_flight == Some(Shown::Failure(1))
        },
    },
    Coverage {
        name: "a reply to a Seed answered Busy shows no failure the registry holds",
        on: everywhere,
        omit: Some(Omit::Path(SeedPath::Copying)),
        reached: |_, state| {
            state.last == Some(Action::Answer)
                && state.path == Some(SeedPath::Copying)
                && state.in_flight == Some(Shown::Slot)
                && matches!(state.receive, Receive::Failed { .. })
        },
    },
    Coverage {
        name: "a Seed answered Busy while a failed task's child outlives the hold",
        on: everywhere,
        omit: Some(Omit::Actions(&[Action::End {
            failed: true,
            lingers: true,
        }])),
        reached: |_, state| {
            state.last == Some(Action::Decide)
                && state.path == Some(SeedPath::Copying)
                && matches!(state.receive, Receive::Failed { settled: false, .. })
        },
    },
    Coverage {
        name: "a run ends on a failure whose dropped child outlives the hold",
        on: everywhere,
        omit: Some(Omit::Actions(&[Action::End {
            failed: true,
            lingers: true,
        }])),
        reached: |_, state| state.last == Some(Action::Deliver) && state.run == Run::Stuck,
    },
    Coverage {
        name: "a run after one ended on a dropped child's failure is answered it once the child exits",
        on: everywhere,
        omit: Some(Omit::Actions(&[Action::Settle])),
        reached: |_, state| {
            state.last == Some(Action::Answer)
                && state.runs > 1
                && matches!(state.in_flight, Some(Shown::Failure(n)) if state.stuck & bit(n) != 0)
        },
    },
    Coverage {
        name: "a resend after a lost reply shows the failure again",
        on: everywhere,
        omit: Some(Omit::Actions(&[Action::Lose, Action::Strand])),
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
        name: "a request that outlived its attempt is admitted while no later run seeded",
        on: everywhere,
        omit: Some(Omit::Actions(&[Action::Strand])),
        reached: |_, state| {
            state.last == Some(Action::Late)
                && state.path.is_some_and(|path| path != SeedPath::Fenced)
        },
    },
    Coverage {
        name: "a request that outlived its run is refused Stale",
        on: everywhere,
        omit: Some(Omit::Actions(&[Action::Strand])),
        reached: |_, state| {
            state.last == Some(Action::Late) && state.path == Some(SeedPath::Fenced)
        },
    },
    Coverage {
        name: "a run whose gen was taken before an ended run's ends on its Stale refusal",
        on: everywhere,
        omit: Some(Omit::Actions(&[Action::Retry { first_gen: true }])),
        reached: |_, state| state.last == Some(Action::Deliver) && state.run == Run::Fenced,
    },
    Coverage {
        name: "the seed run ends done after an earlier run ended without a root",
        on: everywhere,
        omit: Some(Omit::Actions(RETRIES)),
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

const DELIVERED_ONCE: &str = "no copy failure is answered to two runs";
const LINGERING_SHOWN: &str =
    "a reply formed while a failed copy's dropped child lives shows that failure";
const NEVER_HIDDEN: &str = "a copy starts over past a failure Cloud never received only for a Seed of a later run than every one that reached A before it";

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

    /// `Answered::superseded_by` (transfer.rs) for a Seed of gen `seed`.
    fn superseded(self, answered: Answered, seed: u8) -> bool {
        match answered {
            Answered::Not => false,
            Answered::To(to) if self.broken.supersedes_its_own_run => seed >= to,
            Answered::To(to) => seed > to,
        }
    }

    /// Notes a copy that starts over for a Seed of gen `seed` past a failure Cloud never
    /// received, when no run after `seed` had reached A before the failure.
    fn passes(state: &mut State, seed: u8) {
        let hidden =
            state
                .failed_for
                .iter()
                .zip(state.delivered_to)
                .any(|(&failed_for, delivered_to)| {
                    failed_for != 0 && delivered_to == 0 && seed <= failed_for
                });
        state.hidden |= hidden;
    }

    /// `admit_seed`, `seed_progress` and the arm `seed_volume` takes on it (seed.rs), for
    /// a request of run `run`. The root is read first, then the registry, then the slot's
    /// `fs`. `seed_failure` records the gen it answers a failure to.
    fn decide(self, state: &mut State, run: u8) -> SeedPath {
        let seed = state.gen_of(run);
        state.taken = state.taken.max(seed);
        if !self.broken.no_seed_fence {
            if seed < state.admitted {
                return SeedPath::Fenced;
            }
            state.admitted = seed;
        }
        if state.into == Into::Root {
            return SeedPath::Landed;
        }
        match state.receive {
            Receive::Running(_) | Receive::Failed { settled: false, .. } => {
                return SeedPath::Copying;
            }
            Receive::Failed { n, .. } if self.broken.decision_takes() => {
                state.receive = Receive::None;
                return SeedPath::Failed(n);
            }
            Receive::Failed { n, answered, .. } if !self.superseded(answered, seed) => {
                if !self.broken.answer_records_no_run && !self.broken.reply_reads_registry {
                    state.receive = Receive::Failed {
                        n,
                        answered: Self::and(answered, seed),
                        settled: true,
                    };
                }
                return SeedPath::Failed(n);
            }
            Receive::Failed { .. } => state.receive = Receive::None,
            Receive::None => {}
        }
        match state.into {
            Into::Root => unreachable!("the root was read first"),
            Into::Slot { holds_point: true } => {
                Self::passes(state, seed);
                state.into = Into::Root;
                SeedPath::Lands
            }
            Into::Slot { holds_point: false } => {
                Self::passes(state, seed);
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

    /// `Answered::and`: answered to `seed` too.
    fn and(answered: Answered, seed: u8) -> Answered {
        match answered {
            Answered::Not => Answered::To(seed),
            Answered::To(to) => Answered::To(to.max(seed)),
        }
    }

    /// `seeded` (seed.rs) reads the copy. The `Failed` arm's `answering` shows the failure
    /// its decision answered, and every other arm's reply goes through `receiving`, which
    /// shows a failure only while a child its task dropped outlives the hold, as still
    /// receiving, and records no run for it. Under `Broken::reply_reads`, the reply shows a
    /// failure `status_of` reads, and `seed_volume` records the run it showed it to.
    fn answer(self, state: &mut State, request: Request) -> Shown {
        let shown = match (state.path, state.receive, state.into) {
            (Some(SeedPath::Fenced), ..) => Shown::Stale,
            (Some(SeedPath::Failed(n)), ..) => Shown::Failure(n),
            (_, Receive::Failed { n, .. }, _) if self.broken.reply_reads() => Shown::Failure(n),
            (
                _,
                Receive::Failed {
                    n, settled: false, ..
                },
                _,
            ) if !self.broken.lingering_shows_receiving => Shown::Lingering(n),
            (.., Into::Root) => Shown::Root,
            (.., Into::Absent | Into::Slot { .. }) => Shown::Slot,
        };
        state.silent |=
            shown == Shown::Slot && matches!(state.receive, Receive::Failed { settled: false, .. });
        if self.broken.reply_reads()
            && let (
                Shown::Failure(shown),
                Receive::Failed {
                    n,
                    answered,
                    settled,
                },
            ) = (shown, state.receive)
            && shown == n
        {
            state.receive = if self.broken.answer_takes_failure {
                Receive::None
            } else if self.broken.answer_records_no_run || self.broken.answer_leaves_failure {
                Receive::Failed {
                    n,
                    answered,
                    settled,
                }
            } else {
                Receive::Failed {
                    n,
                    answered: Self::and(answered, state.gen_of(request.run)),
                    settled,
                }
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

    fn take(self, state: &mut State, request: Request) {
        state.path = Some(self.decide(state, request.run));
        state.handling = Some(request);
    }

    fn step(self, state: &mut State, action: Action) {
        match action {
            Action::Decide => {
                state.sends += 1;
                let request = Request {
                    run: state.runs,
                    late: false,
                };
                self.take(state, request);
            }
            Action::Strand => {
                state.sends += 1;
                state.strands += 1;
                state.stranded = Some(state.runs);
                Self::out_of_sends(state);
            }
            Action::Late => {
                let run = state
                    .stranded
                    .take()
                    .expect("only a stranded request is late");
                self.take(state, Request { run, late: true });
            }
            Action::Answer => {
                let request = state.handling.take().expect("only a taken request answers");
                let shown = self.answer(state, request);
                if request.late {
                    if let Shown::Failure(n) = shown {
                        state.lost |= bit(n);
                    }
                } else {
                    state.in_flight = Some(shown);
                }
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
                Some(Shown::Lingering(n)) => {
                    state.stuck |= bit(n);
                    state.run = Run::Stuck;
                }
                Some(Shown::Stale) => state.run = Run::Fenced,
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
            Action::End { failed, lingers } => {
                let Receive::Running(n) = state.receive else {
                    unreachable!("only a running receive ends");
                };
                if failed {
                    state.receive = Receive::Failed {
                        n,
                        answered: Answered::Not,
                        settled: !lingers,
                    };
                    if let Some(failed_for) = of(&mut state.failed_for, n) {
                        *failed_for = state.taken;
                    }
                } else {
                    state.receive = Receive::None;
                    state.into = Into::Slot { holds_point: true };
                }
            }
            Action::Settle => {
                if let Receive::Failed { settled, .. } = &mut state.receive {
                    *settled = true;
                }
            }
            Action::Retry { first_gen } => {
                state.runs += 1;
                state.run = Run::Polling;
                state.sends = 0;
                if first_gen {
                    state.first_gen_run = state.runs;
                }
            }
        }
    }

    fn omitted(&self, action: Action, after: &State) -> bool {
        match self.omit {
            None => false,
            Some(Omit::Actions(actions)) => actions.contains(&action),
            Some(Omit::Path(path)) => {
                matches!(action, Action::Decide | Action::Late) && after.path == Some(path)
            }
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
            first_gen_run: 0,
            sends: 0,
            losses: 0,
            strands: 0,
            stranded: None,
            admitted: 0,
            into: Into::Absent,
            receive: Receive::None,
            spawned: 0,
            path: None,
            handling: None,
            in_flight: None,
            taken: 0,
            failed_for: [0; MAX_RECEIVES as usize],
            delivered_to: [0; MAX_RECEIVES as usize],
            lost: 0,
            stuck: 0,
            delivered_twice: false,
            silent: false,
            hidden: false,
            last: None,
        }]
    }

    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        if state.handling.is_some() {
            actions.push(Action::Answer);
        } else {
            if state.in_flight.is_some() {
                actions.push(Action::Deliver);
                if state.losses < MAX_LOSSES {
                    actions.push(Action::Lose);
                }
            } else if state.run == Run::Polling {
                actions.push(Action::Decide);
                if state.strands < MAX_STRANDS {
                    actions.push(Action::Strand);
                }
            } else if state.run != Run::Done && state.runs < MAX_RUNS {
                actions.push(Action::Retry { first_gen: false });
                if state.first_gen_run == 0 {
                    actions.push(Action::Retry { first_gen: true });
                }
            }
            if state.stranded.is_some() {
                actions.push(Action::Late);
            }
        }
        match state.receive {
            Receive::Running(n) => {
                actions.push(Action::End {
                    failed: false,
                    lingers: false,
                });
                if n < MAX_RECEIVES {
                    actions.push(Action::End {
                        failed: true,
                        lingers: false,
                    });
                    actions.push(Action::End {
                        failed: true,
                        lingers: true,
                    });
                }
            }
            Receive::Failed { settled: false, .. } => actions.push(Action::Settle),
            Receive::Failed { settled: true, .. } | Receive::None => {}
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
            Property::always(LINGERING_SHOWN, |_, state: &State| !state.silent),
            Property::always(NEVER_HIDDEN, |_, state: &State| !state.hidden),
        ];
        properties.extend(harness::sometimes(COVERAGE, self));
        properties
    }
}

/// `action` in the code's words, read with `after` for the arm it took.
fn reads_as(action: Action, after: &State) -> String {
    match action {
        Action::Decide | Action::Late => format!(
            "A: {}run {}'s Volume.Seed at gen {} under admit_mutation; admit_seed, then seed_progress reads seed_failure: {:?} (seed.rs seed_volume)",
            if action == Action::Late {
                "the stranded request: "
            } else {
                ""
            },
            after.handling.map_or(0, |request| request.run),
            after
                .handling
                .map_or(0, |request| after.gen_of(request.run)),
            after.path
        ),
        Action::Strand => {
            "Cloud's Seed waits in transit past its attempt, which times out (sendSwitch)"
                .to_owned()
        }
        Action::Answer => format!(
            "A: seeded reads the copy and forms {:?}{}",
            after.in_flight,
            if after.in_flight.is_none() {
                ", which no attempt waits for"
            } else {
                ""
            }
        ),
        Action::Deliver => "seedVolume reads the reply (volume-run.inngest.ts)".to_owned(),
        Action::Lose => {
            "the reply is lost or arrives after the attempt timed out (sendSwitch)".to_owned()
        }
        Action::End {
            failed: true,
            lingers,
        } => format!(
            "the receive task ends failed, outside the mutation{} (seed.rs copy_snapshot)",
            if lingers {
                ", leaving a zfs child alive past the hold"
            } else {
                ""
            }
        ),
        Action::End { failed: false, .. } => {
            "the receive task ends: fs holds the point (seed.rs copy_snapshot)".to_owned()
        }
        Action::Settle => "the zfs child the failed task dropped exits (transfer.rs)".to_owned(),
        Action::Retry { first_gen: false } => {
            "the operator seeds the same point again: a new run".to_owned()
        }
        Action::Retry { first_gen: true } => {
            "a run Postgres gave the first gen, before the earlier runs', is admitted now"
                .to_owned()
        }
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
                "{}. {}\n    run {} {:?} sends={} admitted={} | into={:?} receive={:?} failed_for={:?} delivered_to={:?}\n",
                number + 1,
                reads_as(*action, after),
                after.runs,
                after.run,
                after.sends,
                after.admitted,
                after.into,
                after.receive,
                after.failed_for,
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
    use crate::harness::{breaks, breaks_where, check, last, took_action};

    fn fixed() -> SeedFailure {
        SeedFailure {
            broken: Broken::default(),
            omit: None,
        }
    }

    fn broken(broken: Broken) -> SeedFailure {
        SeedFailure { broken, ..fixed() }
    }

    fn retried(path: &[(State, Option<Action>)]) -> bool {
        RETRIES.iter().any(|retry| took_action(path, *retry))
    }

    fn lingered(path: &[(State, Option<Action>)]) -> bool {
        took_action(
            path,
            Action::End {
                failed: true,
                lingers: true,
            },
        )
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
        assert!(retried(&path), "a second run read it");
        assert!(
            path.iter().any(|(before, action)| before.handling.is_some()
                && matches!(action, Some(Action::End { failed: true, .. }))),
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
        let path = breaks_where(model, NEVER_HIDDEN, |path| took_action(path, Action::Lose));
        assert!(!retried(&path), "the run that lost it started over");
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
        breaks_where(model, NEVER_HIDDEN, |path| took_action(path, Action::Lose));
    }

    /// A decision answers a failure and records no run for it, so the next run reads it as
    /// never answered and is shown it again.
    #[test]
    fn a_seed_records_the_run_it_answered_a_failure_to() {
        let model = broken(Broken {
            answer_records_no_run: true,
            ..Broken::default()
        });
        let path = breaks(model, DELIVERED_ONCE);
        assert!(retried(&path), "a second run read it");
    }

    /// PR #4576's second fix, which the second review found. A failed task's child outlives
    /// the hold, so the registry shows the failure while `seed_failure` answers Busy. The
    /// reply to that Seed shows the failure and records its run, and the next run's Seed,
    /// answered Busy too, is shown it again. It is the only path that shows a failure to
    /// two runs under that reply.
    #[test]
    fn a_reply_to_a_seed_answered_busy_shows_no_failure() {
        let model = broken(Broken {
            reply_reads_registry: true,
            ..Broken::default()
        });
        let path = breaks(model, DELIVERED_ONCE);
        assert!(lingered(&path), "a failed task's child outlived the hold");
        assert!(retried(&path), "a second run read it");
    }

    /// PR #4576's third reply, which the third review found. A failed task's child outlives
    /// the hold, and every Seed is answered Busy with the copy still receiving, so the run
    /// waits out its patience and never learns the copy failed.
    #[test]
    fn a_reply_while_a_failed_copys_child_lives_shows_its_failure() {
        let model = broken(Broken {
            lingering_shows_receiving: true,
            ..Broken::default()
        });
        let path = breaks(model, LINGERING_SHOWN);
        assert!(lingered(&path), "a failed task's child outlived the hold");
        assert!(
            check(model).discovery(DELIVERED_ONCE).is_none()
                && check(model).discovery(NEVER_HIDDEN).is_none(),
            "it keeps the other properties"
        );
    }

    /// PR #4576's second fix, which the second review found. A request of run 1 waits in
    /// transit while run 1 gives up and run 2 starts a copy that fails. The request reaches
    /// A, is answered the failure though no attempt waits for it, and records run 1. Run
    /// 2's next Seed reads the failure as answered to an earlier run and starts the copy
    /// over: run 2 never sees its failure.
    #[test]
    fn a_seed_of_an_ended_run_is_refused() {
        let model = broken(Broken {
            no_seed_fence: true,
            ..Broken::default()
        });
        breaks_where(model, NEVER_HIDDEN, |path| {
            path.iter().any(|(before, action)| {
                *action == Some(Action::Late)
                    && before.stranded.is_some_and(|run| run < before.runs)
            })
        });
    }

    /// PR #4576's second fix, which the second review found. A run that carries the first
    /// gen is admitted after a run with a later gen failed and was shown its failure. Its
    /// Seed reads the failure as answered to no later run than its own, and is shown it
    /// again.
    #[test]
    fn a_run_with_an_earlier_gen_than_an_ended_run_is_refused() {
        let model = broken(Broken {
            no_seed_fence: true,
            ..Broken::default()
        });
        let path = breaks_where(model, DELIVERED_ONCE, |path| {
            took_action(path, Action::Retry { first_gen: true }) && !took_action(path, Action::Late)
        });
        assert!(!lingered(&path));
    }

    /// The orders the plugin tests force on fake ZFS (seed_tests.rs). In
    /// `a_failed_seed_copy_is_answered_to_its_run_and_the_next_run_starts_over`, a resend
    /// finds the receive running, it fails before that resend's reply, and that reply shows
    /// no failure under the fix. The run's next resend is shown it, and so is the one after
    /// a lost reply. Under the first fix, the reply takes the failure and a lost reply
    /// leaves the next resend to start the copy over. Delivered, the operator's next run
    /// starts over under the fix and is shown it again under the old reply.
    #[test]
    fn a_failure_that_ends_during_a_resent_seed_is_shown_to_its_run_only() {
        const RESENT: &[Action] = &[
            Action::Decide,
            Action::Answer,
            Action::Deliver,
            Action::Decide,
            Action::End {
                failed: true,
                lingers: false,
            },
            Action::Answer,
        ];
        let path = harness::follow(fixed(), RESENT);
        assert_eq!(last(&path).in_flight, Some(Shown::Slot));
        let shown = [
            RESENT,
            &[
                Action::Deliver,
                Action::Decide,
                Action::Answer,
                Action::Lose,
                Action::Decide,
                Action::Answer,
            ],
        ]
        .concat();
        let path = harness::follow(fixed(), &shown);
        assert_eq!(last(&path).in_flight, Some(Shown::Failure(1)));
        let retried = [
            &shown[..],
            &[
                Action::Deliver,
                Action::Retry { first_gen: false },
                Action::Decide,
            ],
        ]
        .concat();
        let path = harness::follow(fixed(), &retried);
        assert_eq!(last(&path).path, Some(SeedPath::Restarts));
        assert!(!last(&path).hidden);

        let first_fix = broken(Broken {
            answer_takes_failure: true,
            ..Broken::default()
        });
        let path = harness::follow(
            first_fix,
            &[RESENT, &[Action::Lose, Action::Decide]].concat(),
        );
        assert_eq!(last(&path).path, Some(SeedPath::Restarts));
        assert!(last(&path).hidden);

        let old = broken(Broken {
            answer_leaves_failure: true,
            ..Broken::default()
        });
        let path = harness::follow(
            old,
            &[
                RESENT,
                &[
                    Action::Deliver,
                    Action::Retry { first_gen: false },
                    Action::Decide,
                    Action::Answer,
                    Action::Deliver,
                ],
            ]
            .concat(),
        );
        assert!(last(&path).delivered_twice);
    }

    /// The orders the two plugin tests of PR #4576's second review force on fake ZFS
    /// (seed_tests.rs). In
    /// `a_seed_of_an_earlier_run_is_refused_and_leaves_the_failure_to_the_later_run`, run
    /// 2's copy fails before it is shown, and a request of run 1 that outlived it arrives:
    /// it is refused, and run 2 is shown the failure. In
    /// `a_seed_answered_busy_shows_a_failure_as_receiving_while_a_dropped_child_lives`, the
    /// Seed answered Busy shows the failure as still receiving and records no run, so Cloud
    /// ends the run and the next run, once the child exits, is answered it.
    #[test]
    fn the_second_review_orders_end_as_the_plugin_tests_expect() {
        // Run 1's first request is stranded. The run starts the copy and gives up once it
        // sent all it may, and run 2's Seed finds the copy still running before it fails.
        let stranded = [
            Action::Strand,
            Action::Decide,
            Action::Answer,
            Action::Deliver,
            Action::Decide,
            Action::Answer,
            Action::Deliver,
            Action::Decide,
            Action::Answer,
            Action::Deliver,
            Action::Retry { first_gen: false },
            Action::Decide,
            Action::End {
                failed: true,
                lingers: false,
            },
            Action::Answer,
            Action::Deliver,
            Action::Late,
        ];
        let path = harness::follow(fixed(), &stranded);
        assert_eq!(last(&path).path, Some(SeedPath::Fenced));
        let path = harness::follow(
            fixed(),
            &[&stranded[..], &[Action::Answer, Action::Decide]].concat(),
        );
        assert_eq!(last(&path).path, Some(SeedPath::Failed(1)));
        let path = harness::follow(
            broken(Broken {
                no_seed_fence: true,
                ..Broken::default()
            }),
            &[&stranded[..], &[Action::Answer, Action::Decide]].concat(),
        );
        assert_eq!(last(&path).path, Some(SeedPath::Restarts));
        assert!(last(&path).hidden);

        let busy = [
            Action::Decide,
            Action::Answer,
            Action::Deliver,
            Action::End {
                failed: true,
                lingers: true,
            },
            Action::Decide,
            Action::Answer,
        ];
        let path = harness::follow(fixed(), &busy);
        assert_eq!(last(&path).path, Some(SeedPath::Copying));
        assert_eq!(last(&path).in_flight, Some(Shown::Lingering(1)));
        let path = harness::follow(
            fixed(),
            &[
                &busy[..],
                &[
                    Action::Deliver,
                    Action::Settle,
                    Action::Retry { first_gen: false },
                    Action::Decide,
                    Action::Answer,
                ],
            ]
            .concat(),
        );
        assert_eq!(last(&path).in_flight, Some(Shown::Failure(1)));
        let path = harness::follow(
            broken(Broken {
                lingering_shows_receiving: true,
                ..Broken::default()
            }),
            &busy,
        );
        assert_eq!(last(&path).in_flight, Some(Shown::Slot));
        let path = harness::follow(
            broken(Broken {
                reply_reads_registry: true,
                ..Broken::default()
            }),
            &busy,
        );
        assert_eq!(last(&path).in_flight, Some(Shown::Failure(1)));
    }
}
