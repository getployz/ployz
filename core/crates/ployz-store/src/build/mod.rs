//! Git and uploaded builds. Before a Deployment's runner reads a Git Service's source,
//! Cloud pins the commit it builds. A pin never moves, so every retry of the Deployment
//! builds the same commit. Services without a source of their own build from the
//! Deployment's upload. The runner reports each build's progress and log here.

mod github;
mod proof;
mod sources;

pub use github::{
    GithubAsked, GithubBuild, GithubBuildId, GithubClaims, GithubEnd, GithubGrant, GithubHandOff,
    GithubInput, GithubOwed, GithubRecorded, GithubReport, GithubRule, GithubRun, GithubSettled,
    RunEnd, UNRESERVED,
};
pub(crate) use github::{
    end_github_builds, github_ask, github_authorize, github_begin, github_build, github_by_run,
    github_cancel_confirmed, github_check_in, github_dispatched, github_input, github_outranked,
    github_outstanding, github_owed, github_owed_by, github_pass_on, github_proven, github_report,
    github_rule, github_run_end, github_settle, github_stop,
};
pub use proof::{ImageToken, VerifiedClaim};
pub(crate) use proof::{image_tokens, keep_verified, verified, verified_proof};
pub use sources::GitSource;
pub(crate) use sources::{
    pin, pin_admitted, pin_running, pinner, retried, routed, sources, uploads_of,
};

use ployz_core::authority::{CommitApproval, Signed};
use ployz_core::{RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::deployment::{Stored, clipped};
use crate::error;
use crate::id::{CommitSha, DeploymentId, OrganizationId};
use crate::storage::{Param, Tx, name_of};

use github::GithubHold;
use sources::pending;

/// The most log a build keeps; older output goes first.
const LOG_LIMIT: usize = 256 * 1024;

/// Where one Git build is.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    /// Pinned; not started.
    Pending,
    Building,
    Built,
    /// A matching image from an earlier build served it.
    Reused,
    Failed,
}

/// A runner's report on one build: where it is now, and its log output since the
/// last report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct BuildReport {
    /// Its Private DNS name, which a rename keeps.
    pub service: ServiceName,
    pub status: BuildStatus,
    /// Why it failed. Users read it: it holds no secret.
    pub message: Option<String>,
    pub log: String,
    /// The Server building it, by name, once one was chosen.
    #[serde(default)]
    pub server: Option<String>,
}

/// One build of a Deployment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct BuildView {
    /// The Service's name when admitted.
    pub service: ServiceName,
    /// The commit it builds; none when it builds from the Deployment's upload.
    pub commit: Option<CommitSha>,
    pub status: BuildStatus,
    /// Why it failed.
    pub message: Option<String>,
    /// When it first reported Building since it last waited for a Builder, in
    /// Unix seconds.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub started_at: Option<i64>,
    /// When it reached Built, Reused or Failed, in Unix seconds; cleared when a
    /// later Builder starts it again or it waits for one.
    #[serde(default)]
    #[ts(type = "number | null")]
    pub finished_at: Option<i64>,
    /// The Server that built it, by name; none until one starts it, and when GitHub
    /// builds it.
    #[serde(default)]
    pub server: Option<String>,
    /// Why GitHub passed it to the next Builder, when it did.
    #[serde(default)]
    pub github_skipped: Option<String>,
    /// Set on a Failed build that never reported how it ended: its Deployment ended,
    /// or lost its runner, while the Servers built it. Sent as a marker beside
    /// `failed` rather than a status of its own, so a reader from before it reads
    /// Failed. Private, so no struct literal or assignment outside the Store sets it:
    /// the Store's view sets it beside `status`. Decoding a `BuildView`, or changing
    /// its `status` after, can still pair it with any status, so every reader asks
    /// [`BuildView::is_interrupted`], which reads it only with Failed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    interrupted: bool,
}

impl BuildView {
    /// Whether it failed only because its Deployment ended, or lost its runner,
    /// before it reported how it ended: nothing says the build itself failed.
    #[must_use]
    pub fn is_interrupted(&self) -> bool {
        self.status == BuildStatus::Failed && self.interrupted
    }
}

/// What a build reads once its Deployment ended, or lost its runner, before it
/// reported how it ended. It says only what the Store saw.
pub(crate) const INTERRUPTED: &str =
    "The deployment ended or lost its runner before this build reported how it ended.";

