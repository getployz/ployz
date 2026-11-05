//! One Deployment's run, from the runner that claimed it, against the Config Store that
//! holds its status. The runner (`run_deployment_expecting` and `Run`, ployz's
//! sdk/store_runner.rs:163-222, :702-706) dials the Servers under `connecting`, then
//! plans or builds and prepares, executes, deletes the Volumes the Deploy removes and
//! fills ingress, each piece of work under `renewing` (:733-746, :764-768, :778,
//! :807-809). While it awaits that work, `connecting` and `renewing` call `status()`
//! every `CANCEL_POLL`, 2 s (:40, :1190-1198, :1208-1220), and the work can end before
//! the next call (:1199, :1211). `execute` also calls it once after executing (:771) and
//! once before `fill_ingress` (:782). `status()` is `record(RunEvidence::Alive)`
//! (:1373-1377), and each site compares the answer with its own set of statuses (:1195,
//! :1214-1217, :1368, :782). Meanwhile a user cancels the Deployment (ployz-store
//! deployment/lifecycle.rs:1472), a newer Deployment of the Environment is claimed and
//! sets it Unknown (lifecycle.rs:1320-1332), or forgetting the Cluster cancels it without
//! waiting on the runner (lifecycle.rs:1503, teardown.rs:458).
//!
//! The finding: `lost_to_another_runner` is the one read after executing that acts on a
//! takeover (store_runner.rs:771, :1367-1369), and `remove_volumes`, `fill_ingress` and
//! the `Executed` record come after it (:776-791). A takeover that lands after it leaves
//! the Deployment Unknown, and `record` then refuses an `Executed` that is not Interrupted
//! (lifecycle.rs:1619-1622). The run has deleted Volumes, and neither its outcome nor the
//! deletions are kept. No read has to fail for this.
//!
//! The second finding has two causes. The sites read `Err` as keep: once
//! `cancel_in_flight` ended the Deployment, `status()` answers `Err` ("This Deployment
//! already ended", deployment/mod.rs:1663-1665, :1738-1740), and `renewing` keeps the work
//! in hand going (store_runner.rs:1213-1219). And `connecting` and the gate before
//! `fill_ingress` read `Ok(Unknown)` as keep (:1195, :782).
//!
//! The fix variant changes the runner and the store. The runner classifies every read into
//! one `Standing`, latches Lost and Ended, and stops on either in `connecting`, `renewing`,
//! the read after executing and the gate, which `NO_WORK_AFTER_END` needs. It sends the
//! `Executed` record as the code does. The store's `record` turns a non-Interrupted
//! `Executed` for an Unknown Deployment into Interrupted in the same transaction, where
//! the code refuses it (lifecycle.rs:1619-1622), which `TAKEOVER_KEEPS_OUTCOME` needs. Its
//! projection shows the deletions of an Interrupted outcome, which `DELETIONS_KEPT` needs,
//! where the code reads them NotAttempted for any outcome but a Success
//! (lifecycle.rs:2164-2168), and an Interrupted outcome projects to Failed (ployz-core
//! config/runtime_outcome.rs:208-220).
//!
//! Each store call is one step, since `record`, `claim`, `cancel` and `cancel_in_flight`
//! each take one transaction (lifecycle.rs:1512-1517, :1232-1237, :1463-1467,
//! :1487-1490). A record that follows a read, or the end of a piece of work, is one step
//! with it, except the `Executed` record.
//!
//! Out of scope: a takeover or `cancel_in_flight` between a read and the record merged
//! with it, build's records, warnings, the lease lapsing, a second claim by the same
//! runner, Cloud's `abandonDeployment`, a dial that fails, a store call other than
//! `status()` failing, and an abort that comes too late to stop its work.

use std::fmt::Write as _;

use stateright::{Model, Property};

/// The Deployment's status as `record` reads it. `stored` (deployment/mod.rs) remaps a
/// lapsed lease to Unknown, which the model leaves out.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Status {
    Running,
    Cancelling,
    /// In the model, `claim`'s takeover (lifecycle.rs:1320-1332), or the run's own
    /// `Abandoned` after a preview (lifecycle.rs:1901).
    Unknown,
    /// Applied, Failed or Cancelled: `end` (lifecycle.rs), or `cancel_in_flight`.
    Ended,
}

/// What the store keeps as the run's outcome.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Outcome {
    NotExecuted,
    /// `RunEvidence::Executed`: whether its outcome is Interrupted, and whether `removed`
    /// names the Volumes the run deleted (store_runner.rs:786-790). One that is not
    /// Interrupted and names deletions is a Success, since only a Success deletes
    /// (store_runner.rs:776-781).
    Executed {
        interrupted: bool,
        removed: bool,
    },
}

/// The Deployment's row (`config_deployment`), as far as the run's records read it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Store {
    status: Status,
    /// `run.preview`, set by the `Prepared` record.
    preview: bool,
    /// `run.outcome`.
    outcome: Option<Outcome>,
    /// The deleted Volumes' nodes read Removed. `removed_volume` reads the deletions only
    /// for a Success outcome, and NotAttempted for any other (lifecycle.rs:2152-2174).
    shows_removed: bool,
}

/// What `status()` (store_runner.rs) returned: `record(RunEvidence::Alive)`'s summary
/// status, or its error.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Read {
    Running,
    Cancelling,
    Unknown,
    /// `Err(conflict "This Deployment already ended")` from `running` (deployment/mod.rs).
    Ended,
    /// `Err(unavailable)` from `store_call` (sdk/store_call.rs): the Store was busy, slow
    /// or closed.
    Unavailable,
}

impl Read {
    /// The store no longer takes this run as running: `running` (deployment/mod.rs)
    /// refuses Unknown and ended Deployments alike.
    fn over(self) -> bool {
        matches!(self, Self::Unknown | Self::Ended)
    }
}

/// Whether a `status()` call reaches the store.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Call {
    Answers,
    Fails,
}

/// How executing ended: `executing` (store_runner.rs) returned this outcome.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Exec {
    Success,
    Failed,
}

/// Work on the Machines.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Work {
    /// `session.plan`, or `build` then `prepare` (store_runner.rs `execute`).
    Prepare,
    /// `executing` (store_runner.rs `execute`).
    Execute,
    /// `remove_volumes` (store_runner.rs `execute`).
    RemoveVolumes,
    /// `fill_ingress` (store_runner.rs).
    FillIngress,
}

/// Where the runner is in `run_deployment_expecting` and `execute` (store_runner.rs).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Phase {
    /// `connecting(dial(..))`.
    Connecting,
    /// Planning, or building and preparing, under `renewing`; `aborted` once its abort ran.
    Preparing { aborted: bool },
    /// `executing` under `renewing`.
    Executing { aborted: bool },
    /// Executing ended; `lost_to_another_runner` is next.
    Checking(Exec),
    /// `remove_volumes` under `renewing` with the no-op abort `|| ()`.
    Removing,
    /// The gate before `fill_ingress`: `status().ok() != Some(Cancelling)`.
    Gating(Exec),
    /// `fill_ingress`, its `renewing` stopping it with `stop.cancel()`.
    Filling { exec: Exec, stopped: bool },
    /// `record(RunEvidence::Executed { .. })` is next.
    Recording(Exec),
    /// The run returned.
    Returned,
}

