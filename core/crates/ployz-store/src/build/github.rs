//! Builds handed to a GitHub Actions run, from its dispatch through its check-in and reports to its end.

use ployz_core::authority::{AutoDeployRule, BuiltImageDigest, CommitAuthority, Signed};
use ployz_core::{MachineId, RpcError, RpcErrorCode, ServiceName};
use ployz_decide::github_build::{
    CANCELLED, EndKind, Hold, Target, github_check_in_holds, github_end_holds,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::deployment::{self, Stored, clipped};
use crate::error;
use crate::id::{CommitSha, DeploymentId, OrganizationId, RepositoryName};
use crate::storage::{Tx, json_text, name_of};

use super::proof::ImageToken;
use super::sources::unrouted;
use super::{BuildRow, BuildStatus, By, Stage, append, clock, rows};

/// What a GitHub build id that names no build is refused with.
fn no_such_build() -> RpcError {
    error::not_found("No GitHub build has this id", json!({}))
}

/// A Git build as its GitHub workflow names it: `DEPLOYMENT.SERVICE`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
pub struct GithubBuildId {
    pub deployment: DeploymentId,
    /// Its Private DNS name, which a rename keeps.
    pub service: ServiceName,
}

impl GithubBuildId {
    /// # Errors
    /// Returns `not_found` for anything else, as no build has that ID.
    pub fn parse(value: &str) -> Result<Self, RpcError> {
        let (deployment, service) = value.split_once('.').ok_or_else(no_such_build)?;
        Ok(Self {
            deployment: DeploymentId::parse(deployment).map_err(|_| no_such_build())?,
            service: ServiceName::parse(service).map_err(|_| no_such_build())?,
        })
    }
}

impl std::fmt::Display for GithubBuildId {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}.{}", self.deployment, self.service)
    }
}

impl TryFrom<String> for GithubBuildId {
    type Error = RpcError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<GithubBuildId> for String {
    fn from(value: GithubBuildId) -> Self {
        value.to_string()
    }
}

/// The GitHub Actions run Cloud dispatched for a build.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct GithubRun {
    #[ts(type = "number")]
    pub run_id: u64,
    pub run_url: String,
    /// The workflow and branch its OIDC token must name.
    pub workflow_ref: String,
    /// The repository, as `owner/name` when dispatched.
    pub repository: RepositoryName,
    #[ts(type = "number")]
    pub installation_id: u64,
    /// Whose run it is: `true` for a push's own run, which Cloud follows and never
    /// cancels, `false` for one Cloud dispatched. Absent on a run recorded before this was
    /// kept, which Cloud can't tell apart and so never cancels.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub push: Option<bool>,
    /// The worker attempt that recorded it, as its runner. Absent on a run recorded
    /// before attempts were kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub attempt: Option<String>,
}

impl GithubRun {
    /// Whether Cloud may cancel it: only a run Cloud itself dispatched.
    #[must_use]
    pub fn cancellable(&self) -> bool {
        self.push == Some(false)
    }
}

/// A GitHub run Cloud owes a cancel: the Store ended its build for a stop, and the run
/// may still be running until GitHub confirms the cancel.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct GithubOwed {
    /// The build, as `deployment.service`.
    pub build: String,
    pub run: GithubRun,
}

/// What recording the run GitHub answered with did.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum GithubRecorded {
    /// The run holds the build.
    Held,
    /// The build no longer takes it: the Deployment stopped, another run holds it, or
    /// another attempt took the Deployment. The run Cloud dispatched is owed a cancel,
    /// committed with the refusal, until Cloud confirms it.
    Owed,
    /// The build no longer takes it, and Cloud owes the run nothing: a push's own run,
    /// one recorded before runs were marked, or one whose cancel GitHub already confirmed.
    Refused,
}

/// A build ended by the worker attempt that owns its Deployment: passed on before any
/// run took it, or settled for the run that holds it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct GithubSettled {
    pub status: BuildStatus,
    /// Whether the run it names is owed a cancel, not yet confirmed.
    pub owed: bool,
}

/// What reserving a build before asking GitHub did.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum GithubAsked {
    /// Reserved: Cloud may ask GitHub for its run.
    Reserved,
    /// Pinned before reservations, so a worker may have asked GitHub for it with no
    /// record: it failed, with why, and GitHub is asked for nothing.
    Failed,
}

/// The Build Grant a GitHub run pushes its image with.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct GithubGrant {
    /// Its handle; not secret.
    pub id: String,
    /// The Machine that minted it and receives the image.
    pub machine: MachineId,
    /// The build-input fingerprint the run builds against.
    pub fingerprint: String,
}

/// How far a GitHub run's hold on a build got. Stored as the `github` column's JSON in
/// the shape it always had, [`GithubRecord`]: which fields are set names the step, so a
/// row written before this type reads the same, and a step without the one before it
/// doesn't decode.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "GithubRecord", into = "GithubRecord")]
pub(super) enum GithubHold {
    /// It has not checked in.
    Dispatched {
        run: GithubRun,
    },
    CheckedIn {
        run: GithubRun,
        started: Started,
    },
    /// Its final report came.
    Reported {
        run: GithubRun,
        started: Started,
        end: RunEnd,
    },
}

/// A GitHub run's check-in, and what it reported since.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct Started {
    grant: GithubGrant,
    /// When it checked in, in Unix seconds.
    at: u64,
    /// How many `ployz build --events` lines it took.
    received: u64,
}

impl GithubHold {
    const fn hold(&self) -> Hold {
        match self {
            Self::Dispatched { .. } => Hold::Dispatched,
            Self::CheckedIn { .. } => Hold::CheckedIn,
            Self::Reported { .. } => Hold::Reported,
        }
    }

    const fn run(&self) -> &GithubRun {
        match self {
            Self::Dispatched { run } | Self::CheckedIn { run, .. } | Self::Reported { run, .. } => {
                run
            }
        }
    }

    const fn started(&self) -> Option<&Started> {
        match self {
            Self::Dispatched { .. } => None,
            Self::CheckedIn { started, .. } | Self::Reported { started, .. } => Some(started),
        }
    }

    pub(super) const fn end(&self) -> Option<&RunEnd> {
        match self {
            Self::Dispatched { .. } | Self::CheckedIn { .. } => None,
            Self::Reported { end, .. } => Some(end),
        }
    }
}

