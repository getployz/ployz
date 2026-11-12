//! One Deployment's run, from the runner that claimed it, against the Config Store that
//! holds its status. The runner (`run_deployment_expecting` and `Run`, ployz's
//! sdk/store_runner.rs:163-219, :556-563) dials the Servers under `connecting`, then
//! plans or builds and prepares, executes, deletes the Volumes the Deploy removes and
//! fills ingress, each piece of work under `renewing` (:653-665, :687-691, :708,
//! :726-738). While it awaits that work, `connecting` and `renewing` call `standing()`
//! every `CANCEL_POLL`, 2 s (:40, :1116-1132, :1136-1149), and the work can end before
//! the next call (:1129, :1141). `execute` also calls it once after executing (:694) and
//! once before `fill_ingress` (:712). `standing()` is `record(RunEvidence::Alive)` read
//! through one `standing` (:590-606, :1326-1335), and every site matches the one
//! `Standing` it returns. Meanwhile a user cancels the Deployment (ployz-store
//! deployment/lifecycle.rs:1494), a newer Deployment of the Environment is claimed and
//! sets it Unknown (lifecycle.rs:1320-1332), or forgetting the Cluster cancels it without
//! waiting on the runner (lifecycle.rs:1525, teardown.rs:460).
//!
//! The model found two problems in the code before `Standing` (#3839), and the code
//! now keeps out all but one path of the first. Each fixed part has a `Broken` flag
//! that puts the old code back, and a test that shows the property it keeps break.
//!
//! The first finding: a takeover that lands after the run's last read leaves the
//! Deployment Unknown, and `record` then refuses an `Executed` that is not Interrupted
//! (lifecycle.rs:1641-1644), so the store keeps no outcome. `Run::executed` now sends a
//! refused outcome again as Interrupted (store_runner.rs:1200-1222), which the store
//! keeps. Not when the run tried to delete Volumes: the store reads an Interrupted
//! outcome's removed Volumes NotAttempted (lifecycle.rs:2186-2190). On that path the
//! store still keeps neither the outcome nor the deletions, and `TAKEOVER_KEEPS_OUTCOME`
//! and `DELETIONS_KEPT` still break. No read has to fail for this.
//!
//! The second finding had two causes, both fixed. The sites read `Err` as keep: once
//! `cancel_in_flight` ended the Deployment, every read answers `Err` ("This Deployment
//! already ended", deployment/mod.rs:1813-1815, :1888-1890), and `renewing` kept the work
//! in hand going. And `connecting` and the gate before `fill_ingress` read `Ok(Unknown)`
//! as keep. Now `standing` reads the conflict as Ended, the first Lost or Ended read
//! latches (`Run::over`, store_runner.rs:556-563), and every site stops on either.
//!
//! The store fix variant (`store_fix`) is the store change that would close the rest of
//! the first finding: `record` keeps a non-Interrupted `Executed` for an Unknown
//! Deployment as Interrupted in the same transaction, and its projection shows the
//! deletions of an Interrupted outcome, where the code reads them NotAttempted for any
//! outcome but a Success (lifecycle.rs:2186-2190), and an Interrupted outcome projects
//! to Failed (ployz-core config/runtime_outcome.rs:208-220). The code does not have it.
//!
//! Each store call is one step, since `record`, `claim`, `cancel` and `cancel_in_flight`
//! each take one transaction (lifecycle.rs:1534-1539, :1232-1237, :1485-1489,
//! :1509-1512). A record that follows a read, or the end of a piece of work, is one step
//! with it, except the `Executed` record. `Run::executed`'s two records are one step:
//! within these bounds the store refuses the first only for a Deployment that is Unknown
//! or ended, and no store step leaves either.
//!
//! Out of scope: a takeover or `cancel_in_flight` between a read and the record merged
//! with it, build's records, warnings, the lease lapsing, a second claim by the same
//! runner, Cloud's `abandonDeployment`, a dial that fails, a store call other than
//! `standing()`'s failing, and an abort that comes too late to stop its work.

use std::fmt::Write as _;

use stateright::{Model, Property};

/// The Deployment's status as `record` reads it. `stored` (deployment/mod.rs) remaps a
/// lapsed lease to Unknown, which the model leaves out.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Status {
    Running,
    Cancelling,
    /// In the model, `claim`'s takeover (lifecycle.rs:1320-1332), or the run's own
    /// `Abandoned` after a preview (lifecycle.rs:1923).
    Unknown,
    /// Applied, Failed or Cancelled: `end` (lifecycle.rs), or `cancel_in_flight`.
    Ended,
}

/// What the store keeps as the run's outcome.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Outcome {
    NotExecuted,
    /// `RunEvidence::Executed`: whether its outcome is Interrupted, and whether `removed`
    /// names the Volumes the run deleted (store_runner.rs:1209-1215). One that is not
    /// Interrupted and names deletions is a Success, since only a Success deletes
    /// (store_runner.rs:706-711).
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
    /// for a Success outcome, and NotAttempted for any other (lifecycle.rs:2174-2196).
    shows_removed: bool,
}

/// What the read in `Run::standing` (store_runner.rs) returned:
/// `record(RunEvidence::Alive)`'s summary status, or its error.
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

/// Whether a `standing()` call's read reaches the store.
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
    /// Executing ended; the read after executing is next.
    Checking(Exec),
    /// `remove_volumes` under `renewing` with the no-op abort `|| ()`.
    Removing,
    /// The gate before `fill_ingress`: it fills on Keep or Unread.
    Gating(Exec),
    /// `fill_ingress`, its `renewing` stopping it with `stop.cancel()`.
    Filling { exec: Exec, stopped: bool },
    /// `Run::executed` is next.
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

