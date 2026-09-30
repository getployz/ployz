//! Deployments. Admission freezes what a Deployment ships: a Saved revision and its
//! target nodes, checked to lower. A runner `claim`s it and receives the Deploy
//! Intent lowered from them, secrets unsealed; it `record`s its evidence, and each
//! confirmed Node Outcome advances Applied State.
//! The Head that reviews compare against comes from here too.

pub(crate) mod admit;
mod history;
mod lifecycle;
mod lowering;
pub(crate) mod query;
pub(crate) use history::*;
pub(crate) use lifecycle::*;
pub(crate) use lowering::*;

use std::collections::BTreeMap;

use ployz_core::config::{
    CompiledNodeConfig, EncryptedSecretValue, LowerDeploymentInput, LowerDeploymentSnapshot,
    LowerDeploymentVolume, RuntimeOutcomeProjection, SavedEnvironmentIntent, SavedServiceIntent,
    SavedVolumeIntent, ServiceConfig, ServiceImageCredentials, ServiceSource,
    canonicalize_environment_intent, compile_environment_intent, lower_deployment,
    parse_runtime_preview, project_runtime_outcome,
};
use ployz_core::{
    DeployIntent, DeployOutcome, DeployPreview, DockerVolumeId, ExecutionError, FailedOperation,
    Namespace, RpcError, ServiceAttempt, ServiceName, VolumeRemoval, VolumeRemovalOutcome,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::Actor;
use crate::build::{self, BuildReport, BuildView, GitSource};
use crate::error;
use crate::id::{
    CommitSha, DeploymentId, EnvironmentId, Hostname, OrganizationId, Principal, Revision,
    RunnerId, ServiceLineageId, VolumeId, VolumeName,
};
use crate::registry;
use crate::removal::VolumeLoss;
use crate::review::Head;
use crate::scope::{self, Environment, EnvironmentSummary, revision_param};
use crate::sealing::SealingKey;
use crate::storage::{Row, Tx, name_of};
use crate::variables;

/// Where a Deployment is in its life.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentStatus {
    /// Admitted, waiting for a runner.
    Queued,
    /// A newer admission replaced it before any runner claimed it.
    Superseded,
    /// Its runner claimed it and has not recorded an outcome yet.
    Running,
    /// Every planned operation completed.
    Applied,
    /// It stopped before every operation completed. Confirmed Node Outcomes still count.
    Failed,
    /// Its runner was lost before it recorded an outcome, so what ran is unknown.
    Unknown,
    /// Cancelled while running: its runner stops it and records what ran.
    Cancelling,
    /// Cancelled. Node Outcomes confirmed before it stopped still count.
    Cancelled,
}

impl DeploymentStatus {
    /// Whether it may still run: queued, running or cancelling.
    #[must_use]
    pub const fn in_flight(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Cancelling)
    }
}

/// A Deployment as lists show it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DeploymentSummary {
    pub id: DeploymentId,
    /// The Environment it deploys.
    pub environment_id: EnvironmentId,
    /// Counts from 1 within its Environment.
    pub number: u64,
    pub status: DeploymentStatus,
    /// The Saved revision it ships.
    pub saved: Revision,
    /// The Services it targets; empty targets every Service.
    pub services: Vec<ServiceName>,
    /// The runner that claimed it.
    pub runner: Option<RunnerId>,
    /// What its Services without a source of their own build from.
    pub upload: Option<UploadedSource>,
    /// Whether it removes the Environment from the Servers: it ships the empty
    /// Environment ([`NOTHING`]) and deletes the data it accepted.
    pub remove: bool,
    /// Who admitted it, as Cloud authenticated them; none when the Store's own
    /// automation did, or the hidden local Store.
    pub admitted_by: Option<Principal>,
    /// When it was admitted, in Unix seconds.
    #[ts(type = "number")]
    pub admitted_at: i64,
    /// When its runner claimed it, in Unix seconds.
    #[ts(type = "number | null")]
    pub started_at: Option<i64>,
    /// When it ended, in Unix seconds: its outcome recorded, or cancelled before
    /// it ran.
    #[ts(type = "number | null")]
    pub ended_at: Option<i64>,
    /// What whoever admitted it said it ships.
    pub message: Option<String>,
    /// Whether it may still run: queued, or claimed by a runner still there.
    pub in_flight: bool,
}