impl Phase {
    /// The work `renewing` wraps in this phase, and whether its abort already ran.
    fn renewing(self) -> Option<(Work, bool)> {
        match self {
            Self::Preparing { aborted } => Some((Work::Prepare, aborted)),
            Self::Executing { aborted } => Some((Work::Execute, aborted)),
            Self::Removing => Some((Work::RemoveVolumes, false)),
            Self::Filling { stopped, .. } => Some((Work::FillIngress, stopped)),
            Self::Connecting
            | Self::Checking(_)
            | Self::Gating(_)
            | Self::Recording(_)
            | Self::Returned => None,
        }
    }

    fn abort(&mut self) {
        match self {
            Self::Preparing { aborted } | Self::Executing { aborted } => *aborted = true,
            Self::Filling { stopped, .. } => *stopped = true,
            Self::Connecting
            | Self::Checking(_)
            | Self::Removing
            | Self::Gating(_)
            | Self::Recording(_)
            | Self::Returned => {}
        }
    }
}

/// The fix's view of the Deployment, latched from every read: the first `Ok(Unknown)` sets
/// Lost and the first ended `Err` sets Ended, and no later read changes either
/// (`Standing::after`). `the_fix_needs_lost_to_stick` shows what a Lost that does not stick
/// lets through.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Standing {
    Keep,
    Lost,
    Ended,
}

impl Standing {
    fn after(self, read: Read) -> Self {
        match (self, read) {
            (Self::Keep, Read::Unknown) => Self::Lost,
            (Self::Keep, Read::Ended) => Self::Ended,
            (standing, _) => standing,
        }
    }
}

/// What `record` answered to an `Executed`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Answer {
    Kept,
    /// The fix: the Deployment is Unknown and the `Executed` not Interrupted, so `record`
    /// kept it as Interrupted, where the code refuses it (lifecycle.rs:1619-1622).
    Converted,
    /// `running` refused it: the Deployment is Unknown (lifecycle.rs `record`, the
    /// `taken_over` check).
    RefusedUnknown,
    /// `finish` refused it through `running`: the Deployment ended.
    RefusedEnded,
}

/// The last `Executed` the runner sent.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Sent {
    interrupted: bool,
    removed: bool,
    answer: Answer,
}

/// The branch the last runner step took.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Branch {
    /// `connecting` read Cancelling and the run records "Cancelled before connecting".
    ConnectingCancelled,
    /// A `renewing` tick read Cancelling or Unknown (or, in the fix, Ended) and aborted.
    Aborts,
    /// The work `renewing` aborted ended, and the run records `NotExecuted`.
    Unprepared,
    /// `record(Prepared)` failed and `?` returned its error.
    PreparedRefused,
    /// `record(Prepared)`'s summary read Cancelling: "Cancelled before it executed".
    PreparedCancelling,
    /// `record(Prepared)` was kept and executing starts.
    Executes,
    /// The session ended mid-execution and the run records `Abandoned`.
    Abandons,
    /// `lost_to_another_runner` read Unknown and the run records Interrupted.
    CheckFindsLost,
    /// The read after executing let the run go on.
    CheckGoesOn,
    /// The fix: a `renewing` tick during `remove_volumes` latched Lost or Ended.
    Latches,
    /// The gate let `fill_ingress` start.
    Fills,
    /// The gate skipped `fill_ingress`.
    GateSkips,
    /// The run sent `Executed` with its outcome.
    Records,
    /// The fix: `record` kept the `Executed` as Interrupted.
    Converted,
    /// The fix: the latch said the run is over, so the run returned without more work.
    StopsOver,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Action {
    /// `forget_cluster` (ployz-store teardown.rs) runs `cancel_in_flight`, which sets
    /// the Deployment Cancelled without waiting on the runner.
    CancelInFlight,
    /// Another Deployment of the Environment is claimed: `claim`'s UPDATE sets this one
    /// unknown and leaves its runner column.
    Takeover,
    /// A user's `cancel` moves Running to Cancelling.
    Cancel,
    /// The dial completes and `connecting` returns the session.
    Connected,
    /// Planning, or building and preparing, ends.
    Prepare,
    /// `executing` returns this outcome.
    Executed(Exec),
    /// `executing` returns `Err`: the session ended mid-execution.
    SessionEnds,
    /// `lost_to_another_runner`: one `status()` read.
    Check(Call),
    /// `remove_volumes` returns the Volumes it deleted.
    Removed,
    /// The gate's `status()` read.
    Gate(Call),
    /// `fill_ingress` ends.
    Filled,
    /// `record(RunEvidence::Executed { .. })` with the outcome.
    Record,
    /// `connecting`'s poll tick: one `status()` read.
    Poll(Call),
    /// `renewing`'s poll tick: one `status()` read.
    Tick(Call),
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    store: Store,
    phase: Phase,
    /// `remove_volumes` deleted the Volumes the Deploy removes.
    removed: bool,
    /// `status()` calls that failed.
    failed: u8,
    /// What the last `status()` read, or `record(Prepared)`'s summary, said.
    last_read: Option<Read>,
    /// Some read said the run is over.
    told_over: bool,
    /// The fix's latch; `Keep` throughout the code as it is.
    standing: Standing,
    /// The first work started, or kept on by a `renewing` tick, after a read said the run
    /// is over. Deleting Volumes is kept on, since its abort is a no-op by design.
    after_over: Option<Work>,
    /// The first work other than deleting Volumes started, or kept on by a `renewing`
    /// tick, while the last read said Cancelling.
    after_cancel: Option<Work>,
    /// The last `Executed` sent.
    sent: Option<Sent>,
    took: Option<Branch>,
    last: Option<Action>,
}

/// A guard taken out, to show the check catches what it prevents. The first six are
/// guards the code has; the last three are parts of the fix.
#[derive(Clone, Copy, Debug, Default)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag removes one independent guard"
)]
struct Broken {
    /// `connecting` (store_runner.rs) dials on after reading Cancelling.
    connecting_ignores_cancel: bool,
    /// `renewing` (store_runner.rs) never calls `abort`.
    renewing_never_aborts: bool,
    /// `execute` (store_runner.rs) skips the Cancelling check on `record(Prepared)`'s
    /// summary.
    prepared_ignores_cancel: bool,
    /// `lost_to_another_runner` (store_runner.rs) reads Unknown as keep.
    check_ignores_lost: bool,
    /// The gate before `fill_ingress` (store_runner.rs `execute`) fills on Cancelling.
    gate_ignores_cancel: bool,
    /// `record` (lifecycle.rs) skips `running` on a non-Interrupted `Executed` for an
    /// Unknown Deployment.
    store_takes_any_outcome: bool,
    /// The fix's latch drops Lost on a later read that says the run is not over.
    lost_does_not_stick: bool,
    /// The fix's `record` refuses a non-Interrupted `Executed` for an Unknown Deployment,
    /// as the code does (lifecycle.rs:1619-1622), where the fix keeps it as Interrupted.
    store_refuses_late_executed: bool,
    /// The fix's projection reads the deletions of an Interrupted outcome NotAttempted, as
    /// the code does (lifecycle.rs:2164-2168).
    projection_hides_interrupted_deletions: bool,
}

/// What a coverage check takes out of the model, to show the `sometimes` naming it is
/// reached no other way.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Omit {
    /// These actions are never taken.
    Actions(&'static [Action]),
    /// No runner step takes this branch.
    Branch(Branch),
}

/// One `sometimes`: a branch the code handles, read from the state that took it, and
/// what to take out of the model to show nothing else reaches it.
struct Coverage {
    name: &'static str,
    /// Reached only with the fix.
    fix_only: bool,
    omit: Omit,
    reached: fn(&RunnerStanding, &State) -> bool,
}