/// `Run::over` (store_runner.rs:556-563): the first read that said Lost or Ended sets
/// it, and no later read changes it, since `Run::standing` reads nothing more once it is
/// set (:1469-1478). `the_code_needs_its_latch_to_stick` shows what a latch that does
/// not stick lets through.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Standing {
    Keep,
    Lost,
    Ended,
}

/// What `Run::standing` returned (store_runner.rs `Standing`, :710-749): the latch once it
/// is set, else what `standing` makes of the read.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Said {
    Keep,
    Cancel,
    Lost,
    Ended,
    Unread,
}

/// What `record` answered to an `Executed`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Answer {
    Kept,
    /// The store fix: the Deployment is Unknown and the `Executed` not Interrupted, so
    /// `record` kept it as Interrupted, where the code refuses it (lifecycle.rs:1641-1644).
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
    /// A `renewing` tick said Cancel, Lost or Ended and aborted.
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
    /// The read after executing said Lost and the run records Interrupted.
    CheckFindsLost,
    /// The read after executing let the run go on.
    CheckGoesOn,
    /// A `renewing` tick during `remove_volumes` latched Lost or Ended.
    Latches,
    /// The gate let `fill_ingress` start.
    Fills,
    /// The gate skipped `fill_ingress`.
    GateSkips,
    /// The run sent `Executed` with its outcome.
    Records,
    /// The store fix: `record` kept the `Executed` as Interrupted.
    Converted,
    /// `record` refused the `Executed`, and kept it sent again as Interrupted.
    Resends,
    /// The latch said the run is over, so the run returned without more work.
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
    /// The read after executing: one `standing()` call.
    Check(Call),
    /// `remove_volumes` returns the Volumes it deleted.
    Removed,
    /// The gate's `standing()` call.
    Gate(Call),
    /// `fill_ingress` ends.
    Filled,
    /// `record(RunEvidence::Executed { .. })` with the outcome.
    Record,
    /// `connecting`'s poll tick: one `standing()` call.
    Poll(Call),
    /// `renewing`'s poll tick: one `standing()` call.
    Tick(Call),
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct State {
    store: Store,
    phase: Phase,
    /// `remove_volumes` deleted the Volumes the Deploy removes.
    removed: bool,
    /// `standing()` calls whose read failed.
    failed: u8,
    /// What the last read `standing()` made, or `record(Prepared)`'s summary, said.
    last_read: Option<Read>,
    /// Some read said the run is over.
    told_over: bool,
    /// `Run::over`.
    standing: Standing,
    /// The first work started, or kept on by a `renewing` tick, after a read said the run
    /// is over. Deleting Volumes is kept on, since its abort is a no-op by design.
    after_over: Option<Work>,
    /// The first work other than deleting Volumes started, or kept on by a `renewing`
    /// tick, while the last read said Cancelling.
    after_cancel: Option<Work>,
    /// The last `Executed` sent: after a resend, the Interrupted one.
    sent: Option<Sent>,
    took: Option<Branch>,
    last: Option<Action>,
}

/// A guard taken out, to show the check catches what it prevents. The first six are
/// guards the code had before `Standing`, the next six the ones `Standing` added, and the
/// last two parts of the store fix.
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
    /// The read after executing (store_runner.rs `execute`) reads Unknown as keep.
    check_ignores_lost: bool,
    /// The gate before `fill_ingress` (store_runner.rs `execute`) fills on Cancelling.
    gate_ignores_cancel: bool,
    /// `record` (lifecycle.rs) skips `running` on a non-Interrupted `Executed` for an
    /// Unknown Deployment.
    store_takes_any_outcome: bool,
    /// `Run::standing` reads again once the latch is set, and a read that says the run
    /// is not over drops it, as the code before `Standing` had no latch.
    latch_does_not_stick: bool,
    /// `standing` reads an ended Deployment's conflict as Unread, as every site before
    /// `Standing` read `Err` as keep.
    ended_reads_as_keep: bool,
    /// `connecting` and the gate go on when the read says Lost, as they did before
    /// `Standing`, when only Cancelling stopped them.
    unknown_reads_as_keep: bool,
    /// `Run::executed` returns a refused `Executed` as it is, and sends nothing again.
    no_resend: bool,
    /// The read after executing (store_runner.rs `execute`) goes on when it says Ended.
    check_ignores_ended: bool,
    /// The gate before `fill_ingress` (store_runner.rs `execute`) fills when it says Ended.
    gate_ignores_ended: bool,
    /// The store fix's `record` refuses a non-Interrupted `Executed` for an Unknown
    /// Deployment, as the code does (lifecycle.rs:1641-1644).
    store_refuses_late_executed: bool,
    /// The store fix's projection reads the deletions of an Interrupted outcome
    /// NotAttempted, as the code does (lifecycle.rs:2186-2190).
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

/// Which models reach a `sometimes`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    /// The code before `Standing` (`as_was`), the code as it is, and the store fix.
    Always,
    /// The code as it is, with or without the store fix.
    Standing,
    /// The code as it is without the store fix, which never refuses the `Executed` of a
    /// lost run.
    Resend,
    /// Only the store fix.
    StoreFix,
}