/// The most characters a Deployment message has.
pub(crate) const MESSAGE_MAX: usize = 500;

/// How long a runner holds a claimed Deployment without recording anything. Every
/// record renews it; once it lapses the runner is gone and the outcome is unknown.
pub(crate) const LEASE: i64 = 10 * 60;

/// A queued Deployment no runner claimed: its dispatch may have been lost.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Unclaimed {
    pub organization: OrganizationId,
    pub environment: EnvironmentId,
    pub deployment: DeploymentId,
    /// When it was admitted, in Unix seconds.
    #[ts(type = "number")]
    pub admitted_at: i64,
}

/// The Saved revision a removal ships: the empty Environment. Saved revisions count from 1.
pub(crate) const NOTHING: Revision = Revision(0);

/// An Uploaded Source: a directory's content, identified by its digest and never by
/// a commit. Services without a source of their own build from it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct UploadedSource {
    /// Lowercase hex sha256 of the uploaded paths, bytes, modes and links.
    pub digest: String,
    /// The commit the directory was checked out at, if it was a Git checkout.
    /// Provenance only: it never identifies the build.
    #[serde(default)]
    pub base: Option<UploadBase>,
    /// Who uploaded it, as Cloud authenticated them; admission overwrites whatever a
    /// caller sends. Provenance only.
    #[serde(default)]
    #[ts(optional = nullable)]
    pub uploader: Option<Principal>,
}

/// Where an upload came from: "base abc123", plus "+ changes" when it differs from it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct UploadBase {
    pub commit: CommitSha,
    /// Whether the directory held changes the commit doesn't.
    pub changed: bool,
}

impl UploadedSource {
    pub(crate) fn check(&self) -> Result<(), RpcError> {
        if ployz_core::is_lower_hex(&self.digest, 64) {
            return Ok(());
        }
        Err(error::invalid(
            "An upload names a lowercase sha256 digest",
            json!({ "upload": self }),
        ))
    }
}

/// A Deployment with its recorded Deploy Preview and every Node Outcome.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct DeploymentView {
    #[serde(flatten)]
    pub deployment: DeploymentSummary,
    pub environment: EnvironmentSummary,
    /// The runtime Namespace it deploys into.
    pub namespace: Namespace,
    /// Every node it targets.
    pub nodes: Vec<NodeOutcome>,
    /// The Deploy Preview its runner prepared, with environment values removed.
    pub preview: Option<Value>,
    pub outcome: Option<Outcome>,
    /// Its Git Services' builds, once their commits are pinned.
    pub builds: Vec<BuildView>,
    /// Each Service's runtime name (its Private DNS name) by its name when admitted,
    /// for finding its containers.
    pub runtime_names: BTreeMap<ServiceName, ServiceName>,
}

/// What a Deployment did to one of its target nodes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct NodeOutcome {
    #[serde(flatten)]
    pub node: DeployedNode,
    pub outcome: NodeStatus,
}

/// A Node Outcome, or why a node has none.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum NodeStatus {
    /// The Deployment has not recorded an outcome yet.
    Pending,
    /// Every planned operation for it completed: Applied State holds it now.
    Deployed,
    /// It was taken off the Servers: a removed Service, or a Volume whose data is gone.
    Removed,
    /// Work on it started and didn't finish, so Applied State kept the old one.
    Failed,
    /// An earlier failure, or cancellation, stopped the Deployment before it.
    NotAttempted,
    /// It needed no operation.
    Unchanged,
    /// Its runner was lost before recording what ran.
    Unknown,
}

impl NodeStatus {
    /// Whether Applied State holds the node as this Deployment shipped it.
    pub(crate) const fn advances(self) -> bool {
        matches!(self, Self::Deployed | Self::Removed)
    }
}

