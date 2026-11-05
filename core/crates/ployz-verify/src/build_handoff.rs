//! A Git build handed to a GitHub Actions run, between Cloud's walk that follows the run and
//! the runner that checks in for its Build Grant. A bare `:n` is a line of the file named just
//! before it in the same parentheses, and otherwise of
//! `dashboard/src/modules/config-store/store-github-builds.server.ts`.
//!
//! The actors, each its own process:
//! - **Cloud's walk.** Cloud's worker runs the Inngest function `run-store-deployment`, and
//!   `walkGithub` (dashboard/src/modules/config-store/store-deployment.inngest.ts:151-167) runs
//!   one `github-open` step, one wait and one `github-check` step per look. Each check step runs
//!   `checkStoreGithubBuild` (:184-219). Its `store.githubFinish` (:203) calls the Store through
//!   the native SDK in the worker's own process (core/crates/ployz-sdk/src/config_store.rs:647),
//!   so `github_finish` and `inspect_grant` run there too
//!   (core/crates/ployz/src/sdk/github_build.rs:454-539, :567-594).
//! - **The runner.** A GitHub Actions job. Its check-in and its Build Steps reports are HTTP
//!   requests that Cloud web serves (:269-284, :292-315), and each calls the Store through the
//!   native SDK in Cloud web's process: `github_check_in` (core/crates/ployz/src/sdk/github_build.rs:187-274)
//!   and `github_report` (core/crates/ployz/src/sdk/github_build.rs:384-432). The report
//!   handler then calls `githubFinish` itself (:306).
//! - **A user** who cancels the Deployment. Cloud web commits the Store's cancel, and its
//!   `afterWrite` stops the GitHub builds (dashboard/src/modules/config-store/config-store.server.ts:306-310).
//! - **GitHub**, which ends the run once its job exits or Cloud cancelled it, and sends the
//!   `workflow_run` webhook (dashboard/src/routes/api/github/-webhook.handler.ts:78-83).
//! - **The Store**, Postgres. Each write runs in one transaction that first locks the
//!   Deployment's Environment row (`deployment::locked`, core/crates/ployz-store/src/deployment/mod.rs:1072-1076;
//!   core/crates/ployz-store/src/scope.rs:568-580) and then re-reads the build. Writers run read
//!   committed (core/crates/ployz-store/src/storage/pg.rs:107-120), so each write sees every
//!   write committed before it took the lock, and each model write is one atomic step.
//!
//! The finding: the start limit's look reads the build with no grant (:188, :210), and calls
//! `finish(true)` (:213). If the run's check-in commits before `github_finish` re-reads the
//! build (core/crates/ployz/src/sdk/github_build.rs:461), `github_finish` finds the grant. With
//! `timed_out` it settles the run Skipped "the run ran out of time"
//! (core/crates/ployz/src/sdk/github_build.rs:529-531), or Failed when the grant's Machine can't
//! be reached (core/crates/ployz/src/sdk/github_build.rs:497-501). `github_end` accepts either
//! from the run that holds the build (core/crates/ployz-store/src/build.rs:953), and Cloud
//! cancels the run (:214) after it got its Build Grant and build secrets. A check-in that
//! commits one step later, after the re-read, is kept: the Unstarted settle is refused, and
//! `github_finish` answers Waiting, "It checked in meanwhile: the next look finds it started"
//! (core/crates/ployz/src/sdk/github_build.rs:484-488). Whether the start limit keeps a run
//! that checked in turns on which side of the re-read its check-in commits.
//!
//! The fix variant, a design for a later PR, passes `github_finish` its `Look`, and the start
//! limit's `github_finish` issues only
//! `GithubEnd::Unstarted`, whose grant guard (core/crates/ployz-store/src/build.rs:950-952)
//! refuses it once a check-in committed, and `github_finish` then answers Waiting
//! (core/crates/ployz/src/sdk/github_build.rs:484-488). The walk goes on to its next look.

use std::fmt::Write as _;

use stateright::{Model, Property};

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

/// `RunEnd`, the final report's end (core/crates/ployz-store/src/build.rs:590-595).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum RunEnd {
    Built,
    Failed,
}

/// `GithubState` (core/crates/ployz-store/src/build.rs:577-585). `grant` and `checked_in_at` are
/// written together, only by `github_check_in` (core/crates/ployz-store/src/build.rs:874-879), so
/// one field stands for both. `received` decides nothing but a report's own skip, so it is left
/// out.
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
    /// `building_on` (core/crates/ployz-store/src/build.rs:715-719).
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

/// The Deployment's status, as the walk (:191-192) and `github_check_in`'s guard
/// (core/crates/ployz-store/src/build.rs:870) read it. The walk runs before the runner claims
/// the Deployment (dashboard/src/modules/config-store/store-deployment.inngest.ts:62, :79), so it is queued until a user cancels it.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Deployment {
    Queued,
    /// `cancel` turned a queued Deployment Cancelled
    /// (core/crates/ployz-store/src/deployment/lifecycle.rs:1466-1469).
    Cancelled,
}

/// Why a build failed or was passed on, as the message the Store records says.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Why {
    /// "the run ran out of time" (core/crates/ployz/src/sdk/github_build.rs:529-531).
    RanOutOfTime,
    /// "the run ended without pushing an image" (core/crates/ployz/src/sdk/github_build.rs:532-534).
    EndedWithoutPushing,
    /// "no runner started the build in time" (core/crates/ployz/src/sdk/github_build.rs:477).
    NoRunnerInTime,
    /// "the run ended before it started the build" (core/crates/ployz/src/sdk/github_build.rs:479).
    EndedBeforeStart,
    /// `OLD_MINT_SKIPS` (core/crates/ployz/src/sdk/github_build.rs:276-277).
    OldMint,
    /// "a build step failed" (core/crates/ployz/src/sdk/github_build.rs:526-528).
    StepFailed,
    /// "your Machine couldn't be reached to confirm the push"
    /// (core/crates/ployz/src/sdk/github_build.rs:497-501).
    Unreachable,
    /// The Machine's refusal (core/crates/ployz/src/sdk/github_build.rs:493-495).
    Refused,
    /// "cancelled" (core/crates/ployz/src/sdk/github_build.rs:640-642).
    Cancelled,
}