/// A build's `status` column with its `interrupted` one: only a failed build can be
/// interrupted.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum RowStatus {
    /// As its last Builder, or the Store, left it.
    Reported(BuildStatus),
    /// Failed: its Deployment ended, or lost its runner, before it reported its end.
    Interrupted,
}

impl RowStatus {
    /// Its `status` and `interrupted` columns; `corrupt` for a build marked
    /// interrupted that isn't failed.
    fn decode(status: BuildStatus, interrupted: i64) -> Result<Self, RpcError> {
        match (status, interrupted) {
            (status, 0) => Ok(Self::Reported(status)),
            (BuildStatus::Failed, 1) => Ok(Self::Interrupted),
            _ => Err(error::corrupt("build status")),
        }
    }

    const fn interrupted(self) -> bool {
        matches!(self, Self::Interrupted)
    }
}

impl From<RowStatus> for BuildStatus {
    fn from(status: RowStatus) -> Self {
        match status {
            RowStatus::Reported(status) => status,
            RowStatus::Interrupted => Self::Failed,
        }
    }
}

/// Settle each build of Deployment `id` its Servers still hold Building, now that
/// the Deployment can't run any more and shows that it ended at `ended`: Failed,
/// marked interrupted, finished at `ended` unless it already finished. A build
/// GitHub holds stays Building, so Cloud still finds its run to cancel
/// ([`github_outstanding`]). Settling again changes nothing.
pub(crate) fn settle(tx: &mut dyn Tx, id: &DeploymentId, ended: i64) -> Result<(), RpcError> {
    let (failed, building) = (name_of(BuildStatus::Failed), name_of(BuildStatus::Building));
    tx.execute(
        &format!(
            "UPDATE config_build SET status = '{failed}', interrupted = 1, message = ?2, \
         finished = COALESCE(finished, ?3) \
         WHERE deployment_id = ?1 AND status = '{building}' AND github IS NULL"
        ),
        &[id.as_str().into(), INTERRUPTED.into(), ended.into()],
    )?;
    Ok(())
}

/// Ask for the log of `service`'s build in `deployment`; `not_found` when that
/// Deployment built no such Service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct BuildLogQuery {
    pub deployment: DeploymentId,
    /// Its name when admitted, or its Private DNS name.
    pub service: ServiceName,
}

/// A build with its log, newest output last.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct BuildLogView {
    pub deployment: DeploymentId,
    #[serde(flatten)]
    pub build: BuildView,
    pub log: String,
}

/// Record a runner's report on one pinned or uploaded build of `stored`, which it
/// claimed.
pub(crate) fn record(
    tx: &mut dyn Tx,
    stored: &Stored,
    report: &BuildReport,
) -> Result<(), RpcError> {
    let id = &stored.summary.id;
    if uploads_of(tx, stored)?.contains_key(&report.service) {
        pending(tx, id, &report.service, None)?;
    }
    let Some(row) = rows(tx, id)?
        .into_iter()
        .find(|row| row.service == report.service)
    else {
        return Err(error::invalid(
            format!("{} has no pinned build to report on", report.service),
            json!({}),
        ));
    };
    // A build a GitHub run holds, or ended, is that run's: a Server building it too
    // would race the run's own reports into the same row.
    if row.stage.github().is_some() {
        return Err(error::conflict(
            format!("A GitHub run took the build of {}", report.service),
            json!({ "service": report.service }),
        ));
    }
    let mut log = row.log;
    append(&mut log, &report.log);
    let message = report.message.as_deref().map(clipped);
    tx.execute(
        "UPDATE config_build SET status = ?3, message = ?4, log = ?5, github = NULL, \
         server = COALESCE(?6, server) \
         WHERE deployment_id = ?1 AND service = ?2",
        &[
            id.as_str().into(),
            report.service.as_str().into(),
            name_of(report.status).as_str().into(),
            message.as_deref().into(),
            log.as_str().into(),
            report.server.as_deref().into(),
        ],
    )?;
    clock(tx, id, &report.service, report.status)
}

