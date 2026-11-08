//! Git and uploaded builds. Before a Deployment's runner reads a Git Service's source,
//! Cloud pins the commit it builds. A pin never moves, so every retry of the Deployment
//! builds the same commit. Services without a source of their own build from the
//! Deployment's upload. The runner reports each build's progress and log here.

use crate::id::{BranchName, CommitSha, OrganizationId, RepositoryId, RepositoryName};
use std::collections::BTreeMap;

use ployz_core::config::{BuildMethod, ServiceGitAccess, ServiceGitBranch, ServiceSource};
use ployz_core::{MachineId, RpcError, ServiceName};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use crate::builders::{self, Builder};
use crate::deployment::{self, Stored};
use crate::error;
use crate::id::DeploymentId;
use crate::storage::{Param, Tx, name_of};

/// The most log a build keeps; older output goes first.
const LOG_LIMIT: usize = 256 * 1024;

/// A Git Service a Deployment builds, and the commit it is pinned to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct GitSource {
    /// Its Private DNS name, which a rename keeps.
    pub service: ServiceName,
    /// The GitHub repository, as `owner/name`.
    pub repository: RepositoryName,
    pub repository_id: RepositoryId,
    /// How Cloud reads it.
    pub access: ServiceGitAccess,
    /// The branch it follows; none once it was disconnected.
    pub branch: Option<BranchName>,
    /// The directory it builds from, inside the repository.
    pub root_dir: String,
    /// Its Dockerfile, relative to `root_dir`, when it builds from one.
    pub dockerfile_path: Option<String>,
    /// The commit it builds; none until pinned.
    pub commit: Option<CommitSha>,
    /// The Builders its build tries, in turn: its Preferred Builder, then the
    /// Organization's Build Order.
    pub builders: Vec<Builder>,
    /// The Server the servers try first, when it prefers one.
    pub preferred_machine: Option<MachineId>,
    /// Where its build is; none until pinned.
    pub status: Option<BuildStatus>,
    /// Why its build failed, or why the last Builder skipped it.
    pub message: Option<String>,
}

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
/// interrupted. Compares equal to the [`BuildStatus`] its `status` column holds, which
/// is all a writer of the row reads.
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

impl PartialEq<BuildStatus> for RowStatus {
    fn eq(&self, other: &BuildStatus) -> bool {
        BuildStatus::from(*self) == *other
    }
}

/// Settle each build of Deployment `id` its Servers still hold Building, now that
/// the Deployment can't run any more and shows that it ended at `ended`: Failed,
/// marked interrupted, finished at `ended` unless it already finished. A build
/// GitHub holds stays Building, so Cloud still finds its run to cancel
/// ([`github_outstanding`]). Settling again changes nothing.
pub(crate) fn settle(tx: &mut dyn Tx, id: &DeploymentId, ended: i64) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_build SET status = 'failed', interrupted = 1, message = ?2, \
         finished = COALESCE(finished, ?3) \
         WHERE deployment_id = ?1 AND status = 'building' AND github IS NULL",
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