/// The `github` column's JSON.
#[derive(Serialize, Deserialize)]
struct GithubRecord {
    run: GithubRun,
    grant: Option<GithubGrant>,
    checked_in_at: Option<u64>,
    received: u64,
    ended: Option<RunEnd>,
}

impl TryFrom<GithubRecord> for GithubHold {
    type Error = &'static str;

    fn try_from(record: GithubRecord) -> Result<Self, Self::Error> {
        let GithubRecord {
            run,
            grant,
            checked_in_at,
            received,
            ended,
        } = record;
        match (grant, checked_in_at, ended) {
            (None, None, None) if received == 0 => Ok(Self::Dispatched { run }),
            (Some(grant), Some(at), ended) => {
                let started = Started {
                    grant,
                    at,
                    received,
                };
                Ok(match ended {
                    None => Self::CheckedIn { run, started },
                    Some(end) => Self::Reported { run, started, end },
                })
            }
            _ => Err("a GitHub run's step without the check-in before it"),
        }
    }
}

impl From<GithubHold> for GithubRecord {
    fn from(hold: GithubHold) -> Self {
        let (run, started, ended) = match hold {
            GithubHold::Dispatched { run } => (run, None, None),
            GithubHold::CheckedIn { run, started } => (run, Some(started), None),
            GithubHold::Reported { run, started, end } => (run, Some(started), Some(end)),
        };
        Self {
            run,
            received: started.as_ref().map_or(0, |started| started.received),
            checked_in_at: started.as_ref().map(|started| started.at),
            grant: started.map(|started| started.grant),
            ended,
        }
    }
}

/// How a GitHub run ended, as its final report says.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "end", rename_all = "snake_case")]
pub enum RunEnd {
    /// It built the image for these platforms. A build under an automatic deploy rule
    /// also reports its image token, which Cloud relays to each Server with the image.
    Built {
        platforms: Vec<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        image_token: Option<String>,
    },
    /// Its build failed.
    Failed,
}

/// A Git build handed to GitHub, as Cloud follows it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct GithubBuild {
    pub id: GithubBuildId,
    /// The Organization whose Servers receive its image.
    pub organization: OrganizationId,
    pub status: BuildStatus,
    pub run: GithubRun,
    /// Set once the run checked in.
    pub grant: Option<GithubGrant>,
    /// When it checked in, in Unix seconds.
    #[ts(type = "number | null")]
    pub checked_in_at: Option<u64>,
    /// How it ended, once its final report came.
    pub ended: Option<RunEnd>,
}

/// Where a build handed to GitHub is.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum GithubHandOff {
    /// GitHub holds it, or held it until it ended.
    Held(Box<GithubBuild>),
    /// GitHub passed it to the next Builder, and its run holds it no more.
    PassedOn { why: String },
}

/// The claims of a GitHub Actions OIDC token Cloud verified, as GitHub sends them.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct GithubClaims {
    pub repository_id: String,
    pub job_workflow_ref: String,
    pub run_id: String,
    pub event_name: String,
}

/// One batch of a run's log lines, starting at line `from`, and once its build
/// ended, how.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GithubReport {
    pub from: u64,
    pub lines: Vec<String>,
    pub ended: Option<RunEnd>,
}

/// How a GitHub build ended.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "end", rename_all = "snake_case")]
pub enum GithubEnd {
    /// Its image, with the receipt Cloud wrote from what the Machine received.
    /// Without a run: an earlier build's image served it.
    Built {
        #[ts(type = "unknown")]
        receipt: serde_json::Value,
    },
    /// A build step failed. Users read the message.
    Failed { message: String },
    /// GitHub couldn't take it, or failed it for its own reasons: the next Builder
    /// takes it. Users read the message.
    Skipped { message: String },
    /// As `Skipped`, only while its run hasn't checked in: a run that started keeps it.
    Unstarted { message: String },
}

fn github_row(tx: &mut dyn Tx, stored: &Stored, id: &GithubBuildId) -> Result<BuildRow, RpcError> {
    rows(tx, &stored.summary.id)?
        .into_iter()
        .find(|row| row.service == id.service)
        .ok_or_else(no_such_build)
}

fn no_longer_wanted() -> RpcError {
    error::conflict(
        "This build already checked in or is no longer wanted",
        json!({}),
    )
}

fn save_github(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    status: impl Into<BuildStatus>,
    message: Option<&str>,
    log: &str,
    github: Option<&GithubHold>,
) -> Result<(), RpcError> {
    let status = status.into();
    let github = github.map(json_text);
    tx.execute(
        "UPDATE config_build SET status = ?3, message = ?4, log = ?5, github = ?6 \
         WHERE deployment_id = ?1 AND service = ?2",
        &[
            id.deployment.as_str().into(),
            id.service.as_str().into(),
            name_of(status).as_str().into(),
            message.into(),
            log.into(),
            github.as_deref().into(),
        ],
    )?;
    clock(tx, &id.deployment, &id.service, status)
}

impl BuildRow {
    fn github(&self, id: &GithubBuildId) -> Option<GithubBuild> {
        let github = self.stage.github()?;
        let started = github.started();
        Some(GithubBuild {
            id: id.clone(),
            organization: self.organization.clone(),
            status: self.stage.status().into(),
            run: github.run().clone(),
            grant: started.map(|started| started.grant.clone()),
            checked_in_at: started.map(|started| started.at),
            ended: github.end().cloned(),
        })
    }

    /// The run it holds while it builds on GitHub, if `run_id` is that run.
    fn building_on(&self, run_id: u64) -> Option<&GithubHold> {
        match &self.stage {
            Stage::Building(By::Github(github)) if github.run().run_id == run_id => Some(github),
            Stage::Waiting | Stage::Building(_) | Stage::Ended { .. } => None,
        }
    }
}