/// What a Deployment's runner recorded at its end.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outcome {
    /// Execution ran. `summary` counts operations, without inputs or provider
    /// messages; each node's Node Outcome is on the Deployment's nodes. `reason`
    /// says why it failed, naming the Service by its current name; users read it.
    Executed {
        summary: Value,
        #[serde(default)]
        reason: Option<String>,
    },
    /// Nothing executed: preparation failed first. `needs_upload` names the Services
    /// that can build only from a new upload.
    NotExecuted {
        reason: String,
        #[serde(default)]
        needs_upload: Vec<ServiceName>,
    },
}

/// Evidence a runner records about the Deployment it claimed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "evidence", content = "value", rename_all = "snake_case")]
pub enum RunEvidence {
    /// The Deploy Preview preparation produced, recorded before it is confirmed.
    Prepared(DeployPreview),
    /// What executing that preview did, and what deleting the Docker Volumes of the
    /// Volumes it removes did: the runner deletes them only after a successful Deploy.
    Executed {
        outcome: Box<DeployOutcome<ExecutionError>>,
        #[serde(default)]
        removed: Vec<VolumeRemoval>,
    },
    /// Preparation failed, so nothing executed. Users read the reason: it holds no secret.
    NotExecuted(String),
    /// The runner stopped without knowing whether it executed: the outcome is unknown
    /// once it recorded a Deploy Preview, and nothing executed before that.
    Abandoned,
    /// The build receipts preparation produced, by runtime Service name. Each replaces
    /// that Service's latest receipt in the Environment.
    Built(BTreeMap<ServiceName, Value>),
    /// Progress and log output of one Git or uploaded build.
    Build(BuildReport),
    /// Nothing executed: these uploaded Services have no upload to build from and no
    /// usable image to reuse.
    UploadNeeded(Vec<ServiceName>),
    /// The runner still holds the Deployment: it renews the lease and reads the
    /// status back, which says whether it was cancelled.
    Alive,
}

/// A claimed Deployment and the frozen Deploy Intent its runner executes.
#[derive(Clone, Debug, PartialEq)]
pub struct Claimed {
    pub deployment: DeploymentSummary,
    /// Services without a source of their own are left out; they build from
    /// `deployment.upload` through the SDK's preparation of `input`.
    pub intent: DeployIntent,
    /// The lowering input `intent` came from: what SDK preparation takes.
    pub input: Value,
    /// The latest build receipt of each Service, by runtime name: hints preparation verifies.
    pub receipts: BTreeMap<ServiceName, Value>,
    /// The Git Services it builds, each with its pinned commit, if any.
    pub sources: Vec<GitSource>,
    /// The Docker Volumes to delete once the Deploy succeeds: exactly those whose
    /// loss admission accepted, each on its Server.
    pub deletes: Vec<DockerVolumeId>,
    /// The Services it builds from `deployment.upload`, by runtime name.
    pub uploads: Vec<ServiceName>,
    /// The Organization's Build Order as the build starts. Uploaded Services skip
    /// GitHub in it: GitHub can't build uploaded source, so they build on Servers.
    pub build_order: Vec<crate::Builder>,
}

/// One node a Deployment targets, as frozen at admission.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub(crate) enum TargetNode {
    Service {
        id: ServiceLineageId,
        name: ServiceName,
        /// The runtime Service it lowers to, which its Node Outcome is confirmed by.
        runtime: ServiceName,
    },
    Volume {
        id: VolumeId,
        name: VolumeName,
        /// For a Volume the Deployment removes: the Docker Volumes it deletes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deletes: Option<Vec<DockerVolumeId>>,
    },
}

impl TargetNode {
    fn service(service: &SavedServiceIntent) -> Result<Self, RpcError> {
        Ok(Self::Service {
            id: parse_stored(&service.id)?,
            name: ServiceName::parse(service.slug.as_str())
                .map_err(|_| error::corrupt("Service name"))?,
            runtime: service.config.private_dns.clone(),
        })
    }