/// Start the build's clock at its first Building, stop it at its first terminal
/// status, reopen it when a later Builder starts again, and clear it when the
/// build goes back to Pending to wait for the next Builder.
fn clock(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    service: &ServiceName,
    status: BuildStatus,
) -> Result<(), RpcError> {
    let (set, now) = match status {
        BuildStatus::Building => (
            "started = COALESCE(started, ?3), finished = NULL",
            Some(crate::storage::now()),
        ),
        BuildStatus::Built | BuildStatus::Reused | BuildStatus::Failed => (
            "finished = COALESCE(finished, ?3)",
            Some(crate::storage::now()),
        ),
        BuildStatus::Pending => ("started = NULL, finished = NULL", None),
    };
    let mut params: Vec<Param<'_>> = vec![id.as_str().into(), service.as_str().into()];
    params.extend(now.map(Param::from));
    tx.execute(
        &format!("UPDATE config_build SET {set} WHERE deployment_id = ?1 AND service = ?2"),
        &params,
    )?;
    Ok(())
}

/// Every build of `stored`, by the Service names it was admitted with.
pub(crate) fn views(tx: &mut dyn Tx, stored: &Stored) -> Result<Vec<BuildView>, RpcError> {
    Ok(rows(tx, &stored.summary.id)?
        .into_iter()
        .map(|row| row.view(stored))
        .collect())
}

/// One build's log; `service` is its Service's name or runtime name.
pub(crate) fn log(
    tx: &mut dyn Tx,
    stored: &Stored,
    service: &ServiceName,
) -> Result<BuildLogView, RpcError> {
    let row = rows(tx, &stored.summary.id)?
        .into_iter()
        .find(|row| row.service == *service || row.view(stored).service == *service)
        .ok_or_else(|| {
            error::not_found(
                format!(
                    "Deployment {} built no Service {service}",
                    stored.summary.id
                ),
                json!({}),
            )
        })?;
    Ok(BuildLogView {
        deployment: stored.summary.id.clone(),
        build: row.view(stored),
        log: row.log,
    })
}

struct BuildRow {
    service: ServiceName,
    /// None when it builds from the Deployment's upload.
    commit: Option<CommitSha>,
    stage: Stage,
    message: Option<String>,
    log: String,
    organization: OrganizationId,
    started: Option<i64>,
    finished: Option<i64>,
    /// Why GitHub passed it on to the next Builder.
    github_skipped: Option<String>,
    /// The receipt of the image it built or reused, or its source's for a redeploy.
    receipt: Option<serde_json::Value>,
    /// The image token of that image, or its source's for a redeploy.
    image_token: Option<ImageToken>,
    /// What a Server issued for that image once it checked it online, or its source's
    /// for a redeploy.
    verified: Option<VerifiedClaim>,
    /// The worker attempt that reserved or recorded its GitHub run.
    attempt: Option<String>,
    /// Pinned since Cloud reserves a build before asking GitHub for it.
    reservable: bool,
    /// A person's approval of its commit for one preview, which its check-in relays and
    /// every Server checks in place of the branch head.
    approval: Option<Signed<CommitApproval>>,
}

/// Where a build is, with who holds it while it builds: a GitHub run or the Servers,
/// never both.
#[derive(Clone, Debug)]
enum Stage {
    /// Pinned, or passed on: it waits for a Builder.
    Waiting,
    Building(By),
    /// Built, Reused or Failed. `github` is the run that ended it, which Cloud still
    /// reads; `server` the Server that ran it, by name.
    Ended {
        status: RowStatus,
        github: Option<GithubHold>,
        server: Option<String>,
    },
}

/// Who builds a build.
#[derive(Clone, Debug)]
#[expect(
    clippy::large_enum_variant,
    reason = "a row's stage, decoded one row at a time"
)]
enum By {
    /// `server` once they chose one, by name.
    Servers {
        server: Option<String>,
    },
    Github(GithubHold),
}

impl Stage {
    /// The row's `status`, `interrupted`, `github` and `server` columns. Some rows come
    /// from `main`'s [`record`], which allowed a Server's report on a build a GitHub run
    /// held and left the run's `github` on a row of any status. A Building row with both
    /// reads as the run's, since its hold is what GitHub's steps read, and the Server goes.
    /// A Pending row with a run reads as waiting, and `record` clears the run.
    fn decode(status: RowStatus, github: Option<GithubHold>, server: Option<String>) -> Self {
        match (status, github) {
            (RowStatus::Reported(BuildStatus::Pending), _) => Self::Waiting,
            (RowStatus::Reported(BuildStatus::Building), Some(github)) => {
                Self::Building(By::Github(github))
            }
            (RowStatus::Reported(BuildStatus::Building), None) => {
                Self::Building(By::Servers { server })
            }
            (status, github) => Self::Ended {
                status,
                github,
                server,
            },
        }
    }