/// Hand a pinned, unstarted build to GitHub run `run`. Handing it the same run again
/// changes nothing. Only while the Deployment still takes GitHub runs (queued or
/// running, not cancelling or ended), the build waits for its first Builder, and the
/// run's attempt holds the Deployment (taking it from an older one). Anything else is
/// [`GithubRecorded::Refused`], committed with a cancel owed for a run Cloud
/// dispatched, so a reply that reaches the Store after a stop is still cancelled.
pub(crate) fn github_dispatched(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    run: &GithubRun,
) -> Result<GithubRecorded, RpcError> {
    // A run of a Deployment, Environment or build the Store no longer has can't hold
    // anything, but it may still run: it is owed its cancel all the same.
    let found = deployment::locked(tx, &id.deployment)
        .and_then(|stored| github_row(tx, &stored, id).map(|row| (stored, row)));
    let (stored, row) = match found {
        Ok(found) => found,
        Err(error) if error.code == RpcErrorCode::NotFound => {
            if !run.cancellable() {
                return Ok(GithubRecorded::Refused);
            }
            owe(tx, id, run)?;
            return Ok(GithubRecorded::Owed);
        }
        Err(error) => return Err(error),
    };
    if row.building_on(run.run_id).is_some() {
        return Ok(GithubRecorded::Held);
    }
    let reserved_by_another = row
        .attempt
        .as_ref()
        .is_some_and(|attempt| run.attempt.as_ref() != Some(attempt));
    let wanted = takes_github_runs(&stored)
        && matches!(row.stage, Stage::Waiting)
        && row.github_skipped.is_none()
        && !reserved_by_another;
    // A run with no attempt names no owner, so it never takes a build.
    let owned = match &run.attempt {
        Some(attempt) => wanted && owns(tx, &stored, attempt)?,
        None => false,
    };
    if !owned {
        let owed = run.cancellable();
        if owed {
            owe(tx, id, run)?;
        }
        return Ok(if owed {
            GithubRecorded::Owed
        } else {
            GithubRecorded::Refused
        });
    }
    if let Some(attempt) = &run.attempt {
        save_attempt(tx, id, attempt)?;
    }
    let mut log = row.log;
    append(
        &mut log,
        &format!("Building on GitHub Actions: {}\n", run.run_url),
    );
    let github = GithubHold::Dispatched { run: run.clone() };
    save_github(tx, id, BuildStatus::Building, None, &log, Some(&github))?;
    Ok(GithubRecorded::Held)
}

/// Whether Deployment `stored` takes GitHub runs: queued or running. A cancelling one,
/// or one that ended or shows unknown, takes none.
fn takes_github_runs(stored: &Stored) -> bool {
    matches!(
        stored.summary.status,
        deployment::DeploymentStatus::Queued | deployment::DeploymentStatus::Running
    )
}

/// The worker attempt that owns Deployment `deployment`'s GitHub runs: the one with the
/// highest number of those that began, if any did.
fn github_owner(tx: &mut dyn Tx, deployment: &DeploymentId) -> Result<Option<String>, RpcError> {
    let rows = tx.query(
        "SELECT runner FROM config_github_attempt WHERE deployment_id = ?1 \
         ORDER BY number DESC LIMIT 1",
        &[deployment.as_str().into()],
    )?;
    rows.first()
        .map(|row| row.text(0).map(str::to_owned))
        .transpose()
}

/// Whether worker attempt `attempt` owns Deployment `stored`'s GitHub runs: it began
/// last, and no other runner claimed the Deployment.
pub(super) fn owns(tx: &mut dyn Tx, stored: &Stored, attempt: &str) -> Result<bool, RpcError> {
    let claimed_by_another = stored
        .summary
        .runner
        .as_ref()
        .is_some_and(|runner| runner.as_str() != attempt);
    Ok(!claimed_by_another && github_owner(tx, &stored.summary.id)?.as_deref() == Some(attempt))
}

pub(super) fn newer_attempt(deployment: &DeploymentId) -> RpcError {
    error::conflict(
        "A newer attempt runs this Deployment",
        json!({ "deployment": deployment, "newer": true }),
    )
}

/// The attempt migration `0060_github_stop` gave each build a GitHub run held when the
/// Store upgraded. It is no runner ID, so no worker names it.
const UPGRADED_ATTEMPT: &str = "(held at upgrade)";

/// Why a build an older attempt held leaves GitHub once a newer attempt takes over.
const TAKEN_OVER: &str = "A newer run of this Deployment took it over";

/// Worker attempt `attempt` begins running Deployment `deployment`, before it prepares
/// any build: the Store numbers it after every attempt that began before, and it owns
/// the Deployment's GitHub runs from then on. Beginning again answers the same number.
/// In the same write it takes the Deployment over from the attempts it outranks: each
/// of their reservations is passed on, each build their runs hold is passed on to the
/// next Builder, and each run Cloud dispatched is owed a cancel. An attempt that a later
/// one outranks, or a Deployment another runner claimed, is `newer`.
pub(crate) fn github_begin(
    tx: &mut dyn Tx,
    deployment: &DeploymentId,
    attempt: &str,
) -> Result<u64, RpcError> {
    let stored = deployment::locked(tx, deployment)?;
    if stored
        .summary
        .runner
        .as_ref()
        .is_some_and(|runner| runner.as_str() != attempt)
    {
        return Err(newer_attempt(deployment));
    }
    let attempts = tx.query(
        "SELECT runner, number FROM config_github_attempt WHERE deployment_id = ?1 \
         ORDER BY number DESC",
        &[deployment.as_str().into()],
    )?;
    let mut began = Vec::with_capacity(attempts.len());
    for row in &attempts {
        began.push((row.text(0)?.to_owned(), row.int(1)?));
    }
    // A Deployment whose GitHub run held a build when the Store upgraded has the
    // attempt the migration gave it, which names no runner: the first attempt to begin
    // continues it, keeping its builds and its number, so a worker that upgraded while
    // its build ran keeps that build.
    if let [(owner, number)] = began.as_slice()
        && owner == UPGRADED_ATTEMPT
    {
        tx.execute(
            "UPDATE config_github_attempt SET runner = ?2 WHERE deployment_id = ?1 AND runner = ?3",
            &[
                deployment.as_str().into(),
                attempt.into(),
                UPGRADED_ATTEMPT.into(),
            ],
        )?;
        tx.execute(
            "UPDATE config_build SET attempt = ?2 WHERE deployment_id = ?1 AND attempt = ?3",
            &[
                deployment.as_str().into(),
                attempt.into(),
                UPGRADED_ATTEMPT.into(),
            ],
        )?;
        return u64::try_from(*number).map_err(|_| error::corrupt("attempt number"));
    }
    if let Some((_, number)) = began.iter().find(|(runner, _)| runner == attempt) {
        return match began.first() {
            Some((owner, _)) if owner == attempt => {
                u64::try_from(*number).map_err(|_| error::corrupt("attempt number"))
            }
            _ => Err(newer_attempt(deployment)),
        };
    }
    let number = began.first().map_or(1, |(_, number)| number + 1);
    // Every GitHub build is an earlier attempt's: this one has written nothing yet.
    for build in rows(tx, deployment)? {
        let id = GithubBuildId {
            deployment: deployment.clone(),
            service: build.service.clone(),
        };
        match &build.stage {
            Stage::Building(By::Github(hold)) => {
                let run = hold.run().clone();
                let end = GithubEnd::Skipped {
                    message: TAKEN_OVER.to_owned(),
                };
                github_end(tx, &id, Some(run.run_id), &end)?;
                if run.cancellable() {
                    owe(tx, &id, &run)?;
                }
            }
            Stage::Waiting if build.message.as_deref() == Some(ASKING_GITHUB) => {
                let end = GithubEnd::Skipped {
                    message: STOPPED_ASKING.to_owned(),
                };
                github_end(tx, &id, None, &end)?;
            }
            Stage::Waiting | Stage::Building(By::Servers { .. }) | Stage::Ended { .. } => {}
        }
    }
    tx.execute(
        "INSERT INTO config_github_attempt (deployment_id, runner, number) VALUES (?1, ?2, ?3)",
        &[deployment.as_str().into(), attempt.into(), number.into()],
    )?;
    u64::try_from(number).map_err(|_| error::corrupt("attempt number"))
}

