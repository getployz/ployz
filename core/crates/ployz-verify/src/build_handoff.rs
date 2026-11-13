//! A Git build handed to a GitHub Actions run, between Cloud's walk that follows the run and
//! the runner that checks in for its Build Grant. A bare `:n` is a line of the file named just
//! before it in the same parentheses, and otherwise of
//! `dashboard/src/modules/config-store/store-github-builds.server.ts`.
//!
//! The actors, each its own process:
//! - **Cloud's walk.** Cloud's worker runs the Inngest function `run-store-deployment`, and
//!   `walkGithub` (dashboard/src/modules/config-store/store-deployment.inngest.ts:151-167) runs
//!   one `github-open` step, one wait and one `github-check` step per look. Each check step runs
//!   `checkStoreGithubBuild` (:190-225). Its `store.githubFinish` (:209) calls the Store through
//!   the native SDK in the worker's own process (core/crates/ployz-sdk/src/config_store.rs:648),
//!   so `github_finish` and `inspect_grant` run there too
//!   (core/crates/ployz/src/sdk/github_build.rs:466-556, :584-611).
//! - **The runner.** A GitHub Actions job. Its check-in and its Build Steps reports are HTTP
//!   requests that Cloud web serves (:275-290, :298-321), and each calls the Store through the
//!   native SDK in Cloud web's process: `github_check_in` (core/crates/ployz/src/sdk/github_build.rs:187-274)
//!   and `github_report` (core/crates/ployz/src/sdk/github_build.rs:384-432). The report
//!   handler then calls `githubFinish` itself (:312).
//! - **A user** who cancels the Deployment. Cloud web commits the Store's cancel, and its
//!   `afterWrite` stops the GitHub builds (dashboard/src/modules/config-store/config-store.server.ts:306-310).
//! - **GitHub**, which ends the run once its job exits or Cloud cancelled it, and sends the
//!   `workflow_run` webhook (dashboard/src/routes/api/github/-webhook.handler.ts:78-83).
//! - **The Deployment runner** on the Deployment's Servers, which reports a build it runs
//!   through `deployment::record::record` (core/crates/ployz-store/src/deployment/record.rs). Its
//!   `run-deployment` step runs once every walk returned
//!   (dashboard/src/modules/config-store/store-deployment.inngest.ts:62, :79).
//! - **The Store**, Postgres. Each write runs in one transaction that first locks the
//!   Deployment's Environment row (`deployment::locked`, core/crates/ployz-store/src/deployment/mod.rs:1604-1608;
//!   `scope::lock_all` in core/crates/ployz-store/src/scope.rs) and then re-reads the build. Writers run read
//!   committed (core/crates/ployz-store/src/storage/pg.rs:107-120), so each write sees every
//!   write committed before it took the lock, and each model write is one atomic step.
//!
//! The start limit's look reads the build with no grant (:194, :216) and calls
//! `finish("start_limit")` (:219). The run's check-in can commit before `github_finish`
//! re-reads the build (core/crates/ployz/src/sdk/github_build.rs:473). At the start limit
//! `github_finish` issues only `GithubEnd::Unstarted`
//! (core/crates/ployz/src/sdk/github_build.rs:489-504), whose guard
//! (core/crates/ployz-store/src/build.rs:1248-1250) refuses it once a check-in committed, and
//! `github_finish` then answers Waiting (core/crates/ployz/src/sdk/github_build.rs:499-503).
//! Cloud cancels the run only on `"done"` (:220), so a run that checked in keeps its build and
//! the walk goes on to its next look. `Broken::start_limit_reads_the_grant` is the code before
//! the look was passed, `github_finish(timed_out=true)`: it settled such a run Skipped "the run
//! ran out of time", or Failed when the grant's Machine couldn't be reached, and Cloud cancelled
//! it after it got its Build Grant and build secrets.
//!
//! The model starts from two `github-start` steps. On the first try, `githubDispatched`
//! committed the run and answered (:164-167). On the retry, the first try's `githubDispatched`
//! timed out, so Cloud cancelled its run (:166) and Inngest ran the step again
//! (dashboard/src/modules/config-store/store-deployment.inngest.ts:41, :153). The timed-out
//! call keeps running (core/crates/ployz/src/sdk/store_call.rs:35-55), so its write
//! (`Who::Delayed`) can commit at any step after: before the retry reads the build, between
//! that read and the retry's own hand-off, or never in time. The retry follows a run that
//! holds the build (:118-119); otherwise it dispatches its own run and hands it the build. A
//! refused hand-off means another run's committed first, and the retry reads the build again
//! and follows that run (:122, :172). If the retry's hand-off wins, the first try's write is
//! refused, and the walk follows the retry's run. A retry that skips GitHub instead goes to the
//! same second read when its skip is refused (:126, :135, :172), so the refused hand-off stands
//! for it; a skip that commits refuses the first try's write, and the walk returns.
//! `Broken::start_leaves_held_run` answers `done` for a refused hand-off without the second
//! read, so the walk returns while the first try's run holds the build
//! (dashboard/src/modules/config-store/store-deployment.inngest.ts:154).
//!
//! The Deployment runner's report on the build goes through `record`
//! (core/crates/ployz-store/src/build.rs:264-314), which refuses a build a GitHub run holds or
//! ended (core/crates/ployz-store/src/build.rs:289-296). The runner then stops, Cloud reads the
//! conflict as nothing to run
//! (dashboard/src/modules/config-store/store-deployment.server.ts:107), and the Deployment
//! lapses Unknown. `Broken::record_ignores_github` drops that guard: a Server then builds what
//! the run holds.

use std::fmt::Write as _;

use ployz_decide::github_build::{
    self as real, EndKind, EndedRun, First, GithubLook, Hold, Target, github_end_holds,
};
use stateright::{Model, Property};

use crate::harness::{self, everywhere};

/// The most looks the walk takes (`for (let check = 0; ; check += 1)`,
/// dashboard/src/modules/config-store/store-deployment.inngest.ts:155). Check 0 is the start
/// limit, and check 1 the later look that ends a run the start limit waited for: with one
/// look, the rows "after the start limit waited for a run that checked in, …" are never
/// reached.
const MAX_CHECKS: u8 = 2;
/// How many times Inngest runs a failed step again (`retries: 2`,
/// dashboard/src/modules/config-store/store-deployment.inngest.ts:41).
const RETRIES: u8 = 2;

/// A GitHub run. The model runs one, `RUN`; a test builds a row held by another.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Run(u8);

const RUN: Run = Run(0);

/// The check-in request whose mint a grant came from: the job's, or a second request of the
/// same run, which only the two-request lift sends.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Request {
    Job,
    Again,
}

/// `BuildStatus` (core/crates/ployz-store/src/build.rs:56-64). Reused is only written before a
/// run is dispatched, which the model starts after.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Status {
    Pending,
    Building,
    Built,
    Failed,
}

/// `RunEnd`, the final report's end (core/crates/ployz-store/src/build.rs:875-880).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum RunEnd {
    Built,
    Failed,
}

/// `GithubHold` (core/crates/ployz-store/src/build.rs:764-779). A check-in's grant and time
/// are one `Started` (core/crates/ployz-store/src/build.rs:784-790), written only by
/// `github_check_in` (core/crates/ployz-store/src/build.rs:1155-1164), so one field stands for
/// both. `received` decides nothing but a report's own skip, so it is left out.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Github {
    run: Run,
    grant: Option<Request>,
    ended: Option<RunEnd>,
}

/// The build's `config_build` row: `status`, `github`, and whether `github_skipped` is set.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Row {
    status: Status,
    github: Option<Github>,
    skipped: bool,
}

impl Row {
    /// `building_on` (core/crates/ployz-store/src/build.rs:1001-1006).
    fn building_on(self, run: Run) -> Option<Github> {
        self.github
            .filter(|github| self.status == Status::Building && github.run == run)
    }

    /// Whether `run` holds the build with a committed check-in.
    fn started_on(self, run: Run) -> bool {
        self.building_on(run)
            .is_some_and(|github| github.grant.is_some())
    }
}

/// The Deployment's status, as the walk (:197-198) and `github_check_in`'s guard
/// (core/crates/ployz-store/src/build.rs:1152) read it. The walk runs before the runner claims
/// the Deployment (dashboard/src/modules/config-store/store-deployment.inngest.ts:62, :79), so it is queued until a user cancels it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Deployment {
    Queued,
    /// `cancel` turned a queued Deployment Cancelled
    /// (`deployment::cancel`, core/crates/ployz-store/src/deployment/lifecycle.rs).
    Cancelled,
}

/// Why a build failed or was passed on, as the message the Store records says.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Why {
    /// `Why::RanOutOfTime` in `ployz-decide`.
    RanOutOfTime,
    /// `Why::EndedWithoutPushing` in `ployz-decide`.
    EndedWithoutPushing,
    /// `Why::NoRunnerInTime` in `ployz-decide`.
    NoRunnerInTime,
    /// `Why::EndedBeforeStart` in `ployz-decide`.
    EndedBeforeStart,
    /// `OLD_MINT_SKIPS` (core/crates/ployz/src/sdk/github_build.rs:276-277).
    OldMint,
    /// `Why::StepFailed` in `ployz-decide`.
    StepFailed,
    /// `Why::Unreachable` in `ployz-decide`.
    Unreachable,
    /// `Why::Refused` in `ployz-decide`: the Machine's refusal.
    Refused,
    /// "cancelled" (core/crates/ployz/src/sdk/github_build.rs:657-659).
    Cancelled,
}

impl Why {
    /// The sentence the Store records, which the real [`real::Why`] says for the ones
    /// `github_finish` ends a build with.
    fn message(self) -> String {
        match self {
            Self::RanOutOfTime => real::Why::RanOutOfTime.message().to_owned(),
            Self::EndedWithoutPushing => real::Why::EndedWithoutPushing.message().to_owned(),
            Self::NoRunnerInTime => real::Why::NoRunnerInTime.message().to_owned(),
            Self::EndedBeforeStart => real::Why::EndedBeforeStart.message().to_owned(),
            Self::StepFailed => real::Why::StepFailed.message().to_owned(),
            Self::Unreachable => real::Why::Unreachable.message().to_owned(),
            Self::OldMint => {
                "GitHub builds a sealed value only with an up-to-date Ployz on the runner and your server".to_owned()
            }
            Self::Refused => "<the Machine's refusal>".to_owned(),
            Self::Cancelled => "cancelled".to_owned(),
        }
    }

    /// The model's name for what `github_finish` ends a build with.
    fn of(why: &real::Why) -> Self {
        match why {
            real::Why::RanOutOfTime => Self::RanOutOfTime,
            real::Why::EndedWithoutPushing => Self::EndedWithoutPushing,
            real::Why::NoRunnerInTime => Self::NoRunnerInTime,
            real::Why::EndedBeforeStart => Self::EndedBeforeStart,
            real::Why::StepFailed => Self::StepFailed,
            real::Why::Unreachable => Self::Unreachable,
            real::Why::Refused(_) => Self::Refused,
        }
    }
}

/// `GithubEnd` (core/crates/ployz-store/src/build.rs:930-944).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum End {
    Built,
    Failed(Why),
    Skipped(Why),
    Unstarted(Why),
}

impl std::fmt::Display for End {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Built => formatter.write_str("Built"),
            Self::Failed(why) => write!(formatter, "Failed \"{}\"", why.message()),
            Self::Skipped(why) => write!(formatter, "Skipped \"{}\"", why.message()),
            Self::Unstarted(why) => write!(formatter, "Unstarted \"{}\"", why.message()),
        }
    }
}

/// The look as Cloud passes it to `githubFinish` (`code`): the budget (:214), the start
/// limit (:219), or a run that ended (:217, :223, and the report's own at :312).
fn code(look: GithubLook) -> &'static str {
    match look {
        GithubLook::Budget => "budget",
        GithubLook::StartLimit => "start_limit",
        GithubLook::Ended => "ended",
    }
}

/// What `inspect_grant` read on the grant's Machine (`GrantRead`,
/// core/crates/ployz/src/sdk/github_build.rs:573-582).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum GrantRead {
    /// `Pushed`: what the Machine received, read from the model's `pushed` while the Machine
    /// still holds the grant. Once it lost the grant (`lost`), `inspect` answers NotFound
    /// whatever was pushed (core/crates/ployzd/src/management/build_grant.rs:203-221), which
    /// reads as nothing pushed (core/crates/ployz/src/sdk/github_build.rs:629-630).
    Pushed,
    /// `Refused`: the Machine belongs to another Cluster or refused the read (core/crates/ployz/src/sdk/github_build.rs:597-601, :637-638).
    Refused,
    /// `Unreachable`: no connection names the Machine, the dial failed, or the read failed
    /// without a refusal (core/crates/ployz/src/sdk/github_build.rs:593-594, :599-600, :603-604, :639).
    Unreachable,
}

/// What the Machine's mint answered (`mint`, core/crates/ployz/src/sdk/github_build.rs:296-321).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Mint {
    /// `Ok(Some(..))`: a grant, held on the Machine.
    Minted,
    /// `Ok(None)`: an old Action or Machine with a sealed value (core/crates/ployz/src/sdk/github_build.rs:308-311).
    Old,
    /// `Err`: the Machine refused or couldn't be reached (core/crates/ployz/src/sdk/github_build.rs:303, :319, :237-240).
    Fails,
}

/// A digest for what the grant's Machine received: the model tracks only that it did.
fn digest() -> ployz_core::ImageDigest {
    ployz_core::ImageDigest::parse(format!("sha256:{}", "a".repeat(64)))
        .expect("a digest is sha256 and 64 hex")
}

/// A `github_finish` statement next (`github_finish` in core/crates/ployz/src/sdk/github_build.rs).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Finish {
    /// The re-read of the build, then `first` in `ployz-decide`.
    Read,
    /// `inspect_grant`, then `after_read` in `ployz-decide`, with the end the re-read found.
    Inspect { ended: Option<RunEnd> },
    /// `settle`'s `github_end`.
    Settle(End),
}