/// The last step took `branch`.
fn took(state: &State, branch: Branch) -> bool {
    state.took == Some(branch)
}

/// Each branch the bounds reach, once. `every_sometimes_needs_what_it_names` takes each
/// row's `omit` out and expects its `name` unreached.
const COVERAGE: &[Coverage] = &[
    Coverage {
        name: "connecting reads a cancel: Cancelled before connecting",
        fix_only: false,
        omit: Omit::Branch(Branch::ConnectingCancelled),
        reached: |_, state| took(state, Branch::ConnectingCancelled),
    },
    Coverage {
        name: "renewing aborts the prepare",
        fix_only: false,
        omit: Omit::Branch(Branch::Aborts),
        reached: |_, state| {
            took(state, Branch::Aborts) && state.phase == (Phase::Preparing { aborted: true })
        },
    },
    Coverage {
        name: "renewing aborts executing",
        fix_only: false,
        omit: Omit::Branch(Branch::Aborts),
        reached: |_, state| {
            took(state, Branch::Aborts) && state.phase == (Phase::Executing { aborted: true })
        },
    },
    Coverage {
        name: "renewing stops fill_ingress",
        fix_only: false,
        omit: Omit::Branch(Branch::Aborts),
        reached: |_, state| {
            took(state, Branch::Aborts)
                && matches!(state.phase, Phase::Filling { stopped: true, .. })
        },
    },
    Coverage {
        name: "an aborted prepare records NotExecuted",
        fix_only: false,
        omit: Omit::Branch(Branch::Unprepared),
        reached: |_, state| {
            took(state, Branch::Unprepared) && state.store.outcome == Some(Outcome::NotExecuted)
        },
    },
    Coverage {
        name: "record(Prepared) refuses a run that is over",
        fix_only: false,
        omit: Omit::Branch(Branch::PreparedRefused),
        reached: |_, state| took(state, Branch::PreparedRefused),
    },
    Coverage {
        name: "record(Prepared) reads a cancel: Cancelled before it executed",
        fix_only: false,
        omit: Omit::Branch(Branch::PreparedCancelling),
        reached: |_, state| took(state, Branch::PreparedCancelling),
    },
    Coverage {
        name: "the session ends mid-execution and Abandoned leaves the run Unknown",
        fix_only: false,
        omit: Omit::Branch(Branch::Abandons),
        reached: |_, state| took(state, Branch::Abandons) && state.store.status == Status::Unknown,
    },
    Coverage {
        name: "lost_to_another_runner reads the takeover and the run records Interrupted",
        fix_only: false,
        omit: Omit::Branch(Branch::CheckFindsLost),
        reached: |_, state| {
            took(state, Branch::CheckFindsLost) && state.last_read == Some(Read::Unknown)
        },
    },
    Coverage {
        name: "the read after executing fails and the run goes on",
        fix_only: false,
        omit: Omit::Actions(&[Action::Check(Call::Fails)]),
        reached: |_, state| {
            took(state, Branch::CheckGoesOn) && state.last_read == Some(Read::Unavailable)
        },
    },
    Coverage {
        name: "the gate skips fill_ingress on a cancel",
        fix_only: false,
        omit: Omit::Branch(Branch::GateSkips),
        reached: |_, state| {
            took(state, Branch::GateSkips) && state.last_read == Some(Read::Cancelling)
        },
    },
    Coverage {
        name: "the store keeps a Success and shows the deleted Volumes removed",
        fix_only: false,
        omit: Omit::Actions(&[Action::Removed]),
        reached: |_, state| {
            took(state, Branch::Records)
                && state.store.outcome
                    == Some(Outcome::Executed {
                        interrupted: false,
                        removed: true,
                    })
                && state.store.shows_removed
        },
    },
    Coverage {
        name: "record refuses the Executed of a run cancel_in_flight ended",
        fix_only: false,
        omit: Omit::Actions(&[Action::CancelInFlight]),
        reached: |_, state| {
            took(state, Branch::Records)
                && state
                    .sent
                    .is_some_and(|sent| sent.answer == Answer::RefusedEnded)
        },
    },
    Coverage {
        name: "cancel_in_flight ends the run under its runner",
        fix_only: false,
        omit: Omit::Actions(&[Action::CancelInFlight]),
        reached: |_, state| {
            state.last == Some(Action::CancelInFlight) && state.phase != Phase::Returned
        },
    },
    Coverage {
        name: "renewing latches a takeover during remove_volumes",
        fix_only: true,
        omit: Omit::Branch(Branch::Latches),
        reached: |_, state| took(state, Branch::Latches) && state.standing == Standing::Lost,
    },
    Coverage {
        name: "record keeps a late Executed as Interrupted and shows the deleted Volumes",
        fix_only: true,
        omit: Omit::Branch(Branch::Converted),
        reached: |_, state| took(state, Branch::Converted) && state.store.shows_removed,
    },
    Coverage {
        name: "renewing aborts once a read says the run ended",
        fix_only: true,
        omit: Omit::Branch(Branch::Aborts),
        reached: |_, state| took(state, Branch::Aborts) && state.last_read == Some(Read::Ended),
    },
    Coverage {
        name: "the gate skips fill_ingress once the latch says the run is over",
        fix_only: true,
        omit: Omit::Branch(Branch::GateSkips),
        reached: |_, state| {
            took(state, Branch::GateSkips)
                && state.standing != Standing::Keep
                && state.last_read != Some(Read::Cancelling)
        },
    },
    Coverage {
        name: "the run stops in connecting once a read says it lost",
        fix_only: true,
        omit: Omit::Branch(Branch::StopsOver),
        reached: |_, state| {
            took(state, Branch::StopsOver)
                && state.standing == Standing::Lost
                && matches!(state.last, Some(Action::Poll(_)))
        },
    },
    Coverage {
        name: "the run stops once a read says it ended",
        fix_only: true,
        omit: Omit::Branch(Branch::StopsOver),
        reached: |_, state| took(state, Branch::StopsOver) && state.standing == Standing::Ended,
    },
];

#[derive(Clone, Copy, Debug)]
struct RunnerStanding {
    /// The fix: one `Standing` from every read, latched, and `connecting`, `renewing`, the
    /// read after executing and the gate stop on Lost or Ended; `record` keeps a late
    /// `Executed` as Interrupted, and the projection shows an Interrupted outcome's
    /// deletions.
    fix: bool,
    broken: Broken,
    /// How many `status()` calls may fail.
    failures: u8,
    /// A coverage check's cut, `None` to run the whole model.
    omit: Option<Omit>,
}

/// `running` (deployment/mod.rs).
fn running(store: Store) -> Result<(), Answer> {
    match store.status {
        Status::Running | Status::Cancelling => Ok(()),
        Status::Unknown => Err(Answer::RefusedUnknown),
        Status::Ended => Err(Answer::RefusedEnded),
    }
}

/// `finish` then `end` (lifecycle.rs): `running`, then the outcome and the end. Every
/// call the model makes finds no outcome recorded.
fn finish(store: &mut Store, outcome: Outcome) -> Answer {
    if let Err(refused) = running(*store) {
        return refused;
    }
    store.status = Status::Ended;
    store.outcome = Some(outcome);
    Answer::Kept
}