/// One `sometimes`: a branch the code handles, read from the state that took it, and
/// what to take out of the model to show nothing else reaches it.
struct Coverage {
    name: &'static str,
    reach: Reach,
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
        reach: Reach::Always,
        omit: Omit::Branch(Branch::ConnectingCancelled),
        reached: |_, state| took(state, Branch::ConnectingCancelled),
    },
    Coverage {
        name: "renewing aborts the prepare",
        reach: Reach::Always,
        omit: Omit::Branch(Branch::Aborts),
        reached: |_, state| {
            took(state, Branch::Aborts) && state.phase == (Phase::Preparing { aborted: true })
        },
    },
    Coverage {
        name: "renewing aborts executing",
        reach: Reach::Always,
        omit: Omit::Branch(Branch::Aborts),
        reached: |_, state| {
            took(state, Branch::Aborts) && state.phase == (Phase::Executing { aborted: true })
        },
    },
    Coverage {
        name: "renewing stops fill_ingress",
        reach: Reach::Always,
        omit: Omit::Branch(Branch::Aborts),
        reached: |_, state| {
            took(state, Branch::Aborts)
                && matches!(state.phase, Phase::Filling { stopped: true, .. })
        },
    },
    Coverage {
        name: "an aborted prepare records NotExecuted",
        reach: Reach::Always,
        omit: Omit::Branch(Branch::Unprepared),
        reached: |_, state| {
            took(state, Branch::Unprepared) && state.store.outcome == Some(Outcome::NotExecuted)
        },
    },
    Coverage {
        name: "record(Prepared) refuses a run that is over",
        reach: Reach::Always,
        omit: Omit::Branch(Branch::PreparedRefused),
        reached: |_, state| took(state, Branch::PreparedRefused),
    },
    Coverage {
        name: "record(Prepared) reads a cancel: Cancelled before it executed",
        reach: Reach::Always,
        omit: Omit::Branch(Branch::PreparedCancelling),
        reached: |_, state| took(state, Branch::PreparedCancelling),
    },
    Coverage {
        name: "the session ends mid-execution and Abandoned leaves the run Unknown",
        reach: Reach::Always,
        omit: Omit::Branch(Branch::Abandons),
        reached: |_, state| took(state, Branch::Abandons) && state.store.status == Status::Unknown,
    },
    Coverage {
        name: "the read after executing reads the takeover and the run records Interrupted",
        reach: Reach::Always,
        omit: Omit::Branch(Branch::CheckFindsLost),
        reached: |_, state| {
            took(state, Branch::CheckFindsLost) && state.last_read == Some(Read::Unknown)
        },
    },
    Coverage {
        name: "the read after executing fails and the run goes on",
        reach: Reach::Always,
        omit: Omit::Actions(&[Action::Check(Call::Fails)]),
        reached: |_, state| {
            took(state, Branch::CheckGoesOn) && state.last_read == Some(Read::Unavailable)
        },
    },
    Coverage {
        name: "the gate skips fill_ingress on a cancel",
        reach: Reach::Always,
        omit: Omit::Branch(Branch::GateSkips),
        reached: |_, state| {
            took(state, Branch::GateSkips) && state.last_read == Some(Read::Cancelling)
        },
    },
    Coverage {
        name: "the store keeps a Success and shows the deleted Volumes removed",
        reach: Reach::Always,
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
        reach: Reach::Always,
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
        reach: Reach::Always,
        omit: Omit::Actions(&[Action::CancelInFlight]),
        reached: |_, state| {
            state.last == Some(Action::CancelInFlight) && state.phase != Phase::Returned
        },
    },
    Coverage {
        name: "renewing latches a takeover during remove_volumes",
        reach: Reach::Standing,
        omit: Omit::Branch(Branch::Latches),
        reached: |_, state| took(state, Branch::Latches) && state.standing == Standing::Lost,
    },
    Coverage {
        name: "record keeps a late Executed as Interrupted and shows the deleted Volumes",
        reach: Reach::StoreFix,
        omit: Omit::Branch(Branch::Converted),
        reached: |_, state| took(state, Branch::Converted) && state.store.shows_removed,
    },
    Coverage {
        name: "renewing aborts once a read says the run ended",
        reach: Reach::Standing,
        omit: Omit::Branch(Branch::Aborts),
        reached: |_, state| took(state, Branch::Aborts) && state.last_read == Some(Read::Ended),
    },
    Coverage {
        name: "the gate skips fill_ingress once the latch says the run is over",
        reach: Reach::Standing,
        omit: Omit::Branch(Branch::GateSkips),
        reached: |_, state| {
            took(state, Branch::GateSkips)
                && state.standing != Standing::Keep
                && state.last_read != Some(Read::Cancelling)
        },
    },
    Coverage {
        name: "the run stops in connecting once a read says it lost",
        reach: Reach::Standing,
        omit: Omit::Branch(Branch::StopsOver),
        reached: |_, state| {
            took(state, Branch::StopsOver)
                && state.standing == Standing::Lost
                && matches!(state.last, Some(Action::Poll(_)))
        },
    },
    Coverage {
        name: "the run stops once a read says it ended",
        reach: Reach::Standing,
        omit: Omit::Branch(Branch::StopsOver),
        reached: |_, state| took(state, Branch::StopsOver) && state.standing == Standing::Ended,
    },
    Coverage {
        name: "record refuses a lost run's Executed and keeps it sent again as Interrupted",
        reach: Reach::Resend,
        omit: Omit::Branch(Branch::Resends),
        reached: |_, state| {
            took(state, Branch::Resends)
                && state.store.outcome
                    == Some(Outcome::Executed {
                        interrupted: true,
                        removed: false,
                    })
        },
    },
];