/// How `github_finish` returned.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Finished {
    /// `GithubFinish::Ended`.
    Ended(Status),
    /// `GithubFinish::Waiting`.
    Waiting,
    /// The Store's `conflict` from a settle that is not Unstarted, which `storeTry` fails with.
    Conflict,
}

/// `cancelStoreGithubBuilds` (dashboard/src/modules/config-store/config-store.server.ts:67-72):
/// `github_cancel` (core/crates/ployz/src/sdk/github_build.rs:650-667), then each run cancelled.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Stop {
    /// `github_outstanding`'s read (core/crates/ployz/src/sdk/github_build.rs:654).
    Outstanding,
    /// The build it found: its `github_end` Failed "cancelled" (core/crates/ployz/src/sdk/github_build.rs:657-664).
    Ending,
    /// `cancelDispatchedGithubRun` for it (dashboard/src/modules/config-store/config-store.server.ts:70).
    Cancelling,
}

/// Where a `github-check` step is in `checkStoreGithubBuild`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum At {
    /// `heldGithubBuild` is next (:194).
    Read,
    /// The Deployment's read is next (:197). `grant` and `ended` are what :194 read.
    Deployment { grant: bool, ended: bool },
    /// `cancelStoreGithubBuilds` (:199).
    Stopping(Stop),
    /// `cancelDispatchedGithubRun` past the budget (:213).
    BudgetCancel,
    /// `runEnded` is next: at :217 without a grant, at :223 with one.
    AskingGithub { grant: bool },
    /// `finish` (:207-211), called with `look`.
    Finishing { look: GithubLook, at: Finish },
    /// `cancelDispatchedGithubRun` after the start limit's finish (:220). `waiting` is what
    /// the finish answered, which :221 returns.
    LimitCancel { waiting: bool },
}

/// Where Cloud's walk is (`walkGithub`, dashboard/src/modules/config-store/store-deployment.inngest.ts:151-167).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Walk {
    /// Inside the retried `github-start` step (:112-174).
    Start(Start),
    /// `github-open-${check}` is next (dashboard/src/modules/config-store/store-deployment.inngest.ts:156).
    Open(u8),
    /// `github-wait-${check}` (dashboard/src/modules/config-store/store-deployment.inngest.ts:158-161). `woken`: the run's event
    /// came while it waited.
    Waiting { check: u8, woken: bool },
    /// Inside `github-check-${check}` (dashboard/src/modules/config-store/store-deployment.inngest.ts:163). `ended` is the wait's
    /// `seen.ended`; `attempt` counts this step's runs before this one.
    Checking {
        check: u8,
        ended: bool,
        attempt: u8,
        at: At,
    },
    /// `walkGithub` returned (dashboard/src/modules/config-store/store-deployment.inngest.ts:154, :156, :164-165).
    Done,
    /// A step failed with no retry left: the function fails, and its `onFailure` runs
    /// (dashboard/src/modules/config-store/store-deployment.inngest.ts:44-51).
    GaveUp,
}

/// The retried `github-start`'s next statement.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Start {
    /// `holder`'s first read (:118-119).
    Read,
    /// `githubDispatched` for the retry's own run (:164-166), which `github_dispatched`
    /// (core/crates/ployz-store/src/build.rs:1013-1036) commits only for a waiting build of a
    /// Deployment in flight.
    HandOff,
    /// `holder`'s read after a refused hand-off (`refused`, :122, :172).
    Reread,
}

/// A check-in request's next statement (core/crates/ployz/src/sdk/github_build.rs:187-274).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum CheckIn {
    /// The pre-check passed; `mint` is next (core/crates/ployz/src/sdk/github_build.rs:222).
    Minting,
    /// `mint` answered None; the old-mint skip's `github_end` is next (core/crates/ployz/src/sdk/github_build.rs:225-235).
    OldMint,
    /// A grant was minted; the Store's `github_check_in` is next (core/crates/ployz/src/sdk/github_build.rs:247-251).
    Committing,
}

/// Where the runner's job is.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Job {
    /// It has not checked in: its check-in's pre-check is next (:280-281,
    /// core/crates/ployz/src/sdk/github_build.rs:195-204).
    Queued,
    /// Inside its check-in request, which Cloud web serves.
    CheckingIn(CheckIn),
    /// Checked in, with its grant and secrets; it builds.
    Building,
    /// It pushed into the grant's Machine.
    Pushed,
    /// Its final Build Steps report is next (:308, core/crates/ployz-store/src/build.rs:1177-1233).
    Reporting(RunEnd),
    /// The report handler's own `githubFinish(build, "ended")` (:309-314).
    Finishing(Finish),
    /// The report handler's `sendInngestEvent` (:315).
    Waking,
    /// The job has exited, or GitHub ended it.
    Exited,
}

impl Job {
    /// Between requests: GitHub ending the run ends the job here. Inside a request, Cloud web
    /// goes on serving it.
    fn between_requests(self) -> bool {
        match self {
            Self::Queued | Self::Building | Self::Pushed => true,
            Self::CheckingIn(_)
            | Self::Reporting(_)
            | Self::Finishing(_)
            | Self::Waking
            | Self::Exited => false,
        }
    }
}

/// A second check-in request of the same run, under the two-request lift.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Again {
    /// The model runs one request.
    Absent,
    /// Not sent yet.
    Unsent,
    CheckingIn(CheckIn),
    /// It returned.
    Returned,
}

/// Where the user's cancel is.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum User {
    /// The Store's `cancel` is next.
    Idle,
    /// `afterWrite`'s `cancelStoreGithubBuilds` (dashboard/src/modules/config-store/config-store.server.ts:306-310).
    Stopping(Stop),
    Done,
}

/// Where the Deployment runner's report on the build is. The runner's `run-deployment` step
/// runs after every walk returned (dashboard/src/modules/config-store/store-deployment.inngest.ts:62, :79),
/// so its first Building report lands once the walk is `Done`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Servers {
    /// Its report is next, while the build is pending or building.
    Idle,
    /// It reported, or `record` refused it: the runner stopped, and the Deployment lapses
    /// Unknown.
    Done,
}

/// Who stopped the builds: the walk at :199, or a user's cancel.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Stopper {
    Walk,
    User,
}

/// Who cancelled the run on GitHub.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Canceller {
    /// Past the budget (:213).
    Budget,
    /// After the start limit's finish (:220).
    StartLimit,
    /// `cancelStoreGithubBuilds` (dashboard/src/modules/config-store/config-store.server.ts:70).
    Stop(Stopper),
}

/// Who ran a `github_finish`: the walk, or the report handler (:312).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Finisher {
    Walk,
    Report,
}

/// What one `github_finish` statement did.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum FinishStep {
    /// The re-read found the build passed on, and it returned `Ended(Pending)` (core/crates/ployz/src/sdk/github_build.rs:480).
    PassedOn,
    /// The re-read found the build no longer building, and it returned it (core/crates/ployz/src/sdk/github_build.rs:482-484).
    NotBuilding(Status),
    /// The re-read found it building; `grant` is whether it found the grant.
    Reread { grant: bool },
    /// `inspect_grant` read this.
    Inspected(GrantRead),
    /// `inspect_grant` read nothing pushed for a grant that received the push, because its
    /// Machine lost the grant, and the re-read found a Built report: Pushed(None)
    /// (core/crates/ployzd/src/management/build_grant.rs:203-221,
    /// core/crates/ployz/src/sdk/github_build.rs:629-630).
    LostPush,
    /// An unreachable Machine at an Ended look: Waiting (core/crates/ployz/src/sdk/github_build.rs:511-513).
    WaitsForMachine,
    /// `github_end` accepted this end.
    Settled(End),
    /// `github_end` refused this end.
    Refused(End),
}

/// The branch a step took, for the `COVERAGE` rows and the story.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Branch {
    Open {
        open: bool,
    },
    WaitEnded {
        woken: bool,
    },
    /// `heldGithubBuild` at :194. `held` is false when :195 returns done; `rerun` is a step
    /// that already ran before.
    Looked {
        held: bool,
        grant: bool,
        rerun: bool,
    },
    DeploymentCancelled,
    PastBudget,
    /// `seen.ended`, or with a grant the :194 read's `ended`, chose `finish("ended")`.
    SeenEnded {
        grant: bool,
    },
    /// The Deployment is queued and nothing ended the look yet.
    StillQueued {
        grant: bool,
    },
    /// `runEnded` answered `ended`; `down` is GitHub unreachable read as running.
    Asked {
        grant: bool,
        ended: bool,
        down: bool,
        start_limit: bool,
    },
    Cancelled(Canceller),
    Finish {
        by: Finisher,
        look: GithubLook,
        step: FinishStep,
    },
    /// The step failed, by a crash or a refused settle; `retried`: Inngest runs it again.
    StepFailed {
        crash: bool,
        retried: bool,
    },
    Outstanding {
        by: Stopper,
        found: bool,
    },
    CancelEnded {
        by: Stopper,
        ok: bool,
    },
    PreCheck {
        request: Request,
        pass: bool,
    },
    Minted {
        request: Request,
        mint: Mint,
    },
    OldMintEnded {
        request: Request,
        ok: bool,
    },
    CheckedIn {
        request: Request,
        ok: bool,
    },
    Pushed,
    GrantLost,
    Reported {
        end: RunEnd,
        ok: bool,
    },
    Quit,
    Woke {
        waiting: bool,
    },
    UserCancelled,
    RunCompleted {
        killed: bool,
    },
    /// The Deployment runner's Building report: `record` took it, or refused it.
    ServersReported {
        ok: bool,
    },
    /// The retried start's read of the build: `follows` when a run holds it.
    StartRead {
        at: Start,
        follows: bool,
    },
    /// The retried start's own hand-off: `won` when it committed.
    HandOff {
        won: bool,
    },
    /// The first try's timed-out hand-off: `ok` when it committed.
    DelayedHandOff {
        ok: bool,
    },
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag is a separate fact the properties and rows read"
)]
struct State {
    deployment: Deployment,
    row: Row,
    /// The grant's Machine received the push.
    pushed: bool,
    /// The grant's Machine no longer holds the grant: it expired, or the daemon restarted
    /// (core/crates/ployzd/src/management/build_grant.rs:6-9, :203-221).
    lost: bool,
    /// Cloud asked GitHub to cancel the run.
    cancelled: bool,
    /// GitHub says the run completed.
    completed: bool,
    walk: Walk,
    job: Job,
    again: Again,
    user: User,
    /// How many crashes the model has spent.
    crashes: u8,
    /// The start limit ended a run whose check-in committed, or cancelled one while it held the
    /// build with its check-in committed.
    limit_took: bool,
    /// The start limit's Unstarted was refused because the run had checked in.
    limit_waited: bool,
    /// A check-in committed for a Deployment the user cancelled.
    granted_after_cancel: bool,
    /// A check-in committed over a grant already recorded.
    grant_replaced: bool,
    /// The walk's `github-start` was a retry, whose first try's `githubDispatched` timed out,
    /// so Cloud cancelled the first try's run (:166).
    retried: bool,
    /// The first try's timed-out `githubDispatched` has not committed or been refused yet.
    delayed: bool,
    /// The retry's own hand-off committed, so `RUN` is the retry's run and the first try's
    /// run never held the build.
    retry_won: bool,
    servers: Servers,
    /// A Server's report landed on a build a GitHub run held.
    server_on_github: bool,
    /// `record` refused a Server's report: the runner stopped.
    servers_refused: bool,
    took: Option<Branch>,
}

impl State {
    /// `github_end` in core/crates/ployz-store/src/build.rs, for run `run`; its guard is
    /// `github_end_holds` in `ployz-decide`.
    fn github_end(&mut self, broken: Broken, run: Run, end: End) -> Result<Status, ()> {
        // Without the grant guard the Store treats an Unstarted end like any other.
        let kind = match end {
            End::Unstarted(_) if broken.unstarted_ignores_grant => EndKind::Failed,
            End::Unstarted(_) => EndKind::Unstarted,
            End::Built => EndKind::Built,
            End::Failed(_) => EndKind::Failed,
            End::Skipped(_) => EndKind::Skipped,
        };
        let building_on =
            self.row
                .building_on(run)
                .map(|github| match (github.grant, github.ended) {
                    (None, _) => Hold::Dispatched,
                    (Some(_), None) => Hold::CheckedIn,
                    (Some(_), Some(_)) => Hold::Reported,
                });
        if !github_end_holds(kind, Target::Run { building_on }) {
            return Err(());
        }
        let status = match end {
            End::Built => Status::Built,
            End::Failed(_) => Status::Failed,
            End::Skipped(_) | End::Unstarted(_) => {
                self.row.github = None;
                self.row.skipped = true;
                Status::Pending
            }
        };
        self.row.status = status;
        Ok(status)
    }

    /// The run's event reaches the walk only while it waits (`step.waitForEvent`,
    /// dashboard/src/modules/config-store/store-deployment.inngest.ts:158-161).
    fn wake(&mut self) -> bool {
        if let Walk::Waiting {
            check,
            woken: false,
        } = self.walk
        {
            self.walk = Walk::Waiting { check, woken: true };
            return true;
        }
        false
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "each flag removes one independent guard"
)]
struct Broken {
    /// `github_end` accepts Unstarted from a run that checked in: the grant guard
    /// (core/crates/ployz-store/src/build.rs:1248-1250) is gone.
    unstarted_ignores_grant: bool,
    /// `github_check_in` commits for a Deployment no longer in flight, and over a grant: its
    /// guard (core/crates/ployz-store/src/build.rs:1152-1154) is gone.
    check_in_unguarded: bool,
    /// The start limit cancels the run whatever its finish answered: the `found === "done"`
    /// gate (:220) is gone.
    cancel_ignores_done: bool,
    /// The start limit settles by the grant, as `github_finish(timed_out=true)` did before
    /// the code passed its look: the finding.
    start_limit_reads_the_grant: bool,
    /// `record` takes a Server's report on a build a GitHub run holds: its guard is gone.
    record_ignores_github: bool,
    /// A retried `github-start` whose hand-off is refused answers `done`, without reading the
    /// build again to follow the run that won (:122): the walk returns while that run holds it.
    start_leaves_held_run: bool,
}