/// The message a waiting build shows once Cloud reserved it to ask GitHub for a run.
const ASKING_GITHUB: &str = "Asking GitHub for a run";
/// Why a build pinned before reservations fails rather than go to GitHub.
pub const UNRESERVED: &str =
    "Cloud restarted while it may have been starting this build on GitHub. Push again to build it.";
/// Why a reserved build GitHub never answered for leaves GitHub when its attempt stops.
const STOPPED_ASKING: &str = "Cloud stopped before GitHub answered";

/// Reserve a pinned, unstarted build for worker attempt `attempt` before Cloud asks
/// GitHub for its run, so GitHub is asked once: a build already reserved, or no longer
/// waiting, is `conflict`, and Cloud asks for nothing. The reservation is the build's
/// message, which the run's hand-off or a pass-on replaces, and its attempt, which a stop
/// of another attempt leaves alone. A build pinned before reservations may have been
/// asked for already with no record of it, so it fails with why instead.
pub(crate) fn github_ask(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    attempt: &str,
) -> Result<GithubAsked, RpcError> {
    let stored = deployment::locked(tx, &id.deployment)?;
    let row = github_row(tx, &stored, id)?;
    // A stop ended what Cloud had asked for, so nothing more is asked once it started.
    if !takes_github_runs(&stored)
        || !matches!(row.stage, Stage::Waiting)
        || row.github_skipped.is_some()
        || row.message.is_some()
    {
        return Err(error::conflict(
            "This build was already asked for or is no longer waiting",
            json!({}),
        ));
    }
    // Only the attempt that owns the Deployment asks for its builds.
    if !owns(tx, &stored, attempt)? {
        return Err(newer_attempt(&stored.summary.id));
    }
    let mut log = row.log;
    if !row.reservable {
        append(&mut log, &format!("{UNRESERVED}\n"));
        save_github(tx, id, BuildStatus::Failed, Some(UNRESERVED), &log, None)?;
        return Ok(GithubAsked::Failed);
    }
    append(&mut log, &format!("{ASKING_GITHUB}\n"));
    save_github(
        tx,
        id,
        BuildStatus::Pending,
        Some(ASKING_GITHUB),
        &log,
        None,
    )?;
    save_attempt(tx, id, attempt)?;
    Ok(GithubAsked::Reserved)
}

/// Whether a worker attempt other than `runner` owns Deployment `id`'s GitHub runs.
pub(crate) fn github_outranked(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    runner: &crate::id::RunnerId,
) -> Result<bool, RpcError> {
    // The attempt an upgrade gave a Deployment names no runner, so it outranks none.
    Ok(github_owner(tx, id)?
        .is_some_and(|owner| owner != runner.as_str() && owner != UPGRADED_ATTEMPT))
}

fn save_attempt(tx: &mut dyn Tx, id: &GithubBuildId, attempt: &str) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_build SET attempt = ?3 WHERE deployment_id = ?1 AND service = ?2",
        &[
            id.deployment.as_str().into(),
            id.service.as_str().into(),
            attempt.into(),
        ],
    )
    .map(drop)
}

/// Owe run `run` of build `id` a cancel, until GitHub confirms it. Owing a run already
/// owed changes nothing. A new debt takes the current sweep turn, so it waits behind
/// debts from earlier turns and beside the ones the last sweep retried.
fn owe(tx: &mut dyn Tx, id: &GithubBuildId, run: &GithubRun) -> Result<(), RpcError> {
    let run_id = i64::try_from(run.run_id).map_err(|_| error::corrupt("GitHub run id"))?;
    let stored = json_text(run);
    tx.execute(
        "INSERT INTO config_github_cancel (run_id, deployment_id, service, run, turn) \
         SELECT ?1, ?2, ?3, ?4, COALESCE(MAX(turn), 0) FROM config_github_cancel WHERE true \
         ON CONFLICT (run_id) DO NOTHING",
        &[
            run_id.into(),
            id.deployment.as_str().into(),
            id.service.as_str().into(),
            stored.as_str().into(),
        ],
    )
    .map(drop)
}

/// Every run Deployment `deployment` still owes a cancel.
fn owed_by(tx: &mut dyn Tx, deployment: &DeploymentId) -> Result<Vec<GithubOwed>, RpcError> {
    let rows = tx.query(
        "SELECT service, run FROM config_github_cancel \
         WHERE deployment_id = ?1 ORDER BY run_id",
        &[deployment.as_str().into()],
    )?;
    rows.iter()
        .map(|row| {
            Ok(GithubOwed {
                build: format!("{deployment}.{}", row.text(0)?),
                run: row.json(1, "owed run")?,
            })
        })
        .collect()
}