impl Why {
    fn message(self) -> &'static str {
        match self {
            Self::RanOutOfTime => "the run ran out of time",
            Self::EndedWithoutPushing => "the run ended without pushing an image",
            Self::NoRunnerInTime => "no runner started the build in time",
            Self::EndedBeforeStart => "the run ended before it started the build",
            Self::OldMint => {
                "GitHub builds a sealed value only with an up-to-date Ployz on the runner and your server"
            }
            Self::StepFailed => "a build step failed",
            Self::Unreachable => "your Machine couldn't be reached to confirm the push",
            Self::Refused => "<the Machine's refusal>",
            Self::Cancelled => "cancelled",
        }
    }
}

/// `GithubEnd` (core/crates/ployz-store/src/build.rs:645-659).
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

/// The look that calls `githubFinish`: the budget (:208), the start limit (:213),
/// or a run that ended (:211, :217, and the report's own at :306). The code passes only
/// `timed_out`, true for the first two (`timed_out`).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Look {
    Budget,
    StartLimit,
    Ended,
}

impl Look {
    fn timed_out(self) -> bool {
        match self {
            Self::Budget | Self::StartLimit => true,
            Self::Ended => false,
        }
    }
}

/// What `inspect_grant` read on the grant's Machine (`GrantRead`,
/// core/crates/ployz/src/sdk/github_build.rs:556-565).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum GrantRead {
    /// `Pushed`: what the Machine received, read from the model's `pushed` while the Machine
    /// still holds the grant. Once it lost the grant (`lost`), `inspect` answers NotFound
    /// whatever was pushed (core/crates/ployzd/src/management/build_grant.rs:203-221), which
    /// reads as nothing pushed (core/crates/ployz/src/sdk/github_build.rs:612-613).
    Pushed,
    /// `Refused`: the Machine belongs to another Cluster or refused the read (core/crates/ployz/src/sdk/github_build.rs:580-584, :620-621).
    Refused,
    /// `Unreachable`: no connection names the Machine, the dial failed, or the read failed
    /// without a refusal (core/crates/ployz/src/sdk/github_build.rs:576-577, :582-583, :586-587, :622).
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

/// A `github_finish` statement next (core/crates/ployz/src/sdk/github_build.rs:454-539).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Finish {
    /// The re-read of the build (core/crates/ployz/src/sdk/github_build.rs:461-472).
    Read,
    /// `inspect_grant` (core/crates/ployz/src/sdk/github_build.rs:491), with the end the re-read found (core/crates/ployz/src/sdk/github_build.rs:504).
    Inspect { ended: Option<RunEnd> },
    /// `settle`'s `github_end` (core/crates/ployz/src/sdk/github_build.rs:541-552).
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
/// `github_cancel` (core/crates/ployz/src/sdk/github_build.rs:633-650), then each run cancelled.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Stop {
    /// `github_outstanding`'s read (core/crates/ployz/src/sdk/github_build.rs:637).
    Outstanding,
    /// The build it found: its `github_end` Failed "cancelled" (core/crates/ployz/src/sdk/github_build.rs:640-647).
    Ending,
    /// `cancelDispatchedGithubRun` for it (dashboard/src/modules/config-store/config-store.server.ts:70).
    Cancelling,
}

/// Where a `github-check` step is in `checkStoreGithubBuild`.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum At {
    /// `heldGithubBuild` is next (:188).
    Read,
    /// The Deployment's read is next (:191). `grant` and `ended` are what :188 read.
    Deployment { grant: bool, ended: bool },
    /// `cancelStoreGithubBuilds` (:193).
    Stopping(Stop),
    /// `cancelDispatchedGithubRun` past the budget (:207).
    BudgetCancel,
    /// `runEnded` is next: at :211 without a grant, at :217 with one.
    AskingGithub { grant: bool },
    /// `finish` (:201-205), called with `look`.
    Finishing { look: Look, at: Finish },
    /// `cancelDispatchedGithubRun` after the start limit's finish (:214). `waiting` is what
    /// the finish answered, which :215 returns.
    LimitCancel { waiting: bool },
}

/// Where Cloud's walk is (`walkGithub`, dashboard/src/modules/config-store/store-deployment.inngest.ts:151-167).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Walk {
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
    /// It has not checked in: its check-in's pre-check is next (:274-275,
    /// core/crates/ployz/src/sdk/github_build.rs:195-204).
    Queued,
    /// Inside its check-in request, which Cloud web serves.
    CheckingIn(CheckIn),
    /// Checked in, with its grant and secrets; it builds.
    Building,
    /// It pushed into the grant's Machine.
    Pushed,
    /// Its final Build Steps report is next (:302, core/crates/ployz-store/src/build.rs:892-935).
    Reporting(RunEnd),
    /// The report handler's own `githubFinish(false)` (:303-308).
    Finishing(Finish),
    /// The report handler's `sendInngestEvent` (:309).
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

/// Who stopped the builds: the walk at :193, or a user's cancel.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Stopper {
    Walk,
    User,
}

/// Who cancelled the run on GitHub.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Canceller {
    /// Past the budget (:207).
    Budget,
    /// After the start limit's finish (:214).
    StartLimit,
    /// `cancelStoreGithubBuilds` (dashboard/src/modules/config-store/config-store.server.ts:70).
    Stop(Stopper),
}

/// Who ran a `github_finish`: the walk, or the report handler (:306).
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Finisher {
    Walk,
    Report,
}

/// What one `github_finish` statement did.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum FinishStep {
    /// The re-read found the build passed on, and it returned `Ended(Pending)` (core/crates/ployz/src/sdk/github_build.rs:468).
    PassedOn,
    /// The re-read found the build no longer building, and it returned it (core/crates/ployz/src/sdk/github_build.rs:470-472).
    NotBuilding(Status),
    /// The re-read found it building; `grant` is whether it found the grant.
    Reread { grant: bool },
    /// `inspect_grant` read this.
    Inspected(GrantRead),
    /// `inspect_grant` read nothing pushed for a grant that received the push, because its
    /// Machine lost the grant, and the re-read found a Built report: Pushed(None)
    /// (core/crates/ployzd/src/management/build_grant.rs:203-221,
    /// core/crates/ployz/src/sdk/github_build.rs:612-613).
    LostPush,
    /// An unreachable Machine with `timed_out` false: Waiting (core/crates/ployz/src/sdk/github_build.rs:496).
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
    /// `heldGithubBuild` at :188. `held` is false when :189 returns done; `rerun` is a step
    /// that already ran before.
    Looked {
        held: bool,
        grant: bool,
        rerun: bool,
    },
    DeploymentCancelled,
    PastBudget,
    /// `seen.ended`, or with a grant the :188 read's `ended`, chose `finish(false)`.
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
        look: Look,
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
    took: Option<Branch>,
}