/// One `sometimes`: a branch the code handles, read from the state that took it.
type Coverage = harness::Coverage<BuildHandoff>;

fn took(state: &State, branch: Branch) -> bool {
    state.took == Some(branch)
}

fn finished(state: &State, by: Finisher, look: GithubLook, step: FinishStep) -> bool {
    took(state, Branch::Finish { by, look, step })
}

fn walk_settled(state: &State, look: GithubLook, end: End) -> bool {
    finished(state, Finisher::Walk, look, FinishStep::Settled(end))
}

fn settled_by(state: &State, by: Finisher, look: GithubLook) -> bool {
    matches!(
        state.took,
        Some(Branch::Finish { by: b, look: l, step: FinishStep::Settled(_) }) if b == by && l == look
    )
}

/// A `look`'s inspect read nothing pushed for a run that pushed and reported Built, because
/// its grant's Machine lost the grant, and its settle of Skipped `why` is next.
fn lost_after_built(state: &State, look: GithubLook, why: Why) -> bool {
    let Some(Branch::Finish {
        by,
        look: read_by,
        step: FinishStep::LostPush,
    }) = state.took
    else {
        return false;
    };
    let next = match by {
        Finisher::Walk => match state.walk {
            Walk::Checking {
                at: At::Finishing { at, .. },
                ..
            } => Some(at),
            Walk::Start(_)
            | Walk::Open(_)
            | Walk::Waiting { .. }
            | Walk::Checking { .. }
            | Walk::Done
            | Walk::GaveUp => None,
        },
        Finisher::Report => match state.job {
            Job::Finishing(at) => Some(at),
            Job::Queued
            | Job::CheckingIn(_)
            | Job::Building
            | Job::Pushed
            | Job::Reporting(_)
            | Job::Waking
            | Job::Exited => None,
        },
    };
    read_by == look && next == Some(Finish::Settle(End::Skipped(why)))
}

/// Each branch the bounds reach, on the code and with `start_limit_reads_the_grant`, or only
/// with it. `the_code_keeps_a_started_run` expects every `Both` row, and
/// `timed_out_breaks_only_the_finding` every row.
const COVERAGE: &[Coverage] = &[
    // The walk's steps (dashboard/src/modules/config-store/store-deployment.inngest.ts:151-167).
    Coverage {
        name: "github-open finds the build on GitHub and the walk waits",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::Open { open: true }),
    },
    Coverage {
        name: "github-open finds the build no longer on GitHub and the walk ends",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::Open { open: false }),
    },
    Coverage {
        name: "github-wait ends with the run's event",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::WaitEnded { woken: true }),
    },
    Coverage {
        name: "github-wait times out",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::WaitEnded { woken: false }),
    },
    // checkStoreGithubBuild (:190-225).
    Coverage {
        name: "checkStoreGithubBuild's read finds the build no longer building and returns done",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Looked {
                    held: false,
                    rerun: false,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "checkStoreGithubBuild's read finds a run with no grant",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Looked {
                    held: true,
                    grant: false,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "checkStoreGithubBuild's read finds a run that checked in",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Looked {
                    held: true,
                    grant: true,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "the walk finds the Deployment cancelled and stops its GitHub builds",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::DeploymentCancelled),
    },
    Coverage {
        name: "the walk finds a run past its budget",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::PastBudget),
    },
    Coverage {
        name: "the wait saw the run end, so a run with no grant gets finish(\"ended\")",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::SeenEnded { grant: false }),
    },
    Coverage {
        name: "the wait saw the run end, or its final report came, so a run that checked in gets finish(\"ended\")",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::SeenEnded { grant: true }),
    },
    Coverage {
        name: "runEnded says a run with no grant completed, and it gets finish(\"ended\")",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Asked {
                    grant: false,
                    ended: true,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "the start limit calls finish(\"start_limit\") for a run its read found with no grant",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Asked {
                    grant: false,
                    ended: false,
                    start_limit: true,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "runEnded says a run that checked in completed, and it gets finish(\"ended\")",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Asked {
                    grant: true,
                    ended: true,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "the walk waits for a run that checked in and is still running",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Asked {
                    grant: true,
                    ended: false,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "runEnded reads GitHub unreachable as a run still running",
        on: everywhere,
        omit: None,
        reached: |_, state| matches!(state.took, Some(Branch::Asked { down: true, .. })),
    },
    Coverage {
        name: "the budget cancels the run",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::Cancelled(Canceller::Budget)),
    },
    Coverage {
        name: "the start limit cancels the run its finish ended",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::Cancelled(Canceller::StartLimit)),
    },
    Coverage {
        name: "the walk's cancelStoreGithubBuilds cancels the run",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::Cancelled(Canceller::Stop(Stopper::Walk))),
    },
    Coverage {
        name: "the user's cancelStoreGithubBuilds cancels the run",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::Cancelled(Canceller::Stop(Stopper::User))),
    },
    // github_finish (core/crates/ployz/src/sdk/github_build.rs:466-556).
    Coverage {
        name: "github_finish's re-read finds the build passed on",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Finish {
                    step: FinishStep::PassedOn,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "github_finish's re-read finds the build built or failed",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Finish {
                    step: FinishStep::NotBuilding(Status::Built | Status::Failed),
                    ..
                })
            )
        },
    },
    Coverage {
        name: "github_finish's re-read finds the grant the start limit's read did not",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            finished(
                state,
                Finisher::Walk,
                GithubLook::StartLimit,
                FinishStep::Reread { grant: true },
            )
        },
    },
    Coverage {
        name: "the start limit passes on a run that never checked in, Unstarted \"no runner started the build in time\"",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            walk_settled(
                state,
                GithubLook::StartLimit,
                End::Unstarted(Why::NoRunnerInTime),
            )
        },
    },
    Coverage {
        name: "an Ended look passes on a run that never checked in, Unstarted \"the run ended before it started the build\"",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            walk_settled(
                state,
                GithubLook::Ended,
                End::Unstarted(Why::EndedBeforeStart),
            )
        },
    },
    Coverage {
        name: "the start limit's Unstarted is refused because the run checked in, and github_finish answers Waiting",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            finished(
                state,
                Finisher::Walk,
                GithubLook::StartLimit,
                FinishStep::Refused(End::Unstarted(Why::NoRunnerInTime)),
            ) && state.limit_waited
        },
    },
    Coverage {
        name: "the start limit settles a run that checked in Skipped \"the run ran out of time\"",
        on: |model| model.broken.start_limit_reads_the_grant,
        omit: None,
        reached: |_, state| {
            walk_settled(
                state,
                GithubLook::StartLimit,
                End::Skipped(Why::RanOutOfTime),
            )
        },
    },
    Coverage {
        name: "the start limit fails a run that checked in, its grant's Machine unreachable",
        on: |model| model.broken.start_limit_reads_the_grant,
        omit: None,
        reached: |_, state| {
            walk_settled(state, GithubLook::StartLimit, End::Failed(Why::Unreachable))
        },
    },
    Coverage {
        name: "github_finish settles Built from a pushed grant and a Built report",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Finish {
                    step: FinishStep::Settled(End::Built),
                    ..
                })
            )
        },
    },
    Coverage {
        name: "github_finish settles Failed \"a build step failed\"",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Finish {
                    step: FinishStep::Settled(End::Failed(Why::StepFailed)),
                    ..
                })
            )
        },
    },
    Coverage {
        name: "an Ended look settles Skipped \"the run ended without pushing an image\"",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Finish {
                    look: GithubLook::Ended,
                    step: FinishStep::Settled(End::Skipped(Why::EndedWithoutPushing)),
                    ..
                })
            )
        },
    },
    Coverage {
        name: "the budget settles Skipped \"the run ran out of time\"",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            walk_settled(state, GithubLook::Budget, End::Skipped(Why::RanOutOfTime))
        },
    },
    Coverage {
        name: "the budget fails the build, its grant's Machine unreachable",
        on: everywhere,
        omit: None,
        reached: |_, state| walk_settled(state, GithubLook::Budget, End::Failed(Why::Unreachable)),
    },
    Coverage {
        name: "an Ended look waits for an unreachable grant Machine",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Finish {
                    look: GithubLook::Ended,
                    step: FinishStep::WaitsForMachine,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "github_finish fails the build on the Machine's refusal",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Finish {
                    step: FinishStep::Settled(End::Failed(Why::Refused)),
                    ..
                })
            )
        },
    },
    Coverage {
        name: "after the start limit waited for a run that checked in, an Ended look settles it",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            state.limit_waited && settled_by(state, Finisher::Walk, GithubLook::Ended)
        },
    },
    Coverage {
        name: "after the start limit waited for a run that checked in, the budget settles it",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            state.limit_waited && settled_by(state, Finisher::Walk, GithubLook::Budget)
        },
    },
    Coverage {
        name: "after the start limit waited for a run that checked in, the report's own github_finish settles it",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            state.limit_waited && settled_by(state, Finisher::Report, GithubLook::Ended)
        },
    },
    // Inngest's at-least-once steps (dashboard/src/modules/config-store/store-deployment.inngest.ts:41).
    Coverage {
        name: "a github-check step dies after an effect landed, and Inngest runs it again",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::StepFailed {
                    crash: true,
                    retried: true,
                },
            )
        },
    },
    Coverage {
        name: "a github-check step's refused settle fails it, and Inngest runs it again",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::StepFailed {
                    crash: false,
                    retried: true,
                },
            )
        },
    },
    Coverage {
        name: "a re-run of a github-check step finds the build no longer building",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Looked {
                    held: false,
                    rerun: true,
                    ..
                })
            )
        },
    },
    // cancelStoreGithubBuilds (dashboard/src/modules/config-store/config-store.server.ts:67-72).
    Coverage {
        name: "the walk's github_cancel finds the build still on GitHub",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Outstanding {
                    by: Stopper::Walk,
                    found: true,
                },
            )
        },
    },
    Coverage {
        name: "the user's github_cancel finds the build still on GitHub",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Outstanding {
                    by: Stopper::User,
                    found: true,
                },
            )
        },
    },
    Coverage {
        name: "the user's github_cancel finds nothing on GitHub",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Outstanding {
                    by: Stopper::User,
                    found: false,
                },
            )
        },
    },
    Coverage {
        name: "github_cancel fails the build \"cancelled\"",
        on: everywhere,
        omit: None,
        reached: |_, state| matches!(state.took, Some(Branch::CancelEnded { ok: true, .. })),
    },
    Coverage {
        name: "github_cancel's end is refused, another step having ended the build",
        on: everywhere,
        omit: None,
        reached: |_, state| matches!(state.took, Some(Branch::CancelEnded { ok: false, .. })),
    },
    Coverage {
        name: "a user cancels the queued Deployment",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::UserCancelled),
    },
    // The runner (core/crates/ployz/src/sdk/github_build.rs:187-274, :384-432).
    Coverage {
        name: "the check-in's pre-check passes",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::PreCheck {
                    request: Request::Job,
                    pass: true,
                },
            )
        },
    },
    Coverage {
        name: "the check-in's pre-check refuses a build no longer on GitHub",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::PreCheck {
                    request: Request::Job,
                    pass: false,
                },
            )
        },
    },
    Coverage {
        name: "the Machine mints a grant",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Minted {
                    request: Request::Job,
                    mint: Mint::Minted,
                },
            )
        },
    },
    Coverage {
        name: "the mint fails, and the check-in fails",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Minted {
                    request: Request::Job,
                    mint: Mint::Fails,
                },
            )
        },
    },
    Coverage {
        name: "the old-mint skip passes the build on Unstarted",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::OldMintEnded {
                    request: Request::Job,
                    ok: true,
                },
            )
        },
    },
    Coverage {
        name: "the old-mint skip's Unstarted is refused",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::OldMintEnded {
                    request: Request::Job,
                    ok: false,
                },
            )
        },
    },
    Coverage {
        name: "the Store's github_check_in commits the grant",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::CheckedIn {
                    request: Request::Job,
                    ok: true,
                },
            )
        },
    },
    Coverage {
        name: "the Store's github_check_in refuses, leaving the minted grant unrecorded",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::CheckedIn {
                    request: Request::Job,
                    ok: false,
                },
            )
        },
    },
    Coverage {
        name: "the runner pushes into the grant's Machine",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::Pushed),
    },
    Coverage {
        name: "the final report says Built",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Reported {
                    end: RunEnd::Built,
                    ok: true,
                },
            )
        },
    },
    Coverage {
        name: "the final report says Failed",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::Reported {
                    end: RunEnd::Failed,
                    ok: true,
                },
            )
        },
    },
    Coverage {
        name: "the final report is refused",
        on: everywhere,
        omit: None,
        reached: |_, state| matches!(state.took, Some(Branch::Reported { ok: false, .. })),
    },
    Coverage {
        name: "the job stops before its final report",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::Quit),
    },
    Coverage {
        name: "the report's event wakes the waiting walk",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::Woke { waiting: true }),
    },
    Coverage {
        name: "the report's event finds no wait",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::Woke { waiting: false }),
    },
    // The grant's Machine.
    Coverage {
        name: "after the grant's Machine lost a grant that received the push, an Ended look reads nothing pushed for a Built report, and its Skipped \"the run ended without pushing an image\" settle is next",
        on: everywhere,
        omit: None,
        reached: |_, state| lost_after_built(state, GithubLook::Ended, Why::EndedWithoutPushing),
    },
    Coverage {
        name: "after the grant's Machine lost a grant that received the push, the budget reads nothing pushed for a Built report, and its Skipped \"the run ran out of time\" settle is next",
        on: everywhere,
        omit: None,
        reached: |_, state| lost_after_built(state, GithubLook::Budget, Why::RanOutOfTime),
    },
    // The Deployment runner.
    Coverage {
        name: "the Servers take a build GitHub passed on",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(state, Branch::ServersReported { ok: true }) && !state.server_on_github
        },
    },
    // The retried start.
    Coverage {
        name: "the first try's hand-off commits before the retry reads the build, and the retry follows its run",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::StartRead {
                    at: Start::Read,
                    follows: true,
                },
            )
        },
    },
    Coverage {
        name: "the first try's hand-off commits after the retry read the build: the retry's hand-off is refused, and its second read follows the first try's run",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::StartRead {
                    at: Start::Reread,
                    follows: true,
                },
            )
        },
    },
    Coverage {
        name: "the retry's second read finds the first try's run no longer holding the build, and the walk returns",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            took(
                state,
                Branch::StartRead {
                    at: Start::Reread,
                    follows: false,
                },
            )
        },
    },
    Coverage {
        name: "the retry's hand-off commits, and the walk follows the retry's run",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::HandOff { won: true }),
    },
    Coverage {
        name: "the first try's late hand-off is refused, since the build is no longer waiting",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::DelayedHandOff { ok: false }),
    },
    Coverage {
        name: "after a retried start, a look passes on the run Cloud cancelled, and the Servers take the build",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            state.retried
                && !state.retry_won
                && !state.limit_waited
                && took(state, Branch::ServersReported { ok: true })
        },
    },
    Coverage {
        name: "after a retried start, the run Cloud cancelled checked in first: the start limit waits for it, and a later look settles it",
        on: everywhere,
        omit: None,
        reached: |_, state| {
            state.retried
                && !state.retry_won
                && state.limit_waited
                && [GithubLook::Ended, GithubLook::Budget]
                    .into_iter()
                    .any(|look| settled_by(state, Finisher::Walk, look))
        },
    },
    // GitHub.
    Coverage {
        name: "GitHub ends a cancelled run and its job",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::RunCompleted { killed: true }),
    },
    Coverage {
        name: "GitHub completes a run whose job exited",
        on: everywhere,
        omit: None,
        reached: |_, state| took(state, Branch::RunCompleted { killed: false }),
    },
];