/// The Git Services Deployment `id` builds, each with its pin, if any.
pub(crate) fn sources(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Vec<GitSource>, RpcError> {
    let stored = deployment::load(tx, id)?;
    sources_of(tx, &stored)
}

/// Pin the commits of `commits` (by runtime Service name) that aren't pinned yet. A
/// pinned commit never changes, and one its PR Environment's pull request doesn't
/// admit never pins.
pub(crate) fn pin(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    commits: &BTreeMap<ServiceName, CommitSha>,
) -> Result<(), RpcError> {
    let stored = deployment::locked(tx, id)?;
    if !stored.summary.status.in_flight() {
        return Err(error::conflict(
            "This Deployment is no longer waiting to run",
            json!({ "deployment": id, "status": stored.summary.status }),
        ));
    }
    let sources = unrouted(tx, &stored)?;
    for (service, commit) in commits {
        let Some(source) = sources.iter().find(|source| source.service == *service) else {
            return Err(error::invalid(
                format!("This Deployment builds no Git Service {service}"),
                json!({ "services": sources.iter().map(|source| &source.service).collect::<Vec<_>>() }),
            ));
        };
        let environment = &stored.summary.environment_id;
        if !crate::pull_request::admits(tx, environment, source.repository_id, commit)? {
            return Err(error::invalid(
                format!("{commit} needs approval before it deploys"),
                json!({ "service": service, "commit": commit }),
            ));
        }
        tx.execute(
            "INSERT INTO config_build \
             (deployment_id, service, organization_id, commit_sha, status, message, log) \
             SELECT id, ?2, organization_id, ?3, 'pending', NULL, '' \
             FROM config_deployment WHERE id = ?1 \
             ON CONFLICT (deployment_id, service) DO NOTHING",
            &[
                id.as_str().into(),
                service.as_str().into(),
                commit.as_str().into(),
            ],
        )?;
    }
    Ok(())
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
        tx.execute(
            "INSERT INTO config_build \
             (deployment_id, service, organization_id, commit_sha, status, message, log) \
             SELECT id, ?2, organization_id, NULL, 'pending', NULL, '' \
             FROM config_deployment WHERE id = ?1 \
             ON CONFLICT (deployment_id, service) DO NOTHING",
            &[id.as_str().into(), report.service.as_str().into()],
        )?;
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
    let message = report.message.as_deref().map(trimmed);
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
            Some(deployment::now()),
        ),
        BuildStatus::Built | BuildStatus::Reused | BuildStatus::Failed => {
            ("finished = COALESCE(finished, ?3)", Some(deployment::now()))
        }
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
                .find_map(|node| match node {
                    crate::deployment::TargetNode::Service { name, runtime, .. }
                        if *runtime == self.service =>
                    {
                        Some(name.clone())
                    }
                    crate::deployment::TargetNode::Service { .. }
                    | crate::deployment::TargetNode::Volume { .. }
                    | crate::deployment::TargetNode::Config { .. } => None,
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
         started, finished, server, github_skipped, interrupted \
         FROM config_build \
         WHERE deployment_id = ?1 ORDER BY service",
        &[id.as_str().into()],
    )?
    .iter()
    .map(|row| {
        let github = row
            .optional_text(5)?
            .map(|github| serde_json::from_str(github).map_err(|_| error::corrupt("GitHub build")))
            .transpose()?;
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
        })
    })
    .collect()
}

/// The Services `stored` targets that have no source of their own: they build from
/// its upload.
pub(crate) fn uploads_of(
    tx: &mut dyn Tx,
    stored: &Stored,
) -> Result<BTreeMap<ServiceName, Option<ployz_core::UploadDigest>>, RpcError> {
    let saved = deployment::saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
    Ok(saved
        .services
        .iter()
        .filter(|service| stored.nodes.iter().any(|node| node.id() == service.id))
        .filter(|service| matches!(service.config.source, ServiceSource::Empty { .. }))
        .map(|service| {
            (
                service.config.private_dns.clone(),
                deployment::uploads::own(stored, &service.id).cloned(),
            )
        })
        .collect())
}

/// The Git Services `stored` builds, with the Builders each tries.
pub(crate) fn sources_of(tx: &mut dyn Tx, stored: &Stored) -> Result<Vec<GitSource>, RpcError> {
    let saved = deployment::saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
    routed(tx, stored, &saved)
}

/// The Git Services `stored` builds from `saved`, each with the Builders it tries.
pub(crate) fn routed(
    tx: &mut dyn Tx,
    stored: &Stored,
    saved: &ployz_core::config::SavedEnvironmentIntent,
) -> Result<Vec<GitSource>, RpcError> {
    let organization = organization(tx, &stored.summary.id)?;
    let mut sources = Vec::new();
    for (id, mut source) in git_sources(tx, stored, saved)? {
        (source.builders, source.preferred_machine) = builders::walk(
            tx,
            organization.as_str(),
            &stored.summary.environment_id,
            &id,
        )?;
        sources.push(source);
    }
    Ok(sources)
}

/// The Git Services `stored` builds, before any Builder is chosen.
fn unrouted(tx: &mut dyn Tx, stored: &Stored) -> Result<Vec<GitSource>, RpcError> {
    let saved = deployment::saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
    Ok(git_sources(tx, stored, &saved)?
        .into_iter()
        .map(|(_, source)| source)
        .collect())
}