#[derive(Clone, Copy, Debug)]
struct RunnerStanding {
    /// The store fix, which the code does not have: `record` keeps a late `Executed` as
    /// Interrupted, and the projection shows an Interrupted outcome's deletions.
    store_fix: bool,
    broken: Broken,
    /// How many `standing()` calls' reads may fail.
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
    /// `Run::standing` (store_runner.rs:1326-1335): the latch with no call once it is
    /// set, else `record(RunEvidence::Alive)` read through `standing` (:733-749), latching
    /// Lost and Ended. The Alive arm of `record` (lifecycle.rs) answers an Unknown
    /// Deployment, and otherwise calls `running`, which refuses an ended one as a
    /// conflict. `heeds_lost` is false where `check_ignores_lost` reads Unknown as keep.
    /// Returns what it said, and the read when it made one.
    fn read(&self, state: &mut State, call: Call, heeds_lost: bool) -> (Said, Option<Read>) {
        let broken = self.broken;
        if !broken.latch_does_not_stick {
            match state.standing {
                Standing::Lost => return (Said::Lost, None),
                Standing::Ended => return (Said::Ended, None),
                Standing::Keep => {}
            }
        }
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
        let said = match read {
            Read::Running => Said::Keep,
            Read::Cancelling => Said::Cancel,
            Read::Unknown if heeds_lost => Said::Lost,
            Read::Unknown => Said::Keep,
            Read::Ended if broken.ended_reads_as_keep => Said::Unread,
            Read::Ended => Said::Ended,
            Read::Unavailable => Said::Unread,
        };
        state.standing = match said {
            Said::Lost => Standing::Lost,
            Said::Ended => Standing::Ended,
            Said::Keep | Said::Cancel | Said::Unread => Standing::Keep,
        };
        (said, Some(read))
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

    /// One `renewing` tick (store_runner.rs:1136-1149): Cancel, Lost or Ended calls
    /// `abort`, and Keep or Unread skips the check.
    fn tick(&self, state: &mut State, call: Call) {
        let Some((work, stopped)) = state.phase.renewing() else {
            return;
        };
        let before = state.standing;
        let (said, read) = self.read(state, call, true);
        if stopped || work == Work::RemoveVolumes {
            if work == Work::RemoveVolumes && before != state.standing {
                state.took = Some(Branch::Latches);
            }
            return;
        }
        let heard = match said {
            Said::Cancel | Said::Lost | Said::Ended => true,
            Said::Keep | Said::Unread => false,
        };
        if heard && !self.broken.renewing_never_aborts {
            state.phase.abort();
            state.took = Some(Branch::Aborts);
            return;
        }
        if matches!(said, Said::Lost | Said::Ended) || read.is_some_and(Read::over) {
            state.after_over.get_or_insert(work);
        }
        if said == Said::Cancel {
            state.after_cancel.get_or_insert(work);
        }
    }

    /// `record(RunEvidence::NotExecuted(..))`: `finish` with a `NotExecuted` outcome.
    fn not_executed(state: &mut State) {
        finish(&mut state.store, Outcome::NotExecuted);
        state.phase = Phase::Returned;
    }

    /// `record(RunEvidence::Executed { .. })` (lifecycle.rs:1616-1801). An Unknown
    /// Deployment takes only an Interrupted outcome, keeps it and stays Unknown
    /// (:1641-1644, :1789-1799); any other goes through `finish` (:1800). The store fix
    /// keeps a non-Interrupted one for an Unknown Deployment as Interrupted. The
    /// projection shows the deletions for a Success (:2186-2190), and in the store fix
    /// for an Interrupted outcome too.
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
        } else if self.store_fix && !broken.store_refuses_late_executed {
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
            store.shows_removed = removed
                && (!kept || (self.store_fix && !broken.projection_hides_interrupted_deletions));
        }
        state.sent = Some(Sent {
            interrupted,
            removed,
            answer,
        });
        answer
    }

    /// `Run::executed` (store_runner.rs:1200-1222): `record(Executed)` with the outcome,
    /// and one the store refused sent again as Interrupted, unless the run deleted
    /// Volumes. Returns the last answer and whether the run sent it again.
    fn record(&self, state: &mut State) -> (Answer, bool) {
        let answer = self.executed(state, false);
        let refused = matches!(answer, Answer::RefusedUnknown | Answer::RefusedEnded);
        if refused && !state.removed && !self.broken.no_resend {
            return (self.executed(state, true), true);
        }
        (answer, false)
    }

    /// `record(RunEvidence::Abandoned)` (lifecycle.rs:1904-1926): a no-op on an Unknown
    /// Deployment or one with an outcome (:1905-1907), `running` otherwise (:1908), then
    /// Unknown after a preview (:1923-1925). The model sends it only from executing, after
    /// the preview, so the arm's `NotExecuted` for a run with no preview (:1909-1922) is out
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
            // `connecting` (store_runner.rs:1116-1132) returns `Ok(None)` on Cancel, Lost
            // or Ended, and the run records NotExecuted (:202), which the store refuses
            // from a run it no longer holds.
            Action::Poll(call) => match self.read(state, call, true).0 {
                Said::Cancel if !broken.connecting_ignores_cancel => {
                    Self::not_executed(state);
                    state.took = Some(Branch::ConnectingCancelled);
                }
                Said::Lost if broken.unknown_reads_as_keep => {}
                Said::Lost | Said::Ended => {
                    Self::not_executed(state);
                    state.took = Some(Branch::StopsOver);
                }
                Said::Keep | Said::Cancel | Said::Unread => {}
            },
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
            // The read after executing (store_runner.rs:694-703): Lost records Interrupted,
            // Ended records the outcome, which the store refuses, without deleting.
            Action::Check(call) => {
                let Phase::Checking(exec) = state.phase else {
                    return;
                };
                match self.read(state, call, !broken.check_ignores_lost).0 {
                    Said::Lost => {
                        self.executed(state, true);
                        state.phase = Phase::Returned;
                        state.took = Some(Branch::CheckFindsLost);
                    }
                    Said::Ended if !broken.check_ignores_ended => {
                        self.record(state);
                        state.phase = Phase::Returned;
                        state.took = Some(Branch::StopsOver);
                    }
                    Said::Keep | Said::Cancel | Said::Ended | Said::Unread => {
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
            }
            Action::Removed => {
                state.removed = true;
                state.phase = Phase::Gating(Exec::Success);
            }
            // The gate (store_runner.rs:712-718): Keep or Unread fills ingress.
            Action::Gate(call) => {
                let Phase::Gating(exec) = state.phase else {
                    return;
                };
                let skips = match self.read(state, call, true).0 {
                    Said::Cancel => !broken.gate_ignores_cancel,
                    Said::Lost => !broken.unknown_reads_as_keep,
                    Said::Ended => !broken.gate_ignores_ended,
                    Said::Keep | Said::Unread => false,
                };
                if skips {
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
                let (answer, resent) = self.record(state);
                state.phase = Phase::Returned;
                state.took = Some(match answer {
                    Answer::Converted => Branch::Converted,
                    Answer::Kept if resent => Branch::Resends,
                    Answer::Kept | Answer::RefusedUnknown | Answer::RefusedEnded => Branch::Records,
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
                .filter(|row| match row.reach {
                    Reach::Always | Reach::Standing => true,
                    Reach::Resend => !self.store_fix,
                    Reach::StoreFix => self.store_fix,
                })
                .map(|row| Property::sometimes(row.name, row.reached)),
        );
        properties
    }
}

const TAKEOVER_KEEPS_OUTCOME: &str =
    "a run never returns with its Executed refused because the Deployment went Unknown";
/// Only the `removed_volume` node projection (lifecycle.rs:2174-2196). Applied State still
/// lists the deleted Volume under the fix, since a run that was taken over moves no Applied
/// State (lifecycle.rs:1789-1799). `cancel_in_flight` is exempt: `forget_cluster`, which
/// runs it, lets go of the Organization's Applied State and Volumes' fixed storage
/// (teardown.rs:444-447, :460-471).
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
            "{n:2}. {what}\n    store: status={:?} preview={} outcome={:?} shows_removed={} | runner: {:?} last_read={read} removed={} standing={:?}",
            state.store.status,
            state.store.preview,
            state.store.outcome,
            state.store.shows_removed,
            state.phase,
            state.removed,
            state.standing,
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
    // A call that made no read leaves `failed` and the last read as they were, and the
    // latch was already set.
    let read = if before.standing != Standing::Keep && !broken.latch_does_not_stick {
        format!("{:?} from the latch, with no read", before.standing)
    } else {
        let said = match after.last_read {
            Some(Read::Running) => "Ok(Running): Keep",
            Some(Read::Cancelling) => "Ok(Cancelling): Cancel",
            Some(Read::Unknown) => "Ok(Unknown): Lost",
            Some(Read::Ended) if broken.ended_reads_as_keep => {
                "Err(conflict: This Deployment already ended): Unread (under Broken)"
            }
            Some(Read::Ended) => "Err(conflict: This Deployment already ended): Ended",
            Some(Read::Unavailable) => "Err(unavailable: the Store call failed): Unread",
            None => "?",
        };
        said.to_owned()
    };
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
                Some(Branch::StopsOver) => {
                    "returns Ok(None); the run records NotExecuted, which the store refuses"
                }
                _ if after.last_read == Some(Read::Cancelling) => {
                    "dials on (under Broken: the Cancelling check skipped)"
                }
                _ if after.standing == Standing::Lost => "dials on (under Broken: Lost read as keep)",
                _ => "dials on",
            };
            format!("Runner: connecting's poll reads standing() = {read}; {then} (store_runner.rs connecting)")
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
                    "the abort is || (), deleting goes on; the latch keeps {:?}",
                    after.standing
                ),
                _ if before.phase.renewing().is_some_and(|(_, stopped)| stopped) => {
                    "its abort already ran".to_owned()
                }
                _ if before.phase == Phase::Removing => "the abort is || (): deleting goes on".to_owned(),
                _ if broken.renewing_never_aborts => {
                    "never aborts (under Broken): the work goes on".to_owned()
                }
                _ => "Keep or Unread skips the check: the work goes on".to_owned(),
            };
            format!("Runner: renewing's tick during {work} reads standing() = {read}; {then} (store_runner.rs renewing)")
        }
        Action::Prepare => match after.took {
            Some(Branch::Unprepared) => {
                "Runner: the aborted prepare returns Err; the run records NotExecuted (store_runner.rs execute, unprepared)".to_owned()
            }
            Some(Branch::PreparedRefused) => format!(
                "Runner: record(Prepared) answers Err, {}; the run returns it (store_runner.rs execute)",
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
                Some(Branch::CheckFindsLost) => "Lost: the run records Interrupted with no deletions".to_owned(),
                Some(Branch::StopsOver) => {
                    "Ended: the run records its outcome with no deletions, which the store refuses".to_owned()
                }
                _ if after.last_read == Some(Read::Unknown) => {
                    "Unknown read as keep (under Broken)".to_owned()
                }
                _ if after.standing == Standing::Ended => "Ended read as keep (under Broken)".to_owned(),
                _ => {
                    let next = if after.phase == Phase::Removing {
                        "remove_volumes starts under renewing with the no-op abort || ()"
                    } else {
                        "the outcome is not a Success, so no Volume is deleted"
                    };
                    format!("the run goes on; {next}")
                }
            };
            format!("Runner: the read after executing reads standing() = {read}; {then} (store_runner.rs execute)")
        }
        Action::Removed => {
            "Runner: remove_volumes returns the Volumes it deleted (store_runner.rs execute)".to_owned()
        }
        Action::Gate(_) => {
            let then = match after.took {
                Some(Branch::GateSkips) => "fill_ingress is skipped",
                _ if after.last_read == Some(Read::Cancelling) => {
                    "Cancel, skipped (under Broken): fill_ingress starts under renewing"
                }
                _ if after.standing == Standing::Lost => {
                    "Lost read as keep (under Broken): fill_ingress starts under renewing"
                }
                _ if after.standing == Standing::Ended => {
                    "Ended read as keep (under Broken): fill_ingress starts under renewing"
                }
                _ => "fill_ingress starts under renewing",
            };
            format!("Runner: the gate reads standing() = {read}; {then} (store_runner.rs execute)")
        }
        Action::Filled => "Runner: fill_ingress ends (store_runner.rs fill_ingress)".to_owned(),
        Action::Record => {
            let sent = after.sent.expect("record(Executed) sets sent");
            let removed = if sent.removed {
                ", naming the deleted Volumes"
            } else {
                ""
            };
            let refusal = match before.store.status {
                Status::Unknown => {
                    "the Deployment is Unknown and the outcome not Interrupted: running() refuses it"
                }
                Status::Running | Status::Cancelling | Status::Ended => {
                    "the Deployment ended: finish's running() refuses it"
                }
            };
            let answer = match (after.took, sent.answer) {
                (Some(Branch::Resends), _) => format!(
                    "{refusal}; the run sends it again as Interrupted, and the store keeps it; the Deployment stays Unknown"
                ),
                (_, Answer::Kept) if before.store.status == Status::Unknown => {
                    "running() skipped (under Broken): the store keeps it; the Deployment stays Unknown".to_owned()
                }
                (_, Answer::Kept) => "the store keeps it and ends the Deployment".to_owned(),
                (_, Answer::Converted) => {
                    "the Deployment is Unknown: the store keeps it as Interrupted (the store fix)".to_owned()
                }
                (_, Answer::RefusedUnknown | Answer::RefusedEnded) if sent.interrupted => {
                    format!("{refusal}; sent again as Interrupted, it is refused again; executed returns the first refusal")
                }
                (_, Answer::RefusedUnknown) if sent.removed && !broken.no_resend => format!(
                    "{refusal}; the run deleted Volumes, so it sends nothing again; executed returns that Err"
                ),
                (_, Answer::RefusedUnknown | Answer::RefusedEnded) => {
                    format!("{refusal}; executed returns that Err")
                }
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
                "Runner: executed records its outcome{removed}; {answer}{shown} (store_runner.rs executed, lifecycle.rs record)"
            )
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use stateright::Checker;

    use super::*;
    use crate::harness::{Narrated, breaks, breaks_where, check, last, took_action};

    impl Narrated for RunnerStanding {
        fn story(&self, path: &[(State, Option<Action>)]) -> String {
            story(self, path)
        }
    }

    /// The code as it is, with every `standing()` call's read answering.
    fn as_is() -> RunnerStanding {
        RunnerStanding {
            store_fix: false,
            broken: Broken::default(),
            failures: 0,
            omit: None,
        }
    }

    /// The code before `Standing`: no latch, an ended Deployment's conflict read as keep,
    /// `connecting` and the gate going on after Unknown, and a refused `Executed`
    /// returned as it is.
    fn as_was() -> RunnerStanding {
        as_is_without(Broken {
            latch_does_not_stick: true,
            ended_reads_as_keep: true,
            unknown_reads_as_keep: true,
            no_resend: true,
            ..Broken::default()
        })
    }

    fn as_is_without(broken: Broken) -> RunnerStanding {
        RunnerStanding { broken, ..as_is() }
    }

    /// The code as it is with the store fix: a late `Executed` kept as Interrupted and its
    /// deletions shown.
    fn store_fixed() -> RunnerStanding {
        RunnerStanding {
            store_fix: true,
            ..as_is()
        }
    }

    /// `model` with one `standing()` call whose read may fail.
    fn failing(model: RunnerStanding) -> RunnerStanding {
        RunnerStanding {
            failures: 1,
            ..model
        }
    }

    /// Every state, in the whole of `model`, that breaks the `always` named `property`.
    fn every_break(model: RunnerStanding, property: &'static str) -> Vec<State> {
        let holds = model
            .properties()
            .into_iter()
            .find(|candidate| candidate.name == property)
            .expect("a model property")
            .condition;
        let broken = Arc::new(Mutex::new(Vec::new()));
        let seen = Arc::clone(&broken);
        model
            .checker()
            .threads(1)
            .visitor(move |path: stateright::Path<State, Action>| {
                let state = path.last_state();
                if !holds(&model, state) {
                    seen.lock().expect("the breaks").push(*state);
                }
            })
            .spawn_bfs()
            .join();
        broken.lock().expect("the breaks").clone()
    }

    /// What is left of the first finding. A Success deletes the Volumes the Deploy
    /// removes, and a newer claim then sets the Deployment Unknown. `record(Executed)`
    /// is refused, and the run sends nothing again, since the store would read the
    /// deleted Volumes NotAttempted under an Interrupted outcome. Neither the outcome nor
    /// the deletions are kept. No `standing()` read fails on the path. This test passes
    /// by finding it.
    #[test]
    fn the_code_still_loses_the_volumes_a_run_deleted_once_taken_over() {
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
            .expect("the read after executing read the store");
        let read = path.get(check + 1).expect("the state the read led to").0;
        assert_eq!(read.last_read, Some(Read::Running));
        let takeover = path
            .iter()
            .position(|(_, action)| *action == Some(Action::Takeover))
            .expect("a takeover");
        assert!(takeover > check, "the takeover landed after the read");
        let before = path.get(takeover).expect("the state the takeover left").0;
        assert_eq!(before.phase, Phase::Removing);
        assert_eq!(broken.failed, 0, "no standing() read failed");
    }

    /// The code as it is, with a failed read, loses an outcome or the deletions only
    /// after deleting Volumes, keeps every other property, and reaches every `sometimes`
    /// it has.
    #[test]
    fn the_code_loses_an_outcome_only_after_deleting_volumes() {
        let checker = check(failing(as_is()));
        println!("as is: {} states", checker.unique_state_count());
        for property in [NO_WORK_AFTER_END, HEEDS_CANCEL, TAKEN_OVER_INTERRUPTED] {
            assert!(checker.discovery(property).is_none(), "{property} fails");
        }
        for property in [TAKEOVER_KEEPS_OUTCOME, DELETIONS_KEPT] {
            assert!(checker.discovery(property).is_some(), "{property} holds");
            let breaks = every_break(failing(as_is()), property);
            assert!(
                breaks.iter().all(|state| state.removed),
                "{property} breaks with no Volume deleted"
            );
        }
        for row in COVERAGE.iter().filter(|row| row.reach != Reach::StoreFix) {
            assert!(
                checker.discovery(row.name).is_some(),
                "{} is never reached",
                row.name
            );
        }
    }

    /// The code before `Standing`, with one failed read, breaks only the properties the
    /// findings name, and reaches every `sometimes` it had.
    #[test]
    fn the_code_as_it_was_breaks_only_the_findings() {
        let checker = check(failing(as_was()));
        println!("as was: {} states", checker.unique_state_count());
        for property in [HEEDS_CANCEL, TAKEN_OVER_INTERRUPTED] {
            assert!(checker.discovery(property).is_none(), "{property} fails");
        }
        for property in [TAKEOVER_KEEPS_OUTCOME, DELETIONS_KEPT, NO_WORK_AFTER_END] {
            assert!(checker.discovery(property).is_some(), "{property} holds");
        }
        for row in COVERAGE.iter().filter(|row| row.reach == Reach::Always) {
            assert!(
                checker.discovery(row.name).is_some(),
                "{} is never reached",
                row.name
            );
        }
    }

    /// The store fix, which the code does not have, holds with a failed read, and every
    /// `sometimes` it has is reached.
    #[test]
    fn a_converting_store_would_keep_the_outcome_and_the_deletions() {
        let checker = check(failing(store_fixed()));
        println!("store fixed: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// Every `sometimes` needs what it names: with the row's `omit` taken out of a model
    /// that reaches it, no other path reaches its predicate.
    #[test]
    fn every_sometimes_needs_what_it_names() {
        for row in COVERAGE {
            let model = match row.reach {
                Reach::Always | Reach::Standing | Reach::Resend => failing(as_is()),
                Reach::StoreFix => failing(store_fixed()),
            };
            assert!(
                check(model).discovery(row.name).is_some(),
                "{} is never reached",
                row.name
            );
            let cut = check(RunnerStanding {
                omit: Some(row.omit),
                ..model
            });
            assert!(
                cut.discovery(row.name).is_none(),
                "{} is reached without {:?}",
                row.name,
                row.omit
            );
        }
    }

    /// The first finding as the code before `Standing` had it, kept by the resend. With
    /// it dropped, the read after executing and the gate before `fill_ingress` are the
    /// run's last reads, and a newer claim lands after them. The Deployment is Unknown,
    /// `record(Executed)` is refused, and the store keeps no outcome, with no Volume
    /// deleted and no read failed.
    #[test]
    fn the_code_needs_to_send_a_refused_outcome_again() {
        let path = breaks_where(
            as_is_without(Broken {
                no_resend: true,
                ..Broken::default()
            }),
            TAKEOVER_KEEPS_OUTCOME,
            |path| !last(path).removed,
        );
        let broken = last(&path);
        assert_eq!(broken.store.status, Status::Unknown);
        assert_eq!(broken.store.outcome, None);
        assert_eq!(
            broken.sent,
            Some(Sent {
                interrupted: false,
                removed: false,
                answer: Answer::RefusedUnknown,
            })
        );
        assert_eq!(broken.last, Some(Action::Record));
        assert_eq!(broken.failed, 0, "no standing() read failed");
    }

    /// The second finding's first cause, as the code before `Standing` had it. With an
    /// ended Deployment's conflict read as keep, `forget_cluster` cancels the Deployment in
    /// flight, and `renewing`'s next tick skips the `Err` ("already ended"): the work in
    /// hand keeps going on a Deployment the store ended.
    #[test]
    fn the_code_needs_an_ended_read_to_stop_it() {
        let path = breaks(
            as_is_without(Broken {
                ended_reads_as_keep: true,
                ..Broken::default()
            }),
            NO_WORK_AFTER_END,
        );
        let broken = last(&path);
        assert!(took_action(&path, Action::CancelInFlight));
        assert_eq!(broken.store.status, Status::Ended);
        assert_eq!(broken.last, Some(Action::Tick(Call::Answers)));
        assert_eq!(broken.last_read, Some(Read::Ended));
        assert_eq!(broken.after_over, Some(Work::Prepare));
    }

    /// The second finding's second cause, as the code before `Standing` had it, with no
    /// `cancel_in_flight` and no failed read. With Lost read as keep, after a takeover
    /// `connecting` reads `Ok(Unknown)` and dials on, and the prepare starts. The gate
    /// before `fill_ingress` reads `Ok(Unknown)` as keep too.
    #[test]
    fn the_code_needs_connecting_and_the_gate_to_stop_on_lost() {
        let model = RunnerStanding {
            omit: Some(Omit::Actions(&[Action::CancelInFlight])),
            ..as_is_without(Broken {
                unknown_reads_as_keep: true,
                ..Broken::default()
            })
        };
        let path = breaks(model, NO_WORK_AFTER_END);
        let broken = last(&path);
        assert_eq!(broken.store.status, Status::Unknown);
        assert_eq!(broken.after_over, Some(Work::Prepare));
        assert!(took_action(&path, Action::Poll(Call::Answers)));
        assert_eq!(broken.failed, 0, "no standing() read failed");
        let path = breaks_where(model, NO_WORK_AFTER_END, |path| {
            last(path).after_over == Some(Work::FillIngress)
        });
        let broken = last(&path);
        assert_eq!(broken.last, Some(Action::Gate(Call::Answers)));
        assert_eq!(broken.last_read, Some(Read::Unknown));
    }

    /// Without the latch sticking, a failed read after one that said the run is over
    /// reads as keep, and the run starts work the latch would have stopped. Only a failed
    /// read can follow Lost or Ended with another answer, since no store step leaves
    /// Unknown or an end.
    #[test]
    fn the_code_needs_its_latch_to_stick() {
        let path = breaks(
            failing(as_is_without(Broken {
                latch_does_not_stick: true,
                ..Broken::default()
            })),
            NO_WORK_AFTER_END,
        );
        let broken = last(&path);
        assert_eq!(broken.failed, 1);
        assert!(broken.after_over.is_some());
        assert_eq!(broken.standing, Standing::Keep);
    }

    /// Without `connecting`'s Cancelling check, a run cancelled while it dials starts
    /// preparing.
    #[test]
    fn connecting_needs_its_cancel_check() {
        let path = breaks(
            as_is_without(Broken {
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
            as_is_without(Broken {
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
            as_is_without(Broken {
                prepared_ignores_cancel: true,
                ..Broken::default()
            }),
            HEEDS_CANCEL,
        );
        assert_eq!(last(&path).after_cancel, Some(Work::Execute));
    }

    /// Without the read after executing heeding Lost, a run taken over while it
    /// executes, with no tick in between, starts deleting Volumes though that read said
    /// Unknown.
    #[test]
    fn deleting_needs_the_read_after_executing() {
        let path = breaks(
            as_is_without(Broken {
                check_ignores_lost: true,
                ..Broken::default()
            }),
            NO_WORK_AFTER_END,
        );
        assert_eq!(last(&path).after_over, Some(Work::RemoveVolumes));
    }

    /// Without the read after executing stopping on Ended, a run `cancel_in_flight` ended
    /// after `renewing`'s last tick starts deleting Volumes though that read said it
    /// ended.
    #[test]
    fn deleting_needs_the_read_after_executing_to_stop_on_ended() {
        let path = breaks(
            as_is_without(Broken {
                check_ignores_ended: true,
                ..Broken::default()
            }),
            NO_WORK_AFTER_END,
        );
        let broken = last(&path);
        assert!(took_action(&path, Action::CancelInFlight));
        assert_eq!(broken.standing, Standing::Ended);
        assert_eq!(broken.after_over, Some(Work::RemoveVolumes));
    }

    /// Without the gate stopping on Ended, a run `cancel_in_flight` ended after the read
    /// after executing fills ingress though the gate said it ended.
    #[test]
    fn fill_ingress_needs_the_gate_to_stop_on_ended() {
        let path = breaks_where(
            as_is_without(Broken {
                gate_ignores_ended: true,
                ..Broken::default()
            }),
            NO_WORK_AFTER_END,
            |path| matches!(last(path).last, Some(Action::Gate(_))),
        );
        let broken = last(&path);
        assert!(took_action(&path, Action::CancelInFlight));
        assert_eq!(broken.standing, Standing::Ended);
        assert_eq!(broken.after_over, Some(Work::FillIngress));
    }

    /// Without the gate's Cancelling check, a cancelled run fills ingress.
    #[test]
    fn fill_ingress_needs_the_gate() {
        let path = breaks(
            as_is_without(Broken {
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
            as_is_without(Broken {
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

    /// Without the store fix keeping a late `Executed` as Interrupted, a takeover after
    /// the run deleted Volumes gets its outcome refused, as in the code as it is.
    #[test]
    fn the_store_fix_needs_record_to_keep_a_late_executed() {
        let path = breaks(
            RunnerStanding {
                broken: Broken {
                    store_refuses_late_executed: true,
                    ..Broken::default()
                },
                ..store_fixed()
            },
            TAKEOVER_KEEPS_OUTCOME,
        );
        let broken = last(&path);
        assert_eq!(broken.store.outcome, None);
        assert!(broken.removed);
    }

    /// Without the store fix's projection showing an Interrupted outcome's deletions, the
    /// store keeps the outcome and still reads the deleted Volumes NotAttempted.
    #[test]
    fn the_store_fix_needs_the_projection_to_show_interrupted_deletions() {
        let path = breaks(
            RunnerStanding {
                broken: Broken {
                    projection_hides_interrupted_deletions: true,
                    ..Broken::default()
                },
                ..store_fixed()
            },
            DELETIONS_KEPT,
        );
        assert!(last(&path).store.outcome.is_some());
    }
}