/// Who steps.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Who {
    Walk,
    Job,
    /// The second check-in request, under the two-request lift.
    Again,
    User,
    Github,
    /// The grant's Machine.
    Machine,
    /// The Deployment runner, which builds on the Servers.
    Servers,
    /// The first try's timed-out `githubDispatched`, still running in the native SDK.
    Delayed,
}

/// Which way a step goes, where the code or the world decides.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Pick {
    Go,
    /// The clock is past the budget (:212).
    PastBudget,
    /// GitHub can't be reached, which `runEnded` reads as running (:179).
    GithubDown,
    Grant(GrantRead),
    Mint(Mint),
    Push,
    ReportBuilt,
    ReportFailed,
    Quit,
    /// The step's process dies here, or a Store call times out after its work committed
    /// (core/crates/ployz/src/sdk/store_call.rs:45-52), and Inngest runs the step again.
    Crash,
}

#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
struct Action {
    who: Who,
    pick: Pick,
}

#[derive(Clone, Copy, Debug)]
struct BuildHandoff {
    /// How many check-in requests the run sends: 1, or 2 under the lift.
    requests: u8,
    /// How many times a step may crash.
    crashes: u8,
    broken: Broken,
}

/// Where one `github_finish` statement leads.
enum Next {
    At(Finish),
    Returned(Finished),
}

impl BuildHandoff {
    /// One `github_finish` statement under `look`, by `by`.
    fn finish(
        &self,
        state: &mut State,
        by: Finisher,
        look: GithubLook,
        at: Finish,
        pick: Pick,
    ) -> Next {
        // Under the finding, the start limit passes the look the code passed before it had one.
        let called = if look == GithubLook::StartLimit && self.broken.start_limit_reads_the_grant {
            GithubLook::Budget
        } else {
            look
        };
        let step = |step| Some(Branch::Finish { by, look, step });
        match at {
            Finish::Read => {
                // core/crates/ployz/src/sdk/github_build.rs:473-484: `github_build` reads the hand-off.
                let Some(github) = state.row.github else {
                    state.took = step(FinishStep::PassedOn);
                    return Next::Returned(Finished::Ended(Status::Pending));
                };
                if state.row.status != Status::Building {
                    state.took = step(FinishStep::NotBuilding(state.row.status));
                    return Next::Returned(Finished::Ended(state.row.status));
                }
                state.took = step(FinishStep::Reread {
                    grant: github.grant.is_some(),
                });
                // core/crates/ployz/src/sdk/github_build.rs: `github_finish` asks `first`.
                Next::At(match real::first(github.grant.as_ref(), called) {
                    First::Unstarted(why) => Finish::Settle(End::Unstarted(Why::of(&why))),
                    First::ReadGrant(_) => Finish::Inspect {
                        ended: github.ended,
                    },
                })
            }
            Finish::Inspect { ended } => {
                let Pick::Grant(read) = pick else {
                    unreachable!("an inspect is offered only grant reads")
                };
                // What the Machine answers for the push (core/crates/ployz/src/sdk/github_build.rs:506-507).
                let read_push = state.pushed && !state.lost;
                state.took = step(
                    if read == GrantRead::Pushed
                        && state.pushed
                        && !read_push
                        && ended == Some(RunEnd::Built)
                    {
                        FinishStep::LostPush
                    } else {
                        FinishStep::Inspected(read)
                    },
                );
                // `inspect_grant`'s answer, as `github_finish` hands it to `after_read`.
                let answer = match read {
                    GrantRead::Pushed => real::GrantRead::Pushed(read_push.then(digest)),
                    GrantRead::Refused => real::GrantRead::Refused(Why::Refused.message()),
                    GrantRead::Unreachable => real::GrantRead::Unreachable,
                };
                let ended = ended.map(|ended| match ended {
                    RunEnd::Built => EndedRun::Built(()),
                    RunEnd::Failed => EndedRun::Failed,
                });
                match real::after_read(ended, called, answer) {
                    None => {
                        state.took = step(FinishStep::WaitsForMachine);
                        Next::Returned(Finished::Waiting)
                    }
                    Some(ending) => Next::At(Finish::Settle(match ending {
                        real::Ending::Built { .. } => End::Built,
                        real::Ending::Failed(why) => End::Failed(Why::of(&why)),
                        real::Ending::Skipped(why) => End::Skipped(Why::of(&why)),
                    })),
                }
            }
            Finish::Settle(end) => {
                // core/crates/ployz/src/sdk/github_build.rs:485 takes the run from the re-read; the model has one.
                let started = state.row.started_on(RUN);
                let limit = by == Finisher::Walk && look == GithubLook::StartLimit;
                match state.github_end(self.broken, RUN, end) {
                    Ok(status) => {
                        state.took = step(FinishStep::Settled(end));
                        if limit && started {
                            state.limit_took = true;
                        }
                        Next::Returned(Finished::Ended(status))
                    }
                    Err(()) => {
                        state.took = step(FinishStep::Refused(end));
                        match end {
                            // core/crates/ployz/src/sdk/github_build.rs:499-503.
                            End::Unstarted(_) => {
                                if limit && started {
                                    state.limit_waited = true;
                                }
                                Next::Returned(Finished::Waiting)
                            }
                            End::Built | End::Failed(_) | End::Skipped(_) => {
                                Next::Returned(Finished::Conflict)
                            }
                        }
                    }
                }
            }
        }
    }

    /// One `cancelStoreGithubBuilds` statement (dashboard/src/modules/config-store/config-store.server.ts:67-72), or None once it
    /// returned.
    fn stop(state: &mut State, by: Stopper, at: Stop) -> Option<Stop> {
        match at {
            Stop::Outstanding => {
                // `github_outstanding` (core/crates/ployz-store/src/build.rs:1303-1317).
                let found = state.row.status == Status::Building && state.row.github.is_some();
                state.took = Some(Branch::Outstanding { by, found });
                found.then_some(Stop::Ending)
            }
            Stop::Ending => {
                // Its end is ignored: another step may have ended it (core/crates/ployz/src/sdk/github_build.rs:660-664).
                let ok = state
                    .github_end(Broken::default(), RUN, End::Failed(Why::Cancelled))
                    .is_ok();
                state.took = Some(Branch::CancelEnded { by, ok });
                Some(Stop::Cancelling)
            }
            Stop::Cancelling => {
                state.cancelled = true;
                state.took = Some(Branch::Cancelled(Canceller::Stop(by)));
                None
            }
        }
    }

    /// The walk's steps.
    /// The retried `github-start`'s steps (:112-174).
    fn start(&self, state: &mut State, at: Start) {
        state.walk = match at {
            Start::Read | Start::Reread => {
                // `holder`: a run holds the build while it is building on GitHub (:114-117).
                let follows = state.row.status == Status::Building && state.row.github.is_some();
                state.took = Some(Branch::StartRead { at, follows });
                match (follows, at) {
                    (true, _) => Walk::Open(0),
                    (false, Start::Read) => Walk::Start(Start::HandOff),
                    (false, Start::HandOff | Start::Reread) => Walk::Done,
                }
            }
            Start::HandOff => {
                let won = Self::hand_off(state);
                state.took = Some(Branch::HandOff { won });
                if won {
                    // `RUN` is the retry's run from here: GitHub has neither ended it nor been
                    // asked to cancel it, and its job has yet to check in. The first try's run
                    // never held the build, so its job's check-in is refused and its late
                    // hand-off too.
                    state.retry_won = true;
                    state.cancelled = false;
                    state.completed = false;
                    state.job = Job::Queued;
                    state.again = if self.requests > 1 {
                        Again::Unsent
                    } else {
                        Again::Absent
                    };
                    Walk::Open(0)
                } else if self.broken.start_leaves_held_run {
                    Walk::Done
                } else {
                    Walk::Start(Start::Reread)
                }
            }
        };
    }

    /// `github_dispatched` (core/crates/ployz-store/src/build.rs:1013-1036) for a run that
    /// never held the build: it commits only for a waiting build of a Deployment in flight.
    fn hand_off(state: &mut State) -> bool {
        let waiting = state.deployment == Deployment::Queued
            && state.row.status == Status::Pending
            && state.row.github.is_none()
            && !state.row.skipped;
        if waiting {
            state.row.status = Status::Building;
            state.row.github = Some(Github {
                run: RUN,
                grant: None,
                ended: None,
            });
        }
        waiting
    }

    fn walk(&self, state: &mut State, pick: Pick) {
        match state.walk {
            Walk::Start(at) => self.start(state, at),
            Walk::Open(check) => {
                // `storeGithubBuildOpen` (:228-230).
                let open = state.row.github.is_some() && state.row.status == Status::Building;
                state.took = Some(Branch::Open { open });
                state.walk = if open {
                    Walk::Waiting {
                        check,
                        woken: false,
                    }
                } else {
                    Walk::Done
                };
            }
            Walk::Waiting { check, woken } => {
                state.took = Some(Branch::WaitEnded { woken });
                state.walk = Walk::Checking {
                    check,
                    ended: woken,
                    attempt: 0,
                    at: At::Read,
                };
            }
            Walk::Checking {
                check,
                ended,
                attempt,
                at,
            } => {
                if pick == Pick::Crash {
                    state.crashes += 1;
                    Self::fail_step(state, check, ended, attempt, true);
                    return;
                }
                let next = self.check(state, check, ended, attempt, at, pick);
                if let Some(at) = next {
                    state.walk = Walk::Checking {
                        check,
                        ended,
                        attempt,
                        at,
                    };
                }
            }
            Walk::Done | Walk::GaveUp => unreachable!("a walk that ended takes no step"),
        }
    }

    fn fail_step(state: &mut State, check: u8, ended: bool, attempt: u8, crash: bool) {
        let retried = attempt < RETRIES;
        state.took = Some(Branch::StepFailed { crash, retried });
        state.walk = if retried {
            Walk::Checking {
                check,
                ended,
                attempt: attempt + 1,
                at: At::Read,
            }
        } else {
            Walk::GaveUp
        };
    }

    /// One statement of `checkStoreGithubBuild`. Returns where the step is next, or None when
    /// it set the walk itself.
    fn check(
        &self,
        state: &mut State,
        check: u8,
        ended: bool,
        attempt: u8,
        at: At,
        pick: Pick,
    ) -> Option<At> {
        let waits = |state: &mut State| state.walk = Walk::Open(check + 1);
        let finishing = |look| {
            Some(At::Finishing {
                look,
                at: Finish::Read,
            })
        };
        match at {
            At::Read => {
                // :194-195.
                let held = state
                    .row
                    .github
                    .filter(|_| state.row.status == Status::Building);
                state.took = Some(Branch::Looked {
                    held: held.is_some(),
                    grant: held.is_some_and(|github| github.grant.is_some()),
                    rerun: attempt > 0,
                });
                let Some(github) = held else {
                    state.walk = Walk::Done;
                    return None;
                };
                Some(At::Deployment {
                    grant: github.grant.is_some(),
                    ended: github.ended.is_some(),
                })
            }
            At::Deployment {
                grant,
                ended: reported,
            } => {
                // :197-201. An installation the Organization can use (:202-206).
                if state.deployment == Deployment::Cancelled {
                    state.took = Some(Branch::DeploymentCancelled);
                    return Some(At::Stopping(Stop::Outstanding));
                }
                // :212.
                if pick == Pick::PastBudget {
                    state.took = Some(Branch::PastBudget);
                    return Some(At::BudgetCancel);
                }
                // :217 and :223 read `seen.ended`, and :223 the read's `ended`, before GitHub.
                if ended || (grant && reported) {
                    state.took = Some(Branch::SeenEnded { grant });
                    return finishing(GithubLook::Ended);
                }
                state.took = Some(Branch::StillQueued { grant });
                Some(At::AskingGithub { grant })
            }
            At::Stopping(stop) => {
                let next = Self::stop(state, Stopper::Walk, stop);
                if next.is_none() {
                    state.walk = Walk::Done;
                }
                next.map(At::Stopping)
            }
            At::BudgetCancel => {
                state.cancelled = true;
                state.took = Some(Branch::Cancelled(Canceller::Budget));
                finishing(GithubLook::Budget)
            }
            At::AskingGithub { grant } => {
                // `runEnded` (:177-179).
                let down = pick == Pick::GithubDown;
                let says_ended = state.completed && !down;
                let start_limit = check == 0;
                state.took = Some(Branch::Asked {
                    grant,
                    ended: says_ended,
                    down,
                    start_limit,
                });
                if says_ended {
                    return finishing(GithubLook::Ended);
                }
                // :218 and :224. Check 0 is the start limit, `check === 0 && target.hasNext`
                // (dashboard/src/modules/config-store/store-deployment.inngest.ts:157): the model's walk has a next Builder.
                if grant || !start_limit {
                    waits(state);
                    return None;
                }
                finishing(GithubLook::StartLimit)
            }
            At::Finishing { look, at } => {
                match self.finish(state, Finisher::Walk, look, at, pick) {
                    Next::At(at) => Some(At::Finishing { look, at }),
                    Next::Returned(Finished::Conflict) => {
                        Self::fail_step(state, check, ended, attempt, false);
                        None
                    }
                    Next::Returned(found) => {
                        let waiting = found == Finished::Waiting;
                        if look == GithubLook::StartLimit
                            && (!waiting || self.broken.cancel_ignores_done)
                        {
                            return Some(At::LimitCancel { waiting });
                        }
                        if waiting {
                            waits(state);
                        } else {
                            state.walk = Walk::Done;
                        }
                        None
                    }
                }
            }
            At::LimitCancel { waiting } => {
                // :220.
                if state.row.started_on(RUN) {
                    state.limit_took = true;
                }
                state.cancelled = true;
                state.took = Some(Branch::Cancelled(Canceller::StartLimit));
                if waiting {
                    waits(state);
                } else {
                    state.walk = Walk::Done;
                }
                None
            }
        }
    }