/// Each Git Service of `saved` that `stored` builds, by Service ID.
fn git_sources(
    tx: &mut dyn Tx,
    stored: &Stored,
    saved: &ployz_core::config::SavedEnvironmentIntent,
) -> Result<Vec<(String, GitSource)>, RpcError> {
    let rows = rows(tx, &stored.summary.id)?;
    Ok(saved
        .services
        .iter()
        .filter(|service| stored.nodes.iter().any(|node| node.id() == service.id))
        .filter_map(|service| {
            let row = rows
                .iter()
                .find(|row| row.service == service.config.private_dns);
            Some((service.id.clone(), source_of(service, row)?))
        })
        .collect())
}

fn source_of(
    service: &ployz_core::config::SavedServiceIntent,
    row: Option<&BuildRow>,
) -> Option<GitSource> {
    let config = &service.config;
    let ServiceSource::Git {
        repository,
        repository_id,
        access,
        root_dir,
        branch,
        ..
    } = &config.source
    else {
        return None;
    };
    Some(GitSource {
        service: config.private_dns.clone(),
        repository: RepositoryName::parse(repository.as_str()).ok()?,
        repository_id: RepositoryId::parse(*repository_id).ok()?,
        access: access.clone(),
        branch: match branch {
            ServiceGitBranch::Connected { name } => BranchName::parse(name.as_str()).ok(),
            ServiceGitBranch::Disconnected { .. } => None,
        },
        root_dir: root_dir.clone(),
        dockerfile_path: (config.build.build_method == BuildMethod::Dockerfile).then(|| {
            config
                .build
                .dockerfile_path
                .clone()
                .unwrap_or_else(|| "Dockerfile".into())
        }),
        commit: row.and_then(|row| row.commit.clone()),
        builders: Vec::new(),
        preferred_machine: None,
        status: row.map(|row| row.stage.status().into()),
        message: row.and_then(|row| row.message.clone()),
    })
}