impl State {
    /// `github_end` (core/crates/ployz-store/src/build.rs:941-989), for run `run`.
    fn github_end(&mut self, broken: Broken, run: Run, end: End) -> Result<Status, ()> {
        let holds = match end {
            End::Unstarted(_) => self
                .row
                .building_on(run)
                .is_some_and(|github| broken.unstarted_ignores_grant || github.grant.is_none()),
            End::Built | End::Failed(_) | End::Skipped(_) => self.row.building_on(run).is_some(),
        };
        if !holds {
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
    /// (core/crates/ployz-store/src/build.rs:950-952) is gone.
    unstarted_ignores_grant: bool,
    /// `github_check_in` commits for a Deployment no longer in flight, and over a grant: its
    /// guard (core/crates/ployz-store/src/build.rs:870-872) is gone.
    check_in_unguarded: bool,
    /// The start limit cancels the run whatever its finish answered: the `found === "done"`
    /// gate (:214) is gone.
    cancel_ignores_done: bool,
    /// The fix's start limit settles by the grant, with `timed_out`, as the code does.
    fix_start_limit_reads_the_grant: bool,
}

/// Which model a `COVERAGE` row is reached on.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Reach {
    Both,
    /// Only on the code as it is.
    AsIs,
}

/// One `sometimes`: a branch the code handles, read from the state that took it.
struct Coverage {
    name: &'static str,
    reach: Reach,
    reached: fn(&BuildHandoff, &State) -> bool,
}

fn took(state: &State, branch: Branch) -> bool {
    state.took == Some(branch)
}

fn finished(state: &State, by: Finisher, look: Look, step: FinishStep) -> bool {
    took(state, Branch::Finish { by, look, step })
}

fn walk_settled(state: &State, look: Look, end: End) -> bool {
    finished(state, Finisher::Walk, look, FinishStep::Settled(end))
}

fn settled_by(state: &State, by: Finisher, look: Look) -> bool {
    matches!(
        state.took,
        Some(Branch::Finish { by: b, look: l, step: FinishStep::Settled(_) }) if b == by && l == look
    )
}

/// A `look`'s inspect read nothing pushed for a run that pushed and reported Built, because
/// its grant's Machine lost the grant, and its settle of Skipped `why` is next.
fn lost_after_built(state: &State, look: Look, why: Why) -> bool {
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
            Walk::Open(_)
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

/// Each branch the bounds reach, on the code as it is, on the fix, or on both.
/// `current_code_breaks_only_the_finding` and `the_fix_keeps_a_started_run` expect every row
/// their model reaches.
const COVERAGE: &[Coverage] = &[
    // The walk's steps (dashboard/src/modules/config-store/store-deployment.inngest.ts:151-167).
    Coverage {
        name: "github-open finds the build on GitHub and the walk waits",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::Open { open: true }),
    },
    Coverage {
        name: "github-open finds the build no longer on GitHub and the walk ends",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::Open { open: false }),
    },
    Coverage {
        name: "github-wait ends with the run's event",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::WaitEnded { woken: true }),
    },
    Coverage {
        name: "github-wait times out",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::WaitEnded { woken: false }),
    },
    // checkStoreGithubBuild (:184-219).
    Coverage {
        name: "checkStoreGithubBuild's read finds the build no longer building and returns done",
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::DeploymentCancelled),
    },
    Coverage {
        name: "the walk finds a run past its budget",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::PastBudget),
    },
    Coverage {
        name: "the wait saw the run end, so a run with no grant gets finish(false)",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::SeenEnded { grant: false }),
    },
    Coverage {
        name: "the wait saw the run end, or its final report came, so a run that checked in gets finish(false)",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::SeenEnded { grant: true }),
    },
    Coverage {
        name: "runEnded says a run with no grant completed, and it gets finish(false)",
        reach: Reach::Both,
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
        name: "the start limit calls finish(true) for a run its read found with no grant",
        reach: Reach::Both,
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
        name: "runEnded says a run that checked in completed, and it gets finish(false)",
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::Asked { down: true, .. })),
    },
    Coverage {
        name: "the budget cancels the run",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::Cancelled(Canceller::Budget)),
    },
    Coverage {
        name: "the start limit cancels the run its finish ended",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::Cancelled(Canceller::StartLimit)),
    },
    Coverage {
        name: "the walk's cancelStoreGithubBuilds cancels the run",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::Cancelled(Canceller::Stop(Stopper::Walk))),
    },
    Coverage {
        name: "the user's cancelStoreGithubBuilds cancels the run",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::Cancelled(Canceller::Stop(Stopper::User))),
    },
    // github_finish (core/crates/ployz/src/sdk/github_build.rs:454-539).
    Coverage {
        name: "github_finish's re-read finds the build passed on",
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
        reached: |_, state| {
            finished(
                state,
                Finisher::Walk,
                Look::StartLimit,
                FinishStep::Reread { grant: true },
            )
        },
    },
    Coverage {
        name: "the start limit passes on a run that never checked in, Unstarted \"no runner started the build in time\"",
        reach: Reach::Both,
        reached: |_, state| {
            walk_settled(state, Look::StartLimit, End::Unstarted(Why::NoRunnerInTime))
        },
    },
    Coverage {
        name: "an Ended look passes on a run that never checked in, Unstarted \"the run ended before it started the build\"",
        reach: Reach::Both,
        reached: |_, state| walk_settled(state, Look::Ended, End::Unstarted(Why::EndedBeforeStart)),
    },
    Coverage {
        name: "the start limit's Unstarted is refused because the run checked in, and github_finish answers Waiting",
        reach: Reach::Both,
        reached: |_, state| {
            finished(
                state,
                Finisher::Walk,
                Look::StartLimit,
                FinishStep::Refused(End::Unstarted(Why::NoRunnerInTime)),
            ) && state.limit_waited
        },
    },
    Coverage {
        name: "the start limit settles a run that checked in Skipped \"the run ran out of time\"",
        reach: Reach::AsIs,
        reached: |_, state| walk_settled(state, Look::StartLimit, End::Skipped(Why::RanOutOfTime)),
    },
    Coverage {
        name: "the start limit fails a run that checked in, its grant's Machine unreachable",
        reach: Reach::AsIs,
        reached: |_, state| walk_settled(state, Look::StartLimit, End::Failed(Why::Unreachable)),
    },
    Coverage {
        name: "github_finish settles Built from a pushed grant and a Built report",
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Finish {
                    look: Look::Ended,
                    step: FinishStep::Settled(End::Skipped(Why::EndedWithoutPushing)),
                    ..
                })
            )
        },
    },
    Coverage {
        name: "the budget settles Skipped \"the run ran out of time\"",
        reach: Reach::Both,
        reached: |_, state| walk_settled(state, Look::Budget, End::Skipped(Why::RanOutOfTime)),
    },
    Coverage {
        name: "the budget fails the build, its grant's Machine unreachable",
        reach: Reach::Both,
        reached: |_, state| walk_settled(state, Look::Budget, End::Failed(Why::Unreachable)),
    },
    Coverage {
        name: "an Ended look waits for an unreachable grant Machine",
        reach: Reach::Both,
        reached: |_, state| {
            matches!(
                state.took,
                Some(Branch::Finish {
                    look: Look::Ended,
                    step: FinishStep::WaitsForMachine,
                    ..
                })
            )
        },
    },
    Coverage {
        name: "github_finish fails the build on the Machine's refusal",
        reach: Reach::Both,
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
        reach: Reach::Both,
        reached: |_, state| state.limit_waited && settled_by(state, Finisher::Walk, Look::Ended),
    },
    Coverage {
        name: "after the start limit waited for a run that checked in, the budget settles it",
        reach: Reach::Both,
        reached: |_, state| state.limit_waited && settled_by(state, Finisher::Walk, Look::Budget),
    },
    Coverage {
        name: "after the start limit waited for a run that checked in, the report's own github_finish settles it",
        reach: Reach::Both,
        reached: |_, state| state.limit_waited && settled_by(state, Finisher::Report, Look::Ended),
    },
    // Inngest's at-least-once steps (dashboard/src/modules/config-store/store-deployment.inngest.ts:41).
    Coverage {
        name: "a github-check step dies after an effect landed, and Inngest runs it again",
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::CancelEnded { ok: true, .. })),
    },
    Coverage {
        name: "github_cancel's end is refused, another step having ended the build",
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::CancelEnded { ok: false, .. })),
    },
    Coverage {
        name: "a user cancels the queued Deployment",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::UserCancelled),
    },
    // The runner (core/crates/ployz/src/sdk/github_build.rs:187-274, :384-432).
    Coverage {
        name: "the check-in's pre-check passes",
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::Pushed),
    },
    Coverage {
        name: "the final report says Built",
        reach: Reach::Both,
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
        reach: Reach::Both,
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
        reach: Reach::Both,
        reached: |_, state| matches!(state.took, Some(Branch::Reported { ok: false, .. })),
    },
    Coverage {
        name: "the job stops before its final report",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::Quit),
    },
    Coverage {
        name: "the report's event wakes the waiting walk",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::Woke { waiting: true }),
    },
    Coverage {
        name: "the report's event finds no wait",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::Woke { waiting: false }),
    },
    // The grant's Machine.
    Coverage {
        name: "after the grant's Machine lost a grant that received the push, an Ended look reads nothing pushed for a Built report, and its Skipped \"the run ended without pushing an image\" settle is next",
        reach: Reach::Both,
        reached: |_, state| lost_after_built(state, Look::Ended, Why::EndedWithoutPushing),
    },
    Coverage {
        name: "after the grant's Machine lost a grant that received the push, the budget reads nothing pushed for a Built report, and its Skipped \"the run ran out of time\" settle is next",
        reach: Reach::Both,
        reached: |_, state| lost_after_built(state, Look::Budget, Why::RanOutOfTime),
    },
    // GitHub.
    Coverage {
        name: "GitHub ends a cancelled run and its job",
        reach: Reach::Both,
        reached: |_, state| took(state, Branch::RunCompleted { killed: true }),
    },
    Coverage {
        name: "GitHub completes a run whose job exited",
        reach: Reach::Both,
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
}