    /// One statement of a check-in request after its pre-check. Returns its next one, or None
    /// once it returned; `Ok` is whether it checked in.
    fn check_in(
        &self,
        state: &mut State,
        request: Request,
        at: CheckIn,
        pick: Pick,
    ) -> Result<Option<CheckIn>, ()> {
        match at {
            CheckIn::Minting => {
                let Pick::Mint(mint) = pick else {
                    unreachable!("a mint is offered only its answers")
                };
                state.took = Some(Branch::Minted { request, mint });
                match mint {
                    Mint::Minted => Ok(Some(CheckIn::Committing)),
                    Mint::Old => Ok(Some(CheckIn::OldMint)),
                    Mint::Fails => Err(()),
                }
            }
            CheckIn::OldMint => {
                // core/crates/ployz/src/sdk/github_build.rs:225-235: it ends the build Unstarted, then returns `conflict` whatever.
                let ok = state
                    .github_end(self.broken, RUN, End::Unstarted(Why::OldMint))
                    .is_ok();
                state.took = Some(Branch::OldMintEnded { request, ok });
                Err(())
            }
            CheckIn::Committing => {
                // `github_check_in` (core/crates/ployz-store/src/build.rs:1141-1173).
                let ok = match state.row.building_on(RUN) {
                    None => false,
                    Some(github) => {
                        let wanted = state.deployment == Deployment::Queued;
                        if !self.broken.check_in_unguarded && (!wanted || github.grant.is_some()) {
                            false
                        } else {
                            state.granted_after_cancel |= !wanted;
                            state.grant_replaced |= github.grant.is_some();
                            state.row.github = Some(Github {
                                grant: Some(request),
                                ..github
                            });
                            true
                        }
                    }
                };
                state.took = Some(Branch::CheckedIn { request, ok });
                if ok { Ok(None) } else { Err(()) }
            }
        }
    }

    /// The check-in's pre-check: Cloud web's `heldGithubBuild` (:280-281), then
    /// `github_authorize` and the SDK's check (core/crates/ployz/src/sdk/github_build.rs:195-204).
    fn pre_check(state: &mut State, request: Request) -> bool {
        let pass = state
            .row
            .building_on(RUN)
            .is_some_and(|github| github.grant.is_none());
        state.took = Some(Branch::PreCheck { request, pass });
        pass
    }

    fn job(&self, state: &mut State, pick: Pick) {
        state.job = match state.job {
            Job::Queued => {
                if Self::pre_check(state, Request::Job) {
                    Job::CheckingIn(CheckIn::Minting)
                } else {
                    Job::Exited
                }
            }
            Job::CheckingIn(at) => match self.check_in(state, Request::Job, at, pick) {
                Ok(Some(at)) => Job::CheckingIn(at),
                Ok(None) => Job::Building,
                Err(()) => Job::Exited,
            },
            Job::Building | Job::Pushed => match pick {
                Pick::Push => {
                    state.pushed = true;
                    state.took = Some(Branch::Pushed);
                    Job::Pushed
                }
                Pick::ReportBuilt => Job::Reporting(RunEnd::Built),
                Pick::ReportFailed => Job::Reporting(RunEnd::Failed),
                Pick::Quit => {
                    state.took = Some(Branch::Quit);
                    Job::Exited
                }
                Pick::Go
                | Pick::PastBudget
                | Pick::GithubDown
                | Pick::Grant(_)
                | Pick::Mint(_)
                | Pick::Crash => unreachable!("{pick:?} is not offered to a building job"),
            },
            Job::Reporting(end) => {
                // `github_report` (core/crates/ployz-store/src/build.rs:1177-1233).
                let ok = state
                    .row
                    .building_on(RUN)
                    .is_some_and(|github| github.grant.is_some() && github.ended.is_none());
                if ok {
                    state.row.github = state.row.github.map(|github| Github {
                        ended: Some(end),
                        ..github
                    });
                }
                state.took = Some(Branch::Reported { end, ok });
                if ok {
                    Job::Finishing(Finish::Read)
                } else {
                    Job::Exited
                }
            }
            Job::Finishing(at) => {
                match self.finish(state, Finisher::Report, GithubLook::Ended, at, pick) {
                    Next::At(at) => Job::Finishing(at),
                    // :312-318: what it returned, or its error, is logged.
                    Next::Returned(_) => Job::Waking,
                }
            }
            Job::Waking => {
                let waiting = state.wake();
                state.took = Some(Branch::Woke { waiting });
                Job::Exited
            }
            Job::Exited => unreachable!("an exited job takes no step"),
        };
        // Its process is gone once GitHub ended the run.
        if state.completed && state.job.between_requests() {
            state.job = Job::Exited;
        }
    }

    fn again(&self, state: &mut State, pick: Pick) {
        state.again = match state.again {
            Again::Unsent => {
                if Self::pre_check(state, Request::Again) {
                    Again::CheckingIn(CheckIn::Minting)
                } else {
                    Again::Returned
                }
            }
            Again::CheckingIn(at) => match self.check_in(state, Request::Again, at, pick) {
                Ok(Some(at)) => Again::CheckingIn(at),
                Ok(None) | Err(()) => Again::Returned,
            },
            Again::Absent | Again::Returned => unreachable!("no second request steps"),
        };
    }

    fn user(state: &mut State) {
        state.user = match state.user {
            User::Idle => {
                // `deployment::cancel` (core/crates/ployz-store/src/deployment/lifecycle.rs).
                state.deployment = Deployment::Cancelled;
                state.took = Some(Branch::UserCancelled);
                User::Stopping(Stop::Outstanding)
            }
            User::Stopping(stop) => {
                Self::stop(state, Stopper::User, stop).map_or(User::Done, User::Stopping)
            }
            User::Done => unreachable!("a user who cancelled takes no step"),
        };
    }

    /// The runner's Building report: `record` (core/crates/ployz-store/src/build.rs:264-314).
    /// It refuses a build a GitHub run holds or ended
    /// (core/crates/ployz-store/src/build.rs:289-296); otherwise the Servers take
    /// it, and `record` clears a `github` no run holds (core/crates/ployz-store/src/build.rs:301).
    /// The model's row has none then, so it is left as it was, as `main`'s `record` left it.
    fn servers(&self, state: &mut State) {
        let github = state.row.github.is_some();
        let ok = !github || self.broken.record_ignores_github;
        if ok {
            state.server_on_github |= github;
            state.row.status = Status::Building;
        }
        state.servers_refused |= !ok;
        state.took = Some(Branch::ServersReported { ok });
        state.servers = Servers::Done;
    }

    fn github(state: &mut State) {
        let killed = state.job.between_requests();
        state.completed = true;
        if killed {
            state.job = Job::Exited;
        }
        // The `workflow_run` webhook (dashboard/src/routes/api/github/-webhook.handler.ts:78-83).
        state.wake();
        state.took = Some(Branch::RunCompleted { killed });
    }

    /// The ways `who` can step from `state`.
    fn offers(&self, state: &State, who: Who) -> Vec<Pick> {
        let grant_reads = || {
            vec![
                Pick::Grant(GrantRead::Pushed),
                Pick::Grant(GrantRead::Refused),
                Pick::Grant(GrantRead::Unreachable),
            ]
        };
        let mints = || {
            vec![
                Pick::Mint(Mint::Minted),
                Pick::Mint(Mint::Old),
                Pick::Mint(Mint::Fails),
            ]
        };
        match who {
            Who::Walk => match state.walk {
                Walk::Start(_) => vec![Pick::Go],
                Walk::Open(check) if check < MAX_CHECKS => vec![Pick::Go],
                Walk::Open(_) | Walk::Done | Walk::GaveUp => Vec::new(),
                Walk::Waiting { .. } => vec![Pick::Go],
                Walk::Checking { at, .. } => {
                    let mut picks = match at {
                        At::Deployment { grant: true, .. }
                            if state.deployment == Deployment::Queued =>
                        {
                            vec![Pick::Go, Pick::PastBudget]
                        }
                        At::AskingGithub { .. } if state.completed => {
                            vec![Pick::Go, Pick::GithubDown]
                        }
                        At::Finishing {
                            at: Finish::Inspect { .. },
                            ..
                        } => grant_reads(),
                        At::Read
                        | At::Deployment { .. }
                        | At::Stopping(_)
                        | At::BudgetCancel
                        | At::AskingGithub { .. }
                        | At::Finishing { .. }
                        | At::LimitCancel { .. } => vec![Pick::Go],
                    };
                    if at != At::Read && state.crashes < self.crashes {
                        picks.push(Pick::Crash);
                    }
                    picks
                }
            },
            Who::Job => match state.job {
                Job::Queued
                | Job::CheckingIn(CheckIn::OldMint | CheckIn::Committing)
                | Job::Reporting(_)
                | Job::Finishing(Finish::Read | Finish::Settle(_))
                | Job::Waking => vec![Pick::Go],
                Job::CheckingIn(CheckIn::Minting) => mints(),
                // A lost grant admits no push (core/crates/ployzd/src/management/build_grant.rs:237-245).
                Job::Building if state.lost => vec![Pick::ReportFailed, Pick::Quit],
                Job::Building => vec![Pick::Push, Pick::ReportFailed, Pick::Quit],
                Job::Pushed => vec![Pick::ReportBuilt, Pick::Quit],
                Job::Finishing(Finish::Inspect { .. }) => grant_reads(),
                Job::Exited => Vec::new(),
            },
            Who::Again => match state.again {
                Again::Unsent if !state.completed && state.job != Job::Exited => vec![Pick::Go],
                Again::CheckingIn(CheckIn::Minting) => mints(),
                Again::CheckingIn(CheckIn::OldMint | CheckIn::Committing) => vec![Pick::Go],
                Again::Unsent | Again::Absent | Again::Returned => Vec::new(),
            },
            Who::User => match state.user {
                User::Idle | User::Stopping(_) => vec![Pick::Go],
                User::Done => Vec::new(),
            },
            Who::Github => {
                if !state.completed && (state.cancelled || state.job == Job::Exited) {
                    vec![Pick::Go]
                } else {
                    Vec::new()
                }
            }
            Who::Servers => {
                if state.servers == Servers::Idle
                    && state.walk == Walk::Done
                    && matches!(state.row.status, Status::Pending | Status::Building)
                {
                    vec![Pick::Go]
                } else {
                    Vec::new()
                }
            }
            Who::Delayed => {
                if state.delayed {
                    vec![Pick::Go]
                } else {
                    Vec::new()
                }
            }
            // A grant the row records can be lost while the build is on GitHub; a lost grant
            // that no read follows changes nothing the model holds.
            Who::Machine => {
                let granted = state
                    .row
                    .building_on(RUN)
                    .is_some_and(|github| github.grant.is_some());
                if granted && !state.lost {
                    vec![Pick::Go]
                } else {
                    Vec::new()
                }
            }
        }
    }
}

impl Model for BuildHandoff {
    type State = State;
    type Action = Action;

    /// The run was dispatched and `githubDispatched` committed it (:157-167,
    /// core/crates/ployz-store/src/build.rs:1034-1035); the walk's first `github-open` is next.
    /// The other state is the retried start: the build is still waiting, the first try's run
    /// (`RUN`) is cancelled and its hand-off still running, and the retry's read is next. The
    /// checker explores the last start state first, so a story starts from the first try
    /// wherever both reach a state at the same depth.
    fn init_states(&self) -> Vec<State> {
        let first = State {
            deployment: Deployment::Queued,
            row: Row {
                status: Status::Building,
                github: Some(Github {
                    run: RUN,
                    grant: None,
                    ended: None,
                }),
                skipped: false,
            },
            pushed: false,
            lost: false,
            cancelled: false,
            completed: false,
            walk: Walk::Open(0),
            job: Job::Queued,
            again: if self.requests > 1 {
                Again::Unsent
            } else {
                Again::Absent
            },
            user: User::Idle,
            crashes: 0,
            limit_took: false,
            limit_waited: false,
            granted_after_cancel: false,
            grant_replaced: false,
            retried: false,
            delayed: false,
            retry_won: false,
            servers: Servers::Idle,
            server_on_github: false,
            servers_refused: false,
            took: None,
        };
        let retried = State {
            row: Row {
                status: Status::Pending,
                github: None,
                skipped: false,
            },
            cancelled: true,
            walk: Walk::Start(Start::Read),
            retried: true,
            delayed: true,
            ..first
        };
        vec![retried, first]
    }