    const fn status(&self) -> RowStatus {
        match self {
            Self::Waiting => RowStatus::Reported(BuildStatus::Pending),
            Self::Building(_) => RowStatus::Reported(BuildStatus::Building),
            Self::Ended { status, .. } => *status,
        }
    }

    /// The GitHub run that holds it, or that ended it.
    const fn github(&self) -> Option<&GithubHold> {
        match self {
            Self::Building(By::Github(github))
            | Self::Ended {
                github: Some(github),
                ..
            } => Some(github),
            Self::Waiting | Self::Building(By::Servers { .. }) | Self::Ended { .. } => None,
        }
    }

    fn server(&self) -> Option<&str> {
        match self {
            Self::Building(By::Servers { server }) | Self::Ended { server, .. } => {
                server.as_deref()
            }
            Self::Waiting | Self::Building(By::Github(_)) => None,
        }
    }
}

impl BuildRow {
    /// It as `stored` shows it. A build its Servers still hold Building under a
    /// Deployment that can't run any more and shows an end reads as [`settle`]
    /// leaves it, before anything writes it: a lapsed runner lease writes nothing.
    fn view(&self, stored: &Stored) -> BuildView {
        let settled = match (&self.stage, stored.summary.ended_at) {
            (Stage::Building(By::Servers { .. }), Some(ended)) if !stored.summary.in_flight => {
                Some(ended)
            }
            _ => None,
        };
        let (status, message, finished) = match settled {
            Some(ended) => (
                RowStatus::Interrupted,
                Some(INTERRUPTED.to_owned()),
                self.finished.or(Some(ended)),
            ),
            None => (self.stage.status(), self.message.clone(), self.finished),
        };
        BuildView {
            service: stored
                .nodes
                .iter()
                .find_map(|node| {
                    node.service()
                        .filter(|ours| *ours.runtime == self.service)
                        .map(|ours| ours.name.clone())
                })
                .unwrap_or_else(|| self.service.clone()),
            commit: self.commit.clone(),
            status: status.into(),
            message,
            started_at: self.started,
            finished_at: finished,
            server: self.stage.server().map(str::to_owned),
            github_skipped: self.github_skipped.clone(),
            interrupted: status.interrupted(),
        }
    }
}

fn rows(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Vec<BuildRow>, RpcError> {
    tx.query(
        "SELECT service, commit_sha, status, message, log, github, organization_id, \
         started, finished, server, github_skipped, interrupted, receipt, image_token, verified, \
         attempt, reservable, commit_approval \
         FROM config_build \
         WHERE deployment_id = ?1 ORDER BY service",
        &[id.as_str().into()],
    )?
    .iter()
    .map(|row| {
        let github = row.optional_json(5, "GitHub build")?;
        Ok(BuildRow {
            service: row.parse::<ServiceName>(0, "build")?,
            commit: row.parse_optional(1, "build commit")?,
            stage: Stage::decode(
                RowStatus::decode(row.variant(2, "build status")?, row.int(11)?)?,
                github,
                row.optional_text(9)?.map(str::to_owned),
            ),
            message: row.optional_text(3)?.map(str::to_owned),
            log: row.text(4)?.to_owned(),
            organization: row.parse(6, "build")?,
            started: row.optional_int(7)?,
            finished: row.optional_int(8)?,
            github_skipped: row.optional_text(10)?.map(str::to_owned),
            receipt: row.optional_json(12, "build receipt")?,
            image_token: row.optional_json(13, "image token")?,
            verified: row.optional_json(14, "verified build")?,
            attempt: row.optional_text(15)?.map(str::to_owned),
            reservable: row.int(16)? != 0,
            approval: row.optional_json(17, "commit approval")?,
        })
    })
    .collect()
}

fn append(log: &mut String, more: &str) {
    log.push_str(more);
    if log.len() > LOG_LIMIT {
        let cut = log.len() - LOG_LIMIT;
        let cut = (cut..log.len())
            .find(|index| log.is_char_boundary(*index))
            .unwrap_or(log.len());
        log.drain(..cut);
    }
}