impl RunnerStanding {
    /// `status()` (store_runner.rs): `record(RunEvidence::Alive)`. The Alive arm of
    /// `record` (lifecycle.rs) answers an Unknown Deployment, and otherwise calls
    /// `running`, which refuses an ended one. `heeds_lost` is false where a `Broken` flag
    /// reads Unknown as keep.
    fn read(&self, state: &mut State, call: Call, heeds_lost: bool) -> Read {
        let read = match call {
            Call::Fails => {
                state.failed += 1;
                Read::Unavailable
            }
            Call::Answers => match state.store.status {
                Status::Running => Read::Running,
                Status::Cancelling => Read::Cancelling,
                Status::Unknown => Read::Unknown,
                Status::Ended => Read::Ended,
            },
        };
        state.last_read = Some(read);
        state.told_over |= read.over();
        if self.fix && (heeds_lost || read != Read::Unknown) {
            if self.broken.lost_does_not_stick && state.standing == Standing::Lost && !read.over() {
                state.standing = Standing::Keep;
            }
            state.standing = state.standing.after(read);
        }
        read
    }

    /// The fix's latch says the run is over.
    fn stops(&self, state: &State) -> bool {
        self.fix && state.standing != Standing::Keep
    }

    /// The runner starts `work` on the Machines.
    fn start(state: &mut State, work: Work) {
        if state.told_over {
            state.after_over.get_or_insert(work);
        }
        if work != Work::RemoveVolumes && state.last_read == Some(Read::Cancelling) {
            state.after_cancel.get_or_insert(work);
        }
    }

    /// One `renewing` tick (store_runner.rs): `Cancelling | Unknown` calls `abort`, and
    /// any `Err` skips the check. The fix also aborts on Ended.
    fn tick(&self, state: &mut State, call: Call) {
        let Some((work, stopped)) = state.phase.renewing() else {
            return;
        };
        let before = state.standing;
        let read = self.read(state, call, true);
        let heard = match read {
            Read::Cancelling | Read::Unknown => true,
            Read::Ended => self.fix,
            Read::Running | Read::Unavailable => false,
        };
        if stopped || work == Work::RemoveVolumes {
            if self.fix && work == Work::RemoveVolumes && before != state.standing {
                state.took = Some(Branch::Latches);
            }
            return;
        }
        if heard && !self.broken.renewing_never_aborts {
            state.phase.abort();
            state.took = Some(Branch::Aborts);
            return;
        }
        if read.over() {
            state.after_over.get_or_insert(work);
        }
        if read == Read::Cancelling {
            state.after_cancel.get_or_insert(work);
        }
    }

    /// `record(RunEvidence::NotExecuted(..))`: `finish` with a `NotExecuted` outcome.
    fn not_executed(state: &mut State) {
        finish(&mut state.store, Outcome::NotExecuted);
        state.phase = Phase::Returned;
    }

    /// `record(RunEvidence::Executed { .. })` (lifecycle.rs:1594-1779). An Unknown
    /// Deployment takes only an Interrupted outcome, keeps it and stays Unknown
    /// (:1619-1622, :1767-1777); any other goes through `finish` (:1778). The fix keeps a
    /// non-Interrupted one for an Unknown Deployment as Interrupted. The projection shows
    /// the deletions for a Success (:2164-2173), and in the fix for an Interrupted outcome
    /// too.
    fn executed(&self, state: &mut State, interrupted: bool) -> Answer {
        let broken = self.broken;
        let store = &mut state.store;
        let removed = state.removed;
        let taken_over = store.status == Status::Unknown;
        let (answer, kept) = if !taken_over {
            let outcome = Outcome::Executed {
                interrupted,
                removed,
            };
            (finish(store, outcome), interrupted)
        } else if interrupted || broken.store_takes_any_outcome {
            (Answer::Kept, interrupted)
        } else if self.fix && !broken.store_refuses_late_executed {
            (Answer::Converted, true)
        } else {
            (Answer::RefusedUnknown, interrupted)
        };
        if taken_over && matches!(answer, Answer::Kept | Answer::Converted) {
            store.outcome = Some(Outcome::Executed {
                interrupted: kept,
                removed,
            });
        }
        if matches!(answer, Answer::Kept | Answer::Converted) {
            store.shows_removed =
                removed && (!kept || (self.fix && !broken.projection_hides_interrupted_deletions));
        }
        state.sent = Some(Sent {
            interrupted,
            removed,
            answer,
        });
        answer
    }

    /// `record(RunEvidence::Abandoned)` (lifecycle.rs:1882-1904): a no-op on an Unknown
    /// Deployment or one with an outcome (:1883-1885), `running` otherwise (:1886), then
    /// Unknown after a preview (:1901-1903). The model sends it only from executing, after
    /// the preview, so the arm's `NotExecuted` for a run with no preview (:1887-1900) is out
    /// of scope.
    fn abandoned(state: &mut State) {
        let store = &mut state.store;
        if store.outcome.is_none() && store.status != Status::Unknown && running(*store).is_ok() {
            store.status = Status::Unknown;
        }
    }

    #[expect(
        clippy::too_many_lines,
        reason = "one arm per statement of the run the model copies"
    )]
    fn step(&self, state: &mut State, action: Action) {
        let broken = self.broken;
        state.took = None;
        match action {
            Action::CancelInFlight => state.store.status = Status::Ended,
            Action::Takeover => state.store.status = Status::Unknown,
            Action::Cancel => state.store.status = Status::Cancelling,
            Action::Poll(call) => {
                let read = self.read(state, call, true);
                if read == Read::Cancelling && !broken.connecting_ignores_cancel {
                    Self::not_executed(state);
                    state.took = Some(Branch::ConnectingCancelled);
                } else if self.stops(state) {
                    state.phase = Phase::Returned;
                    state.took = Some(Branch::StopsOver);
                }
            }
            Action::Tick(call) => self.tick(state, call),
            Action::Connected => {
                Self::start(state, Work::Prepare);
                state.phase = Phase::Preparing { aborted: false };
            }
            Action::Prepare => {
                if state.phase == (Phase::Preparing { aborted: true }) {
                    Self::not_executed(state);
                    state.took = Some(Branch::Unprepared);
                    return;
                }
                if running(state.store).is_err() {
                    state.phase = Phase::Returned;
                    state.took = Some(Branch::PreparedRefused);
                    return;
                }
                state.store.preview = true;
                let summary = match state.store.status {
                    Status::Cancelling => Read::Cancelling,
                    Status::Running | Status::Unknown | Status::Ended => Read::Running,
                };
                state.last_read = Some(summary);
                if summary == Read::Cancelling && !broken.prepared_ignores_cancel {
                    Self::not_executed(state);
                    state.took = Some(Branch::PreparedCancelling);
                    return;
                }
                Self::start(state, Work::Execute);
                state.phase = Phase::Executing { aborted: false };
                state.took = Some(Branch::Executes);
            }
            Action::Executed(exec) => state.phase = Phase::Checking(exec),
            Action::SessionEnds => {
                Self::abandoned(state);
                state.phase = Phase::Returned;
                state.took = Some(Branch::Abandons);
            }
            Action::Check(call) => {
                let Phase::Checking(exec) = state.phase else {
                    return;
                };
                let read = self.read(state, call, !broken.check_ignores_lost);
                let lost = if self.fix {
                    state.standing == Standing::Lost
                } else {
                    read == Read::Unknown && !broken.check_ignores_lost
                };
                if lost {
                    self.executed(state, true);
                    state.phase = Phase::Returned;
                    state.took = Some(Branch::CheckFindsLost);
                } else if self.stops(state) {
                    state.phase = Phase::Returned;
                    state.took = Some(Branch::StopsOver);
                } else {
                    state.phase = match exec {
                        Exec::Success => {
                            Self::start(state, Work::RemoveVolumes);
                            Phase::Removing
                        }
                        Exec::Failed => Phase::Gating(Exec::Failed),
                    };
                    state.took = Some(Branch::CheckGoesOn);
                }
            }
            Action::Removed => {
                state.removed = true;
                state.phase = Phase::Gating(Exec::Success);
            }
            Action::Gate(call) => {
                let Phase::Gating(exec) = state.phase else {
                    return;
                };
                let read = self.read(state, call, true);
                if (read == Read::Cancelling && !broken.gate_ignores_cancel) || self.stops(state) {
                    state.phase = Phase::Recording(exec);
                    state.took = Some(Branch::GateSkips);
                } else {
                    Self::start(state, Work::FillIngress);
                    state.phase = Phase::Filling {
                        exec,
                        stopped: false,
                    };
                    state.took = Some(Branch::Fills);
                }
            }
            Action::Filled => {
                if let Phase::Filling { exec, .. } = state.phase {
                    state.phase = Phase::Recording(exec);
                }
            }
            Action::Record => {
                let answer = self.executed(state, false);
                state.phase = Phase::Returned;
                state.took = Some(if answer == Answer::Converted {
                    Branch::Converted
                } else {
                    Branch::Records
                });
            }
        }
    }

    /// `action`, which led to `after`, is what the coverage check cut.
    fn omitted(&self, action: Action, after: &State) -> bool {
        match self.omit {
            None => false,
            Some(Omit::Actions(actions)) => actions.contains(&action),
            Some(Omit::Branch(branch)) => after.took == Some(branch),
        }
    }
}