    /// Each actor's ways to step, in the order walk, job, second request, user, GitHub, the
    /// grant's Machine. The order decides only which of the shortest paths a story shows.
    fn actions(&self, state: &State, actions: &mut Vec<Action>) {
        for who in [
            Who::Walk,
            Who::Job,
            Who::Again,
            Who::User,
            Who::Github,
            Who::Machine,
            Who::Servers,
            Who::Delayed,
        ] {
            actions.extend(
                self.offers(state, who)
                    .into_iter()
                    .map(|pick| Action { who, pick }),
            );
        }
    }

    fn next_state(&self, last: &State, action: Action) -> Option<State> {
        let mut state = *last;
        state.took = None;
        match action.who {
            Who::Walk => self.walk(&mut state, action.pick),
            Who::Job => self.job(&mut state, action.pick),
            Who::Again => self.again(&mut state, action.pick),
            Who::User => Self::user(&mut state),
            Who::Github => Self::github(&mut state),
            Who::Machine => {
                state.lost = true;
                state.took = Some(Branch::GrantLost);
            }
            Who::Servers => self.servers(&mut state),
            Who::Delayed => {
                let ok = Self::hand_off(&mut state);
                state.delayed = false;
                state.took = Some(Branch::DelayedHandOff { ok });
            }
        }
        Some(state)
    }

    fn properties(&self) -> Vec<Property<Self>> {
        let mut properties = vec![
            Property::always(STARTED_RUN_KEPT, |_, state: &State| !state.limit_took),
            Property::always(NO_GRANT_AFTER_CANCEL, |_, state: &State| {
                !state.granted_after_cancel
            }),
            Property::always(ONE_GRANT_PER_RUN, |_, state: &State| !state.grant_replaced),
            Property::always(NO_SERVER_ON_GITHUB_BUILD, |_, state: &State| {
                !state.server_on_github
            }),
            Property::always(SERVERS_REPORT_LANDS, |_, state: &State| {
                !state.servers_refused
            }),
        ];
        properties.extend(harness::sometimes(COVERAGE, self));
        properties
    }
}

/// The Build Order moves a build on when its Builder does not start it in time, and a started
/// build only when GitHub fails it for infrastructure reasons, one being that the run ran out
/// of time (dashboard/CONTEXT.md:213): the 2-hour budget on GitHub Actions
/// (docs/user/builds/overview.md:56-57). The start limit's settle writes that message for a
/// run that checked in after the start limit ran out. The property reads "in time" as
/// `github_finish`'s Waiting already does (core/crates/ployz/src/sdk/github_build.rs:499-503):
/// a run started in time when its check-in committed before the start limit settled. That is a
/// reading of the rule, not its words.
const STARTED_RUN_KEPT: &str = "the start limit never ends a run whose check-in committed, nor cancels one while it holds the build";
const NO_GRANT_AFTER_CANCEL: &str = "no check-in commits for a Deployment the user cancelled";
const ONE_GRANT_PER_RUN: &str = "no check-in commits over a grant the run already recorded";
const NO_SERVER_ON_GITHUB_BUILD: &str = "no Server's report lands on a build a GitHub run holds";
/// A refused report stops the Deployment runner, and the Deployment lapses Unknown.
const SERVERS_REPORT_LANDS: &str =
    "record never refuses the Servers' report on a build the walk left them";

/// How the walk calls `finish` for `look`.
fn call(look: GithubLook) -> String {
    format!("finish(\"{}\")", code(look))
}

/// The name a story gives `who`.
fn name(who: Who) -> &'static str {
    match who {
        Who::Walk => "Cloud walk",
        Who::Job => "Runner",
        Who::Again => "Runner's second check-in",
        Who::User => "User",
        Who::Github => "GitHub",
        Who::Machine => "Grant's Machine",
        Who::Servers => "Deployment runner",
        Who::Delayed => "First try's hand-off",
    }
}

fn status(status: Status) -> &'static str {
    match status {
        Status::Pending => "pending",
        Status::Building => "building",
        Status::Built => "built",
        Status::Failed => "failed",
    }
}

/// The Store row, as a story prints it.
fn row(row: Row) -> String {
    let github = row.github.map_or_else(
        || "none".to_owned(),
        |github| {
            let grant = match github.grant {
                None => "no grant".to_owned(),
                Some(Request::Job) => "grant".to_owned(),
                Some(Request::Again) => "second grant".to_owned(),
            };
            let ended = match github.ended {
                None => String::new(),
                Some(RunEnd::Built) => ", ended Built".to_owned(),
                Some(RunEnd::Failed) => ", ended Failed".to_owned(),
            };
            format!("run {}, {grant}{ended}", github.run.0)
        },
    );
    format!(
        "{}, github {github}{}",
        status(row.status),
        if row.skipped { ", github_skipped" } else { "" }
    )
}

/// The start state in the code's words.
fn start(state: &State) -> String {
    if state.retried {
        "start: a retried github-start: the first try's githubDispatched timed out with its write still running (core/crates/ployz/src/sdk/store_call.rs:35-55), so Cloud cancelled the first try's run (:166); the retry's read of the build is next (:118)".to_owned()
    } else {
        "start: the run was dispatched, and the walk's github-open-0 is next".to_owned()
    }
}

/// `path` in the code's words: each step, then the Store row, the run, and each actor.
fn story(model: &BuildHandoff, path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    // Stateright pairs each state with the action taken from it; the row lists the action
    // that led to the state instead, so the last row shows the broken state.
    let mut led_by: Option<(State, Action)> = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = led_by.map_or_else(
            || start(state),
            |(before, action)| reads_as(model, &before, action, state),
        );
        let what = if model.broken == Broken::default() {
            what.replace(" (under Broken)", "")
        } else {
            what
        };
        led_by = action.map(|action| (*state, action));
        let _ = writeln!(
            story,
            "{n:2}. {what}\n    row: {} | Deployment: {:?} | run: {}{}{}{} | walk: {:?} | job: {:?}{}",
            row(state.row),
            state.deployment,
            if state.completed {
                "completed"
            } else {
                "running"
            },
            if state.cancelled { ", cancel sent" } else { "" },
            if state.pushed { ", pushed" } else { "" },
            if state.lost { ", grant lost" } else { "" },
            state.walk,
            state.job,
            match state.again {
                Again::Absent => String::new(),
                Again::Unsent | Again::CheckingIn(_) | Again::Returned => {
                    format!(" | second check-in: {:?}", state.again)
                }
            },
        );
    }
    story
}