pub(crate) fn organization(tx: &mut dyn Tx, id: &DeploymentId) -> Result<OrganizationId, RpcError> {
    tx.query(
        "SELECT organization_id FROM config_deployment WHERE id = ?1",
        &[id.as_str().into()],
    )?
    .first()
    .ok_or_else(|| error::corrupt("Deployment"))?
    .parse(0, "Deployment")
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

fn trimmed(message: &str) -> String {
    message.chars().take(500).collect()
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
        let unknown = || error::not_found("No GitHub build has this id", json!({}));
        let (deployment, service) = value.split_once('.').ok_or_else(unknown)?;
        Ok(Self {
            deployment: DeploymentId::parse(deployment).map_err(|_| unknown())?,
            service: ServiceName::parse(service).map_err(|_| unknown())?,
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
enum GithubHold {
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
struct Started {
    grant: GithubGrant,
    /// When it checked in, in Unix seconds.
    at: u64,
    /// How many `ployz build --events` lines it took.
    received: u64,
}

impl GithubHold {
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

    const fn end(&self) -> Option<&RunEnd> {
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
    /// It built the image for these platforms.
    Built { platforms: Vec<String> },
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
        .ok_or_else(|| error::not_found("No GitHub build has this id", json!({})))
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
    let github =
        github.map(|github| serde_json::to_string(github).expect("a GitHub build is JSON"));
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
/// changes nothing; any other hand-off, or one after GitHub passed it on, is
/// `conflict`, and Cloud cancels its run.
pub(crate) fn github_dispatched(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    run: &GithubRun,
) -> Result<(), RpcError> {
    let stored = deployment::locked(tx, &id.deployment)?;
    let row = github_row(tx, &stored, id)?;
    if row.building_on(run.run_id).is_some() {
        return Ok(());
    }
    if !stored.summary.status.in_flight()
        || !matches!(row.stage, Stage::Waiting)
        || row.github_skipped.is_some()
    {
        return Err(no_longer_wanted());
    }
    let mut log = row.log;
    append(
        &mut log,
        &format!("Building on GitHub Actions: {}\n", run.run_url),
    );
    let github = GithubHold::Dispatched { run: run.clone() };
    save_github(tx, id, BuildStatus::Building, None, &log, Some(&github))
}

pub(crate) fn github_build(tx: &mut dyn Tx, id: &GithubBuildId) -> Result<GithubHandOff, RpcError> {
    let stored = deployment::load(tx, &id.deployment)?;
    let row = github_row(tx, &stored, id)?;
    if let Some(build) = row.github(id) {
        return Ok(GithubHandOff::Held(Box::new(build)));
    }
    row.github_skipped
        .map(|why| GithubHandOff::PassedOn { why })
        .ok_or_else(|| error::not_found("No GitHub build has this id", json!({})))
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
        .ok_or_else(|| error::not_found("No GitHub build has this id", json!({})))?;
    let source = unrouted(tx, &stored)?
        .into_iter()
        .find(|source| source.service == id.service)
        .ok_or_else(|| error::not_found("No GitHub build has this id", json!({})))?;
    let refused = |message: &str| {
        Err(ployz_core::RpcError {
            code: ployz_core::RpcErrorCode::Unauthenticated,
            message: message.to_owned(),
            details: json!({}),
            cause: Vec::new(),
        })
    };
    if claims.repository_id != source.repository_id.to_string() {
        return refused("The token is for another repository");
    }
    if claims.job_workflow_ref != build.run.workflow_ref {
        return refused("The token is for another workflow or branch");
    }
    if claims.run_id != build.run.run_id.to_string() {
        return refused("The token is for another run");
    }
    if claims.event_name != "workflow_dispatch" {
        return refused("The run was not dispatched by Ployz");
    }
    Ok(build)
}

/// What a GitHub build builds.
#[derive(Clone, Debug, PartialEq)]
pub struct GithubInput {
    /// The Deployment's lowering input, with every value sealed in the browser left
    /// out but the built Service's.
    pub input: serde_json::Value,
    pub commit: CommitSha,
    /// The Service's latest receipt.
    pub receipt: Option<serde_json::Value>,
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
    let built = if sealed {
        let built = saved
            .services
            .iter()
            .find(|service| service.config.private_dns == id.service)
            .ok_or_else(|| error::corrupt("GitHub build Service"))?;
        Some(&built.id)
    } else {
        None
    };
    for snapshot in &mut input.snapshots {
        if snapshot.service_id.as_ref() != built {
            snapshot.resolved_env.retain(|_, value| !value.is_sealed());
        }
    }
    let input = deployment::json_value(&input);
    // The latest receipt of the Service, a cache hint; its own when it has one.
    let receipt = deployment::receipts(tx, &stored.summary.environment_id)?
        .remove(&id.service)
        .and_then(|receipts| receipts.into_iter().next());
    Ok(GithubInput {
        input,
        commit,
        receipt,
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
    let row = github_row(tx, &stored, id)?;
    let Some(GithubHold::Dispatched { run }) = row.building_on(run_id) else {
        return Err(no_longer_wanted());
    };
    if !stored.summary.status.in_flight() {
        return Err(no_longer_wanted());
    }
    let github = GithubHold::CheckedIn {
        run: run.clone(),
        started: Started {
            grant: grant.clone(),
            at: std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |since| since.as_secs()),
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
    let holds = match (run_id, end) {
        (Some(run_id), GithubEnd::Unstarted { .. }) => {
            matches!(row.building_on(run_id), Some(GithubHold::Dispatched { .. }))
        }
        (Some(run_id), _) => row.building_on(run_id).is_some(),
        (None, _) => matches!(row.stage, Stage::Waiting) && row.github_skipped.is_none(),
    };
    if !holds {
        return Err(error::conflict("This build already ended", json!({})));
    }
    let mut log = row.log;
    let (status, message, github) = match end {
        GithubEnd::Built { receipt } => {
            deployment::save_receipt(tx, &stored.summary.environment_id, &id.service, receipt)?;
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
                Some(trimmed(message)),
                row.stage.github(),
            )
        }
        // The next Builder starts it afresh: the run holds it no more.
        GithubEnd::Skipped { message } | GithubEnd::Unstarted { message } => {
            append(&mut log, &format!("GitHub skipped: {message}\n"));
            (BuildStatus::Pending, Some(trimmed(message)), None)
        }
    };
    save_github(tx, id, status, message.as_deref(), &log, github)?;
    if let (GithubEnd::Skipped { .. } | GithubEnd::Unstarted { .. }, Some(why)) = (end, &message) {
        skip(tx, id, why)?;
    }
    Ok(status)
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