impl Model for RunnerStanding {
    type State = State;
    type Action = Action;

    fn init_states(&self) -> Vec<State> {
        vec![State {
            store: Store {
                status: Status::Running,
                preview: false,
                outcome: None,
                shows_removed: false,
            },
            phase: Phase::Connecting,
            removed: false,
            failed: 0,
            last_read: None,
            told_over: false,
            standing: Standing::Keep,
            after_over: None,
            after_cancel: None,
            sent: None,
            took: None,
            last: None,
        }]
    }

    /// The world's steps first, then the runner's work ending, then its reads. The order
    /// decides only which of the shortest breaking paths a story shows: where two first
    /// differ, BFS visits a world step before a runner step.
    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        if matches!(state.store.status, Status::Running | Status::Cancelling) {
            actions.push(Action::CancelInFlight);
            actions.push(Action::Takeover);
        }
        if state.store.status == Status::Running {
            actions.push(Action::Cancel);
        }
        let reads = |actions: &mut Vec<Action>, read: fn(Call) -> Action| {
            actions.push(read(Call::Answers));
            if state.failed < self.failures {
                actions.push(read(Call::Fails));
            }
        };
        match state.phase {
            Phase::Connecting => {
                actions.push(Action::Connected);
                reads(actions, Action::Poll);
            }
            Phase::Preparing { .. } => {
                actions.push(Action::Prepare);
                reads(actions, Action::Tick);
            }
            Phase::Executing { aborted } => {
                if !aborted {
                    actions.push(Action::Executed(Exec::Success));
                }
                actions.push(Action::Executed(Exec::Failed));
                actions.push(Action::SessionEnds);
                reads(actions, Action::Tick);
            }
            Phase::Checking(_) => reads(actions, Action::Check),
            Phase::Removing => {
                actions.push(Action::Removed);
                reads(actions, Action::Tick);
            }
            Phase::Gating(_) => reads(actions, Action::Gate),
            Phase::Filling { .. } => {
                actions.push(Action::Filled);
                reads(actions, Action::Tick);
            }
            Phase::Recording(_) => actions.push(Action::Record),
            Phase::Returned => {}
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
            Property::always(TAKEOVER_KEEPS_OUTCOME, |_, state: &State| {
                !(state.phase == Phase::Returned
                    && state.sent.is_some_and(|sent| {
                        !sent.interrupted && sent.answer == Answer::RefusedUnknown
                    }))
            }),
            Property::always(DELETIONS_KEPT, |_, state: &State| {
                let cancelled_in_flight =
                    state.store.status == Status::Ended && state.store.outcome.is_none();
                !(state.phase == Phase::Returned
                    && state.removed
                    && !state.store.shows_removed
                    && !cancelled_in_flight)
            }),
            Property::always(NO_WORK_AFTER_END, |_, state: &State| {
                state.after_over.is_none()
            }),
            Property::always(HEEDS_CANCEL, |_, state: &State| {
                state.after_cancel.is_none()
            }),
            Property::always(TAKEN_OVER_INTERRUPTED, |_, state: &State| {
                !(state.store.status == Status::Unknown
                    && matches!(
                        state.store.outcome,
                        Some(Outcome::Executed {
                            interrupted: false,
                            ..
                        })
                    ))
            }),
        ];
        properties.extend(
            COVERAGE
                .iter()
                .filter(|row| self.fix || !row.fix_only)
                .map(|row| Property::sometimes(row.name, row.reached)),
        );
        properties
    }
}

const TAKEOVER_KEEPS_OUTCOME: &str =
    "a run never returns with its Executed refused because the Deployment went Unknown";
/// Only the `removed_volume` node projection (lifecycle.rs:2152-2174). Applied State still
/// lists the deleted Volume under the fix, since a run that was taken over moves no Applied
/// State (lifecycle.rs:1767-1777). `cancel_in_flight` is exempt: `forget_cluster`, which
/// runs it, lets go of the Organization's Applied State and Volumes' fixed storage
/// (teardown.rs:442-445, :458-469).
const DELETIONS_KEPT: &str = "once the run returned, the Deployment's nodes show the Volumes it deleted as Removed, unless cancel_in_flight ended the Deployment";
const NO_WORK_AFTER_END: &str = "once a read said the run is over, the runner starts no work, and keeps on none but deleting Volumes";
const HEEDS_CANCEL: &str = "while its last read said Cancelling, the runner starts or keeps on no work but deleting Volumes";
const TAKEN_OVER_INTERRUPTED: &str =
    "the store keeps no outcome but Interrupted on a Deployment that went Unknown";

/// `path` in the code's words: each step, then the Deployment's row and the runner once
/// it ran.
fn story(model: &RunnerStanding, path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    // Stateright pairs each state with the action taken from it; the row lists the
    // action that led to the state instead, so the last row shows the broken state.
    let mut led_by: Option<(State, Action)> = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = led_by.map_or_else(
            || "start: the runner claimed the Deployment (lifecycle.rs claim)".to_owned(),
            |(before, action)| reads_as(model, &before, action, state),
        );
        led_by = action.map(|action| (*state, action));
        let read = state
            .last_read
            .map_or_else(|| "-".to_owned(), |read| format!("{read:?}"));
        let _ = writeln!(
            story,
            "{n:2}. {what}\n    store: status={:?} preview={} outcome={:?} shows_removed={} | runner: {:?} last_read={read} removed={}{}",
            state.store.status,
            state.store.preview,
            state.store.outcome,
            state.store.shows_removed,
            state.phase,
            state.removed,
            if model.fix {
                format!(" standing={:?}", state.standing)
            } else {
                String::new()
            },
        );
    }
    story
}