/// End build `id` before any run took it, for worker attempt `attempt`: reused, or
/// passed on with why. Only the attempt that owns the Deployment may: an outranked one
/// is refused as newer, so its replay never discards the owner's reservation. With `owe`, a run Cloud dispatched whose record never landed,
/// and after this one never can, is owed a cancel in the same write.
pub(crate) fn github_pass_on(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    end: &GithubEnd,
    owe: Option<&GithubRun>,
    attempt: &str,
) -> Result<GithubSettled, RpcError> {
    let stored = deployment::locked(tx, &id.deployment)?;
    if !owns(tx, &stored, attempt)? {
        return Err(newer_attempt(&stored.summary.id));
    }
    let status = github_end(tx, id, None, end)?;
    let owed = match owe.filter(|run| run.cancellable()) {
        Some(run) => self::owe(tx, id, run).map(|()| true)?,
        None => false,
    };
    Ok(GithubSettled { status, owed })
}

/// End build `id` for the run `run_id` that holds it, for worker attempt `attempt`: the
/// walk's look or the run's final report. Only the attempt that owns the Deployment may,
/// and only for the build's current run: anything else is `conflict`, and a newer
/// attempt's `newer`. With `owe`, a run Cloud dispatched is owed a cancel in the same
/// write: a run past its budget or its start limit.
pub(crate) fn github_settle(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    run_id: u64,
    end: &GithubEnd,
    attempt: &str,
    owe: bool,
) -> Result<GithubSettled, RpcError> {
    let stored = deployment::locked(tx, &id.deployment)?;
    if !owns(tx, &stored, attempt)? {
        return Err(newer_attempt(&stored.summary.id));
    }
    let row = github_row(tx, &stored, id)?;
    let run = row.building_on(run_id).map(|hold| hold.run().clone());
    let status = github_end(tx, id, Some(run_id), end)?;
    let owed = match run.filter(|run| owe && run.cancellable()) {
        Some(run) => self::owe(tx, id, &run).map(|()| true)?,
        None => false,
    };
    Ok(GithubSettled { status, owed })
}

/// End build `id` for its run `run_id`, from that run's own check-in: only while the run
/// holds it and the run's attempt owns the Deployment.
pub(crate) fn github_run_end(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    run_id: u64,
    end: &GithubEnd,
) -> Result<BuildStatus, RpcError> {
    let stored = deployment::locked(tx, &id.deployment)?;
    held_by_owner(tx, &stored, id, run_id)?;
    github_end(tx, id, Some(run_id), end)
}

/// Refuse a run's own write unless it holds build `id` and its attempt owns the
/// Deployment. Beginning an attempt passes on every build the attempts it outranks
/// held, so this holds for every run that holds a build; it is checked all the same.
fn held_by_owner(
    tx: &mut dyn Tx,
    stored: &Stored,
    id: &GithubBuildId,
    run_id: u64,
) -> Result<(), RpcError> {
    let row = github_row(tx, stored, id)?;
    // The build's own attempt, which a run held at upgrade got from the migration.
    let attempt = row.building_on(run_id).and(row.attempt.clone());
    match attempt {
        Some(attempt) if owns(tx, stored, &attempt)? => Ok(()),
        Some(_) | None => Err(no_longer_wanted()),
    }
}

/// Take up to `limit` runs Cloud owes a cancel, in any Organization, for the sweep: the
/// lowest turn first, each then stamped with the next turn. A debt waits only behind
/// those of lower turns, and each sweep raises the turn, so a cancel GitHub keeps
/// refusing never starves new debts, nor new debts a retried one. The order is fairness
/// only: nothing decides from it.
pub(crate) fn github_owed(tx: &mut dyn Tx, limit: u32) -> Result<Vec<GithubOwed>, RpcError> {
    let rows = tx.query(
        "SELECT run_id, deployment_id, service, run FROM config_github_cancel \
         ORDER BY turn, run_id LIMIT ?1",
        &[i64::from(limit).into()],
    )?;
    let turns = tx.query(
        "SELECT COALESCE(MAX(turn), 0) + 1 FROM config_github_cancel",
        &[],
    )?;
    let turn = turns.first().map_or(Ok(1), |row| row.int(0))?;
    let mut owed = Vec::with_capacity(rows.len());
    for row in &rows {
        tx.execute(
            "UPDATE config_github_cancel SET turn = ?2 WHERE run_id = ?1",
            &[row.int(0)?.into(), turn.into()],
        )?;
        owed.push(GithubOwed {
            build: format!("{}.{}", row.text(1)?, row.text(2)?),
            run: row.json(3, "owed run")?,
        });
    }
    Ok(owed)
}

/// Stop Deployment `deployment`'s GitHub builds for worker attempt `attempt`. In one
/// write under the Environment's lock, so no record lands between the check and the
/// ends:
///
/// - The attempt must still be the Deployment's: its runner, or, before any claim, the
///   attempt that began last. A newer attempt's is `conflict`.
/// - The Deployment must keep, committed, a status that takes no more GitHub runs: a
///   lapsed lease only shows Unknown, so the attempt's own lapsed claim is ended first.
/// - Each build a GitHub run holds fails as cancelled, and a run Cloud dispatched is
///   owed a cancel. Each reservation GitHub never answered for is passed on, so a late
///   record of it is refused.
///
/// Returns every run the Deployment still owes a cancel, this stop's and earlier ones'
/// whose cancel GitHub hasn't confirmed.
pub(crate) fn github_stop(
    tx: &mut dyn Tx,
    deployment: &DeploymentId,
    attempt: &crate::id::RunnerId,
) -> Result<Vec<GithubOwed>, RpcError> {
    let stored = deployment::locked(tx, deployment)?;
    let (kept, runner) = deployment::kept_for_stop(tx, stored, Some(attempt))?;
    let theirs = match &runner {
        Some(runner) => runner != attempt,
        None => github_outranked(tx, deployment, attempt)?,
    };
    if theirs {
        return Err(newer_attempt(deployment));
    }
    use deployment::DeploymentStatus as Status;
    // A Deployment waiting for CI takes GitHub runs once CI passes; any other takes none once it is out of flight.
    let stopped = match kept {
        Status::Cancelling => true,
        Status::WaitingForCi => false,
        Status::Queued
        | Status::Superseded
        | Status::Running
        | Status::Applied
        | Status::Failed
        | Status::Unknown
        | Status::Cancelled => !kept.in_flight(),
    };
    if !stopped {
        return Err(error::conflict(
            "This Deployment still takes GitHub runs",
            json!({ "deployment": deployment }),
        ));
    }
    end_github_builds(tx, deployment)?;
    owed_by(tx, deployment)
}

