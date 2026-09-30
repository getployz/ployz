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
use crate::storage::{Tx, name_of};

/// The most log a build keeps; older output goes first.
const LOG_LIMIT: usize = 256 * 1024;

/// A Git Service a Deployment builds, and the commit it is pinned to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct GitSource {
    /// Its runtime Service name.
    pub service: ServiceName,
    /// The GitHub repository, as `owner/name`.
    pub repository: RepositoryName,
    /// GitHub's ID for it.
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
    /// The runtime Service it builds.
    pub service: ServiceName,
    pub status: BuildStatus,
    /// Why it failed. Users read it: it holds no secret.
    pub message: Option<String>,
    pub log: String,
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
}

/// One build's log.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct BuildLogQuery {
    pub deployment: DeploymentId,
    /// The Service it built.
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

/// Pin the commits of `commits` (by runtime Service name) that aren't pinned yet,
/// and return every source with its pin. A pinned commit never changes.
pub(crate) fn pin(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    commits: &BTreeMap<ServiceName, CommitSha>,
) -> Result<Vec<GitSource>, RpcError> {
    let stored = deployment::locked(tx, id)?;
    if !stored.summary.status.in_flight() {
        return Err(error::conflict(
            "This Deployment is no longer waiting to run",
            json!({ "deployment": id, "status": stored.summary.status }),
        ));
    }
    let sources = sources_of(tx, &stored)?;
    for (service, commit) in commits {
        if !sources.iter().any(|source| source.service == *service) {
            return Err(error::invalid(
                format!("This Deployment builds no Git Service {service}"),
                json!({ "services": sources.iter().map(|source| &source.service).collect::<Vec<_>>() }),
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
    sources_of(tx, &stored)
}

/// Record a runner's report on one pinned or uploaded build of `stored`, which it
/// claimed.
pub(crate) fn record(
    tx: &mut dyn Tx,
    stored: &Stored,
    report: &BuildReport,
) -> Result<(), RpcError> {
    let id = &stored.summary.id;
    if uploads_of(tx, stored)?.contains(&report.service) {
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
    let mut log = row.log;
    append(&mut log, &report.log);
    let message = report.message.as_deref().map(trimmed);
    tx.execute(
        "UPDATE config_build SET status = ?3, message = ?4, log = ?5 \
         WHERE deployment_id = ?1 AND service = ?2",
        &[
            id.as_str().into(),
            report.service.as_str().into(),
            name_of(report.status).as_str().into(),
            message.as_deref().into(),
            log.as_str().into(),
        ],
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
    status: BuildStatus,
    message: Option<String>,
    log: String,
    /// The GitHub run it was handed to, if any.
    github: Option<GithubState>,
    organization: OrganizationId,
}

impl BuildRow {
    fn view(&self, stored: &Stored) -> BuildView {
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
                    | crate::deployment::TargetNode::Volume { .. } => None,
                })
                .unwrap_or_else(|| self.service.clone()),
            commit: self.commit.clone(),
            status: self.status,
            message: self.message.clone(),
        }
    }
}

fn rows(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Vec<BuildRow>, RpcError> {
    tx.query(
        "SELECT service, commit_sha, status, message, log, github, organization_id \
         FROM config_build \
         WHERE deployment_id = ?1 ORDER BY service",
        &[id.as_str().into()],
    )?
    .iter()
    .map(|row| {
        Ok(BuildRow {
            service: row.parse::<ServiceName>(0, "build")?,
            commit: row.parse_optional(1, "build commit")?,
            status: row.variant(2, "build status")?,
            message: row.optional_text(3)?.map(str::to_owned),
            log: row.text(4)?.to_owned(),
            github: row
                .optional_text(5)?
                .map(|github| {
                    serde_json::from_str(github).map_err(|_| error::corrupt("GitHub build"))
                })
                .transpose()?,
            organization: row.parse(6, "build")?,
        })
    })
    .collect()
}

/// The Services `stored` targets that have no source of their own: they build from
/// its upload.
pub(crate) fn uploads_of(tx: &mut dyn Tx, stored: &Stored) -> Result<Vec<ServiceName>, RpcError> {
    let saved = deployment::saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
    Ok(saved
        .services
        .iter()
        .filter(|service| stored.nodes.iter().any(|node| node.id() == service.id))
        .filter(|service| matches!(service.config.source, ServiceSource::Empty { .. }))
        .map(|service| service.config.private_dns.clone())
        .collect())
}

pub(crate) fn sources_of(tx: &mut dyn Tx, stored: &Stored) -> Result<Vec<GitSource>, RpcError> {
    let saved = deployment::saved_at(tx, &stored.summary.environment_id, stored.summary.saved)?;
    let rows = rows(tx, &stored.summary.id)?;
    let organization = organization(tx, &stored.summary.id)?;
    let mut sources = Vec::new();
    for service in &saved.services {
        if !stored.nodes.iter().any(|node| node.id() == service.id) {
            continue;
        }
        let row = rows
            .iter()
            .find(|row| row.service == service.config.private_dns);
        let Some(mut source) = source_of(service, row) else {
            continue;
        };
        (source.builders, source.preferred_machine) = builders::walk(
            tx,
            organization.as_str(),
            &stored.summary.environment_id,
            &service.id,
        )?;
        sources.push(source);
    }
    Ok(sources)
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
        status: row.map(|row| row.status),
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
    /// Its runtime Service name.
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct GithubState {
    run: GithubRun,
    grant: Option<GithubGrant>,
    checked_in_at: Option<u64>,
    /// How many `ployz build --events` lines it took.
    received: u64,
    /// How it ended, once its final report came.
    ended: Option<RunEnd>,
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
    status: BuildStatus,
    message: Option<&str>,
    log: &str,
    github: Option<&GithubState>,
) -> Result<(), RpcError> {
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
    Ok(())
}

impl BuildRow {
    fn github(&self, id: &GithubBuildId) -> Option<GithubBuild> {
        let github = self.github.as_ref()?;
        Some(GithubBuild {
            id: id.clone(),
            organization: self.organization.clone(),
            status: self.status,
            run: github.run.clone(),
            grant: github.grant.clone(),
            checked_in_at: github.checked_in_at,
            ended: github.ended.clone(),
        })
    }

    /// The run it holds while it builds on GitHub, if `run_id` is that run.
    fn building_on(&self, run_id: u64) -> Option<&GithubState> {
        self.github
            .as_ref()
            .filter(|github| self.status == BuildStatus::Building && github.run.run_id == run_id)
    }
}

/// Hand a pinned, unstarted build to GitHub run `run`. Handing it the same run again
/// changes nothing; any other hand-off is `conflict`, and Cloud cancels its run.
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
        || row.status != BuildStatus::Pending
        || row.github.is_some()
    {
        return Err(no_longer_wanted());
    }
    let mut log = row.log;
    append(
        &mut log,
        &format!("Building on GitHub Actions: {}\n", run.run_url),
    );
    let github = GithubState {
        run: run.clone(),
        grant: None,
        checked_in_at: None,
        received: 0,
        ended: None,
    };
    save_github(tx, id, BuildStatus::Building, None, &log, Some(&github))
}

/// The GitHub build `id`.
pub(crate) fn github_build(tx: &mut dyn Tx, id: &GithubBuildId) -> Result<GithubBuild, RpcError> {
    let stored = deployment::load(tx, &id.deployment)?;
    github_row(tx, &stored, id)?
        .github(id)
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
    let source = sources_of(tx, &stored)?
        .into_iter()
        .find(|source| source.service == id.service)
        .ok_or_else(|| error::not_found("No GitHub build has this id", json!({})))?;
    let refused = |message: &str| {
        Err(ployz_core::RpcError {
            code: ployz_core::RpcErrorCode::Unauthenticated,
            message: message.to_owned(),
            details: json!({}),
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

/// What GitHub build `id` builds: its Deployment's lowering input with secrets
/// unsealed, its pinned commit, and its Service's latest receipt. In-process only.
pub(crate) fn github_input(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    sealing: &crate::SealingKey,
) -> Result<(serde_json::Value, CommitSha, Option<serde_json::Value>), RpcError> {
    let stored = deployment::load(tx, &id.deployment)?;
    let row = github_row(tx, &stored, id)?;
    let commit = row
        .commit
        .ok_or_else(|| error::corrupt("GitHub build commit"))?;
    let input = deployment::input(tx, &stored, sealing)?;
    let receipt = deployment::receipts(tx, &stored.summary.environment_id)?.remove(&id.service);
    Ok((input, commit, receipt))
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
    let Some(github) = row.building_on(run_id) else {
        return Err(no_longer_wanted());
    };
    if !stored.summary.status.in_flight() || github.grant.is_some() {
        return Err(no_longer_wanted());
    }
    let mut github = github.clone();
    github.grant = Some(grant.clone());
    github.checked_in_at = Some(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |since| since.as_secs()),
    );
    save_github(
        tx,
        id,
        row.status,
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
    let Some(github) = row.building_on(run_id) else {
        return Err(error::conflict("This build already ended", json!({})));
    };
    if github.grant.is_none() {
        return Err(error::conflict("This build has not checked in", json!({})));
    }
    if github.ended.is_some() {
        return Err(error::conflict("This build already ended", json!({})));
    }
    if report.from > github.received {
        return Err(error::conflict(
            format!(
                "Build Steps before line {} are missing; send from line {}",
                report.from, github.received
            ),
            json!({ "received": github.received }),
        ));
    }
    let skip = usize::try_from(github.received - report.from).unwrap_or(usize::MAX);
    let mut log = row.log.clone();
    for line in report.lines.iter().skip(skip) {
        append(&mut log, line);
    }
    let mut github = github.clone();
    github.received = github.received.max(report.from + report.lines.len() as u64);
    github.ended.clone_from(&report.ended);
    save_github(
        tx,
        id,
        row.status,
        row.message.as_deref(),
        &log,
        Some(&github),
    )?;
    Ok(github.received)
}

/// End GitHub build `id`: its run `run_id`'s, or with none, before GitHub took it
/// (an earlier image served it, or GitHub couldn't take it). Ending it again, or a
/// run that no longer holds it, is `conflict`.
pub(crate) fn github_end(
    tx: &mut dyn Tx,
    id: &GithubBuildId,
    run_id: Option<u64>,
    end: &GithubEnd,
) -> Result<BuildStatus, RpcError> {
    let stored = deployment::locked(tx, &id.deployment)?;
    let row = github_row(tx, &stored, id)?;
    let holds = match (run_id, end) {
        (Some(run_id), GithubEnd::Unstarted { .. }) => row
            .building_on(run_id)
            .is_some_and(|github| github.grant.is_none()),
        (Some(run_id), _) => row.building_on(run_id).is_some(),
        (None, _) => row.status == BuildStatus::Pending && row.github.is_none(),
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
            (status, None, row.github.as_ref())
        }
        GithubEnd::Failed { message } => {
            append(&mut log, &format!("GitHub: {message}\n"));
            (
                BuildStatus::Failed,
                Some(trimmed(message)),
                row.github.as_ref(),
            )
        }
        // The next Builder starts it afresh.
        GithubEnd::Skipped { message } | GithubEnd::Unstarted { message } => {
            append(&mut log, &format!("GitHub skipped: {message}\n"));
            (BuildStatus::Pending, Some(trimmed(message)), None)
        }
    };
    save_github(tx, id, status, message.as_deref(), &log, github)?;
    Ok(status)
}

/// Deployment `id`'s builds still on GitHub: what cancelling it stops.
pub(crate) fn github_outstanding(
    tx: &mut dyn Tx,
    id: &DeploymentId,
) -> Result<Vec<GithubBuild>, RpcError> {
    Ok(rows(tx, id)?
        .into_iter()
        .filter(|row| row.status == BuildStatus::Building)
        .filter_map(|row| {
            row.github(&GithubBuildId {
                deployment: id.clone(),
                service: row.service.clone(),
            })
        })
        .collect())
}