/// `action` from `before` under `model`, read with `after` for the branch it took.
#[expect(
    clippy::too_many_lines,
    reason = "one arm per statement of the run the model copies"
)]
fn reads_as(model: &RunnerStanding, before: &State, action: Action, after: &State) -> String {
    let broken = model.broken;
    let said = |read: Option<Read>| match read {
        Some(Read::Running) => "Ok(Running)",
        Some(Read::Cancelling) => "Ok(Cancelling)",
        Some(Read::Unknown) => "Ok(Unknown)",
        Some(Read::Ended) => "Err(conflict: This Deployment already ended)",
        Some(Read::Unavailable) => "Err(unavailable: the Store call failed)",
        None => "?",
    };
    let read = said(after.last_read);
    match action {
        Action::CancelInFlight => {
            "Store: forget_cluster's cancel_in_flight sets the Deployment Cancelled without waiting on its runner (lifecycle.rs cancel_in_flight, teardown.rs forget_cluster)".to_owned()
        }
        Action::Takeover => {
            "Store: another Deployment of the Environment is claimed; its UPDATE sets this one unknown and leaves its runner column (lifecycle.rs claim)".to_owned()
        }
        Action::Cancel => "Store: a user's cancel moves Running to Cancelling (lifecycle.rs cancel)".to_owned(),
        Action::Poll(_) => {
            let then = match after.took {
                Some(Branch::ConnectingCancelled) => {
                    "returns Ok(None); the run records NotExecuted \"Cancelled before connecting\""
                }
                Some(Branch::StopsOver) => "the latch says the run is over: the run returns (the fix)",
                _ if after.last_read == Some(Read::Cancelling) => {
                    "dials on (under Broken: the Cancelling check skipped)"
                }
                _ => "only Ok(Cancelling) stops it: dials on",
            };
            format!("Runner: connecting's poll reads status() = {read}; {then} (store_runner.rs connecting)")
        }
        Action::Connected => {
            "Runner: the dial completes; the run plans, or builds and prepares, under renewing (store_runner.rs run_deployment_expecting, execute)".to_owned()
        }
        Action::Tick(_) => {
            let work = before.phase.renewing().map_or("?", |(work, _)| match work {
                Work::Prepare => "the prepare",
                Work::Execute => "executing",
                Work::RemoveVolumes => "remove_volumes",
                Work::FillIngress => "fill_ingress",
            });
            let then = match after.took {
                Some(Branch::Aborts) => "calls abort".to_owned(),
                Some(Branch::Latches) => format!(
                    "the abort is || (), deleting goes on; the latch keeps {:?} (the fix)",
                    after.standing
                ),
                _ if before.phase.renewing().is_some_and(|(_, stopped)| stopped) => {
                    "its abort already ran".to_owned()
                }
                _ if before.phase == Phase::Removing => "the abort is || (): deleting goes on".to_owned(),
                _ if broken.renewing_never_aborts => {
                    "never aborts (under Broken): the work goes on".to_owned()
                }
                _ => match after.last_read {
                    Some(Read::Ended | Read::Unavailable) => {
                        "an Err skips one check (\"ponytail: a failed read skips one check\"): the work goes on".to_owned()
                    }
                    _ => "the work goes on".to_owned(),
                },
            };
            format!("Runner: renewing's tick during {work} reads status() = {read}; {then} (store_runner.rs renewing)")
        }
        Action::Prepare => match after.took {
            Some(Branch::Unprepared) => {
                "Runner: the aborted prepare returns Err; the run records NotExecuted (store_runner.rs execute, unprepared)".to_owned()
            }
            Some(Branch::PreparedRefused) => format!(
                "Runner: record(Prepared) answers Err, {}; `?` returns it (store_runner.rs execute)",
                match before.store.status {
                    Status::Unknown => "the Deployment is Unknown",
                    Status::Running | Status::Cancelling | Status::Ended => "the Deployment ended",
                }
            ),
            Some(Branch::PreparedCancelling) => {
                "Runner: record(Prepared)'s summary reads Cancelling; the run records NotExecuted \"Cancelled before it executed\" (store_runner.rs execute)".to_owned()
            }
            _ if after.last_read == Some(Read::Cancelling) => {
                "Runner: record(Prepared)'s summary reads Cancelling, skipped (under Broken); executing starts under renewing (store_runner.rs execute)".to_owned()
            }
            _ => "Runner: record(Prepared) keeps the preview, its summary reads Running; executing starts under renewing (store_runner.rs execute)".to_owned(),
        },
        Action::Executed(exec) => format!(
            "Runner: executing returns Ok with a {exec:?} outcome (store_runner.rs execute)"
        ),
        Action::SessionEnds => format!(
            "Runner: executing returns Err, the session ended; the run records Abandoned, {} (store_runner.rs execute, lifecycle.rs record)",
            if after.store.status == Status::Unknown && before.store.status != Status::Unknown {
                "which leaves the Deployment Unknown"
            } else {
                "which changes nothing"
            }
        ),
        Action::Check(_) => {
            let then = match after.took {
                Some(Branch::CheckFindsLost) if model.fix => {
                    "the latch says Lost: the run records Interrupted (the fix)".to_owned()
                }
                Some(Branch::CheckFindsLost) => {
                    "Unknown: the run records Interrupted with no deletions".to_owned()
                }
                Some(Branch::StopsOver) => {
                    "the latch says the run ended: the run returns (the fix)".to_owned()
                }
                _ if after.last_read == Some(Read::Unknown) => {
                    "Unknown read as keep (under Broken)".to_owned()
                }
                _ => {
                    let next = if after.phase == Phase::Removing {
                        "remove_volumes starts under renewing with the no-op abort || ()"
                    } else {
                        "the outcome is not a Success, so no Volume is deleted"
                    };
                    format!("not Unknown, so lost_to_another_runner is false; {next}")
                }
            };
            format!(
                "Runner: lost_to_another_runner reads status() = {read}; {then} (store_runner.rs execute, lost_to_another_runner)"
            )
        }
        Action::Removed => {
            "Runner: remove_volumes returns the Volumes it deleted (store_runner.rs execute)".to_owned()
        }
        Action::Gate(_) => {
            let then = match after.took {
                Some(Branch::GateSkips) if after.last_read == Some(Read::Cancelling) => {
                    "Cancelling: fill_ingress is skipped"
                }
                Some(Branch::GateSkips) => "the latch says the run is over: fill_ingress is skipped (the fix)",
                _ if after.last_read == Some(Read::Cancelling) => {
                    "Cancelling, skipped (under Broken): fill_ingress starts under renewing"
                }
                _ => "not Ok(Cancelling): fill_ingress starts under renewing",
            };
            format!("Runner: the gate reads status() = {read}; {then} (store_runner.rs execute)")
        }
        Action::Filled => "Runner: fill_ingress ends (store_runner.rs fill_ingress)".to_owned(),
        Action::Record => {
            let sent = after.sent.expect("record(Executed) sets sent");
            let what = if sent.interrupted {
                "Interrupted"
            } else {
                "its outcome"
            };
            let removed = if sent.removed {
                ", naming the deleted Volumes"
            } else {
                ""
            };
            let answer = match sent.answer {
                Answer::Kept if before.store.status == Status::Unknown && !sent.interrupted => {
                    "running() skipped (under Broken): the store keeps it; the Deployment stays Unknown"
                }
                Answer::Kept if before.store.status == Status::Unknown => {
                    "the store keeps it; the Deployment stays Unknown"
                }
                Answer::Kept => "the store keeps it and ends the Deployment",
                Answer::Converted => {
                    "the Deployment is Unknown: the store keeps it as Interrupted (the fix)"
                }
                Answer::RefusedUnknown => {
                    "the Deployment is Unknown and the outcome not Interrupted: running() refuses it; execute returns that Err"
                }
                Answer::RefusedEnded => "the Deployment ended: finish's running() refuses it",
            };
            let shown = match (sent.removed, after.store.outcome, after.store.shows_removed) {
                (true, Some(_), true) => "; removed_volume reads the deleted Volumes Removed",
                (true, Some(_), false) if model.broken.projection_hides_interrupted_deletions => {
                    "; removed_volume reads the deleted Volumes NotAttempted for an Interrupted outcome (under Broken)"
                }
                (true, Some(_), false) => {
                    "; removed_volume reads the deleted Volumes NotAttempted, the outcome not a Success"
                }
                _ => "",
            };
            format!(
                "Runner: record(Executed) with {what}{removed}; {answer}{shown} (store_runner.rs execute, lifecycle.rs record)"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use stateright::{Checker, Expectation, HasDiscoveries};

    use super::*;

    type Path = Vec<(State, Option<Action>)>;

    /// The code as it is, with every `status()` call answering.
    fn as_is() -> RunnerStanding {
        RunnerStanding {
            fix: false,
            broken: Broken::default(),
            failures: 0,
            omit: None,
        }
    }

    /// The fix: on the runner, one latched `Standing`; in the store, a late `Executed` kept
    /// as Interrupted and its deletions shown.
    fn fixed() -> RunnerStanding {
        RunnerStanding {
            fix: true,
            ..as_is()
        }
    }

    /// `model` with one `status()` call that may fail.
    fn failing(model: RunnerStanding) -> RunnerStanding {
        RunnerStanding {
            failures: 1,
            ..model
        }
    }

    fn check(model: RunnerStanding) -> impl Checker<RunnerStanding> {
        model.checker().threads(4).spawn_bfs().join()
    }

    /// The first path in BFS order whose last state breaks `property` under `model`, so
    /// no shorter path breaks it, printed as a story under the test's name. For a
    /// `sometimes`, such as a `COVERAGE` row's name, it is the first path that reaches it.
    /// It panics when no path does, as for an `always` that holds. The checker's
    /// own discovery is not that path: `check_block` stores every violation it visits,
    /// each overwriting the last, and `finish_when` is read once per 1500-state block, so
    /// a visitor keeps the first. One thread keeps the path the same from run to run.
    fn breaks(model: RunnerStanding, property: &'static str) -> Path {
        first_break(model, property, |_| true, true)
    }

    /// As `breaks`, the first such path that also satisfies `and`. It explores the whole
    /// model, since the first discovery may not pass `and`.
    fn breaks_where(model: RunnerStanding, property: &'static str, and: fn(&Path) -> bool) -> Path {
        first_break(model, property, and, false)
    }

    fn first_break(
        model: RunnerStanding,
        property: &'static str,
        and: fn(&Path) -> bool,
        stop_at_discovery: bool,
    ) -> Path {
        let named = model
            .properties()
            .into_iter()
            .find(|candidate| candidate.name == property)
            .expect("a model property");
        let holds = named.condition;
        let reached = matches!(named.expectation, Expectation::Sometimes);
        let first = Arc::new(Mutex::new(None));
        let seen = Arc::clone(&first);
        let checker =
            model
                .checker()
                .threads(1)
                .visitor(move |path: stateright::Path<State, Action>| {
                    let mut first = seen.lock().expect("the first break");
                    if first.is_none() && holds(&model, path.last_state()) == reached {
                        let path = path.into_vec();
                        if and(&path) {
                            *first = Some(path);
                        }
                    }
                });
        let checker = if stop_at_discovery {
            checker.finish_when(HasDiscoveries::AllOf([property].into()))
        } else {
            checker
        };
        checker.spawn_bfs().join();
        let path = first
            .lock()
            .expect("the first break")
            .take()
            .expect("a path breaks the property");
        let test = std::thread::current();
        println!(
            "[{}] {property} {} under {model:?}:\n{}",
            test.name().unwrap_or("?"),
            if reached { "is reached" } else { "fails" },
            story(&model, &path)
        );
        path
    }

    fn last(path: &Path) -> State {
        path.last().expect("a path has a state").0
    }

    fn took_action(path: &Path, action: Action) -> bool {
        path.iter().any(|(_, taken)| *taken == Some(action))
    }

    /// The finding. `lost_to_another_runner` and the gate before `fill_ingress` are the
    /// run's last reads, and a newer claim lands after them. The Deployment is Unknown,
    /// `record(Executed)` is refused, and the store keeps no outcome. This test passes by
    /// finding that path.
    #[test]
    fn current_code_loses_the_outcome_of_a_run_taken_over_after_its_last_read() {
        let path = breaks(as_is(), TAKEOVER_KEEPS_OUTCOME);
        let broken = last(&path);
        assert_eq!(broken.store.status, Status::Unknown);
        assert_eq!(broken.store.outcome, None);
        assert!(
            broken
                .sent
                .is_some_and(|sent| !sent.interrupted && sent.answer == Answer::RefusedUnknown)
        );
        assert_eq!(broken.last, Some(Action::Record));
        assert_eq!(broken.failed, 0, "no status() call failed");
    }

    /// The finding with Volumes deleted and no cancel. `lost_to_another_runner` reads
    /// Running after a Success. A newer claim then sets the Deployment Unknown while
    /// `remove_volumes` deletes under its no-op abort. `record(Executed)` is refused, and
    /// neither the outcome nor the deletions are kept. No `status()` call fails on the
    /// path.
    #[test]
    fn current_code_loses_the_volumes_a_run_deleted_once_taken_over() {
        let path = breaks_where(as_is(), DELETIONS_KEPT, |path| {
            !took_action(path, Action::Cancel)
        });
        let broken = last(&path);
        assert!(broken.removed && !broken.store.shows_removed);
        assert_eq!(broken.store.status, Status::Unknown);
        assert_eq!(broken.store.outcome, None);
        assert_eq!(
            broken.sent,
            Some(Sent {
                interrupted: false,
                removed: true,
                answer: Answer::RefusedUnknown,
            })
        );
        let check = path
            .iter()
            .position(|(_, action)| *action == Some(Action::Check(Call::Answers)))
            .expect("lost_to_another_runner read the store");
        let read = path.get(check + 1).expect("the state the read led to").0;
        assert_eq!(read.last_read, Some(Read::Running));
        let takeover = path
            .iter()
            .position(|(_, action)| *action == Some(Action::Takeover))
            .expect("a takeover");
        assert!(takeover > check, "the takeover landed after the read");
        let before = path.get(takeover).expect("the state the takeover left").0;
        assert_eq!(before.phase, Phase::Removing);
        assert_eq!(broken.failed, 0, "no status() call failed");
    }

    /// The second finding, first cause. `forget_cluster` cancels the Deployment in flight,
    /// and `renewing`'s next tick reads `Err` ("already ended") and skips it: the work in
    /// hand keeps going on a Deployment the store ended.
    #[test]
    fn current_code_keeps_working_after_cancel_in_flight_ended_the_run() {
        let path = breaks(as_is(), NO_WORK_AFTER_END);
        let broken = last(&path);
        assert!(took_action(&path, Action::CancelInFlight));
        assert_eq!(broken.store.status, Status::Ended);
        assert_eq!(broken.last, Some(Action::Tick(Call::Answers)));
        assert_eq!(broken.last_read, Some(Read::Ended));
        assert_eq!(broken.after_over, Some(Work::Prepare));
    }

    /// The second finding, second cause, with no `cancel_in_flight` and no failed read.
    /// After a takeover, `connecting` reads `Ok(Unknown)` and dials on, and the prepare
    /// starts. The gate before `fill_ingress` reads `Ok(Unknown)` as keep too.
    #[test]
    fn current_code_works_on_after_a_read_said_unknown() {
        let model = RunnerStanding {
            omit: Some(Omit::Actions(&[Action::CancelInFlight])),
            ..as_is()
        };
        let path = breaks(model, NO_WORK_AFTER_END);
        let broken = last(&path);
        assert_eq!(broken.store.status, Status::Unknown);
        assert_eq!(broken.after_over, Some(Work::Prepare));
        assert!(took_action(&path, Action::Poll(Call::Answers)));
        assert_eq!(broken.failed, 0, "no status() call failed");
        let path = breaks_where(model, NO_WORK_AFTER_END, |path| {
            last(path).after_over == Some(Work::FillIngress)
        });
        let broken = last(&path);
        assert_eq!(broken.last, Some(Action::Gate(Call::Answers)));
        assert_eq!(broken.last_read, Some(Read::Unknown));
    }

    /// The code as it is breaks only the properties the findings name, with one failed
    /// read, and reaches every `sometimes` the fix does not add.
    #[test]
    fn current_code_breaks_only_the_findings() {
        let checker = check(failing(as_is()));
        println!("as is: {} states", checker.unique_state_count());
        for property in [HEEDS_CANCEL, TAKEN_OVER_INTERRUPTED] {
            assert!(checker.discovery(property).is_none(), "{property} fails");
        }
        for property in [TAKEOVER_KEEPS_OUTCOME, DELETIONS_KEPT, NO_WORK_AFTER_END] {
            assert!(checker.discovery(property).is_some(), "{property} holds");
        }
        for row in COVERAGE.iter().filter(|row| !row.fix_only) {
            assert!(
                checker.discovery(row.name).is_some(),
                "{} is never reached",
                row.name
            );
        }
    }

    /// The fix holds with a failed read, and every `sometimes` is reached.
    #[test]
    fn a_latched_standing_and_a_converting_store_keep_the_outcome_and_the_deletions() {
        let checker = check(failing(fixed()));
        println!("fixed: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// Every `sometimes` needs what it names: with the row's `omit` taken out of the
    /// fixed model, no other path reaches its predicate.
    #[test]
    fn every_sometimes_needs_what_it_names() {
        let whole = check(failing(fixed()));
        for row in COVERAGE {
            assert!(
                whole.discovery(row.name).is_some(),
                "{} is never reached",
                row.name
            );
            let cut = check(RunnerStanding {
                omit: Some(row.omit),
                ..failing(fixed())
            });
            assert!(
                cut.discovery(row.name).is_none(),
                "{} is reached without {:?}",
                row.name,
                row.omit
            );
        }
    }

    fn fixed_without(broken: Broken) -> RunnerStanding {
        RunnerStanding { broken, ..fixed() }
    }

    /// Without `connecting`'s Cancelling check, a run cancelled while it dials starts
    /// preparing.
    #[test]
    fn connecting_needs_its_cancel_check() {
        let path = breaks(
            fixed_without(Broken {
                connecting_ignores_cancel: true,
                ..Broken::default()
            }),
            HEEDS_CANCEL,
        );
        assert_eq!(last(&path).after_cancel, Some(Work::Prepare));
    }

    /// Without `renewing`'s abort, a cancel read during the work in hand leaves it
    /// running.
    #[test]
    fn renewing_needs_its_abort() {
        let path = breaks(
            fixed_without(Broken {
                renewing_never_aborts: true,
                ..Broken::default()
            }),
            HEEDS_CANCEL,
        );
        assert_eq!(last(&path).last, Some(Action::Tick(Call::Answers)));
    }

    /// Without the Cancelling check on `record(Prepared)`'s summary, a run cancelled
    /// while it prepared executes.
    #[test]
    fn executing_needs_the_prepared_cancel_check() {
        let path = breaks(
            fixed_without(Broken {
                prepared_ignores_cancel: true,
                ..Broken::default()
            }),
            HEEDS_CANCEL,
        );
        assert_eq!(last(&path).after_cancel, Some(Work::Execute));
    }

    /// Without the read after executing, a run taken over while it executes, with no tick
    /// in between, starts deleting Volumes though that read said Unknown.
    #[test]
    fn deleting_needs_the_read_after_executing() {
        let path = breaks(
            fixed_without(Broken {
                check_ignores_lost: true,
                ..Broken::default()
            }),
            NO_WORK_AFTER_END,
        );
        assert_eq!(last(&path).after_over, Some(Work::RemoveVolumes));
    }

    /// Without the gate's Cancelling check, a cancelled run fills ingress.
    #[test]
    fn fill_ingress_needs_the_gate() {
        let path = breaks(
            fixed_without(Broken {
                gate_ignores_cancel: true,
                ..Broken::default()
            }),
            HEEDS_CANCEL,
        );
        assert_eq!(last(&path).after_cancel, Some(Work::FillIngress));
    }

    /// Without `running` on a non-Interrupted `Executed` for an Unknown Deployment, the
    /// store keeps a taken-over run's Success.
    #[test]
    fn the_store_needs_its_takeover_refusal() {
        let path = breaks(
            fixed_without(Broken {
                store_takes_any_outcome: true,
                ..Broken::default()
            }),
            TAKEN_OVER_INTERRUPTED,
        );
        let broken = last(&path);
        assert_eq!(broken.store.status, Status::Unknown);
        assert!(matches!(
            broken.store.outcome,
            Some(Outcome::Executed {
                interrupted: false,
                ..
            })
        ));
    }

    /// Without Lost sticking, a failed read after a takeover puts the fix's latch back to
    /// Keep, and the run starts work the latch would have stopped. Only a failed read can
    /// follow `Ok(Unknown)` with another answer, since no store step leaves Unknown.
    #[test]
    fn the_fix_needs_lost_to_stick() {
        let path = breaks(
            failing(fixed_without(Broken {
                lost_does_not_stick: true,
                ..Broken::default()
            })),
            NO_WORK_AFTER_END,
        );
        let broken = last(&path);
        assert_eq!(broken.failed, 1);
        assert!(broken.after_over.is_some());
        assert_eq!(broken.standing, Standing::Keep);
    }

    /// Without the store keeping a late `Executed` as Interrupted, a takeover after the
    /// run's last read gets its outcome refused, as on the code as it is.
    #[test]
    fn the_fix_needs_the_store_to_keep_a_late_executed() {
        let path = breaks(
            fixed_without(Broken {
                store_refuses_late_executed: true,
                ..Broken::default()
            }),
            TAKEOVER_KEEPS_OUTCOME,
        );
        assert_eq!(last(&path).store.outcome, None);
    }

    /// Without the projection showing an Interrupted outcome's deletions, the store keeps
    /// the outcome and still reads the deleted Volumes NotAttempted.
    #[test]
    fn the_fix_needs_the_projection_to_show_interrupted_deletions() {
        let path = breaks(
            fixed_without(Broken {
                projection_hides_interrupted_deletions: true,
                ..Broken::default()
            }),
            DELETIONS_KEPT,
        );
        assert!(last(&path).store.outcome.is_some());
    }
}