/// End Deployment `deployment`'s GitHub builds for a stop or a cancel, in the caller's
/// locked write: each build a GitHub run holds fails as cancelled, owing a run Cloud
/// dispatched a cancel, and each reservation GitHub never answered for is passed on.
pub(crate) fn end_github_builds(
    tx: &mut dyn Tx,
    deployment: &DeploymentId,
) -> Result<(), RpcError> {
    for build in &rows(tx, deployment)? {
        let id = GithubBuildId {
            deployment: deployment.clone(),
            service: build.service.clone(),
        };
        match &build.stage {
            Stage::Building(By::Github(hold)) => {
                let run = hold.run().clone();
                let end = GithubEnd::Failed {
                    message: CANCELLED.to_owned(),
                };
                github_end(tx, &id, Some(run.run_id), &end)?;
                if run.cancellable() {
                    owe(tx, &id, &run)?;
                }
            }
            Stage::Waiting if build.message.as_deref() == Some(ASKING_GITHUB) => {
                let end = GithubEnd::Skipped {
                    message: STOPPED_ASKING.to_owned(),
                };
                github_end(tx, &id, None, &end)?;
            }
            Stage::Waiting | Stage::Building(By::Servers { .. }) | Stage::Ended { .. } => {}
        }
    }
    Ok(())
}

/// Every run Deployment `deployment` still owes a cancel, for Cloud to cancel after a
/// write that owed them.
pub(crate) fn github_owed_by(
    tx: &mut dyn Tx,
    deployment: &DeploymentId,
) -> Result<Vec<GithubOwed>, RpcError> {
    owed_by(tx, deployment)
}

/// GitHub confirmed it cancelled run `run_id` of build `id`, or a read showed the run
/// completed: Cloud owes it nothing more, and the debt is deleted. Confirming a run owed
/// nothing changes nothing. It needs no Deployment: the debt outlives it.
pub(crate) fn github_cancel_confirmed(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    run_id: u64,
) -> Result<(), RpcError> {
    let run_id = i64::try_from(run_id).map_err(|_| error::corrupt("GitHub run id"))?;
    tx.execute(
        "DELETE FROM config_github_cancel \
         WHERE run_id = ?1 AND deployment_id = ?2 AND service = ?3",
        &[
            run_id.into(),
            id.deployment.as_str().into(),
            id.service.as_str().into(),
        ],
    )
    .map(drop)
}

pub(crate) fn github_build(tx: &mut dyn Tx, id: &GithubBuildId) -> Result<GithubHandOff, RpcError> {
    let stored = deployment::load(tx, &id.deployment)?;
    let row = github_row(tx, &stored, id)?;
    if let Some(build) = row.github(id) {
        return Ok(GithubHandOff::Held(Box::new(build)));
    }
    row.github_skipped
        .map(|why| GithubHandOff::PassedOn { why })
        .ok_or_else(no_such_build)
}

/// Check a runner's verified OIDC claims against GitHub build `id`: its repository,
/// its workflow on the branch it was dispatched on, its run, and a Ployz dispatch.
pub(crate) fn github_authorize(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    claims: &GithubClaims,
) -> Result<GithubBuild, RpcError> {
    let stored = deployment::load(tx, &id.deployment)?;
    let build = github_row(tx, &stored, id)?
        .github(id)
        .ok_or_else(no_such_build)?;
    let source = unrouted(tx, &stored)?
        .into_iter()
        .find(|source| source.service == id.service)
        .ok_or_else(no_such_build)?;
    let refused =
        |message: &str| Err(RpcError::unauthenticated(message.to_owned()).with_details(json!({})));
    if claims.repository_id != source.repository_id.to_string() {
        return refused("The token is for another repository");
    }
    if claims.job_workflow_ref != build.run.workflow_ref {
        return refused("The token is for another workflow or branch");
    }
    if claims.run_id != build.run.run_id.to_string() {
        return refused("The token is for another run");
    }
    // A rule's build of a push runs on the run GitHub started, which Cloud followed. Any
    // other build runs only on the dispatch Cloud sent.
    let event = match github_rule(tx, id)? {
        Some(GithubRule::Push(_)) => "push",
        Some(GithubRule::Dispatch(_)) | None => "workflow_dispatch",
    };
    if claims.event_name != event {
        return refused("The run was not dispatched by Ployz");
    }
    Ok(build)
}

/// The automatic deploy rule a GitHub build builds under, and how its run starts. Cloud
/// relays the rule; the builder and the grant Machine check it.
#[derive(Clone, Debug, PartialEq)]
pub enum GithubRule {
    /// The Store's automation admitted the Deployment for a push: its run is the push's
    /// own, which Cloud follows. The rule may be ended, which the grant Machine refuses.
    Push(Signed<AutoDeployRule>),
    /// A person admitted it while the rule was on: Cloud dispatches the rule's workflow on
    /// its branch, and the grant Machine builds only that branch's head.
    Dispatch(Signed<AutoDeployRule>),
}

impl GithubRule {
    #[must_use]
    pub fn signed(&self) -> &Signed<AutoDeployRule> {
        match self {
            Self::Push(signed) | Self::Dispatch(signed) => signed,
        }
    }
}

/// The automatic deploy rule GitHub build `id` builds under, when the rule covers the
/// Service.
pub(crate) fn github_rule(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
) -> Result<Option<GithubRule>, RpcError> {
    let stored = deployment::load(tx, &id.deployment)?;
    rule_of(tx, &stored, &id.service)
}

/// The automatic deploy rule `stored`'s build of `service` builds under. A push's build
/// does whenever the rule covers the Service. A person's Deploy builds under it only
/// while it is on and approves the Service as the Deployment froze it: its repository,
/// branch, settings and build policy. Any other builds as it would without a rule.
pub(crate) fn rule_of(
    tx: &mut dyn Tx,
    stored: &Stored,
    service: &ServiceName,
) -> Result<Option<GithubRule>, RpcError> {
    let Some(signed) = crate::auto_rule::stored(tx, &stored.summary.environment_id)? else {
        return Ok(None);
    };
    let Some(rule) = crate::auto_rule::opened(&signed) else {
        return Ok(None);
    };
    if rule.service(service).is_none() {
        return Ok(None);
    }
    if stored.summary.admitted_by.is_none() {
        return Ok(Some(GithubRule::Push(signed)));
    }
    if rule.ended {
        return Ok(None);
    }
    // A member's Deploy of a preview builds on its pull request's own push run, which
    // the Server checks against the rule's pushers.
    if rule.preview.is_some() {
        return Ok(Some(GithubRule::Push(signed)));
    }
    let saved = deployment::held_revision(tx, stored)?;
    let lowered = deployment::lowered(tx, stored, &saved)?;
    let approved =
        crate::auto_rule::approved(&rule, stored.namespace(), &saved, &lowered.intent.target);
    Ok(approved
        .contains(service)
        .then_some(GithubRule::Dispatch(signed)))
}