/// Which way a step goes, where the code or the world decides.
#[derive(Clone, Copy, Debug, Hash, PartialEq, Eq)]
enum Pick {
    Go,
    /// The clock is past the budget (:206).
    PastBudget,
    /// GitHub can't be reached, which `runEnded` reads as running (:173).
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
    /// The fix: the start limit's `github_finish` issues only Unstarted.
    fix: bool,
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
    fn finish(&self, state: &mut State, by: Finisher, look: Look, at: Finish, pick: Pick) -> Next {
        let timed_out = look.timed_out();
        let step = |step| Some(Branch::Finish { by, look, step });
        match at {
            Finish::Read => {
                // core/crates/ployz/src/sdk/github_build.rs:461-472: `github_build` reads the hand-off.
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
                if self.fix
                    && look == Look::StartLimit
                    && !self.broken.fix_start_limit_reads_the_grant
                {
                    return Next::At(Finish::Settle(End::Unstarted(Why::NoRunnerInTime)));
                }
                match github.grant {
                    // core/crates/ployz/src/sdk/github_build.rs:475-483.
                    None => Next::At(Finish::Settle(End::Unstarted(if timed_out {
                        Why::NoRunnerInTime
                    } else {
                        Why::EndedBeforeStart
                    }))),
                    Some(_) => Next::At(Finish::Inspect {
                        ended: github.ended,
                    }),
                }
            }
            Finish::Inspect { ended } => {
                let Pick::Grant(read) = pick else {
                    unreachable!("an inspect is offered only grant reads")
                };
                // What the Machine answers for the push (core/crates/ployz/src/sdk/github_build.rs:491-492).
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
                match read {
                    GrantRead::Refused => Next::At(Finish::Settle(End::Failed(Why::Refused))),
                    GrantRead::Unreachable if !timed_out => {
                        state.took = step(FinishStep::WaitsForMachine);
                        Next::Returned(Finished::Waiting)
                    }
                    GrantRead::Unreachable => {
                        Next::At(Finish::Settle(End::Failed(Why::Unreachable)))
                    }
                    // core/crates/ployz/src/sdk/github_build.rs:504-535.
                    GrantRead::Pushed => Next::At(Finish::Settle(match (read_push, ended) {
                        (true, Some(RunEnd::Built)) => End::Built,
                        (_, Some(RunEnd::Failed)) => End::Failed(Why::StepFailed),
                        _ if timed_out => End::Skipped(Why::RanOutOfTime),
                        _ => End::Skipped(Why::EndedWithoutPushing),
                    })),
                }
            }
            Finish::Settle(end) => {
                // core/crates/ployz/src/sdk/github_build.rs:473 takes the run from the re-read; the model has one.
                let started = state.row.started_on(RUN);
                let limit = by == Finisher::Walk && look == Look::StartLimit;
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
                            // core/crates/ployz/src/sdk/github_build.rs:484-488.
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
                // `github_outstanding` (core/crates/ployz-store/src/build.rs:1005-1019).
                let found = state.row.status == Status::Building && state.row.github.is_some();
                state.took = Some(Branch::Outstanding { by, found });
                found.then_some(Stop::Ending)
            }
            Stop::Ending => {
                // Its end is ignored: another step may have ended it (core/crates/ployz/src/sdk/github_build.rs:643-647).
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
    fn walk(&self, state: &mut State, pick: Pick) {
        match state.walk {
            Walk::Open(check) => {
                // `storeGithubBuildOpen` (:222-224).
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
                // :188-189.
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
                // :191-195. An installation the Organization can use (:196-200).
                if state.deployment == Deployment::Cancelled {
                    state.took = Some(Branch::DeploymentCancelled);
                    return Some(At::Stopping(Stop::Outstanding));
                }
                // :206.
                if pick == Pick::PastBudget {
                    state.took = Some(Branch::PastBudget);
                    return Some(At::BudgetCancel);
                }
                // :211 and :217 read `seen.ended`, and :217 the read's `ended`, before GitHub.
                if ended || (grant && reported) {
                    state.took = Some(Branch::SeenEnded { grant });
                    return finishing(Look::Ended);
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
                finishing(Look::Budget)
            }
            At::AskingGithub { grant } => {
                // `runEnded` (:171-173).
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
                    return finishing(Look::Ended);
                }
                // :212 and :218. Check 0 is the start limit, `check === 0 && target.hasNext`
                // (dashboard/src/modules/config-store/store-deployment.inngest.ts:157): the model's walk has a next Builder.
                if grant || !start_limit {
                    waits(state);
                    return None;
                }
                finishing(Look::StartLimit)
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
                        if look == Look::StartLimit && (!waiting || self.broken.cancel_ignores_done)
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
                // :214.
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
                // `github_check_in` (core/crates/ployz-store/src/build.rs:859-888).
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

    /// The check-in's pre-check: Cloud web's `heldGithubBuild` (:274-275), then
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
                // `github_report` (core/crates/ployz-store/src/build.rs:892-935).
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
                match self.finish(state, Finisher::Report, Look::Ended, at, pick) {
                    Next::At(at) => Job::Finishing(at),
                    // :306-308: what it returned, or its error, is logged.
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
                // `cancel` (core/crates/ployz-store/src/deployment/lifecycle.rs:1459-1480).
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

    /// The run was dispatched and `githubDispatched` committed it (:152-160,
    /// core/crates/ployz-store/src/build.rs:746-753); the walk's first `github-open` is next.
    fn init_states(&self) -> Vec<State> {
        vec![State {
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
            took: None,
        }]
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
        ];
        properties.extend(
            COVERAGE
                .iter()
                .filter(|row| match row.reach {
                    Reach::Both => true,
                    Reach::AsIs => !self.fix,
                })
                .map(|row| Property::sometimes(row.name, row.reached)),
        );
        properties
    }
}

/// The Build Order moves a build on when its Builder does not start it in time, and a started
/// build only when GitHub fails it for infrastructure reasons, one being that the run ran out
/// of time (dashboard/CONTEXT.md:209): the 2-hour budget on GitHub Actions
/// (docs/user/builds/overview.md:56-57). The start limit's settle writes that message for a
/// run that checked in after the start limit ran out. The property reads "in time" as
/// `github_finish`'s Waiting already does (core/crates/ployz/src/sdk/github_build.rs:484-488):
/// a run started in time when its check-in committed before the start limit settled. That is a
/// reading of the rule, not its words.
const STARTED_RUN_KEPT: &str = "the start limit never ends a run whose check-in committed, nor cancels one while it holds the build";
const NO_GRANT_AFTER_CANCEL: &str = "no check-in commits for a Deployment the user cancelled";
const ONE_GRANT_PER_RUN: &str = "no check-in commits over a grant the run already recorded";

/// How the walk calls `finish` for `look`: the code passes `timed_out`, and the fix the look.
fn call(model: &BuildHandoff, look: Look) -> String {
    if model.fix {
        format!("finish(Look::{look:?})")
    } else {
        format!("finish({})", look.timed_out())
    }
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

/// `path` in the code's words: each step, then the Store row, the run, and each actor.
fn story(model: &BuildHandoff, path: &[(State, Option<Action>)]) -> String {
    let mut story = String::new();
    // Stateright pairs each state with the action taken from it; the row lists the action
    // that led to the state instead, so the last row shows the broken state.
    let mut led_by: Option<(State, Action)> = None;
    for (n, (state, action)) in path.iter().enumerate() {
        let what = led_by.map_or_else(
            || "start: the run was dispatched, and the walk's github-open-0 is next".to_owned(),
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
        Walk::Done | Walk::GaveUp => 0,
    };
    let Some(branch) = after.took else {
        return format!("{who}: {action:?}");
    };
    let what = match branch {
        Branch::Open { open: true } => format!(
            "github-open-{check}: heldGithubBuild finds it building (:222-224), and github-wait-{check} starts (dashboard/src/modules/config-store/store-deployment.inngest.ts:156-161)"
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
            "github-check-{check}{}: heldGithubBuild finds the build no longer building, and it returns done (:188-189)",
            if rerun { ", run again" } else { "" }
        ),
        Branch::Looked {
            held: true,
            grant,
            rerun,
        } => format!(
            "github-check-{check}{}: heldGithubBuild reads it building, {} (:188)",
            if rerun { ", run again" } else { "" },
            if grant {
                "checked in"
            } else {
                "with build.grant === null"
            }
        ),
        Branch::DeploymentCancelled => "github-check: the Deployment is no longer queued or running, so cancelStoreGithubBuilds runs (:191-193)".to_owned(),
        Branch::PastBudget => "github-check: the run is past GITHUB_RUN_BUDGET_MS since it checked in (:206)".to_owned(),
        Branch::SeenEnded { grant } => format!(
            "github-check: the Deployment is queued (:191); {} so it calls finish(false) (:{})",
            if grant {
                "seen.ended or build.ended is set,"
            } else {
                "seen.ended is set,"
            },
            if grant { 217 } else { 211 }
        ),
        Branch::StillQueued { grant } => format!(
            "github-check: the Deployment is queued (:191), and the installation is usable (:196-200); runEnded is next (:{})",
            if grant { 217 } else { 211 }
        ),
        Branch::Asked {
            grant,
            ended,
            down,
            start_limit,
        } => {
            let read = if down {
                "GitHub can't be reached, which runEnded reads as running (:171-173)"
            } else if ended {
                "runEnded says the run completed (:171-173)"
            } else {
                "runEnded says the run is still running (:171-173)"
            };
            let then = match (grant, ended, start_limit) {
                (false, true, _) => format!("it calls {} (:211)", call(model, Look::Ended)),
                (false, false, false) => "it is not the start limit, so it returns waiting (:212)".to_owned(),
                (false, false, true) => format!("at the start limit it calls {} (:213)", call(model, Look::StartLimit)),
                (true, true, _) => format!("it calls {} (:217)", call(model, Look::Ended)),
                (true, false, _) => "it returns waiting (:218)".to_owned(),
            };
            format!("github-check: {read}; {then}")
        }
        Branch::Cancelled(Canceller::Budget) => format!("github-check: cancelDispatchedGithubRun cancels the run on GitHub (:207); {} is next (:208)", call(model, Look::Budget)),
        Branch::Cancelled(Canceller::StartLimit) => {
            let waiting = matches!(after.walk, Walk::Open(_));
            format!(
                "github-check: {} cancelDispatchedGithubRun cancels the run on GitHub (:214){}",
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
            let finisher = match (by, look, model.fix) {
                (Finisher::Report, _, _) => "the report handler's github_finish(timed_out=false) (:306)".to_owned(),
                (Finisher::Walk, Look::StartLimit, true) if !model.broken.fix_start_limit_reads_the_grant => "github_finish(Look::StartLimit) (the fix)".to_owned(),
                (Finisher::Walk, Look::StartLimit, true) => "github_finish(Look::StartLimit), settling by the grant (under Broken)".to_owned(),
                (Finisher::Walk, look, _) => format!("github_finish(timed_out={})", look.timed_out()),
            };
            let what = match step {
                FinishStep::PassedOn => "re-reads the build: GitHub passed it on, so it returns Ended(Pending) (core/crates/ployz/src/sdk/github_build.rs:461-468)".to_owned(),
                FinishStep::NotBuilding(found) => format!("re-reads the build: it is {}, so it returns it (core/crates/ployz/src/sdk/github_build.rs:461-472)", status(found)),
                FinishStep::Reread { grant: true } => format!(
                    "re-reads the build and finds the grant (core/crates/ployz/src/sdk/github_build.rs:461, :490); {}",
                    if model.fix && look == Look::StartLimit && by == Finisher::Walk && !model.broken.fix_start_limit_reads_the_grant {
                        "the start limit settles Unstarted whatever it found"
                    } else {
                        "inspect_grant is next (core/crates/ployz/src/sdk/github_build.rs:491)"
                    }
                ),
                FinishStep::Reread { grant: false } => "re-reads the build and finds no grant (core/crates/ployz/src/sdk/github_build.rs:461, :475); it settles Unstarted".to_owned(),
                FinishStep::Inspected(GrantRead::Pushed) => format!(
                    "inspect_grant reads the grant on its Machine: Pushed({}) (core/crates/ployz/src/sdk/github_build.rs:567-594, :612-613)",
                    if before.pushed && !before.lost {
                        "Some"
                    } else {
                        "None"
                    }
                ),
                FinishStep::LostPush => "inspect_grant reads the grant on its Machine, which no longer holds it, so the push reads as none: Pushed(None) (core/crates/ployzd/src/management/build_grant.rs:203-221, core/crates/ployz/src/sdk/github_build.rs:612-613)".to_owned(),
                FinishStep::Inspected(GrantRead::Refused) => "inspect_grant: the Machine refuses the read (core/crates/ployz/src/sdk/github_build.rs:493-495)".to_owned(),
                FinishStep::Inspected(GrantRead::Unreachable) => "inspect_grant: the grant's Machine can't be reached (core/crates/ployz/src/sdk/github_build.rs:576-587)".to_owned(),
                FinishStep::WaitsForMachine => "inspect_grant: the grant's Machine can't be reached, and without timed_out it answers Waiting (core/crates/ployz/src/sdk/github_build.rs:496)".to_owned(),
                FinishStep::Settled(end) => format!(
                    "settle: github_end accepts {end} from the run that holds the build (core/crates/ployz-store/src/build.rs:949-955), and writes it {} (core/crates/ployz-store/src/build.rs:960-987)",
                    status(after.row.status)
                ),
                FinishStep::Refused(end) => format!(
                    "settle: github_end refuses {end} (core/crates/ployz-store/src/build.rs:949-958){}",
                    match end {
                        End::Unstarted(_) if before.row.started_on(RUN) => ": the run checked in, so github_finish answers Waiting (core/crates/ployz/src/sdk/github_build.rs:484-488)",
                        End::Unstarted(_) => ": the run no longer holds the build, so github_finish answers Waiting (core/crates/ployz/src/sdk/github_build.rs:484-488)",
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
            "cancelStoreGithubBuilds: github_cancel's github_outstanding {} (core/crates/ployz/src/sdk/github_build.rs:637, core/crates/ployz-store/src/build.rs:1005-1019)",
            if found {
                "finds the build building on GitHub"
            } else {
                "finds nothing building on GitHub"
            }
        ),
        Branch::CancelEnded { ok, .. } => format!(
            "cancelStoreGithubBuilds: github_end Failed \"cancelled\" {} (core/crates/ployz/src/sdk/github_build.rs:640-647)",
            if ok {
                "fails the build"
            } else {
                "is refused, and the refusal is ignored"
            }
        ),
        Branch::PreCheck { pass: true, .. } => "check-in: heldGithubBuild and github_authorize read it building with no grant, so the pre-check passes (:274-275, core/crates/ployz/src/sdk/github_build.rs:195-204)".to_owned(),
        Branch::PreCheck { pass: false, .. } => "check-in: the pre-check finds it passed on, settled or checked in, and returns conflict (:274-275, core/crates/ployz/src/sdk/github_build.rs:195-204)".to_owned(),
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
            "check-in: the Store's github_check_in finds the run holding the build{}, and sets the grant and checked_in_at (core/crates/ployz-store/src/build.rs:859-888); the runner gets its grant and build secrets (core/crates/ployz/src/sdk/github_build.rs:255-273)",
            if model.broken.check_in_unguarded {
                " and, its guard gone (under Broken), commits"
            } else {
                ", in flight with no grant"
            }
        ),
        Branch::CheckedIn { ok: false, .. } => "check-in: the Store's github_check_in refuses \"no longer wanted\", and the minted grant goes unrecorded (core/crates/ployz-store/src/build.rs:867-872, core/crates/ployz/src/sdk/github_build.rs:253-255)".to_owned(),
        Branch::Pushed => "the runner pushes its image into the grant's Machine".to_owned(),
        Branch::GrantLost => "the Machine no longer holds the grant: it expired after GRANT_LIFETIME, or the daemon restarted (core/crates/ployzd/src/management/build_grant.rs:6-9, :49, :203-221)".to_owned(),
        Branch::Reported { end, ok } => format!(
            "the final Build Steps report, {end:?}: github_report {} (:302, core/crates/ployz-store/src/build.rs:892-935)",
            if ok {
                "records the run's end"
            } else {
                "refuses it, the run no longer holding the build"
            }
        ),
        Branch::Quit => "the job stops before its final report".to_owned(),
        Branch::Woke { waiting } => format!(
            "the report handler sends the run's event (:309){}",
            if waiting {
                ", which the walk's wait receives"
            } else {
                ", which no wait receives"
            }
        ),
        Branch::UserCancelled => "cancel turns the queued Deployment Cancelled (core/crates/ployz-store/src/deployment/lifecycle.rs:1466-1469); afterWrite's cancelStoreGithubBuilds is next (dashboard/src/modules/config-store/config-store.server.ts:306-310)".to_owned(),
        Branch::RunCompleted { killed } => format!(
            "the run completes{}; its workflow_run webhook sends the run's event (dashboard/src/routes/api/github/-webhook.handler.ts:78-83)",
            if killed { " and its job ends" } else { "" }
        ),
    };
    format!("{who}: {what}")
}

#[cfg(test)]
mod tests {
    use std::{
        collections::BTreeSet,
        sync::{Arc, Mutex},
    };

    use stateright::{Checker, Expectation, HasDiscoveries};

    use super::*;

    type Path = Vec<(State, Option<Action>)>;

    /// The code as it is: one run, one check-in request, one crash.
    fn as_is() -> BuildHandoff {
        BuildHandoff {
            fix: false,
            requests: 1,
            crashes: 1,
            broken: Broken::default(),
        }
    }

    /// The fix: the start limit's `github_finish` issues only Unstarted.
    fn fixed() -> BuildHandoff {
        BuildHandoff {
            fix: true,
            ..as_is()
        }
    }

    fn as_is_without(broken: Broken) -> BuildHandoff {
        BuildHandoff { broken, ..as_is() }
    }

    fn fixed_without(broken: Broken) -> BuildHandoff {
        BuildHandoff { broken, ..fixed() }
    }

    fn check(model: BuildHandoff) -> impl Checker<BuildHandoff> {
        model.checker().threads(4).spawn_bfs().join()
    }

    /// The first path in BFS order whose last state breaks `property` under `model`, so no
    /// shorter path breaks it, printed as a story under the test's name. For a `sometimes`,
    /// such as a `COVERAGE` row's name, it is the first path that reaches it. It panics when
    /// no path does. The checker's own discovery is not that path: `check_block` stores every
    /// violation it visits, each overwriting the last, so a visitor keeps the first. One
    /// thread keeps the path the same from run to run.
    fn breaks(model: BuildHandoff, property: &'static str) -> Path {
        first_break(model, property, |_| true, true).expect("a path breaks the property")
    }

    /// As `breaks`, the first such path that also satisfies `and`. It explores the whole
    /// model, since the first discovery may not pass `and`. The checker keeps one path to
    /// each state, so `and` sees only the first path to the state it ends in.
    fn breaks_where(
        model: BuildHandoff,
        property: &'static str,
        and: impl Fn(&Path) -> bool + Send + Sync + 'static,
    ) -> Path {
        first_break(model, property, and, false).expect("a path breaks the property")
    }

    /// As `breaks_where`, or `None` when no path breaks `property` and satisfies `and`.
    fn first_break(
        model: BuildHandoff,
        property: &'static str,
        and: impl Fn(&Path) -> bool + Send + Sync + 'static,
        stop_at_discovery: bool,
    ) -> Option<Path> {
        let named = model
            .properties()
            .into_iter()
            .find(|candidate| candidate.name == property)
            .expect("a model property");
        let reached = matches!(named.expectation, Expectation::Sometimes);
        let holds = named.condition;
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
        // The default, `HasDiscoveries::All`, stops once every property has a discovery.
        // An empty `AnyOf` never matches, so the checker visits every state.
        let checker = checker.finish_when(if stop_at_discovery {
            HasDiscoveries::AllOf([property].into())
        } else {
            HasDiscoveries::AnyOf(BTreeSet::new())
        });
        checker.spawn_bfs().join();
        let path = first.lock().expect("the first break").take()?;
        let test = std::thread::current();
        println!(
            "[{}] {property} {} under {model:?}:\n{}",
            test.name().unwrap_or("?"),
            if reached { "is reached" } else { "fails" },
            story(&model, &path)
        );
        Some(path)
    }

    fn last(path: &Path) -> State {
        path.last().expect("a path has a state").0
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

    /// Steps `actions` from the start under `model`, each one offered where it is taken, and
    /// prints the path as a story under the test's name.
    fn follow(model: BuildHandoff, actions: &[Action]) -> Path {
        let mut state = model
            .init_states()
            .into_iter()
            .next()
            .expect("a start state");
        let mut path = Vec::new();
        for &action in actions {
            let mut offered = Vec::new();
            model.actions(&state, &mut offered);
            assert!(offered.contains(&action), "{action:?} is offered");
            let next = model.next_state(&state, action).expect("a next state");
            path.push((state, Some(action)));
            state = next;
        }
        path.push((state, None));
        let test = std::thread::current();
        println!(
            "[{}] a schedule under {model:?}:\n{}",
            test.name().unwrap_or("?"),
            story(&model, &path)
        );
        path
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
    /// says the run is running, so it calls `finish(true)`. `github_finish` re-reads and finds
    /// the grant, `inspect_grant` reads nothing pushed, and the end is Skipped "the run ran out
    /// of time", which `github_end` accepts. Cloud then cancels the run.
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

    /// The finding, on the code as it is. The replay shows the schedule step by step, through
    /// the cancel. The checker's first path to a broken state then shows the same race: the
    /// start limit's read found no grant, a check-in committed after it, and the start limit's
    /// own settle ended the run.
    #[test]
    fn current_code_passes_on_a_run_that_checked_in() {
        let path = follow(as_is(), &THE_FINDING);
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
                look: Look::StartLimit,
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

        let found = breaks(as_is(), STARTED_RUN_KEPT);
        assert!(before(
            &found,
            the_limit_read_no_grant,
            the_check_in_committed
        ));
        assert!(matches!(
            last(&found).took,
            Some(Branch::Finish {
                by: Finisher::Walk,
                look: Look::StartLimit,
                step: FinishStep::Settled(End::Skipped(Why::RanOutOfTime) | End::Failed(_)),
            })
        ));
    }

    /// The same race, with the grant's Machine unreachable: `timed_out` fails the build
    /// (core/crates/ployz/src/sdk/github_build.rs:497-501), and Cloud cancels the run.
    #[test]
    fn current_code_fails_a_run_that_checked_in_when_its_machine_is_unreachable() {
        let found = breaks_where(as_is(), STARTED_RUN_KEPT, |path| {
            last(path).took
                == Some(Branch::Finish {
                    by: Finisher::Walk,
                    look: Look::StartLimit,
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

    /// The code as it is breaks only `STARTED_RUN_KEPT`, and reaches every row.
    #[test]
    fn current_code_breaks_only_the_finding() {
        let checker = check(as_is());
        println!("as is: {} states", checker.unique_state_count());
        assert!(checker.discovery(STARTED_RUN_KEPT).is_some());
        for property in [NO_GRANT_AFTER_CANCEL, ONE_GRANT_PER_RUN] {
            assert!(checker.discovery(property).is_none(), "{property} breaks");
        }
        for row in COVERAGE {
            assert!(
                checker.discovery(row.name).is_some(),
                "{} is never reached",
                row.name
            );
        }
    }

    /// The fix holds every `always` property, and reaches every row it can.
    #[test]
    fn the_fix_keeps_a_started_run() {
        let checker = check(fixed());
        println!("fixed: {} states", checker.unique_state_count());
        checker.assert_properties();
    }

    /// The finding's schedule under the fix: the start limit's Unstarted is refused, the walk
    /// waits, and the run stays checked in.
    #[test]
    fn the_fix_waits_for_the_finding_s_run() {
        let schedule = THE_FINDING
            .get(..THE_FINDING.len() - 3)
            .expect("the finding up to its re-read")
            .iter()
            .copied()
            .chain([walk(GO)])
            .collect::<Vec<_>>();
        let path = follow(fixed(), &schedule);
        let after = last(&path);
        assert_eq!(
            after.took,
            Some(Branch::Finish {
                by: Finisher::Walk,
                look: Look::StartLimit,
                step: FinishStep::Refused(End::Unstarted(Why::NoRunnerInTime)),
            })
        );
        assert!(after.limit_waited && !after.limit_took);
        assert_eq!(after.walk, Walk::Open(1));
        assert!(!after.cancelled);
        assert!(after.row.started_on(RUN));
    }

    /// Under the fix, a run that checked in inside the start limit's window is settled by a
    /// later look: an Ended look once it reported, or the budget.
    #[test]
    fn the_fix_ends_the_started_run_through_a_later_look() {
        for (row, look) in [
            (
                "after the start limit waited for a run that checked in, an Ended look settles it",
                Look::Ended,
            ),
            (
                "after the start limit waited for a run that checked in, the budget settles it",
                Look::Budget,
            ),
        ] {
            let path = breaks(fixed(), row);
            assert!(settled_by(&last(&path), Finisher::Walk, look));
            assert!(before(
                &path,
                |branch| branch
                    == Branch::Finish {
                        by: Finisher::Walk,
                        look: Look::StartLimit,
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

    /// The fix's start limit on each state its re-read can find. It settles only a run that
    /// never checked in; on the rest it changes nothing.
    #[test]
    fn the_fix_s_start_limit_on_each_edge_state() {
        let model = fixed();
        let start = model
            .init_states()
            .into_iter()
            .next()
            .expect("a start state");
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
                match model.finish(&mut state, Finisher::Walk, Look::StartLimit, at, GO) {
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
        // that run's (core/crates/ployz-store/src/build.rs:715-719, :950-952).
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

    /// Without the Unstarted grant guard, the fix's start limit passes on a run that checked
    /// in.
    #[test]
    fn the_fix_needs_the_unstarted_guard() {
        let path = breaks(
            fixed_without(Broken {
                unstarted_ignores_grant: true,
                ..Broken::default()
            }),
            STARTED_RUN_KEPT,
        );
        assert_eq!(
            last(&path).took,
            Some(Branch::Finish {
                by: Finisher::Walk,
                look: Look::StartLimit,
                step: FinishStep::Settled(End::Unstarted(Why::NoRunnerInTime)),
            })
        );
    }

    /// The code needs the guard too: a check-in that commits after `github_finish`'s re-read
    /// found no grant is what its Unstarted conflicts with (core/crates/ployz/src/sdk/github_build.rs:484-488).
    #[test]
    fn current_code_needs_the_unstarted_guard() {
        let path = breaks_where(
            as_is_without(Broken {
                unstarted_ignores_grant: true,
                ..Broken::default()
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
            fixed_without(Broken {
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
            ..fixed()
        };
        let checker = check(two);
        println!("two requests: {} states", checker.unique_state_count());
        for property in [STARTED_RUN_KEPT, NO_GRANT_AFTER_CANCEL, ONE_GRANT_PER_RUN] {
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

    /// Without the done-gate, the fix's start limit cancels a run that checked in while it
    /// still holds the build.
    #[test]
    fn the_fix_needs_the_done_gate() {
        let path = breaks(
            fixed_without(Broken {
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

    /// Without the fix's own guard, its start limit settles by the grant as the code does, and
    /// the finding comes back.
    #[test]
    fn the_fix_needs_its_start_limit_look() {
        let path = breaks(
            fixed_without(Broken {
                fix_start_limit_reads_the_grant: true,
                ..Broken::default()
            }),
            STARTED_RUN_KEPT,
        );
        assert!(before(
            &path,
            the_limit_read_no_grant,
            the_check_in_committed
        ));
    }
}