/// What one step did, in the code's words.
fn reads_as(model: &BuildHandoff, before: &State, action: Action, after: &State) -> String {
    let who = name(action.who);
    let check = match before.walk {
        Walk::Open(check) | Walk::Waiting { check, .. } | Walk::Checking { check, .. } => check,
        Walk::Start(_) | Walk::Done | Walk::GaveUp => 0,
    };
    let Some(branch) = after.took else {
        return format!("{who}: {action:?}");
    };
    let what = match branch {
        Branch::Open { open: true } => format!(
            "github-open-{check}: heldGithubBuild finds it building (:228-230), and github-wait-{check} starts (dashboard/src/modules/config-store/store-deployment.inngest.ts:156-161)"
        ),
        Branch::Open { open: false } => format!(
            "github-open-{check}: heldGithubBuild finds it no longer building, and walkGithub returns (dashboard/src/modules/config-store/store-deployment.inngest.ts:156)"
        ),
        Branch::WaitEnded { woken: true } => format!(
            "github-wait-{check} returns the run's event; github-check-{check} starts with seen.ended (dashboard/src/modules/config-store/store-deployment.inngest.ts:158-163)"
        ),
        Branch::WaitEnded { woken: false } => format!(
            "github-wait-{check} times out after {}; github-check-{check} starts with startLimit {} (dashboard/src/modules/config-store/store-deployment.inngest.ts:157-163)",
            if check == 0 { "START_WITHIN_MINUTES" } else { "GITHUB_CHECK_INTERVAL" },
            check == 0
        ),
        Branch::Looked {
            held: false,
            rerun,
            ..
        } => format!(
            "github-check-{check}{}: heldGithubBuild finds the build no longer building, and it returns done (:194-195)",
            if rerun { ", run again" } else { "" }
        ),
        Branch::Looked {
            held: true,
            grant,
            rerun,
        } => format!(
            "github-check-{check}{}: heldGithubBuild reads it building, {} (:194)",
            if rerun { ", run again" } else { "" },
            if grant {
                "checked in"
            } else {
                "with build.grant === null"
            }
        ),
        Branch::DeploymentCancelled => "github-check: the Deployment is no longer queued or running, so cancelStoreGithubBuilds runs (:197-199)".to_owned(),
        Branch::PastBudget => "github-check: the run is past GITHUB_RUN_BUDGET_MS since it checked in (:212)".to_owned(),
        Branch::SeenEnded { grant } => format!(
            "github-check: the Deployment is queued (:197); {} so it calls finish(\"ended\") (:{})",
            if grant {
                "seen.ended or build.ended is set,"
            } else {
                "seen.ended is set,"
            },
            if grant { 223 } else { 217 }
        ),
        Branch::StillQueued { grant } => format!(
            "github-check: the Deployment is queued (:197), and the installation is usable (:202-206); runEnded is next (:{})",
            if grant { 223 } else { 217 }
        ),
        Branch::Asked {
            grant,
            ended,
            down,
            start_limit,
        } => {
            let read = if down {
                "GitHub can't be reached, which runEnded reads as running (:177-179)"
            } else if ended {
                "runEnded says the run completed (:177-179)"
            } else {
                "runEnded says the run is still running (:177-179)"
            };
            let then = match (grant, ended, start_limit) {
                (false, true, _) => format!("it calls {} (:217)", call(GithubLook::Ended)),
                (false, false, false) => "it is not the start limit, so it returns waiting (:218)".to_owned(),
                (false, false, true) => format!("at the start limit it calls {} (:219)", call(GithubLook::StartLimit)),
                (true, true, _) => format!("it calls {} (:223)", call(GithubLook::Ended)),
                (true, false, _) => "it returns waiting (:224)".to_owned(),
            };
            format!("github-check: {read}; {then}")
        }
        Branch::Cancelled(Canceller::Budget) => format!("github-check: cancelDispatchedGithubRun cancels the run on GitHub (:213); {} is next (:214)", call(GithubLook::Budget)),
        Branch::Cancelled(Canceller::StartLimit) => {
            let waiting = matches!(after.walk, Walk::Open(_));
            format!(
                "github-check: {} cancelDispatchedGithubRun cancels the run on GitHub (:220){}",
                if waiting {
                    "finish answered waiting, and (under Broken) with no done-gate"
                } else {
                    "finish answered done, so"
                },
                if before.row.started_on(RUN) {
                    ", though the run holds the build with its check-in committed"
                } else {
                    ""
                }
            )
        }
        Branch::Cancelled(Canceller::Stop(_)) => "cancelStoreGithubBuilds: cancelDispatchedGithubRun cancels the run on GitHub (dashboard/src/modules/config-store/config-store.server.ts:70)".to_owned(),
        Branch::Finish { by, look, step } => {
            let finisher = match (by, look) {
                (Finisher::Report, _) => "the report handler's github_finish(\"ended\") (:312)".to_owned(),
                (Finisher::Walk, GithubLook::StartLimit) if model.broken.start_limit_reads_the_grant => "github_finish(\"start_limit\"), settling by the grant as timed_out=true did (under Broken)".to_owned(),
                (Finisher::Walk, look) => format!("github_finish(\"{}\")", code(look)),
            };
            let what = match step {
                FinishStep::PassedOn => "re-reads the build: GitHub passed it on, so it returns Ended(Pending) (core/crates/ployz/src/sdk/github_build.rs:473-480)".to_owned(),
                FinishStep::NotBuilding(found) => format!("re-reads the build: it is {}, so it returns it (core/crates/ployz/src/sdk/github_build.rs:473-484)", status(found)),
                FinishStep::Reread { grant: true } => format!(
                    "re-reads the build and finds the grant (core/crates/ployz/src/sdk/github_build.rs:473, :505); {}",
                    if look == GithubLook::StartLimit && by == Finisher::Walk && !model.broken.start_limit_reads_the_grant {
                        "the start limit settles Unstarted whatever it found"
                    } else {
                        "inspect_grant is next (core/crates/ployz/src/sdk/github_build.rs:506)"
                    }
                ),
                FinishStep::Reread { grant: false } => "re-reads the build and finds no grant (core/crates/ployz/src/sdk/github_build.rs:473-481, :486); it settles Unstarted".to_owned(),
                FinishStep::Inspected(GrantRead::Pushed) => format!(
                    "inspect_grant reads the grant on its Machine: Pushed({}) (core/crates/ployz/src/sdk/github_build.rs:584-611, :629-630)",
                    if before.pushed && !before.lost {
                        "Some"
                    } else {
                        "None"
                    }
                ),
                FinishStep::LostPush => "inspect_grant reads the grant on its Machine, which no longer holds it, so the push reads as none: Pushed(None) (core/crates/ployzd/src/management/build_grant.rs:203-221, core/crates/ployz/src/sdk/github_build.rs:629-630)".to_owned(),
                FinishStep::Inspected(GrantRead::Refused) => "inspect_grant: the Machine refuses the read (core/crates/ployz/src/sdk/github_build.rs:508-510)".to_owned(),
                FinishStep::Inspected(GrantRead::Unreachable) => "inspect_grant: the grant's Machine can't be reached (core/crates/ployz/src/sdk/github_build.rs:593-604)".to_owned(),
                FinishStep::WaitsForMachine => "inspect_grant: the grant's Machine can't be reached, and at an Ended look it answers Waiting (core/crates/ployz/src/sdk/github_build.rs:511-513)".to_owned(),
                FinishStep::Settled(end) => format!(
                    "settle: github_end accepts {end} from the run that holds the build (core/crates/ployz-store/src/build.rs:1247-1253), and writes it {} (core/crates/ployz-store/src/build.rs:1258-1285)",
                    status(after.row.status)
                ),
                FinishStep::Refused(end) => format!(
                    "settle: github_end refuses {end} (core/crates/ployz-store/src/build.rs:1247-1256){}",
                    match end {
                        End::Unstarted(_) if before.row.started_on(RUN) => ": the run checked in, so github_finish answers Waiting (core/crates/ployz/src/sdk/github_build.rs:499-503)",
                        End::Unstarted(_) => ": the run no longer holds the build, so github_finish answers Waiting (core/crates/ployz/src/sdk/github_build.rs:499-503)",
                        End::Built | End::Failed(_) | End::Skipped(_) => ", and github_finish returns the conflict",
                    }
                ),
            };
            format!("{finisher}: {what}")
        }
        Branch::StepFailed { crash, retried } => format!(
            "github-check-{check}: {}; {}",
            if crash {
                "the step dies after its last effect landed"
            } else {
                "storeTry fails the step with the Store's conflict"
            },
            if retried {
                "Inngest runs the step again (dashboard/src/modules/config-store/store-deployment.inngest.ts:41)"
            } else {
                "no retry is left, so the function fails and onFailure runs (dashboard/src/modules/config-store/store-deployment.inngest.ts:41, :44-51)"
            }
        ),
        Branch::Outstanding { found, .. } => format!(
            "cancelStoreGithubBuilds: github_cancel's github_outstanding {} (core/crates/ployz/src/sdk/github_build.rs:654, core/crates/ployz-store/src/build.rs:1303-1317)",
            if found {
                "finds the build building on GitHub"
            } else {
                "finds nothing building on GitHub"
            }
        ),
        Branch::CancelEnded { ok, .. } => format!(
            "cancelStoreGithubBuilds: github_end Failed \"cancelled\" {} (core/crates/ployz/src/sdk/github_build.rs:657-664)",
            if ok {
                "fails the build"
            } else {
                "is refused, and the refusal is ignored"
            }
        ),
        Branch::PreCheck { pass: true, .. } => "check-in: heldGithubBuild and github_authorize read it building with no grant, so the pre-check passes (:280-281, core/crates/ployz/src/sdk/github_build.rs:195-204)".to_owned(),
        Branch::PreCheck { pass: false, .. } => "check-in: the pre-check finds it passed on, settled or checked in, and returns conflict (:280-281, core/crates/ployz/src/sdk/github_build.rs:195-204)".to_owned(),
        Branch::Minted { mint: Mint::Minted, .. } => "check-in: the Machine mints a Build Grant (core/crates/ployz/src/sdk/github_build.rs:222)".to_owned(),
        Branch::Minted { mint: Mint::Old, .. } => "check-in: mint answers None for an old Action or Machine and a sealed value (core/crates/ployz/src/sdk/github_build.rs:222-225, :308-311)".to_owned(),
        Branch::Minted { mint: Mint::Fails, .. } => "check-in: the mint fails, and the check-in returns the error (core/crates/ployz/src/sdk/github_build.rs:237-240)".to_owned(),
        Branch::OldMintEnded { ok, .. } => format!(
            "check-in: the old-mint skip's github_end Unstarted {} (core/crates/ployz/src/sdk/github_build.rs:227-235)",
            if ok {
                "passes the build on"
            } else {
                "is refused"
            }
        ),
        Branch::CheckedIn { ok: true, .. } => format!(
            "check-in: the Store's github_check_in finds the run holding the build{}, and sets the grant and checked_in_at (core/crates/ployz-store/src/build.rs:1141-1173); the runner gets its grant and build secrets (core/crates/ployz/src/sdk/github_build.rs:255-273)",
            if model.broken.check_in_unguarded {
                " and, its guard gone (under Broken), commits"
            } else {
                ", in flight with no grant"
            }
        ),
        Branch::CheckedIn { ok: false, .. } => "check-in: the Store's github_check_in refuses \"no longer wanted\", and the minted grant goes unrecorded (core/crates/ployz-store/src/build.rs:1149-1154, core/crates/ployz/src/sdk/github_build.rs:253-255)".to_owned(),
        Branch::Pushed => "the runner pushes its image into the grant's Machine".to_owned(),
        Branch::GrantLost => "the Machine no longer holds the grant: it expired after GRANT_LIFETIME, or the daemon restarted (core/crates/ployzd/src/management/build_grant.rs:6-9, :49, :203-221)".to_owned(),
        Branch::Reported { end, ok } => format!(
            "the final Build Steps report, {end:?}: github_report {} (:308, core/crates/ployz-store/src/build.rs:1177-1233)",
            if ok {
                "records the run's end"
            } else {
                "refuses it, the run no longer holding the build"
            }
        ),
        Branch::Quit => "the job stops before its final report".to_owned(),
        Branch::Woke { waiting } => format!(
            "the report handler sends the run's event (:315){}",
            if waiting {
                ", which the walk's wait receives"
            } else {
                ", which no wait receives"
            }
        ),
        Branch::UserCancelled => "cancel turns the queued Deployment Cancelled (`deployment::cancel`, core/crates/ployz-store/src/deployment/lifecycle.rs); afterWrite's cancelStoreGithubBuilds is next (dashboard/src/modules/config-store/config-store.server.ts:306-310)".to_owned(),
        Branch::ServersReported { ok: true } => format!(
            "run-deployment: record takes the Servers' Building report (core/crates/ployz-store/src/build.rs:264-314){}",
            if before.row.github.is_some() {
                ", though a GitHub run holds the build (under Broken)"
            } else {
                ""
            }
        ),
        Branch::ServersReported { ok: false } => "run-deployment: record refuses the Servers' Building report, since a GitHub run holds the build (core/crates/ployz-store/src/build.rs:289-296)".to_owned(),
        Branch::StartRead { at: Start::Read, follows: true } => "github-start: holder reads the build building on a run, and the retry follows that run (:118-119)".to_owned(),
        Branch::StartRead { at: Start::Read, follows: false } => "github-start: holder finds no run holding the build (:118-119); the retry dispatches its own run, and its githubDispatched is next (:157-166)".to_owned(),
        Branch::StartRead { at: _, follows: true } => "github-start: after the refused hand-off, holder reads the build again, and the retry follows the run that holds it (:122, :172)".to_owned(),
        Branch::StartRead { at: _, follows: false } => "github-start: after the refused hand-off, holder finds no run holding the build, so the step answers done (:122, :172) and walkGithub returns (dashboard/src/modules/config-store/store-deployment.inngest.ts:154)".to_owned(),
        Branch::HandOff { won: true } => "github-start: github_dispatched hands the waiting build to the retry's run (core/crates/ployz-store/src/build.rs:1013-1036), and the walk follows it".to_owned(),
        Branch::HandOff { won: false } => format!(
            "github-start: github_dispatched refuses the retry's run, since the build is no longer waiting (core/crates/ployz-store/src/build.rs:1020-1028); Cloud cancels that run (:166){}",
            if model.broken.start_leaves_held_run {
                " and answers done without reading the build again (under Broken), so walkGithub returns (dashboard/src/modules/config-store/store-deployment.inngest.ts:154)"
            } else {
                ", and holder reads the build again (:122, :172)"
            }
        ),
        Branch::DelayedHandOff { ok: true } => "the first try's githubDispatched commits: github_dispatched hands the waiting build to the first try's run (core/crates/ployz-store/src/build.rs:1013-1036)".to_owned(),
        Branch::DelayedHandOff { ok: false } => "the first try's githubDispatched is refused, since the build is no longer waiting (core/crates/ployz-store/src/build.rs:1020-1028)".to_owned(),
        Branch::RunCompleted { killed } => format!(
            "the run completes{}; its workflow_run webhook sends the run's event (dashboard/src/routes/api/github/-webhook.handler.ts:78-83)",
            if killed { " and its job ends" } else { "" }
        ),
    };
    format!("{who}: {what}")
}

#[cfg(test)]
mod tests {
    use stateright::Checker;

    use super::*;
    use crate::harness::{
        self, Narrated, breaks, breaks_where, check, first_break, follow_from, last,
    };

    type Path = harness::Path<BuildHandoff>;

    impl Narrated for BuildHandoff {
        fn story(&self, path: &[(State, Option<Action>)]) -> String {
            story(self, path)
        }
    }

    /// Steps `actions` from the first try's start state.
    fn follow(model: BuildHandoff, actions: &[Action]) -> Path {
        let start = model
            .init_states()
            .into_iter()
            .find(|state| !state.retried)
            .expect("the first try's start state");
        follow_from(model, start, actions)
    }

    /// The code: one run, one check-in request, one crash.
    fn code() -> BuildHandoff {
        BuildHandoff {
            requests: 1,
            crashes: 1,
            broken: Broken::default(),
        }
    }

    fn code_without(broken: Broken) -> BuildHandoff {
        BuildHandoff { broken, ..code() }
    }

    /// The start limit before it passed its look: `github_finish(timed_out=true)` settles by
    /// the grant. The finding's tests run on it.
    const TIMED_OUT: Broken = Broken {
        unstarted_ignores_grant: false,
        check_in_unguarded: false,
        cancel_ignores_done: false,
        start_limit_reads_the_grant: true,
        record_ignores_github: false,
        start_leaves_held_run: false,
    };

    fn timed_out() -> BuildHandoff {
        code_without(TIMED_OUT)
    }

    /// The branch each step took, in order.
    fn branches(path: &Path) -> Vec<Branch> {
        path.iter()
            .skip(1)
            .filter_map(|(state, _)| state.took)
            .collect()
    }

    /// `first` is taken before `then` on `path`.
    fn before(path: &Path, first: impl Fn(Branch) -> bool, then: impl Fn(Branch) -> bool) -> bool {
        let taken = branches(path);
        let Some(at) = taken.iter().position(|branch| first(*branch)) else {
            return false;
        };
        taken.iter().skip(at + 1).any(|branch| then(*branch))
    }

    const fn walk(pick: Pick) -> Action {
        Action {
            who: Who::Walk,
            pick,
        }
    }

    const fn job(pick: Pick) -> Action {
        Action {
            who: Who::Job,
            pick,
        }
    }

    const GO: Pick = Pick::Go;

    /// The finding. The walk opens, its first wait times out, and the start limit's look
    /// reads the build with no grant. The runner's pre-check passes, the Machine mints, and
    /// the Store's `github_check_in` commits. The look reads the Deployment queued and GitHub
    /// says the run is running, so it calls `finish("start_limit")`. Settling by the grant,
    /// `github_finish` re-reads and finds the grant, `inspect_grant` reads nothing pushed, and
    /// the end is Skipped "the run ran out of time", which `github_end` accepts. Cloud then
    /// cancels the run.
    const THE_FINDING: [Action; 12] = [
        walk(GO),
        walk(GO),
        walk(GO),
        job(GO),
        job(Pick::Mint(Mint::Minted)),
        job(GO),
        walk(GO),
        walk(GO),
        walk(GO),
        walk(Pick::Grant(GrantRead::Pushed)),
        walk(GO),
        walk(GO),
    ];

    fn the_limit_read_no_grant(branch: Branch) -> bool {
        matches!(
            branch,
            Branch::Looked {
                held: true,
                grant: false,
                ..
            }
        )
    }

    fn the_check_in_committed(branch: Branch) -> bool {
        matches!(branch, Branch::CheckedIn { ok: true, .. })
    }

    /// The finding, with the start limit settling by the grant. The replay shows the schedule
    /// step by step, through the cancel. The checker's first path to a broken state then shows
    /// the same race: the start limit's read found no grant, a check-in committed after it,
    /// and the start limit's own settle ended the run.
    #[test]
    fn timed_out_passes_on_a_run_that_checked_in() {
        let path = follow(timed_out(), &THE_FINDING);
        assert!(before(
            &path,
            the_limit_read_no_grant,
            the_check_in_committed
        ));
        let settled = path.get(path.len() - 2).expect("the settle's state").0;
        assert_eq!(
            settled.took,
            Some(Branch::Finish {
                by: Finisher::Walk,
                look: GithubLook::StartLimit,
                step: FinishStep::Settled(End::Skipped(Why::RanOutOfTime)),
            })
        );
        assert!(settled.limit_took);
        assert_eq!(settled.row.status, Status::Pending);
        assert!(settled.row.skipped);
        let after = last(&path);
        assert_eq!(after.took, Some(Branch::Cancelled(Canceller::StartLimit)));
        assert!(after.cancelled);
        assert_eq!(after.walk, Walk::Done);
        assert_eq!(
            after.job,
            Job::Building,
            "the runner holds its grant and secrets"
        );

        let found = breaks(timed_out(), STARTED_RUN_KEPT);
        assert!(before(
            &found,
            the_limit_read_no_grant,
            the_check_in_committed
        ));
        assert!(matches!(
            last(&found).took,
            Some(Branch::Finish {
                by: Finisher::Walk,
                look: GithubLook::StartLimit,
                step: FinishStep::Settled(End::Skipped(Why::RanOutOfTime) | End::Failed(_)),
            })
        ));
    }

    /// The same race, with the grant's Machine unreachable: `timed_out` fails the build, and
    /// Cloud cancels the run.
    #[test]
    fn timed_out_fails_a_run_that_checked_in_when_its_machine_is_unreachable() {
        let found = breaks_where(timed_out(), STARTED_RUN_KEPT, |path| {
            last(path).took
                == Some(Branch::Finish {
                    by: Finisher::Walk,
                    look: GithubLook::StartLimit,
                    step: FinishStep::Settled(End::Failed(Why::Unreachable)),
                })
        });
        assert!(before(
            &found,
            the_limit_read_no_grant,
            the_check_in_committed
        ));
        assert_eq!(last(&found).row.status, Status::Failed);
    }