/// Whether `image`, which a person's Deploy under the rule would reuse for GitHub build
/// `id`, is a build a Server verified under the rule and generation the Environment keeps
/// now. An image token doesn't count: it expires, and only a Server's verification
/// outlives it. An image without one is built again under the rule.
pub(crate) fn github_proven(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    image: &str,
) -> Result<bool, RpcError> {
    let Ok(digest) = BuiltImageDigest::parse(image) else {
        return Ok(false);
    };
    let stored = deployment::load(tx, &id.deployment)?;
    let organization = deployment::organization(tx, &id.deployment)?;
    Ok(crate::build::verified_proof(
        tx,
        organization.as_str(),
        (stored.namespace(), &id.service),
        digest,
    )?
    .is_some())
}

/// The GitHub build run `run_id` holds while it builds, if any: a rule build's runner
/// checks in naming only its run.
pub(crate) fn github_by_run(
    tx: &mut dyn Tx,
    run_id: u64,
) -> Result<Option<GithubBuildId>, RpcError> {
    // The run's JSON starts with its id, so this matches only that run, and the decode
    // below confirms it.
    let pattern = format!("%\"run_id\":{run_id},%");
    let rows = tx.query(
        "SELECT deployment_id, service, github FROM config_build \
         WHERE status = ?1 AND github LIKE ?2",
        &[
            name_of(BuildStatus::Building).as_str().into(),
            pattern.as_str().into(),
        ],
    )?;
    for row in &rows {
        let Some(github) = row.optional_text(2)? else {
            continue;
        };
        let hold: GithubHold =
            serde_json::from_str(github).map_err(|_| error::corrupt("GitHub build"))?;
        if hold.run().run_id == run_id {
            return Ok(Some(GithubBuildId {
                deployment: row.parse(0, "build")?,
                service: row.parse(1, "build")?,
            }));
        }
    }
    Ok(None)
}

/// What a GitHub build builds.
#[derive(Clone, Debug, PartialEq)]
pub struct GithubInput {
    /// The Deployment's lowering input, with every value sealed in the browser left
    /// out but the built Service's.
    pub input: serde_json::Value,
    pub commit: CommitSha,
    /// The build's own receipt, as a redeploy copies its source's, else the Service's
    /// latest.
    pub receipt: Option<serde_json::Value>,
    /// The image token a redeploy copied with its source's image.
    pub image_token: Option<ImageToken>,
}

/// What GitHub build `id` builds. In-process only.
pub(crate) fn github_input(tx: &mut dyn Tx, id: &GithubBuildId) -> Result<GithubInput, RpcError> {
    let stored = deployment::load(tx, &id.deployment)?;
    let row = github_row(tx, &stored, id)?;
    let commit = row
        .commit
        .ok_or_else(|| error::corrupt("GitHub build commit"))?;
    let saved = deployment::held_revision(tx, &stored)?;
    let deployment::Lowered { mut input, .. } = deployment::lowered(tx, &stored, &saved)?;
    let sealed = input
        .snapshots
        .iter()
        .flat_map(|snapshot| snapshot.resolved_env.values())
        .any(ployz_core::EnvValue::is_sealed);
    if sealed
        && !input
            .snapshots
            .iter()
            .any(|snapshot| snapshot.serves(&id.service))
    {
        return Err(error::corrupt("GitHub build Service"));
    }
    for snapshot in &mut input.snapshots {
        if !snapshot.serves(&id.service) {
            snapshot.resolved_env.retain(|_, value| !value.is_sealed());
        }
    }
    let input = deployment::json_value(&input);
    // A cache hint: the build's own receipt, as a redeploy copies its source's, else the
    // Service's latest.
    let receipt = match row.receipt {
        Some(receipt) => Some(receipt),
        None => deployment::receipts(tx, &stored.summary.environment_id)?
            .remove(&id.service)
            .and_then(|receipts| receipts.into_iter().next()),
    };
    Ok(GithubInput {
        input,
        commit,
        receipt,
        image_token: row.image_token,
    })
}

/// Run `run_id` checked in and got `grant`: the build starts. Accepted once, while
/// the Deployment still wants it.
pub(crate) fn github_check_in(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    run_id: u64,
    grant: &GithubGrant,
) -> Result<(), RpcError> {
    let stored = deployment::locked(tx, &id.deployment)?;
    held_by_owner(tx, &stored, id, run_id)?;
    let row = github_row(tx, &stored, id)?;
    let held = row.building_on(run_id);
    // A cancelling Deployment takes no more builds: its stop ends this one.
    let in_flight = takes_github_runs(&stored);
    let (Some(held), true) = (
        held,
        github_check_in_holds(held.map(GithubHold::hold), in_flight),
    ) else {
        return Err(no_longer_wanted());
    };
    let github = GithubHold::CheckedIn {
        run: held.run().clone(),
        started: Started {
            grant: grant.clone(),
            at: crate::storage::now().unsigned_abs(),
            received: 0,
        },
    };
    save_github(
        tx,
        id,
        BuildStatus::Building,
        row.message.as_deref(),
        &row.log,
        Some(&github),
    )
}