    fn volume(
        volume: &SavedVolumeIntent,
        deletes: Option<Vec<DockerVolumeId>>,
    ) -> Result<Self, RpcError> {
        Ok(Self::Volume {
            id: parse_stored(&volume.resource_id)?,
            name: parse_stored(&volume.name)?,
            deletes,
        })
    }

    /// Its node ID.
    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Service { id, .. } => id.as_str(),
            Self::Volume { id, .. } => id.as_str(),
        }
    }

    /// The node as views name it.
    fn shown(&self) -> DeployedNode {
        match self {
            Self::Service { id, name, .. } => DeployedNode::Service {
                id: id.clone(),
                name: name.clone(),
            },
            Self::Volume { id, name, .. } => DeployedNode::Volume {
                id: id.clone(),
                name: name.clone(),
            },
        }
    }
}

/// A node a Deployment targets, by its identity and its name when admitted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum DeployedNode {
    Service {
        id: ServiceLineageId,
        name: ServiceName,
    },
    Volume {
        id: VolumeId,
        name: VolumeName,
    },
}

impl DeployedNode {
    /// Its name when admitted.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Service { name, .. } => name.as_str(),
            Self::Volume { name, .. } => name.as_str(),
        }
    }

    /// Its node ID.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Service { id, .. } => id.as_str(),
            Self::Volume { id, .. } => id.as_str(),
        }
    }
}

/// What admission freezes, besides the Saved revision.
pub(crate) struct Frozen {
    pub(crate) nodes: Vec<TargetNode>,
    pub(crate) namespace: Namespace,
    /// Sealed registry credentials by runtime Service, fixed at admission.
    pub(crate) credentials: BTreeMap<ServiceName, EncryptedSecretValue>,
    /// The Cluster Domain its generated domains expand under.
    pub(crate) cluster_domain: Option<Hostname>,
    /// Its target Services with no source of their own: they run only an upload.
    pub(crate) sourceless: Vec<String>,
}

impl Frozen {
    /// Whether this Deployment targets node `id`.
    pub(crate) fn targets(&self, id: &str) -> bool {
        self.nodes.iter().any(|node| node.id() == id)
    }
}

/// The runner's evidence so far, stored as one document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Run {
    preview: Option<Value>,
    outcome: Option<Outcome>,
    /// Each target node's Node Outcome, by node ID, once execution ran.
    #[serde(default)]
    nodes: BTreeMap<String, NodeStatus>,
}

pub(crate) struct Stored {
    pub(crate) summary: DeploymentSummary,
    namespace: Namespace,
    cluster_domain: Option<Hostname>,
    pub(crate) nodes: Vec<TargetNode>,
    run: Run,
    /// Until when, in Unix seconds, its runner holds it without recording anything.
    lease: i64,
}

const COLUMNS: &str = "id, environment_id, number, status, saved_revision, services, nodes, \
     namespace, run, upload, cluster_domain, runner, lease, admitted_by, admitted, started, ended, \
     message";

/// SQL selecting Deployments that may still run: queued, or claimed by a runner
/// whose lease holds. The inverse of [`lapsed`] for claimed ones.
pub(crate) fn in_flight_sql() -> String {
    format!(
        "(status = 'queued' OR (status IN ('running', 'cancelling') AND lease > {}))",
        now()
    )
}

/// Whether a claimed Deployment's runner is gone: its lease lapsed, so what ran
/// is unknown. Every read remaps a lapsed one to `unknown`.
fn lapsed(status: DeploymentStatus, lease: i64) -> bool {
    matches!(
        status,
        DeploymentStatus::Running | DeploymentStatus::Cancelling
    ) && lease <= now()
}

