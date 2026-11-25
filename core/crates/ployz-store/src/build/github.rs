//! Builds handed to a GitHub Actions run, from its dispatch through its check-in and reports to its end.

use ployz_core::authority::{AutoDeployRule, BuiltImageDigest, CommitAuthority, Signed};
use ployz_core::{MachineId, RpcError, ServiceName};
use ployz_decide::github_build::{EndKind, Hold, Target, github_check_in_holds, github_end_holds};
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
    let row = github_row(tx, &stored, id)?;
    let held = row.building_on(run_id);
    let in_flight = stored.summary.status.in_flight();
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