    /// Settling by the grant breaks only `STARTED_RUN_KEPT`, and reaches every row.
    #[test]
    fn timed_out_breaks_only_the_finding() {
        let model = timed_out();
        let checker = check(model);
        println!("timed out: {} states", checker.unique_state_count());
        assert!(checker.discovery(STARTED_RUN_KEPT).is_some());
        for property in [
            NO_GRANT_AFTER_CANCEL,
            ONE_GRANT_PER_RUN,
            NO_SERVER_ON_GITHUB_BUILD,
            SERVERS_REPORT_LANDS,
        ] {
            assert!(checker.discovery(property).is_none(), "{property} breaks");
        }
        harness::assert_reached(COVERAGE, &model, &checker);
    }

    /// The code holds every `always` property, and reaches every row it can.
    #[test]
    fn the_code_keeps_a_started_run() {
        let checker = check(code());
        println!("the code: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// The finding's schedule on the code: the start limit's Unstarted is refused, the walk
    /// waits, and the run stays checked in.
    #[test]
    fn the_start_limit_waits_for_the_finding_s_run() {
        let schedule = THE_FINDING
            .get(..THE_FINDING.len() - 3)
            .expect("the finding up to its re-read")
            .iter()
            .copied()
            .chain([walk(GO)])
            .collect::<Vec<_>>();
        let path = follow(code(), &schedule);
        let after = last(&path);
        assert_eq!(
            after.took,
            Some(Branch::Finish {
                by: Finisher::Walk,
                look: GithubLook::StartLimit,
                step: FinishStep::Refused(End::Unstarted(Why::NoRunnerInTime)),
            })
        );
        assert!(after.limit_waited && !after.limit_took);
        assert_eq!(after.walk, Walk::Open(1));
        assert!(!after.cancelled);
        assert!(after.row.started_on(RUN));
    }

    /// A run that checked in inside the start limit's window is settled by a later look: an
    /// Ended look once it reported, or the budget.
    #[test]
    fn a_later_look_ends_the_run_the_start_limit_waited_for() {
        for (row, look) in [
            (
                "after the start limit waited for a run that checked in, an Ended look settles it",
                GithubLook::Ended,
            ),
            (
                "after the start limit waited for a run that checked in, the budget settles it",
                GithubLook::Budget,
            ),
        ] {
            let path = breaks(code(), row);
            assert!(settled_by(&last(&path), Finisher::Walk, look));
            assert!(before(
                &path,
                |branch| branch
                    == Branch::Finish {
                        by: Finisher::Walk,
                        look: GithubLook::StartLimit,
                        step: FinishStep::Refused(End::Unstarted(Why::NoRunnerInTime)),
                    },
                |branch| matches!(
                    branch,
                    Branch::Finish {
                        by: Finisher::Walk,
                        step: FinishStep::Settled(_),
                        ..
                    }
                ),
            ));
        }
    }

    /// The start limit on each state its re-read can find. It settles only a run that never
    /// checked in; on the rest it changes nothing.
    #[test]
    fn the_start_limit_on_each_edge_state() {
        let model = code();
        let start = model
            .init_states()
            .into_iter()
            .find(|state| !state.retried)
            .expect("the first try's start state");
        let building = |grant, ended| Row {
            status: Status::Building,
            github: Some(Github {
                run: RUN,
                grant,
                ended,
            }),
            skipped: false,
        };
        let cases = [
            (
                "no grant",
                building(None, None),
                Finished::Ended(Status::Pending),
            ),
            (
                "a grant without an end",
                building(Some(Request::Job), None),
                Finished::Waiting,
            ),
            (
                "a grant with its end reported",
                building(Some(Request::Job), Some(RunEnd::Built)),
                Finished::Waiting,
            ),
            (
                "passed on",
                Row {
                    status: Status::Pending,
                    github: None,
                    skipped: true,
                },
                Finished::Ended(Status::Pending),
            ),
            (
                "built",
                Row {
                    status: Status::Built,
                    ..building(Some(Request::Job), Some(RunEnd::Built))
                },
                Finished::Ended(Status::Built),
            ),
            (
                "failed",
                Row {
                    status: Status::Failed,
                    ..building(Some(Request::Job), None)
                },
                Finished::Ended(Status::Failed),
            ),
        ];
        for (what, found, expect) in cases {
            let mut state = State {
                row: found,
                ..start
            };
            let mut at = Finish::Read;
            let returned = loop {
                match model.finish(&mut state, Finisher::Walk, GithubLook::StartLimit, at, GO) {
                    Next::At(next) => at = next,
                    Next::Returned(returned) => break returned,
                }
            };
            assert_eq!(returned, expect, "{what}");
            assert!(!state.limit_took, "{what}");
            if what == "no grant" {
                assert_eq!(state.row.status, Status::Pending, "{what}");
                assert!(state.row.skipped, "{what}");
            } else {
                assert_eq!(state.row, found, "{what} is left as it was");
            }
        }
        // `github_end` decides Unstarted for the run it is given; another run's build is not
        // that run's (core/crates/ployz-store/src/build.rs:1001-1006, :1248-1250).
        let mut state = State {
            row: Row {
                github: Some(Github {
                    run: Run(1),
                    grant: None,
                    ended: None,
                }),
                ..start.row
            },
            ..start
        };
        let theirs = state.row;
        assert_eq!(
            state.github_end(Broken::default(), RUN, End::Unstarted(Why::NoRunnerInTime)),
            Err(())
        );
        assert_eq!(state.row, theirs, "another run's build is left as it was");
    }

    /// Without the Unstarted grant guard, the start limit passes on a run that checked in.
    #[test]
    fn the_start_limit_needs_the_unstarted_guard() {
        let path = breaks(
            code_without(Broken {
                unstarted_ignores_grant: true,
                ..Broken::default()
            }),
            STARTED_RUN_KEPT,
        );
        assert_eq!(
            last(&path).took,
            Some(Branch::Finish {
                by: Finisher::Walk,
                look: GithubLook::StartLimit,
                step: FinishStep::Settled(End::Unstarted(Why::NoRunnerInTime)),
            })
        );
    }

    /// Settling by the grant needs the guard too: a check-in that commits after
    /// `github_finish`'s re-read found no grant is what its Unstarted conflicts with.
    #[test]
    fn timed_out_needs_the_unstarted_guard() {
        let path = breaks_where(
            code_without(Broken {
                unstarted_ignores_grant: true,
                ..TIMED_OUT
            }),
            STARTED_RUN_KEPT,
            |path| {
                matches!(
                    last(path).took,
                    Some(Branch::Finish {
                        step: FinishStep::Settled(End::Unstarted(_)),
                        ..
                    })
                )
            },
        );
        assert!(before(
            &path,
            |branch| matches!(
                branch,
                Branch::Finish {
                    step: FinishStep::Reread { grant: false },
                    ..
                }
            ),
            the_check_in_committed,
        ));
    }

    /// Without its guard, `github_check_in` commits a check-in for a Deployment the user
    /// cancelled.
    #[test]
    fn the_check_in_guard_refuses_a_cancelled_deployment() {
        let path = breaks(
            code_without(Broken {
                check_in_unguarded: true,
                ..Broken::default()
            }),
            NO_GRANT_AFTER_CANCEL,
        );
        assert!(before(
            &path,
            |branch| branch == Branch::UserCancelled,
            the_check_in_committed,
        ));
    }

    /// With a second check-in request of the same run, the guard keeps the first grant;
    /// without it, the second commits over it.
    #[test]
    fn the_check_in_guard_refuses_a_second_grant() {
        let two = BuildHandoff {
            requests: 2,
            crashes: 0,
            ..code()
        };
        let checker = check(two);
        println!("two requests: {} states", checker.unique_state_count());
        for property in [
            STARTED_RUN_KEPT,
            NO_GRANT_AFTER_CANCEL,
            ONE_GRANT_PER_RUN,
            NO_SERVER_ON_GITHUB_BUILD,
            SERVERS_REPORT_LANDS,
        ] {
            assert!(checker.discovery(property).is_none(), "{property} breaks");
        }
        let path = breaks(
            BuildHandoff {
                broken: Broken {
                    check_in_unguarded: true,
                    ..Broken::default()
                },
                ..two
            },
            ONE_GRANT_PER_RUN,
        );
        let committed = branches(&path)
            .into_iter()
            .filter(|branch| the_check_in_committed(*branch))
            .count();
        assert_eq!(committed, 2);
    }

    /// Without the done-gate, the start limit cancels a run that checked in while it still
    /// holds the build.
    #[test]
    fn the_start_limit_needs_the_done_gate() {
        let path = breaks(
            code_without(Broken {
                cancel_ignores_done: true,
                ..Broken::default()
            }),
            STARTED_RUN_KEPT,
        );
        assert_eq!(
            last(&path).took,
            Some(Branch::Cancelled(Canceller::StartLimit))
        );
        assert!(last(&path).row.started_on(RUN));
    }

    /// Without its look, the start limit settles by the grant, and the finding comes back.
    #[test]
    fn the_start_limit_needs_its_look() {
        let path = breaks(timed_out(), STARTED_RUN_KEPT);
        assert!(before(
            &path,
            the_limit_read_no_grant,
            the_check_in_committed
        ));
    }

    /// A retried start follows the run its first try handed the build to. The Servers get that
    /// build once the start limit ends the cancelled run Unstarted, and `record` takes their
    /// report.
    #[test]
    fn a_retried_start_follows_the_held_run() {
        let path = breaks_where(
            code(),
            "after a retried start, a look passes on the run Cloud cancelled, and the Servers take the build",
            |path| {
                branches(path).contains(&Branch::Finish {
                    by: Finisher::Walk,
                    look: GithubLook::StartLimit,
                    step: FinishStep::Settled(End::Unstarted(Why::NoRunnerInTime)),
                })
            },
        );
        assert!(path.first().expect("a start state").0.retried);
    }

    /// The first try's hand-off can commit after the retry read the build with no run. The
    /// retry's own hand-off is then refused, and its second read follows the first try's run.
    #[test]
    fn a_retried_start_follows_a_hand_off_that_commits_after_its_read() {
        let path = breaks(
            code(),
            "the first try's hand-off commits after the retry read the build: the retry's hand-off is refused, and its second read follows the first try's run",
        );
        let taken = branches(&path);
        assert_eq!(
            taken,
            [
                Branch::StartRead {
                    at: Start::Read,
                    follows: false,
                },
                Branch::DelayedHandOff { ok: true },
                Branch::HandOff { won: false },
                Branch::StartRead {
                    at: Start::Reread,
                    follows: true,
                },
            ]
        );
        assert_eq!(last(&path).walk, Walk::Open(0));
    }

    /// A run Cloud cancelled can still check in before GitHub ends it. The start limit waits
    /// for it, and a later look settles it.
    #[test]
    fn a_retried_start_s_run_that_checked_in_keeps_its_build() {
        let path = breaks(
            code(),
            "after a retried start, the run Cloud cancelled checked in first: the start limit waits for it, and a later look settles it",
        );
        assert!(before(&path, the_check_in_committed, |branch| matches!(
            branch,
            Branch::Finish {
                by: Finisher::Walk,
                look: GithubLook::StartLimit,
                step: FinishStep::Refused(End::Unstarted(Why::NoRunnerInTime)),
            }
        )));
        assert!(!last(&path).limit_took);
    }

    /// A retried start that answers `done` for a refused hand-off, without its second read,
    /// returns the walk while the first try's late hand-off holds the build: `record` refuses
    /// the Servers' report, and the Deployment lapses Unknown.
    #[test]
    fn a_retried_start_needs_its_second_read() {
        let path = breaks(
            code_without(Broken {
                start_leaves_held_run: true,
                ..Broken::default()
            }),
            SERVERS_REPORT_LANDS,
        );
        let after = last(&path);
        assert!(after.retried);
        assert_eq!(after.took, Some(Branch::ServersReported { ok: false }));
        assert!(after.row.building_on(RUN).is_some());
        assert!(before(
            &path,
            |branch| branch
                == Branch::StartRead {
                    at: Start::Read,
                    follows: false,
                },
            |branch| branch == Branch::DelayedHandOff { ok: true },
        ));
        assert!(before(
            &path,
            |branch| branch == Branch::DelayedHandOff { ok: true },
            |branch| branch == Branch::HandOff { won: false },
        ));
    }

    /// `record` refuses the Servers' report on a build a GitHub run holds, and takes it once
    /// GitHub passed the build on. The walk leaves the Servers no such build, so only with the
    /// retried start's broken answer does a Server's report land on the run's build without
    /// the guard.
    #[test]
    fn record_needs_its_github_guard() {
        assert!(
            first_break(
                code_without(Broken {
                    record_ignores_github: true,
                    ..Broken::default()
                }),
                NO_SERVER_ON_GITHUB_BUILD,
                |_| true,
                true,
            )
            .is_none(),
            "the walk leaves the Servers no held build"
        );
        let path = breaks(
            code_without(Broken {
                record_ignores_github: true,
                start_leaves_held_run: true,
                ..Broken::default()
            }),
            NO_SERVER_ON_GITHUB_BUILD,
        );
        let after = last(&path);
        assert_eq!(after.took, Some(Branch::ServersReported { ok: true }));
        assert!(
            after.row.building_on(RUN).is_some(),
            "the run still holds it"
        );

        let mut state = code()
            .init_states()
            .into_iter()
            .find(|state| !state.retried)
            .expect("the first try's start state");
        let held = state.row;
        code().servers(&mut state);
        assert_eq!(state.took, Some(Branch::ServersReported { ok: false }));
        assert_eq!(state.row, held, "a held build is left as it was");
        let mut state = State {
            row: Row {
                status: Status::Pending,
                github: None,
                skipped: true,
            },
            servers: Servers::Idle,
            ..state
        };
        code().servers(&mut state);
        assert_eq!(state.took, Some(Branch::ServersReported { ok: true }));
        assert_eq!(state.row.status, Status::Building);
        assert!(!state.server_on_github);
    }
}