/// Take a batch of run `run_id`'s log after it checked in, until its final report.
/// Lines already taken (a retried batch) are skipped. Returns how many it took.
pub(crate) fn github_report(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    run_id: u64,
    report: &GithubReport,
) -> Result<u64, RpcError> {
    let stored = deployment::locked(tx, &id.deployment)?;
    held_by_owner(tx, &stored, id, run_id)?;
    let row = github_row(tx, &stored, id)?;
    let (run, started) = match row.building_on(run_id) {
        Some(GithubHold::CheckedIn { run, started }) => (run, started),
        Some(GithubHold::Dispatched { .. }) => {
            return Err(error::conflict("This build has not checked in", json!({})));
        }
        Some(GithubHold::Reported { .. }) | None => {
            return Err(error::conflict("This build already ended", json!({})));
        }
    };
    if report.from > started.received {
        return Err(error::conflict(
            format!(
                "Build Steps before line {} are missing; send from line {}",
                report.from, started.received
            ),
            json!({ "received": started.received }),
        ));
    }
    let skip = usize::try_from(started.received - report.from).unwrap_or(usize::MAX);
    let mut log = row.log.clone();
    for line in report.lines.iter().skip(skip) {
        append(&mut log, line);
    }
    let received = started
        .received
        .max(report.from + report.lines.len() as u64);
    let run = run.clone();
    let started = Started {
        received,
        ..started.clone()
    };
    let github = match &report.ended {
        None => GithubHold::CheckedIn { run, started },
        Some(end) => GithubHold::Reported {
            run,
            started,
            end: end.clone(),
        },
    };
    save_github(
        tx,
        id,
        BuildStatus::Building,
        row.message.as_deref(),
        &log,
        Some(&github),
    )?;
    Ok(received)
}

/// End GitHub build `id`: its run `run_id`'s, or with none, before GitHub took it
/// (an earlier image served it, or GitHub couldn't take it). Ending it again, a run
/// that no longer holds it, or ending it with none after GitHub passed it on, is
/// `conflict`.
pub(crate) fn github_end(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    run_id: Option<u64>,
    end: &GithubEnd,
) -> Result<BuildStatus, RpcError> {
    let stored = deployment::locked(tx, &id.deployment)?;
    let row = github_row(tx, &stored, id)?;
    let kind = match end {
        GithubEnd::Built { .. } => EndKind::Built,
        GithubEnd::Failed { .. } => EndKind::Failed,
        GithubEnd::Skipped { .. } => EndKind::Skipped,
        GithubEnd::Unstarted { .. } => EndKind::Unstarted,
    };
    let target = match run_id {
        Some(run_id) => Target::Run {
            building_on: row.building_on(run_id).map(GithubHold::hold),
        },
        None => Target::BeforeGithub {
            waiting_unskipped: matches!(row.stage, Stage::Waiting) && row.github_skipped.is_none(),
        },
    };
    if !github_end_holds(kind, target) {
        return Err(error::conflict("This build already ended", json!({})));
    }
    let token = match end {
        GithubEnd::Built { receipt } => built_token(tx, id, &row, run_id.is_some(), receipt)?,
        GithubEnd::Failed { .. } | GithubEnd::Skipped { .. } | GithubEnd::Unstarted { .. } => None,
    };
    let mut log = row.log;
    let (status, message, github) = match end {
        GithubEnd::Built { receipt } => {
            deployment::save_receipt(tx, &stored.summary.environment_id, &id.service, receipt)?;
            keep_built(tx, id, receipt, token.as_ref())?;
            let status = if run_id.is_some() {
                BuildStatus::Built
            } else {
                BuildStatus::Reused
            };
            (status, None, row.stage.github())
        }
        GithubEnd::Failed { message } => {
            append(&mut log, &format!("GitHub: {message}\n"));
            (
                BuildStatus::Failed,
                Some(clipped(message)),
                row.stage.github(),
            )
        }
        // The next Builder starts it afresh: the run holds it no more.
        GithubEnd::Skipped { message } | GithubEnd::Unstarted { message } => {
            append(&mut log, &format!("GitHub skipped: {message}\n"));
            (BuildStatus::Pending, Some(clipped(message)), None)
        }
    };
    save_github(tx, id, status, message.as_deref(), &log, github)?;
    if let (GithubEnd::Skipped { .. } | GithubEnd::Unstarted { .. }, Some(why)) = (end, &message) {
        skip(tx, id, why)?;
    }
    Ok(status)
}

/// The image token a build that ended Built with `receipt` keeps: its run's, with the
/// rule generation and authority its audience names, or, reusing the image a redeploy's
/// source built, the token copied with that image.
fn built_token(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    row: &BuildRow,
    ran: bool,
    receipt: &serde_json::Value,
) -> Result<Option<ImageToken>, RpcError> {
    let Some(image) = receipt
        .pointer("/image/reference")
        .and_then(serde_json::Value::as_str)
    else {
        return Ok(None);
    };
    if !ran {
        return Ok(row.image_token.clone().filter(|token| token.image == image));
    }
    let Some(RunEnd::Built {
        image_token: Some(token),
        ..
    }) = row.stage.github().and_then(GithubHold::end)
    else {
        return Ok(None);
    };
    let Some(rule) = github_rule(tx, id)?
        .as_ref()
        .and_then(|rule| crate::auto_rule::opened(rule.signed()))
    else {
        return Ok(None);
    };
    Ok(Some(ImageToken {
        token: token.clone(),
        image: image.to_owned(),
        generation: rule.generation,
        authority: CommitAuthority::Head,
    }))
}

/// Keep the receipt and token of the image a build ended with, for a redeploy of it.
fn keep_built(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    receipt: &serde_json::Value,
    token: Option<&ImageToken>,
) -> Result<(), RpcError> {
    let receipt = receipt.to_string();
    let token = token.map(json_text);
    tx.execute(
        "UPDATE config_build SET receipt = ?3, image_token = ?4 \
         WHERE deployment_id = ?1 AND service = ?2",
        &[
            id.deployment.as_str().into(),
            id.service.as_str().into(),
            receipt.as_str().into(),
            token.as_deref().into(),
        ],
    )
    .map(drop)
}

/// Record why GitHub passed `id` on.
fn skip(tx: &mut dyn Tx, id: &GithubBuildId, why: &str) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_build SET github_skipped = ?3 WHERE deployment_id = ?1 AND service = ?2",
        &[
            id.deployment.as_str().into(),
            id.service.as_str().into(),
            why.into(),
        ],
    )?;
    Ok(())
}

/// Deployment `id`'s builds still on GitHub: what cancelling it stops.
pub(crate) fn github_outstanding(
    tx: &mut dyn Tx,
    id: &DeploymentId,
) -> Result<Vec<GithubBuild>, RpcError> {
    Ok(rows(tx, id)?
        .into_iter()
        .filter(|row| matches!(row.stage, Stage::Building(By::Github(_))))
        .filter_map(|row| {
            row.github(&GithubBuildId {
                deployment: id.clone(),
                service: row.service.clone(),
            })
        })
        .collect())
}