/// The Cluster Domain `environment`'s latest Deployment that had one expanded its
/// generated domains under, if any.
pub(crate) fn cluster_domain(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<Hostname>, RpcError> {
    tx.query(
        "SELECT cluster_domain FROM config_deployment \
         WHERE environment_id = ?1 AND cluster_domain IS NOT NULL ORDER BY number DESC LIMIT 1",
        &[environment.as_str().into()],
    )?
    .first()
    .map(|row| parse_stored(row.text(0)?))
    .transpose()
}

/// The Namespace `environment` deploys into: fixed at its first admission, so renames
/// never move it. `{project}-{environment}` unless another Environment of the
/// Organization holds that, then suffixed with this Environment's ID. `reserve`
/// fixes a new one; a plan only looks.
pub(crate) fn namespace(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentSummary,
    reserve: bool,
) -> Result<Namespace, RpcError> {
    let rows = tx.query(
        "SELECT namespace FROM config_namespace WHERE environment_id = ?1",
        &[environment.id.as_str().into()],
    )?;
    if let Some(row) = rows.first() {
        return row.parse::<Namespace>(0, "Namespace");
    }
    let base = format!("{}-{}", environment.project, environment.name);
    let suffix: String = environment
        .id
        .as_str()
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect::<String>()
        .to_ascii_lowercase();
    let short = base.get(..54).unwrap_or(&base).trim_end_matches('-');
    for candidate in [base.clone(), format!("{short}-{suffix}")] {
        let Ok(namespace) = Namespace::parse(candidate.as_str()) else {
            continue;
        };
        let taken = !tx
            .query(
                "SELECT environment_id FROM config_namespace \
                 WHERE organization_id = ?1 AND namespace = ?2",
                &[who.organization.as_str().into(), namespace.as_str().into()],
            )?
            .is_empty();
        if namespace.is_reserved() || taken {
            continue;
        }
        if reserve {
            tx.execute(
                "INSERT INTO config_namespace (organization_id, namespace, environment_id) \
                 VALUES (?1, ?2, ?3)",
                &[
                    who.organization.as_str().into(),
                    namespace.as_str().into(),
                    environment.id.as_str().into(),
                ],
            )?;
        }
        return Ok(namespace);
    }
    Err(error::conflict(
        "No free Namespace for this Environment: rename it",
        json!({}),
    ))
}

/// Deployment `id` of `who`'s Organization.
fn owned(tx: &mut dyn Tx, who: &Actor, id: &DeploymentId) -> Result<Stored, RpcError> {
    let rows = tx.query(
        &format!("SELECT {COLUMNS} FROM config_deployment WHERE id = ?1 AND organization_id = ?2"),
        &[id.as_str().into(), who.organization.as_str().into()],
    )?;
    stored(rows.first().ok_or_else(|| missing(id))?)
}

/// Load a Deployment with its Environment locked, so admissions, claims and records
/// of one Environment apply in turn.
pub(crate) fn locked(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Stored, RpcError> {
    let environment = load(tx, id)?.summary.environment_id;
    scope::lock_all(tx, [environment])?;
    load(tx, id)
}

/// Load a Deployment of any Organization: in-process runner calls only.
pub(crate) fn load(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Stored, RpcError> {
    let rows = tx.query(
        &format!("SELECT {COLUMNS} FROM config_deployment WHERE id = ?1"),
        &[id.as_str().into()],
    )?;
    stored(rows.first().ok_or_else(|| missing(id))?)
}

fn stored(row: &Row) -> Result<Stored, RpcError> {
    let status: DeploymentStatus = row.variant(3, "Deployment status")?;
    let lease = row.int(12)?;
    let status = match lapsed(status, lease) {
        true => DeploymentStatus::Unknown,
        false => status,
    };
    Ok(Stored {
        summary: DeploymentSummary {
            id: row.parse(0, "identity")?,
            environment_id: row.parse(1, "identity")?,
            number: row.number(2, "Deployment")?,
            status,
            saved: row.number(4, "revision")?,
            services: row.json(5, "Deployment")?,
            runner: row.parse_optional(11, "identity")?,
            upload: row.json(9, "Deployment upload")?,
            remove: row.number::<Revision>(4, "revision")? == NOTHING,
            admitted_by: row.parse_optional(13, "identity")?,
            admitted_at: row.int(14)?,
            started_at: row.optional_int(15)?,
            ended_at: row.optional_int(16)?,
            message: row.optional_text(17)?.map(str::to_owned),
            in_flight: status.in_flight(),
        },
        nodes: row.json(6, "Deployment")?,
        namespace: row.parse::<Namespace>(7, "Namespace")?,
        run: row.json(8, "Deployment run")?,
        cluster_domain: row.parse_optional(10, "identity")?,
        lease,
    })
}

/// Write `stored`'s status and run, keeping `in_flight` in step with its status.
fn save(tx: &mut dyn Tx, stored: &mut Stored) -> Result<(), RpcError> {
    stored.summary.in_flight = stored.summary.status.in_flight();
    tx.execute(
        "UPDATE config_deployment SET status = ?1, run = ?2, runner = ?4, lease = ?5, \
         started = ?6, ended = ?7 WHERE id = ?3",
        &[
            name_of(stored.summary.status).as_str().into(),
            json_text(&stored.run).as_str().into(),
            stored.summary.id.as_str().into(),
            stored.summary.runner.as_ref().map(RunnerId::as_str).into(),
            stored.lease.into(),
            stored.summary.started_at.into(),
            stored.summary.ended_at.into(),
        ],
    )?;
    Ok(())
}

fn running(stored: &Stored) -> Result<(), RpcError> {
    match stored.summary.status {
        DeploymentStatus::Running | DeploymentStatus::Cancelling => Ok(()),
        DeploymentStatus::Unknown => Err(error::conflict(
            "This Deployment's runner lost it, or another took over, so its outcome stays \
             unknown. Start a new Deployment",
            json!({ "deployment": stored.summary.id }),
        )),
        DeploymentStatus::Applied | DeploymentStatus::Failed | DeploymentStatus::Cancelled => {
            Err(ended(&stored.summary.id))
        }
        DeploymentStatus::Queued | DeploymentStatus::Superseded => Err(error::conflict(
            "Claim this Deployment before recording what it did",
            json!({ "deployment": stored.summary.id }),
        )),
    }
}

pub(crate) fn saved_at(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    revision: Revision,
) -> Result<SavedEnvironmentIntent, RpcError> {
    if revision == NOTHING {
        return scope::load_by_id(tx, environment)
            .map(|loaded| crate::scope::empty(&loaded.working.environment_slug));
    }
    let rows = tx.query(
        "SELECT intent FROM config_saved WHERE environment_id = ?1 AND revision = ?2",
        &[
            environment.as_str().into(),
            revision_param(revision)?.into(),
        ],
    )?;
    rows.first()
        .ok_or_else(|| error::corrupt("Saved State"))?
        .intent(0, "Saved State")
        .map(canonicalize_environment_intent)
}


fn parse_stored<T: TryFrom<String, Error = RpcError>>(value: &str) -> Result<T, RpcError> {
    T::try_from(value.to_owned()).map_err(|_| error::corrupt("identity"))
}

fn json_text(value: &impl Serialize) -> String {
    serde_json::to_string(value).expect("stored documents are JSON")
}

fn missing(id: &DeploymentId) -> RpcError {
    error::not_found(format!("No Deployment {id}"), json!({}))
}

fn superseded(id: &DeploymentId) -> RpcError {
    error::conflict(
        "A newer Deployment replaced this one before it started",
        json!({ "deployment": id }),
    )
}

fn owned_elsewhere(id: &DeploymentId) -> RpcError {
    error::conflict(
        "Another runner owns this Deployment",
        json!({ "deployment": id }),
    )
}

fn ended(id: &DeploymentId) -> RpcError {
    error::conflict("This Deployment already ended", json!({ "deployment": id }))
}

fn invalid_evidence(what: &str) -> RpcError {
    error::invalid(
        format!("The runner's {what} does not match this Deployment"),
        json!({}),
    )
}

/// Seconds since the Unix epoch: when a Deployment was admitted, which the idle
/// rule for Branches reads.
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}
