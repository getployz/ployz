//! Deployments. Admission freezes what a Deployment ships: a Saved revision and its
//! target nodes, checked to lower. A runner `claim`s it and receives the Deploy
//! Intent lowered from them, secrets still sealed; it `record`s its evidence, and each
//! confirmed Node Outcome advances Applied State.
//! The Head that reviews compare against comes from here too.

pub(crate) mod admit;
pub(crate) mod applied;
mod claim;
pub(crate) mod cleanup;
mod history;
mod landing;
mod lifecycle;
pub(crate) mod loss;
mod lowering;
#[cfg(test)]
mod node_tests;
mod opening;
mod outcomes;
pub(crate) mod query;
mod record;
#[cfg(test)]
mod reference_tests;
#[cfg(test)]
mod retire_tests;
mod retry;
mod rows;
mod status;
#[cfg(test)]
mod status_tests;
pub(crate) mod steps;
mod targeting;
pub(crate) mod uploads;
pub(crate) mod versions;
pub(crate) use claim::*;
pub(crate) use history::*;
pub(crate) use landing::*;
pub(crate) use lifecycle::*;
pub(crate) use lowering::*;
pub(crate) use opening::Opening;
use outcomes::*;
pub(crate) use record::*;
pub(crate) use retry::*;
pub(crate) use rows::server_names;
pub(crate) use status::*;
pub(crate) use targeting::Targeting;
use targeting::scope_sql;
pub use versions::{
    Parent, ParentQuery, ParentView, Pinned, PostedVersion, Running, RunningQuery, RunningView,
    TagPin, VersionsFields, VersionsQuery, VersionsView,
};

pub use ployz_core::deploy_progress::{
    FailedAt, Failure, RowPhase, RowState, RowTracker, RowTransition, ServerProgress, ServerRow,
    StoppedBy,
};
pub use ployz_core::deploy_steps::{DeploymentStep, StepName};

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::{
    CompiledNodeConfig, EnvironmentNodeType, LowerDeploymentConfig, LowerDeploymentConfigFile,
    LowerDeploymentInput, LowerDeploymentSnapshot, LowerDeploymentVolume, RuntimeOutcomeProjection,
    SavedConfigIntent, SavedEnvironmentIntent, SavedServiceIntent, SavedVolumeIntent, SealedFor,
    ServiceConfig, ServiceSource, ValuePartOwner, canonicalize_environment_intent,
    compile_environment_intent, lower_deployment, parse_runtime_preview, project_runtime_outcome,
    redacted_runtime_preview,
};
use ployz_core::{
    BlockedKind, ConfigFileName, ConfigName, DeployIntent, DeployOutcome, DeployPreview,
    DeployWarning, DockerVolumeId, ExecutionError, FailedOperation, MachineId, Namespace,
    PruneScope, Region, RegionWait, RpcError, ServiceAttempt, ServiceName, ServiceOutcome,
    ServiceRemovals, VolumeRemoval, VolumeRemovalOutcome,
};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use ts_rs::TS;

use crate::Actor;
use crate::build::{self, BuildReport, BuildView, GitSource};
use crate::error;
use crate::id::{
    BranchName, CommitSha, ConfigId, DeploymentId, EnvironmentId, Hostname, OrganizationId,
    Principal, RepositoryId, Revision, RunnerId, ServiceLineageId, VolumeId, VolumeName,
};
use crate::review::{Head, InFlight};
use crate::scope::{self, Environment, EnvironmentSummary, revision_param};
use crate::storage::json_text;
use crate::storage::{Row, Tx, name_of, now};
use crate::variables;

use self::loss::VolumeLoss;

/// Where a Deployment is in its life.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DeploymentStatus {
    /// A push held until its commit's CI passes: unnumbered, nothing frozen, built
    /// or claimed. It holds no slot and is no pending attempt; passing CI queues it,
    /// numbered next.
    WaitingForCi,
    /// Admitted, waiting for a runner.
    Queued,
    /// A newer admission replaced it before any runner claimed it.
    Superseded,
    /// Its runner claimed it and has not recorded an outcome yet.
    Running,
    /// Every planned operation completed. The planner may have left Services
    /// waiting, which the outcome lists: it planned no operation for them.
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

    /// Whether it ended: it will never run, or ran and stopped. Neither in flight,
    /// replaced, nor waiting for CI.
    #[must_use]
    pub const fn ended(self) -> bool {
        match self {
            Self::Applied | Self::Failed | Self::Unknown | Self::Cancelled => true,
            Self::WaitingForCi
            | Self::Queued
            | Self::Superseded
            | Self::Running
            | Self::Cancelling => false,
        }
    }
}

/// The push of a Git branch that admitted a Deployment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct GitPush {
    pub repository_id: RepositoryId,
    pub branch: BranchName,
    pub head: CommitSha,
}

/// What the check suites of a waiting Deployment's commit say so far.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CiVerdict {
    /// None reported yet, or some still run. Every one passing queues it.
    Pending,
    /// One completed without passing. A rerun that passes still queues it.
    Failed,
}

/// A Deployment as lists show it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct DeploymentSummary {
    pub id: DeploymentId,
    pub environment_id: EnvironmentId,
    /// Counts from 1 within its Environment, in the order Deployments queued. None
    /// while it waits for CI, and ever after when it never queued.
    pub number: Option<u64>,
    pub status: DeploymentStatus,
    /// The Saved revision it ships.
    pub saved: Revision,
    /// The Services it targets; empty targets every Service.
    pub services: Vec<ServiceName>,
    /// Every Service it ships or takes down, as frozen at admission: the ones it
    /// targets, the ones they reference and the ones it removes. Empty while it
    /// waits for CI.
    #[serde(default)]
    pub ships: Vec<ServiceName>,
    /// The runner that claimed it.
    pub runner: Option<RunnerId>,
    /// What its Services without a source of their own build from.
    pub upload: Option<UploadedSource>,
    /// Whether `upload` is chosen again as it starts: it sent none, so its Services
    /// build from the upload each last ran then, which a Deployment ahead of it may
    /// still change.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub chooses_upload_at_start: bool,
    /// Whether it runs but its runner hasn't recorded a Deploy Preview yet: it still
    /// unpacks its upload, builds, and plans.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub preparing: bool,
    /// Whether it applied without deploying or removing a Service: the planner
    /// left all it had to do waiting, so its Servers run what they ran before it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub all_waiting: bool,
    /// Whether it removes the Environment from the Servers: it ships the empty
    /// Environment ([`NOTHING`]) and deletes the data it accepted.
    pub remove: bool,
    /// Whether it also removes every Service running in its Namespace that this
    /// Store never held: another Store's, or one deployed directly. Without it, a
    /// full Deploy removes only the Services this Store dropped.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub prune: bool,
    /// Who admitted it, as Cloud authenticated them; none when the Store's own
    /// automation did, or the hidden local Store.
    pub admitted_by: Option<Principal>,
    /// When it was admitted, in Unix seconds.
    #[ts(type = "number")]
    pub admitted_at: i64,
    /// When its runner claimed it, in Unix seconds.
    #[ts(type = "number | null")]
    pub started_at: Option<i64>,
    /// When the Deploy confirmed its traffic switch, in Unix seconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "number")]
    pub live_at: Option<i64>,
    /// Cleanup can keep running after this Deployment releases its Environment.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub cleanup: Option<CleanupStatus>,
    /// When it left flight, in Unix seconds; none while it waits for CI, is queued, or
    /// is claimed and its runner lease holds. Its runner recording an outcome, or a
    /// cancel, sets it then. A newer admission superseding it sets it then too, though
    /// [`DeploymentStatus::ended`] doesn't count a superseded one as ended. One whose
    /// runner was lost reads Unknown, ended at the earlier of its runner lease and when
    /// that was found: a newer claim took it over, its runner claimed it again or gave
    /// up after preparing, or its runner lease lapsed. Its runner may still act until
    /// that lease. An Interrupted outcome its runner records after moves it to then.
    #[ts(type = "number | null")]
    pub ended_at: Option<i64>,
    /// What whoever admitted it said it ships.
    pub message: Option<String>,
    /// The push that admitted it; none for a manual Deploy, a retry or a removal.
    pub push: Option<GitPush>,
    /// What its commit's CI says, exactly while it waits for CI.
    pub ci: Option<CiVerdict>,
    /// Whether it waits for CI, or was cancelled while it did: it never froze what
    /// it ships, so it can't be redeployed.
    #[serde(default)]
    pub held: bool,
    /// Whether it may still run: queued, or claimed by a runner still there.
    pub in_flight: bool,
    /// What its runner recorded at its end; none while it hasn't ended.
    pub outcome: Option<Outcome>,
    /// What its runner was warned of when it claimed it, and the Ingress Proxies its
    /// Deploy left down or couldn't check.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<DeploymentWarning>>")]
    pub warnings: Vec<DeploymentWarning>,
    /// Each Server's Ingress Proxy it created or started.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<IngressRepair>>")]
    pub ingress: Vec<IngressRepair>,
}

impl DeploymentSummary {
    /// How a list names it: `#N` once numbered, else the short commit of the push
    /// it holds.
    #[must_use]
    pub fn label(&self) -> String {
        match (self.number, &self.push) {
            (Some(number), _) => format!("#{number}"),
            (None, Some(push)) => push.head.short().to_owned(),
            (None, None) => self.id.as_str().get(..8).unwrap_or_default().to_owned(),
        }
    }

    /// Its number, which every Deployment that queued has.
    pub(crate) fn numbered(&self) -> Result<u64, RpcError> {
        self.number.ok_or_else(|| {
            error::conflict(
                "This Deployment never queued: it waited for CI",
                json!({ "deployment": self.id }),
            )
        })
    }
}

/// What a Deployment ships despite. It still ships.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum DeploymentWarning {
    /// A Config file whose resolved variable may break the file's syntax: it holds
    /// a newline or a quote inside quotes, or a newline in YAML.
    Fragile {
        config: ConfigName,
        file: ConfigFileName,
        /// The variable as the file references it, such as `api.TOKEN`.
        variable: String,
    },
    /// A Service variable left out: it is, or reads, a secret without a value, or
    /// with `broken`, directly or through another variable, a Service or variable
    /// that isn't there.
    NotSet {
        service: ServiceName,
        /// The variable's key, such as `TOKEN`.
        variable: String,
        /// Clients before broken references read it as Not set.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(optional, as = "Option<bool>")]
        broken: bool,
        /// The other variable without a value it reads, such as `db.TOKEN`: a name,
        /// never a value. Clients before 0.3.13 read it as Not set.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        reads: Option<String>,
        /// What `reads` names holds a value sealed for another Environment: a node
        /// a Branch shares with its parent, whose secrets it never gets.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(optional, as = "Option<bool>")]
        sealed_elsewhere: bool,
    },
    /// A Config file reference left empty: it is, or reads, a secret without a
    /// value, or with `broken`, a Service or variable that isn't there.
    NotSetInFile {
        config: ConfigName,
        file: ConfigFileName,
        /// The variable as the file references it, such as `api.TOKEN`.
        secret: String,
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(optional, as = "Option<bool>")]
        broken: bool,
    },
    /// A Server's Ingress Proxy the Deploy created or started isn't running.
    IngressNotServing {
        server: String,
        ingress: IngressFailure,
    },
    /// The Deploy couldn't check the Cluster's Ingress Proxies, so it brought none back.
    IngressUnchecked { cause: String },
    /// Something the Deploy found while executing, such as an image that exposes a
    /// port none of a Service's domains send to.
    Deploy { warning: DeployWarning },
}

impl DeploymentWarning {
    /// The Deploy's own warning, when it is one.
    #[must_use]
    pub fn as_deploy(&self) -> Option<&DeployWarning> {
        match self {
            Self::Deploy { warning } => Some(warning),
            Self::Fragile { .. }
            | Self::NotSet { .. }
            | Self::NotSetInFile { .. }
            | Self::IngressNotServing { .. }
            | Self::IngressUnchecked { .. } => None,
        }
    }

    /// Why the Deploy couldn't check the Ingress Proxies, when that is what it warns of.
    #[must_use]
    pub fn ingress_unchecked(&self) -> Option<&str> {
        match self {
            Self::IngressUnchecked { cause } => Some(cause),
            Self::Fragile { .. }
            | Self::NotSet { .. }
            | Self::NotSetInFile { .. }
            | Self::IngressNotServing { .. }
            | Self::Deploy { .. } => None,
        }
    }

    /// Whether it holds only names the Config Store wrote, such as a variable a Deploy
    /// left out, rather than text a Server reported.
    #[must_use]
    pub fn is_named(&self) -> bool {
        match self {
            Self::Fragile { .. } | Self::NotSet { .. } | Self::NotSetInFile { .. } => true,
            Self::IngressNotServing { .. }
            | Self::IngressUnchecked { .. }
            | Self::Deploy { .. } => false,
        }
    }
}

/// What a Deploy did to one Server's missing or stopped Ingress Proxy. It only
/// creates or starts one; it never replaces or removes what runs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct IngressRepair {
    pub machine: MachineId,
    pub server: String,
    pub action: IngressAction,
    /// Why it isn't running; none once it started.
    pub failure: Option<IngressFailure>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum IngressAction {
    Create,
    Start,
}

/// Why a Server's Ingress Proxy didn't start. Users read `cause`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum IngressFailure {
    /// Something else holds port 80 or 443. The Server starts it once they're free.
    PortTaken {
        cause: String,
    },
    Failed {
        cause: String,
    },
}

impl IngressFailure {
    #[must_use]
    pub fn cause(&self) -> &str {
        match self {
            Self::PortTaken { cause } | Self::Failed { cause } => cause,
        }
    }
}

/// The most characters a Deployment message has.
pub(crate) const MESSAGE_MAX: usize = 500;

/// `text` cut to [`MESSAGE_MAX`] characters.
pub(crate) fn clipped(text: &str) -> String {
    text.chars().take(MESSAGE_MAX).collect()
}

/// How long a runner holds a claimed Deployment without renewing its lease. Once
/// it lapses the runner is gone and the outcome is unknown.
pub(crate) const LEASE: i64 = 10 * 60;

/// The frozen credentials a Store from before sealed logins reads: none, so it still
/// deploys public images during a rollout. A later release drops the column.
pub(crate) const NO_LEGACY_CREDENTIALS: &str = "{}";

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
    pub digest: ployz_core::UploadDigest,
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

/// The runner reported its runtime outcome and has yet to confirm cleanup ended.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum CleanupStatus {
    Pending,
    Unconfirmed,
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
    pub preview: Option<DeployPreview>,
    /// Its Git Services' builds, once their commits are pinned.
    pub builds: Vec<BuildView>,
    /// Each Service's runtime name (its Private DNS name) by its name when admitted,
    /// for finding its containers.
    pub runtime_names: BTreeMap<ServiceName, ServiceName>,
    /// The numbers of the Deployments after it that may have changed the Servers,
    /// oldest first: what redeploying it rolls back. Empty when it rolls nothing back.
    #[serde(default)]
    pub rolls_back: Vec<u64>,
    /// Rows for runtime Services its preview removes as obsolete and no node targets,
    /// by runtime name: Services this Store doesn't hold, which a full reconciliation
    /// removes.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(as = "Option<BTreeMap<ServiceName, Vec<ServerRow>>>", optional)]
    pub untargeted: BTreeMap<ServiceName, Vec<ServerRow>>,
    /// When each step of its Deploy ran, in the order they started.
    #[serde(default)]
    pub steps: Vec<DeploymentStep>,
}

/// What a Deployment did to one of its target nodes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct NodeOutcome {
    #[serde(flatten)]
    pub node: DeployedNode,
    pub outcome: NodeStatus,
    /// The Deployment's cancel stopped its work, so it reads Cancelled. Set only
    /// on a Failed `outcome`, which a reader from before Cancelled reads.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub cancelled: bool,
    /// Its work on each Server, once its runner started executing.
    #[serde(default)]
    pub rows: Vec<ServerRow>,
}

impl NodeOutcome {
    /// Its outcome as users read it: a Failed one the Deployment's cancel stopped
    /// reads Cancelled.
    #[must_use]
    pub const fn label(&self) -> NodeLabel {
        match self.outcome {
            NodeStatus::Pending => NodeLabel::Pending,
            NodeStatus::Deployed => NodeLabel::Deployed,
            NodeStatus::Removed => NodeLabel::Removed,
            NodeStatus::Failed if self.cancelled => NodeLabel::Cancelled,
            NodeStatus::Failed => NodeLabel::Failed,
            NodeStatus::NotAttempted => NodeLabel::NotAttempted,
            NodeStatus::Unchanged => NodeLabel::Unchanged,
            NodeStatus::Unknown => NodeLabel::Unknown,
        }
    }
}

/// A [`NodeStatus`] with Failed split by whether the Deployment's cancel stopped it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum NodeLabel {
    Pending,
    Deployed,
    Removed,
    Failed,
    Cancelled,
    NotAttempted,
    Unchanged,
    Unknown,
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
    /// An earlier failure, or cancellation, stopped the Deployment before it; or
    /// nothing it ran does its work (a new Volume no Service mounts).
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

    /// Whether Applied State takes the node from a Deployment that `succeeded` or
    /// not: once it succeeded, an Unchanged node too, since the Deploy covered it
    /// even when the runtime had nothing to do (a rename, a tag).
    pub(crate) const fn applied(self, succeeded: bool) -> bool {
        self.advances() || (succeeded && matches!(self, Self::Unchanged))
    }
}

/// A Service a Deploy left waiting on a condition outside its config.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct WaitingService {
    /// Its current name.
    pub service: ServiceName,
    /// What it waits on, in the planner's words; users read it.
    pub reason: String,
    /// The replicas the Deploy did not bring to its version, by region, when the
    /// Service started in its other regions. Empty when none of it started.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(as = "Option<BTreeMap<Region, RegionWait>>", optional)]
    pub regions: BTreeMap<Region, RegionWait>,
}

/// What a Deployment's runner recorded at its end.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum Outcome {
    /// Execution ran. `summary` counts operations, without inputs or provider
    /// messages; each node's Node Outcome is on the Deployment's nodes. `reason`
    /// says why it failed, naming the Service by its current name; users read it.
    /// `cause` lists what it came from, outermost first. `services` says where
    /// each Service ended.
    Executed {
        summary: Value,
        #[serde(default)]
        reason: Option<String>,
        #[serde(default)]
        cause: Vec<String>,
        #[serde(default)]
        services: Vec<ServiceOutcome>,
        /// What a failed replacement's compensation did to its new and old
        /// Containers, one sentence each; users read it.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        compensation: Vec<String>,
        /// The Deployment's cancel stopped the operation `reason` names, so the
        /// reason only repeats that it was cancelled.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(as = "Option<bool>", optional)]
        cancelled: bool,
        /// The Services the planner left waiting, whole or in some regions,
        /// however the Deployment ended. One left whole reads Not attempted.
        /// They fail nothing, so `reason` never names one. A Deployment that
        /// did all it planned ends Applied with them.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        #[ts(as = "Option<Vec<WaitingService>>", optional)]
        waiting: Vec<WaitingService>,
    },
    /// Nothing executed: preparation failed first. `needs_upload` names the Services
    /// that can build only from a new upload.
    NotExecuted {
        reason: String,
        #[serde(default)]
        cause: Vec<String>,
        #[serde(default)]
        needs_upload: Vec<ServiceName>,
        /// Deploying the same thing again fails this way again: against the same
        /// Cluster, `reason` holds until something staged changes, so a retry alone
        /// never gets past it. Absent from rows written before it, which read false.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(as = "Option<bool>", optional)]
        same_again: bool,
    },
    /// A removal that needed no runner: nothing of its Environment ever ran on a
    /// Server.
    NeverRan,
    /// A removal that completed only in configuration: Cloud counted no Server left,
    /// so whatever the Servers that ran it still run was left there. Its nodes read
    /// Unknown.
    Forgotten,
}

/// Evidence a runner records about the Deployment it claimed.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "evidence", content = "value", rename_all = "snake_case")]
pub enum RunEvidence {
    /// The Deploy Preview preparation produced, recorded before it is confirmed.
    Prepared(DeployPreview),
    /// Record the traffic switch when present. Renew the window in which followers
    /// wait for cleanup while the runner is still present.
    Finishing { live_at: Option<i64> },
    /// The runner finished cleanup. Record its warnings before clearing the cleanup lease.
    Finished { warnings: Vec<DeployWarning> },
    /// These runtime Services completed every planned operation while execution
    /// still runs: each reads Deployed (or Removed) and enters Applied State now,
    /// whatever happens to the runner later.
    Confirmed(Vec<ServiceName>),
    /// What executing that preview did, and what deleting the Docker Volumes of the
    /// Volumes it removes did: the runner deletes them only after a successful Deploy.
    Executed {
        outcome: Box<DeployOutcome<ExecutionError>>,
        /// Final row changes not yet acknowledged while execution was running.
        #[serde(default)]
        progress: Vec<ServerProgress>,
        #[serde(default)]
        removed: Vec<VolumeRemoval>,
    },
    /// Preparation failed, so nothing executed. Users read it: it holds no secret.
    NotExecuted(Failure),
    /// The rows whose state changed while execution runs.
    Progress(Vec<ServerProgress>),
    /// The runner stopped without knowing whether it executed: the outcome is unknown
    /// once it recorded a Deploy Preview, and nothing executed before that.
    Abandoned,
    /// The runtime task failed. Preserve known evidence and make the Deployment Unknown.
    RuntimeFailed,
    /// The build receipts preparation produced, by runtime Service name. Each replaces
    /// that Service's latest receipt in the Environment.
    Built(BTreeMap<ServiceName, Value>),
    /// Progress and log output of one Git or uploaded build.
    Build(BuildReport),
    /// Nothing executed: these uploaded Services have no upload to build from and no
    /// usable image to reuse.
    UploadNeeded(Vec<ServiceName>),
    /// What filling the Cluster's missing or stopped Ingress Proxies did after
    /// execution. A failure warns; it never fails the Deployment.
    Ingress(Vec<IngressRepair>),
    /// Checking the Cluster's Ingress Proxies after execution failed, so none was
    /// filled. It warns; it never fails the Deployment.
    IngressUnchecked(String),
    /// Something the Deploy found while executing. It warns; it never fails the
    /// Deployment.
    Warned(DeployWarning),
    /// The runner still holds the Deployment: it renews the lease and reads the
    /// status back, which says whether it was cancelled.
    Alive,
}

/// A claimed Deployment and the frozen Deploy Intent its runner executes.
#[derive(Clone, Debug, PartialEq)]
pub struct Claimed {
    pub deployment: DeploymentSummary,
    /// When the runner's acknowledged lease expires, in Unix seconds.
    pub lease_until: i64,
    /// Services without a source of their own are left out; each builds from its
    /// upload in `uploads` through the SDK's preparation of `input`.
    pub intent: DeployIntent,
    /// Every environment variable name under which any Container of the Environment,
    /// present or past, may hold a Sealed value. A failed Container's log is scrubbed
    /// of its values under these names before anyone sees it.
    pub sealed_names: BTreeSet<String>,
    /// The lowering input `intent` came from: what SDK preparation takes.
    pub input: Value,
    /// Every build receipt of each Service, by runtime name, its Environment's own
    /// first: hints preparation verifies.
    pub receipts: BTreeMap<ServiceName, Vec<Value>>,
    /// The Git Services it builds, each with its pinned commit, if any.
    pub sources: Vec<GitSource>,
    /// The Docker Volumes to delete once the Deploy succeeds: exactly those whose
    /// loss admission accepted, each on its Server.
    pub deletes: Vec<DockerVolumeId>,
    /// Each target Service without a source of its own, by runtime name, and the
    /// digest of the upload it builds from: `deployment.upload`'s, or an older one
    /// it last built from; none when it has none.
    pub uploads: BTreeMap<ServiceName, Option<ployz_core::UploadDigest>>,
    /// The Organization's Build Order as the build starts. Uploaded Services skip
    /// GitHub in it: GitHub can't build uploaded source, so they build on Servers.
    pub build_order: Vec<crate::Builder>,
    /// Every other Deployment of the Environment that ran and can no longer act:
    /// ended, or lost its runner to this claim or an earlier one. None but this
    /// claim's runner may change the Environment, so a Pending copy one of these
    /// left is no run's.
    pub ended_runs: BTreeSet<DeploymentId>,
    /// The automatic deploy rule a push's Deployment runs under, which the runner hands
    /// each Server with each Service's image token; none for any other Deployment.
    pub rule: Option<ployz_core::authority::Signed<ployz_core::authority::AutoDeployRule>>,
    /// The image token each Service's GitHub build under `rule` reported.
    pub image_tokens: BTreeMap<ServiceName, crate::ImageToken>,
    /// What a Server issued for each Service's image under `rule` once it checked it
    /// online: sent in place of the image token, which may have expired.
    pub verified: BTreeMap<ServiceName, crate::VerifiedClaim>,
    /// What each target Service runs, by runtime name, with the signed versions kept for
    /// it. A Service without one sends its Saved image and no statement.
    pub ran: BTreeMap<ServiceName, Pinned>,
    /// The preview this Deployment closes: a removal that takes a preview's PR Environment
    /// off for good, which its home Server closes rather than its Volumes being deleted
    /// one by one. None for any other Deployment, a preview's shut down included.
    pub closes: Option<Namespace>,
    /// Each Service it builds whose build is a free Build, by runtime name: it runs
    /// unsigned, so a Server builds it with nothing secret and its image runs by the
    /// receipt that Server gives.
    pub free: BTreeSet<ServiceName>,
}

/// One node a Deployment targets, as frozen at admission. A row stores a Service the
/// Deployment tears down as a Service whose `removals` hold its own `runtime`; here it
/// is a [`Self::Retire`], and it stores as that row again.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(from = "NodeRow", into = "NodeRow")]
pub(crate) enum TargetNode {
    /// A Service the Deployment ships.
    Service {
        id: ServiceLineageId,
        name: ServiceName,
        /// The runtime Service it lowers to, which its Node Outcome is confirmed by.
        runtime: ServiceName,
        /// The runtime Services it tears down beside `runtime`, which it ran as and
        /// no longer does.
        removals: ServiceRemovals,
    },
    /// A Service the Deployment tears down.
    Retire {
        id: ServiceLineageId,
        name: ServiceName,
        /// The runtime Service its Node Outcome is confirmed by, among `runtimes`.
        runtime: ServiceName,
        /// Every runtime Service it tears down.
        runtimes: ServiceRemovals,
    },
    Volume {
        id: VolumeId,
        name: VolumeName,
        /// For a Volume the Deployment removes: the Docker Volumes it deletes.
        deletes: Option<Vec<DockerVolumeId>>,
    },
    Config {
        id: ConfigId,
        name: ConfigName,
    },
}

/// The row a [`TargetNode`] is stored as, which older readers and a rolling deploy
/// decode: a teardown is a Service whose `removals` hold its `runtime`.
#[derive(Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum NodeRow {
    Service {
        id: ServiceLineageId,
        name: ServiceName,
        runtime: ServiceName,
        #[serde(default, skip_serializing_if = "ServiceRemovals::is_empty")]
        removals: ServiceRemovals,
    },
    Volume {
        id: VolumeId,
        name: VolumeName,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deletes: Option<Vec<DockerVolumeId>>,
    },
    Config {
        id: ConfigId,
        name: ConfigName,
    },
}

impl From<NodeRow> for TargetNode {
    fn from(row: NodeRow) -> Self {
        match row {
            NodeRow::Service {
                id,
                name,
                runtime,
                removals,
            } if removals.contains(&runtime) => Self::Retire {
                id,
                name,
                runtime,
                runtimes: removals,
            },
            NodeRow::Service {
                id,
                name,
                runtime,
                removals,
            } => Self::Service {
                id,
                name,
                runtime,
                removals,
            },
            NodeRow::Volume { id, name, deletes } => Self::Volume { id, name, deletes },
            NodeRow::Config { id, name } => Self::Config { id, name },
        }
    }
}

impl From<TargetNode> for NodeRow {
    fn from(node: TargetNode) -> Self {
        match node {
            TargetNode::Service {
                id,
                name,
                runtime,
                removals,
            }
            | TargetNode::Retire {
                id,
                name,
                runtime,
                runtimes: removals,
            } => Self::Service {
                id,
                name,
                runtime,
                removals,
            },
            TargetNode::Volume { id, name, deletes } => Self::Volume { id, name, deletes },
            TargetNode::Config { id, name } => Self::Config { id, name },
        }
    }
}

/// The `LIKE` pattern matching a Deployment's `nodes` text when it targets node `id`.
pub(crate) fn node_pattern(id: &str) -> String {
    format!("%\"{id}\"%")
}

/// The Services among `nodes`.
pub(crate) fn shipped(nodes: &[TargetNode]) -> Vec<ServiceName> {
    nodes
        .iter()
        .filter_map(|node| node.service().map(|service| service.name.clone()))
        .collect()
}

/// A Service node's identity, whether its Deployment ships it or tears it down.
pub(crate) struct ServiceNode<'a> {
    pub(crate) id: &'a ServiceLineageId,
    pub(crate) name: &'a ServiceName,
    /// The runtime Service its Node Outcome is confirmed by.
    pub(crate) runtime: &'a ServiceName,
}

impl TargetNode {
    /// The Service it is, shipped or torn down; none for a Volume or Config.
    pub(crate) fn service(&self) -> Option<ServiceNode<'_>> {
        match self {
            Self::Service {
                id, name, runtime, ..
            }
            | Self::Retire {
                id, name, runtime, ..
            } => Some(ServiceNode { id, name, runtime }),
            Self::Volume { .. } | Self::Config { .. } => None,
        }
    }

    fn ships(service: &SavedServiceIntent) -> Result<Self, RpcError> {
        Ok(Self::Service {
            id: parse_stored(&service.id)?,
            name: ServiceName::parse(service.slug.as_str())
                .map_err(|_| error::corrupt("Service name"))?,
            runtime: service.config.private_dns.clone(),
            removals: ServiceRemovals::default(),
        })
    }

    fn retire(retire: &Retire) -> Result<Self, RpcError> {
        let runtime = retire.runtimes.iter().next().ok_or_else(|| {
            error::invalid(
                format!("Service {} has nothing to remove", retire.name),
                json!({ "service": retire.id }),
            )
        })?;
        Ok(Self::Retire {
            id: retire.id.clone(),
            name: retire.name.clone(),
            runtime: runtime.clone(),
            runtimes: retire.runtimes.clone(),
        })
    }

    /// Whether it is a Service the Deployment tears down.
    pub(crate) fn retires(&self) -> bool {
        matches!(self, Self::Retire { .. })
    }

    /// The runtime Services it ships or tears down, whose rows are its own.
    pub(crate) fn runtimes(&self) -> Vec<&ServiceName> {
        match self {
            Self::Retire { runtimes, .. } => runtimes.iter().collect(),
            Self::Service {
                runtime, removals, ..
            } => std::iter::once(runtime).chain(removals.iter()).collect(),
            Self::Volume { .. } | Self::Config { .. } => Vec::new(),
        }
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

    fn config(config: &SavedConfigIntent) -> Result<Self, RpcError> {
        Ok(Self::Config {
            id: parse_stored(&config.resource_id)?,
            name: config.name.clone(),
        })
    }

    pub(crate) fn id(&self) -> &str {
        match self {
            Self::Service { id, .. } | Self::Retire { id, .. } => id.as_str(),
            Self::Volume { id, .. } => id.as_str(),
            Self::Config { id, .. } => id.as_str(),
        }
    }

    /// Where views list it: Services, then Volumes, then Configs, each by name, as the
    /// review does.
    pub(crate) fn listed(&self) -> (EnvironmentNodeType, &str) {
        match self {
            Self::Service { name, .. } | Self::Retire { name, .. } => {
                (EnvironmentNodeType::Service, name.as_str())
            }
            Self::Volume { name, .. } => (EnvironmentNodeType::Volume, name.as_str()),
            Self::Config { name, .. } => (EnvironmentNodeType::Config, name.as_str()),
        }
    }

    /// [`Self::listed`], then a node the Deployment takes away, being gone from `saved`,
    /// before one that took its name; the id only settles what nothing else tells apart.
    pub(crate) fn order(
        &self,
        saved: &SavedEnvironmentIntent,
    ) -> (EnvironmentNodeType, &str, bool, &str) {
        let id = self.id();
        let stays = match self {
            Self::Service { .. } => saved.services.iter().any(|service| service.id == id),
            Self::Retire { .. } => false,
            Self::Volume { deletes, .. } => {
                deletes.is_none() && saved.volumes.iter().any(|volume| volume.resource_id == id)
            }
            Self::Config { .. } => saved.configs.iter().any(|config| config.resource_id == id),
        };
        let (kind, name) = self.listed();
        (kind, name, stays, id)
    }

    /// The node as views name it.
    fn shown(&self) -> DeployedNode {
        match self {
            Self::Service { id, name, .. } | Self::Retire { id, name, .. } => {
                DeployedNode::Service {
                    id: id.clone(),
                    name: name.clone(),
                }
            }
            Self::Volume { id, name, .. } => DeployedNode::Volume {
                id: id.clone(),
                name: name.clone(),
            },
            Self::Config { id, name } => DeployedNode::Config {
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
    Config {
        id: ConfigId,
        name: ConfigName,
    },
}

impl DeployedNode {
    /// Its name when admitted.
    #[must_use]
    pub fn name(&self) -> &str {
        match self {
            Self::Service { name, .. } => name.as_str(),
            Self::Volume { name, .. } => name.as_str(),
            Self::Config { name, .. } => name.as_str(),
        }
    }

    /// Its node ID.
    #[must_use]
    pub fn id(&self) -> &str {
        match self {
            Self::Service { id, .. } => id.as_str(),
            Self::Volume { id, .. } => id.as_str(),
            Self::Config { id, .. } => id.as_str(),
        }
    }
}

/// What admission freezes, besides the Saved revision.
pub(crate) struct Frozen {
    pub(crate) nodes: Vec<TargetNode>,
    /// Whether it ships every Service: nodes alone can't say, since a full
    /// Deployment tears down what Saved dropped too. A named one holds, by runtime
    /// name, every Service it hands the runner: the named ones and each they
    /// reference, changed or not.
    pub(crate) targeting: Targeting<BTreeSet<ServiceName>>,
    pub(crate) namespace: Namespace,
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

    /// Whether this Deployment hands the runner `service`: every one for a full
    /// Deployment, else a named one, one of a Saved change it ships, or one they reference.
    pub(crate) fn hands(&self, service: &SavedServiceIntent) -> bool {
        match &self.targeting {
            Targeting::Full { .. } => true,
            Targeting::Named(shipped) => shipped.contains(&service.config.private_dns),
        }
    }

    /// Whether this Deployment ships Service `name`, named or referenced.
    pub(crate) fn ships(&self, name: &ServiceName) -> bool {
        self.nodes
            .iter()
            .any(|node| node.service().is_some_and(|service| service.name == name))
    }
}

/// The runner's evidence so far, stored as one document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Run {
    /// Parsed and redacted once, when recorded.
    preview: Option<DeployPreview>,
    outcome: Option<Outcome>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    live_at: Option<i64>,
    /// This outcome was written by a Store that records traffic switches.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    tracks_live: bool,
    /// A digest of the Executed evidence recorded: a replay must match it.
    #[serde(default)]
    executed: Option<String>,
    /// Each target node's Node Outcome, by node ID, once execution ran.
    #[serde(default)]
    nodes: BTreeMap<String, NodeStatus>,
    /// The Failed nodes only the Deployment's cancel stopped, which read
    /// Cancelled. Kept apart from `nodes` so a Store from before Cancelled reads
    /// them Failed.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    cancelled: BTreeSet<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    warnings: Vec<DeploymentWarning>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    ingress: Vec<IngressRepair>,
    /// Chosen at admission, not evidence: whether a full Deployment also removes
    /// every Service of the Namespace this Store never held. Kept here so a Store
    /// from before it reads the row unchanged.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    prune: bool,
}

impl Run {
    /// The nodes its Deployment never attempted. On an Applied one, the Services
    /// the planner left waiting whole.
    fn unreached(&self) -> impl Iterator<Item = &String> {
        self.nodes
            .iter()
            .filter(|(_, status)| **status == NodeStatus::NotAttempted)
            .map(|(id, _)| id)
    }

    /// Whether the planner left a Service waiting, whole or in some regions.
    fn waits(&self) -> bool {
        matches!(&self.outcome, Some(Outcome::Executed { waiting, .. }) if !waiting.is_empty())
    }

    fn summary_warnings(&self) -> Vec<DeploymentWarning> {
        let failed = self.ingress.iter().filter_map(|repair| {
            let ingress = repair.failure.clone()?;
            Some(DeploymentWarning::IngressNotServing {
                server: repair.server.clone(),
                ingress,
            })
        });
        self.warnings.iter().cloned().chain(failed).collect()
    }
}

pub(crate) struct Stored {
    pub(crate) summary: DeploymentSummary,
    namespace: Namespace,
    cluster_domain: Option<Hostname>,
    pub(crate) nodes: Vec<TargetNode>,
    run: Run,
    cleanup: cleanup::Cleanup,
    standing: Standing,
    /// Last saved claim lease, even when an evidence write proposes a renewal.
    recorded_runner_lease: i64,
    /// When `stored` read it, in Unix seconds: the moment its summary judges a
    /// runner lease by, so the status it shows holds for the whole transaction.
    read_at: i64,
    /// Where it lists among its Environment's Deployments: taken once, at insert,
    /// and never moved, so a page cursor can hold it.
    pub(crate) listed: i64,
    /// The digest of the upload each target Service without a source of its own
    /// builds from, by lineage; none for one admitted before it was kept, whose
    /// every such Service builds from its upload.
    pub(crate) service_uploads: Option<uploads::ServiceUploads>,
}

impl Stored {
    /// The runtime Namespace it deploys into.
    pub(crate) const fn namespace(&self) -> &Namespace {
        &self.namespace
    }

    /// What it lowers: the Saved Services it ships and the runtime Services it
    /// tears down, beside each shipped one those it no longer runs as. A full
    /// Deploy ships every Service of `saved`, its revision, and tears down each
    /// node `saved` lacks: one frozen before teardowns were, as the runtime
    /// Service it ran as then, unless a Service of `saved` runs as it now and so
    /// replaces it.
    pub(crate) fn lowering(&self, saved: &SavedEnvironmentIntent) -> Ships {
        let full = self.targeting().is_full();
        let mut shipped = Vec::new();
        let mut removals = ServiceRemovals::default();
        for node in &self.nodes {
            match node {
                TargetNode::Retire { runtimes, .. } => {
                    removals.extend(runtimes.iter().cloned());
                }
                TargetNode::Service {
                    id,
                    name,
                    runtime,
                    removals: gone,
                } => {
                    if full
                        && !saved
                            .services
                            .iter()
                            .any(|service| service.id == id.as_str())
                    {
                        if !saved
                            .services
                            .iter()
                            .any(|service| service.config.private_dns == *runtime)
                        {
                            removals.extend([runtime.clone()]);
                        }
                    } else {
                        removals.extend(gone.iter().cloned());
                        shipped.push(name);
                    }
                }
                TargetNode::Volume { .. } | TargetNode::Config { .. } => {}
            }
        }
        Ships {
            targeting: self.targeting().map(|targeted| {
                targeted
                    .iter()
                    .filter(|service| shipped.contains(service))
                    .cloned()
                    .collect()
            }),
            removals,
        }
    }
}

/// The Organization Deployment `id` belongs to.
pub(crate) fn organization(tx: &mut dyn Tx, id: &DeploymentId) -> Result<OrganizationId, RpcError> {
    tx.query(
        "SELECT organization_id FROM config_deployment WHERE id = ?1",
        &[id.as_str().into()],
    )?
    .first()
    .ok_or_else(|| error::corrupt("Deployment"))?
    .parse(0, "Deployment")
}

/// Insert a `config_deployment` row of `columns`, each named beside its value.
fn insert(tx: &mut dyn Tx, columns: &[(&str, crate::storage::Param<'_>)]) -> Result<(), RpcError> {
    let names = columns
        .iter()
        .map(|(name, _)| *name)
        .collect::<Vec<_>>()
        .join(", ");
    let slots = (1..=columns.len())
        .map(|at| format!("?{at}"))
        .collect::<Vec<_>>()
        .join(", ");
    let values: Vec<crate::storage::Param<'_>> = columns.iter().map(|(_, value)| *value).collect();
    tx.execute(
        &format!("INSERT INTO config_deployment ({names}) VALUES ({slots})"),
        &values,
    )
    .map(drop)
}

/// What a Deployment row reads as, from an unaliased `config_deployment`. The
/// second-last column is the [`CiVerdict`] of one waiting for CI
/// ([`crate::automation::CI_FAILED`]). The last is where it lists among its
/// Environment's Deployments.
static COLUMNS: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let failed = &*crate::automation::CI_FAILED;
    format!(
        "id, environment_id, number, status, saved_revision, services, nodes, namespace, run, \
         upload, cluster_domain, runner, lease, admitted_by, admitted, started, ended, message, \
         push_repository_id, push_branch, push_head, \
         CASE WHEN status = 'waiting_for_ci' THEN \
           CASE WHEN {failed} \
           THEN 'failed' ELSE 'pending' END \
         END, listed, service_uploads"
    )
});

fn columns(tx: &dyn Tx) -> String {
    let clock = tx.backend().wall_clock();
    format!(
        "{}, cleanup_status, cleanup_deadline, CAST(CASE
         WHEN cleanup_status = 'pending' AND cleanup_deadline < {clock} THEN 1 ELSE 0 END AS BIGINT)",
        COLUMNS.as_str()
    )
}

/// Whether a claimed Deployment's runner is gone at `now`: its runner lease lapsed,
/// so what ran is unknown. Every read shows a lapsed one as `unknown`
/// ([`Standing::status`]).
fn lapsed(lease: i64, now: i64) -> bool {
    lease <= now
}

/// When a claimed Deployment whose runner is lost ended, found at `now`: the
/// earlier of its runner lease and `now`. While the lease holds that is `now`;
/// once it [`lapsed`], the lease, when reads began to show it Unknown. So it never
/// passes the lease, which [`settling`] bounds such a run by.
fn runner_lost(lease: i64, now: i64) -> i64 {
    lease.min(now)
}

/// Where a Deployment stands, with exactly the columns its status keeps, or every
/// column as read for a row in a shape no Store writes ([`Raw`]). `stored` decodes
/// each row into one, `save` writes `status`, `number`, `runner`, `started`,
/// `lease` and `ended` from it, and [`Standing::shown`] is all the summary shows of
/// them. Each edge out of a state is a method of that state's type.
#[derive(Debug)]
enum Standing {
    Waiting(Waiting),
    Queued(Queued),
    Claimed(ClaimedRun),
    /// A newer admission replaced it before any runner claimed it.
    Superseded {
        /// None when what it replaced was waiting for CI.
        number: Option<u64>,
        ended: i64,
    },
    Ended(Ended),
    Raw(Raw),
}

/// A row whose columns fit no other [`Standing`]. No Store writes one, but a write
/// in SQL can leave one: applied or failed with no `ended`, or a `started` with no
/// `runner`. It keeps every column as read, and goes by its status alone, as every
/// Store before [`Standing`] did:
/// - It reads Unknown once its status says a runner holds it and that runner's
///   lease [`lapsed`], with its `ended` as the column holds it.
/// - Each edge, [`Standing::queue`] through [`Standing::restamp`], writes the
///   columns it writes for the typed standing of the same status, and no others
///   (`parity_tests`).
/// - Whatever picks a row to move or count tests its status or its columns, never
///   its kind of [`Standing`]: lifecycle.rs `queue`, `hold`, `supersede_held` and
///   `cancel_held`, and history.rs `settling`, which counts the lease of any row
///   with no `ended`.
#[derive(Debug)]
struct Raw {
    status: DeploymentStatus,
    number: Option<u64>,
    runner: Option<RunnerId>,
    started: Option<i64>,
    lease: i64,
    ended: Option<i64>,
}

/// Waiting for its commit's CI: unnumbered and unclaimed. The push that admitted
/// it stays on the summary, as it does once it queues.
#[derive(Debug)]
struct Waiting;

#[derive(Debug)]
struct Queued {
    number: u64,
}

/// Claimed by a runner, whose runner lease may have lapsed since.
#[derive(Debug)]
struct ClaimedRun {
    number: u64,
    claim: Claim,
    phase: Phase,
}

/// A runner's claim: `claim` sets all three at once (claim.rs `claim`).
#[derive(Debug)]
struct Claim {
    runner: RunnerId,
    /// When it claimed it, in Unix seconds.
    started: i64,
    /// Until when, in Unix seconds, its runner holds it without recording anything.
    lease: i64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Phase {
    Running,
    Cancelling,
}

/// Out of flight for good.
#[derive(Debug)]
struct Ended {
    /// None when it never queued: cancelled while it waited for CI.
    number: Option<u64>,
    /// None when no runner ever claimed it: cancelled before one did, or a removal
    /// applied without one (lifecycle.rs `forget`).
    claim: Option<Claim>,
    ended: i64,
    end: End,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum End {
    Applied,
    Failed,
    Unknown,
    Cancelled,
}

impl Waiting {
    /// Cancelled while it waited for CI: it never queued, so it has no number.
    const fn cancel(self, now: i64) -> Ended {
        Ended {
            number: None,
            claim: None,
            ended: now,
            end: End::Cancelled,
        }
    }
}

impl Queued {
    /// Claimed by `runner` at `now`, which holds it for [`LEASE`].
    const fn claim(self, runner: RunnerId, now: i64) -> ClaimedRun {
        ClaimedRun {
            number: self.number,
            claim: Claim {
                runner,
                started: now,
                lease: now + LEASE,
            },
            phase: Phase::Running,
        }
    }

    /// Ended `end` at `now` without a runner: cancelled, or a removal applied
    /// without one.
    const fn end(self, end: End, now: i64) -> Ended {
        Ended {
            number: Some(self.number),
            claim: None,
            ended: now,
            end,
        }
    }
}

impl ClaimedRun {
    /// Its runner recorded something at `now`, so it is still there.
    const fn renew(&mut self, now: i64) {
        self.claim.lease = now + LEASE;
    }

    /// Ended `end` at `now`, keeping its claim.
    fn end(self, end: End, now: i64) -> Ended {
        Ended {
            number: Some(self.number),
            claim: Some(self.claim),
            ended: now,
            end,
        }
    }

    /// Its runner is lost, found at `now`: it ends Unknown at [`runner_lost`],
    /// keeping its claim, so its runner may still record an Interrupted outcome.
    fn lose_runner(self, now: i64) -> Ended {
        let ended = runner_lost(self.claim.lease, now);
        self.end(End::Unknown, ended)
    }
}

impl Phase {
    const fn status(self) -> DeploymentStatus {
        match self {
            Self::Running => DeploymentStatus::Running,
            Self::Cancelling => DeploymentStatus::Cancelling,
        }
    }
}

impl End {
    const fn status(self) -> DeploymentStatus {
        match self {
            Self::Applied => DeploymentStatus::Applied,
            Self::Failed => DeploymentStatus::Failed,
            Self::Unknown => DeploymentStatus::Unknown,
            Self::Cancelled => DeploymentStatus::Cancelled,
        }
    }
}

/// What a summary shows of a [`Standing`].
struct Shown {
    status: DeploymentStatus,
    number: Option<u64>,
    runner: Option<RunnerId>,
    started_at: Option<i64>,
    ended_at: Option<i64>,
}

impl Standing {
    /// Decode a row's `status` with its `number`, `runner`, `started`, `lease` and
    /// `ended`. Any row whose columns fit no other standing reads as [`Raw`], so
    /// every row a Store read before [`Standing`] still reads. Migration
    /// `0034_deployment_ended` stamped `ended` on each superseded or Unknown row an
    /// older Store left without one.
    fn decode(
        status: DeploymentStatus,
        number: Option<u64>,
        (runner, started, lease): (Option<RunnerId>, Option<i64>, i64),
        ended: Option<i64>,
    ) -> Self {
        use DeploymentStatus as S;
        let claim = match (&runner, started) {
            (Some(runner), Some(started)) => Some(Some(Claim {
                runner: runner.clone(),
                started,
                lease,
            })),
            (None, None) => Some(None),
            (Some(_), None) | (None, Some(_)) => None,
        };
        match (status, number, claim, ended) {
            (S::WaitingForCi, None, Some(None), None) => Self::Waiting(Waiting),
            (S::Queued, Some(number), Some(None), None) => Self::Queued(Queued { number }),
            (S::Running, Some(number), Some(Some(claim)), None) => Self::Claimed(ClaimedRun {
                number,
                claim,
                phase: Phase::Running,
            }),
            (S::Cancelling, Some(number), Some(Some(claim)), None) => Self::Claimed(ClaimedRun {
                number,
                claim,
                phase: Phase::Cancelling,
            }),
            (S::Superseded, number, Some(None), Some(ended)) => Self::Superseded { number, ended },
            (S::Unknown, number, Some(Some(claim)), Some(ended)) => Self::Ended(Ended {
                number,
                ended,
                claim: Some(claim),
                end: End::Unknown,
            }),
            (S::Applied, number, Some(claim), Some(ended)) => Self::Ended(Ended {
                number,
                claim,
                ended,
                end: End::Applied,
            }),
            (S::Failed, number, Some(claim), Some(ended)) => Self::Ended(Ended {
                number,
                claim,
                ended,
                end: End::Failed,
            }),
            (S::Cancelled, number, Some(claim), Some(ended)) => Self::Ended(Ended {
                number,
                claim,
                ended,
                end: End::Cancelled,
            }),
            _ => Self::Raw(Raw {
                status,
                number,
                runner,
                started,
                lease,
                ended,
            }),
        }
    }

    /// The status its row holds, which a lapsed runner lease leaves `running` or
    /// `cancelling` until something writes it.
    const fn kept(&self) -> DeploymentStatus {
        match self {
            Self::Waiting(_) => DeploymentStatus::WaitingForCi,
            Self::Queued(_) => DeploymentStatus::Queued,
            Self::Claimed(claimed) => claimed.phase.status(),
            Self::Superseded { .. } => DeploymentStatus::Superseded,
            Self::Ended(ended) => ended.end.status(),
            Self::Raw(raw) => raw.status,
        }
    }

    /// Its status at `now`: Unknown once a claimed one's runner lease [`lapsed`].
    fn status(&self, now: i64) -> DeploymentStatus {
        match self {
            Self::Claimed(claimed) if lapsed(claimed.claim.lease, now) => DeploymentStatus::Unknown,
            Self::Raw(raw) if raw.status.claimed() && lapsed(raw.lease, now) => {
                DeploymentStatus::Unknown
            }
            standing @ (Self::Waiting(_)
            | Self::Queued(_)
            | Self::Claimed(_)
            | Self::Superseded { .. }
            | Self::Ended(_)
            | Self::Raw(_)) => standing.kept(),
        }
    }

    const fn claim(&self) -> Option<&Claim> {
        match self {
            Self::Claimed(claimed) => Some(&claimed.claim),
            Self::Ended(ended) => ended.claim.as_ref(),
            Self::Waiting(_) | Self::Queued(_) | Self::Superseded { .. } | Self::Raw(_) => None,
        }
    }

    /// Its `runner` column.
    fn runner(&self) -> Option<&RunnerId> {
        match self {
            Self::Raw(raw) => raw.runner.as_ref(),
            standing @ (Self::Waiting(_)
            | Self::Queued(_)
            | Self::Claimed(_)
            | Self::Superseded { .. }
            | Self::Ended(_)) => standing.claim().map(|claim| &claim.runner),
        }
    }

    /// Its `started` column.
    fn started(&self) -> Option<i64> {
        match self {
            Self::Raw(raw) => raw.started,
            standing @ (Self::Waiting(_)
            | Self::Queued(_)
            | Self::Claimed(_)
            | Self::Superseded { .. }
            | Self::Ended(_)) => standing.claim().map(|claim| claim.started),
        }
    }

    const fn number(&self) -> Option<u64> {
        match self {
            Self::Waiting(_) => None,
            Self::Queued(Queued { number }) | Self::Claimed(ClaimedRun { number, .. }) => {
                Some(*number)
            }
            Self::Superseded { number, .. }
            | Self::Ended(Ended { number, .. })
            | Self::Raw(Raw { number, .. }) => *number,
        }
    }

    /// Its `ended` column: set once it left flight for good.
    const fn ended(&self) -> Option<i64> {
        match self {
            Self::Superseded { ended, .. } | Self::Ended(Ended { ended, .. }) => Some(*ended),
            Self::Raw(raw) => raw.ended,
            Self::Waiting(_) | Self::Queued(_) | Self::Claimed(_) => None,
        }
    }

    /// What a summary shows of it at `now`. A claimed one whose runner lease
    /// lapsed reads Unknown, ended at [`runner_lost`], before anything writes it.
    fn shown(&self, now: i64) -> Shown {
        Shown {
            status: self.status(now),
            number: self.number(),
            runner: self.runner().cloned(),
            started_at: self.started(),
            ended_at: match self {
                Self::Claimed(ClaimedRun { claim, .. }) => {
                    lapsed(claim.lease, now).then(|| runner_lost(claim.lease, now))
                }
                standing @ (Self::Waiting(_)
                | Self::Queued(_)
                | Self::Superseded { .. }
                | Self::Ended(_)
                | Self::Raw(_)) => standing.ended(),
            },
        }
    }

    /// Claimed by `runner` at `now`, when it is queued. Any other stands as it was.
    fn claim_for(self, runner: &RunnerId, now: i64) -> Self {
        match self {
            Self::Queued(queued) => Self::Claimed(queued.claim(runner.clone(), now)),
            Self::Raw(mut raw) => {
                if raw.status == DeploymentStatus::Queued {
                    raw.status = DeploymentStatus::Running;
                    raw.runner = Some(runner.clone());
                    raw.started = Some(now);
                    raw.lease = now + LEASE;
                }
                Self::Raw(raw)
            }
            standing @ (Self::Waiting(_)
            | Self::Claimed(_)
            | Self::Superseded { .. }
            | Self::Ended(_)) => standing,
        }
    }

    /// Its runner recorded something at `now`, when it is claimed. Any other
    /// stands as it was.
    const fn renew(&mut self, now: i64) {
        match self {
            Self::Claimed(claimed) => claimed.renew(now),
            Self::Raw(raw) if raw.status.claimed() => raw.lease = now + LEASE,
            Self::Waiting(_)
            | Self::Queued(_)
            | Self::Superseded { .. }
            | Self::Ended(_)
            | Self::Raw(_) => {}
        }
    }

    /// Queued as `number`, when it waits for CI: its CI passed, so it is admitted in
    /// place. Any other stands as it was.
    fn queue(self, number: u64) -> Self {
        match self {
            Self::Waiting(Waiting) => Self::Queued(Queued { number }),
            Self::Raw(mut raw) => {
                if raw.status == DeploymentStatus::WaitingForCi {
                    raw.status = DeploymentStatus::Queued;
                    raw.number = Some(number);
                }
                Self::Raw(raw)
            }
            standing @ (Self::Queued(_)
            | Self::Claimed(_)
            | Self::Superseded { .. }
            | Self::Ended(_)) => standing,
        }
    }

    /// Replaced at `now` by a newer admission, when it is queued or waits for CI.
    /// Any other stands as it was.
    fn supersede(self, now: i64) -> Self {
        match self {
            Self::Waiting(Waiting) => Self::Superseded {
                number: None,
                ended: now,
            },
            Self::Queued(Queued { number }) => Self::Superseded {
                number: Some(number),
                ended: now,
            },
            Self::Raw(mut raw) => {
                if matches!(
                    raw.status,
                    DeploymentStatus::WaitingForCi | DeploymentStatus::Queued
                ) {
                    raw.status = DeploymentStatus::Superseded;
                    raw.ended = Some(now);
                }
                Self::Raw(raw)
            }
            standing @ (Self::Claimed(_) | Self::Superseded { .. } | Self::Ended(_)) => standing,
        }
    }

    /// Cancelled at `now`, when it may still run: it ends Cancelled, keeping any
    /// claim. Any other stands as it was.
    fn cancel(self, now: i64) -> Self {
        match self {
            Self::Waiting(waiting) => Self::Ended(waiting.cancel(now)),
            Self::Queued(queued) => Self::Ended(queued.end(End::Cancelled, now)),
            Self::Claimed(claimed) => Self::Ended(claimed.end(End::Cancelled, now)),
            Self::Raw(mut raw) => {
                if raw.status == DeploymentStatus::WaitingForCi || raw.status.in_flight() {
                    raw.status = DeploymentStatus::Cancelled;
                    raw.ended = Some(now);
                }
                Self::Raw(raw)
            }
            standing @ (Self::Superseded { .. } | Self::Ended(_)) => standing,
        }
    }

    /// Cancelled while it runs: its runner stops it and records what ran. Any
    /// other stands as it was.
    fn cancelling(mut self) -> Self {
        match &mut self {
            Self::Claimed(claimed) => claimed.phase = Phase::Cancelling,
            Self::Raw(raw) if matches!(raw.status, DeploymentStatus::Running) => {
                raw.status = DeploymentStatus::Cancelling;
            }
            Self::Waiting(_)
            | Self::Queued(_)
            | Self::Superseded { .. }
            | Self::Ended(_)
            | Self::Raw(_) => {}
        }
        self
    }

    /// Ended `end` at `now`, when it is claimed or, for a removal applied without a
    /// runner, queued. Any other stands as it was.
    fn end(self, end: End, now: i64) -> Self {
        match self {
            Self::Claimed(claimed) => Self::Ended(claimed.end(end, now)),
            Self::Queued(queued) => Self::Ended(queued.end(end, now)),
            Self::Raw(mut raw) => {
                if raw.status.in_flight() {
                    raw.status = end.status();
                    raw.ended = Some(now);
                }
                Self::Raw(raw)
            }
            standing @ (Self::Waiting(_) | Self::Superseded { .. } | Self::Ended(_)) => standing,
        }
    }

    /// Its runner is lost, found at `now`, when it is claimed
    /// ([`ClaimedRun::lose_runner`]). Any other stands as it was: one that already
    /// ended keeps its end.
    fn lose_runner(self, now: i64) -> Self {
        match self {
            Self::Claimed(claimed) => Self::Ended(claimed.lose_runner(now)),
            Self::Raw(mut raw) => {
                if raw.status.claimed() {
                    raw.status = DeploymentStatus::Unknown;
                    raw.ended = Some(runner_lost(raw.lease, now));
                }
                Self::Raw(raw)
            }
            standing @ (Self::Waiting(_)
            | Self::Queued(_)
            | Self::Superseded { .. }
            | Self::Ended(_)) => standing,
        }
    }

    fn runtime_failed(self, now: i64) -> Self {
        let mut standing = self.lose_runner(now);
        match &mut standing {
            Self::Ended(ended) => ended.end = End::Unknown,
            Self::Raw(raw) if raw.runner.is_some() => {
                raw.status = DeploymentStatus::Unknown;
                raw.ended.get_or_insert(now);
            }
            Self::Waiting(_)
            | Self::Queued(_)
            | Self::Claimed(_)
            | Self::Superseded { .. }
            | Self::Raw(_) => {}
        }
        standing
    }

    /// Its runner said at `now` that it stopped, when it ended Unknown. Any other
    /// stands as it was.
    const fn restamp(&mut self, now: i64) {
        match self {
            Self::Ended(ended) if matches!(ended.end, End::Unknown) => ended.ended = now,
            Self::Raw(raw) if matches!(raw.status, DeploymentStatus::Unknown) => {
                raw.ended = Some(now);
            }
            Self::Waiting(_)
            | Self::Queued(_)
            | Self::Claimed(_)
            | Self::Superseded { .. }
            | Self::Ended(_)
            | Self::Raw(_) => {}
        }
    }

    /// Its `lease` column: the claim's runner lease, or 0, the column's default,
    /// when no runner claimed it. A [`Raw`] one keeps the lease it was read with.
    fn lease(&self) -> i64 {
        match self {
            Self::Raw(raw) => raw.lease,
            standing @ (Self::Waiting(_)
            | Self::Queued(_)
            | Self::Claimed(_)
            | Self::Superseded { .. }
            | Self::Ended(_)) => standing.claim().map_or(0, |claim| claim.lease),
        }
    }
}

/// The Cluster Domain `environment`'s latest Deployment that had one expanded its
/// generated domains under, if any.
pub(crate) fn cluster_domain(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<Hostname>, RpcError> {
    tx.query(
        "SELECT cluster_domain FROM config_deployment \
         WHERE environment_id = ?1 AND number IS NOT NULL AND cluster_domain IS NOT NULL \
         ORDER BY number DESC LIMIT 1",
        &[environment.as_str().into()],
    )?
    .first()
    .map(|row| parse_stored(row.text(0)?))
    .transpose()
}

/// The Namespace `environment` deploys into: fixed at its first admission, so renames
/// never move it. `{project}-{environment}` unless another Environment of the
/// Organization holds that, then suffixed with this Environment's ID. A plan only
/// looks; [`reserve_namespace`] fixes a new one.
pub(crate) fn namespace(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentSummary,
) -> Result<Namespace, RpcError> {
    match kept_namespace(tx, environment)? {
        Some(namespace) => Ok(namespace),
        None => free_namespace(tx, who, environment),
    }
}

/// [`namespace`], fixed for `environment` if it was new.
pub(crate) fn reserve_namespace(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentSummary,
) -> Result<Namespace, RpcError> {
    if let Some(namespace) = kept_namespace(tx, environment)? {
        return Ok(namespace);
    }
    let namespace = free_namespace(tx, who, environment)?;
    tx.execute(
        "INSERT INTO config_namespace (organization_id, namespace, environment_id) \
         VALUES (?1, ?2, ?3)",
        &[
            who.organization.as_str().into(),
            namespace.as_str().into(),
            environment.id.as_str().into(),
        ],
    )?;
    Ok(namespace)
}

fn kept_namespace(
    tx: &mut dyn Tx,
    environment: &EnvironmentSummary,
) -> Result<Option<Namespace>, RpcError> {
    tx.query(
        "SELECT namespace FROM config_namespace WHERE environment_id = ?1",
        &[environment.id.as_str().into()],
    )?
    .first()
    .map(|row| row.parse::<Namespace>(0, "Namespace"))
    .transpose()
}

/// The first of `environment`'s candidate Namespaces no other Environment of `who`'s
/// Organization holds.
fn free_namespace(
    tx: &mut dyn Tx,
    who: &Actor,
    environment: &EnvironmentSummary,
) -> Result<Namespace, RpcError> {
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
        // A preview's form is minted only for a preview, which names its one Server.
        let preview = ployz_core::PreviewNamespace::of(&namespace).is_some();
        if namespace.is_reserved() || preview || taken {
            continue;
        }
        return Ok(namespace);
    }
    Err(error::conflict(
        "No free Namespace for this Environment: rename it",
        json!({}),
    ))
}

/// Deployment `id` of `who`'s Organization.
pub(crate) fn owned(tx: &mut dyn Tx, who: &Actor, id: &DeploymentId) -> Result<Stored, RpcError> {
    let columns = columns(tx);
    let rows = tx.query(
        &format!("SELECT {columns} FROM config_deployment WHERE id = ?1 AND organization_id = ?2"),
        &[id.as_str().into(), who.organization.as_str().into()],
    )?;
    stored(tx, rows.first().ok_or_else(|| missing(id))?)
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
    let columns = columns(tx);
    let rows = tx.query(
        &format!("SELECT {columns} FROM config_deployment WHERE id = ?1"),
        &[id.as_str().into()],
    )?;
    stored(tx, rows.first().ok_or_else(|| missing(id))?)
}

/// Every Deployment `condition` selects from `config_deployment`, as [`load`] reads
/// one.
fn load_where(
    tx: &mut dyn Tx,
    condition: &str,
    params: &[crate::storage::Param<'_>],
) -> Result<Vec<Stored>, RpcError> {
    let columns = columns(tx);
    tx.query(
        &format!("SELECT {columns} FROM config_deployment WHERE {condition}"),
        params,
    )?
    .iter()
    .map(|row| stored(tx, row))
    .collect()
}

fn stored(tx: &mut dyn Tx, row: &Row) -> Result<Stored, RpcError> {
    let read_at = now();
    let number = row
        .optional_int(2)?
        .map(u64::try_from)
        .transpose()
        .map_err(|_| error::corrupt("Deployment number"))?;
    let admitted_at = row.int(14)?;
    let standing = Standing::decode(
        row.variant(3, "Deployment status")?,
        number,
        (
            row.parse_optional(11, "identity")?,
            row.optional_int(15)?,
            row.int(12)?,
        ),
        row.optional_int(16)?,
    );
    let Shown {
        status,
        number,
        runner,
        started_at,
        ended_at,
    } = standing.shown(read_at);
    let run: Run = row.json(8, "Deployment run")?;
    let nodes: Vec<TargetNode> = row.json(6, "Deployment")?;
    let push = match (
        row.optional_int(18)?,
        row.optional_text(19)?,
        row.optional_text(20)?,
    ) {
        (Some(_), Some(_), Some(_)) => Some(GitPush {
            repository_id: row.number(18, "Deployment push")?,
            branch: row.parse(19, "Deployment push")?,
            head: row.parse(20, "Deployment push")?,
        }),
        (None, None, None) => None,
        _ => return Err(error::corrupt("Deployment push")),
    };
    // Only a push holds, and a push that admitted a Deployment froze what it ships,
    // at least one node: a hold, cancelled or not, froze none.
    let held = push.is_some() && nodes.is_empty();
    let service_uploads: Option<uploads::ServiceUploads> =
        row.optional_json(23, "Deployment uploads")?;
    let id = row.parse(0, "identity")?;
    let cleanup = cleanup::Cleanup::decode(row, 24, 25)?;
    if row.int(26)? != 0 {
        tx.observe_expired_cleanup(&id);
    }
    Ok(Stored {
        summary: DeploymentSummary {
            id,
            environment_id: row.parse(1, "identity")?,
            number,
            status,
            saved: row.number(4, "revision")?,
            services: row.json(5, "Deployment")?,
            ships: shipped(&nodes),
            runner,
            upload: row.optional_json(9, "Deployment upload")?,
            chooses_upload_at_start: chooses_upload_at_start(status, service_uploads.as_ref()),
            preparing: preparing(status, &run),
            all_waiting: all_waiting(status, &nodes, &run),
            remove: row.number::<Revision>(4, "revision")? == NOTHING,
            prune: run.prune,
            admitted_by: row.parse_optional(13, "identity")?,
            admitted_at,
            started_at,
            live_at: run.live_at,
            cleanup: cleanup.status(),
            ended_at,
            message: row.optional_text(17)?.map(str::to_owned),
            push,
            ci: row
                .optional_text(21)?
                .map(|verdict| {
                    serde_json::from_value(Value::String(verdict.to_owned()))
                        .map_err(|_| error::corrupt("CI verdict"))
                })
                .transpose()?,
            held,
            in_flight: status.in_flight(),
            outcome: run.outcome.clone(),
            warnings: run.summary_warnings(),
            ingress: run.ingress.clone(),
        },
        nodes,
        namespace: row.parse::<Namespace>(7, "Namespace")?,
        run,
        cleanup,
        cluster_domain: row.parse_optional(10, "identity")?,
        recorded_runner_lease: standing.lease(),
        standing,
        read_at,
        listed: row.int(22)?,
        service_uploads,
    })
}

/// Whether a Deployment in `status` chooses its upload again as it starts: it is
/// queued, and admission chose `uploads` while one ahead of it could still move them.
fn chooses_upload_at_start(
    status: DeploymentStatus,
    uploads: Option<&uploads::ServiceUploads>,
) -> bool {
    status == DeploymentStatus::Queued
        && uploads.is_some_and(|uploads| uploads.choice == uploads::UploadChoice::AtStart)
}

/// Whether a Deployment in `status` that recorded `run` prepares: it runs, without
/// a Deploy Preview yet.
fn preparing(status: DeploymentStatus, run: &Run) -> bool {
    status == DeploymentStatus::Running && run.preview.is_none()
}

/// Whether a Deployment in `status` of `nodes` that recorded `run` applied without
/// deploying or removing a Service, with work left: a node it never attempted, or a
/// Service that waits in some regions, whatever its node reads. An Unchanged one ran
/// nothing.
fn all_waiting(status: DeploymentStatus, nodes: &[TargetNode], run: &Run) -> bool {
    let mut services = nodes
        .iter()
        .filter(|node| node.service().is_some())
        .filter_map(|node| run.nodes.get(node.id()));
    status == DeploymentStatus::Applied
        && (run.waits() || run.unreached().next().is_some())
        && !services.any(|outcome| outcome.advances())
}

impl Stored {
    /// Show its standing on its summary: the status, number, runner, start and end
    /// [`Standing::shown`] gives at `read_at`, and the fields that follow from that
    /// status.
    fn show(&mut self) {
        let Shown {
            status,
            number,
            runner,
            started_at,
            ended_at,
        } = self.standing.shown(self.read_at);
        self.summary.status = status;
        self.summary.number = number;
        self.summary.runner = runner;
        self.summary.started_at = started_at;
        self.summary.ended_at = ended_at;
        self.summary.live_at = self.run.live_at;
        self.summary.cleanup = self.cleanup.status();
        self.summary.in_flight = status.in_flight();
        self.summary.chooses_upload_at_start =
            chooses_upload_at_start(status, self.service_uploads.as_ref());
        self.summary.preparing = preparing(status, &self.run);
        self.summary.all_waiting = all_waiting(status, &self.nodes, &self.run);
    }
}

/// Write `stored` back, the one writer of a loaded Deployment's standing.
///
/// - It first shows the standing on the summary ([`Stored::show`]), so the summary
///   reads as the row now does, and a caller returns it.
/// - It writes the [`Standing`] as the `status`, `number`, `runner`, `started`,
///   `lease` and `ended` columns, with the run, its outcome and its preview. A claim
///   whose runner lease lapsed it writes as its row holds it, `running` or
///   `cancelling` ([`Standing::kept`]): only an edge that loses the runner writes
///   Unknown, with its end (claim.rs `claim` and record.rs `record`).
///
/// Once the summary says the Deployment can't run any more, at each save:
/// - History drops the sealed values only a Deployment in flight kept, rewriting
///   only a revision that still holds one ([`crate::seal_scrub::history::scrub_sealed`]).
/// - When it shows an end, each build its Servers still hold Building is settled as
///   interrupted, finished at that end ([`crate::build::settle`]): its runner can't
///   report on it any more ([`running`]). One out of flight that shows no end,
///   waiting for CI or a [`Raw`] row with no `ended`, leaves its builds as they are.
fn save(tx: &mut dyn Tx, stored: &mut Stored) -> Result<(), RpcError> {
    if save_when(tx, stored, SaveWhen::Always)? {
        Ok(())
    } else {
        Err(error::conflict(
            "A Deployment that went Live cannot end Cancelled",
            json!({ "deployment": stored.summary.id }),
        ))
    }
}

enum SaveWhen {
    Always,
    ActiveCleanup,
}

fn save_when(tx: &mut dyn Tx, stored: &mut Stored, when: SaveWhen) -> Result<bool, RpcError> {
    stored.show();
    stored.recorded_runner_lease = stored.standing.lease();
    let standing = &stored.standing;
    let number = standing
        .number()
        .map(i64::try_from)
        .transpose()
        .map_err(|_| error::corrupt("Deployment number"))?;
    let guard = match when {
        SaveWhen::Always => String::new(),
        SaveWhen::ActiveCleanup => format!(" AND {}", cleanup::pending_in_time(tx)),
    };
    // The Environment lock keeps the loaded switch evidence stable through this write.
    let changed = tx.execute(
        &format!(
            "UPDATE config_deployment SET status = ?1, run = ?2, runner = ?4, lease = ?5, \
             started = ?6, ended = ?7, number = ?8 WHERE id = ?3{guard} \
             AND (?1 <> 'cancelled' OR CAST(?9 AS BIGINT) IS NULL)"
        ),
        &[
            name_of(standing.kept()).as_str().into(),
            json_text(&stored.run).as_str().into(),
            stored.summary.id.as_str().into(),
            standing.runner().map(RunnerId::as_str).into(),
            standing.lease().into(),
            standing.started().into(),
            standing.ended().into(),
            number.into(),
            stored.run.live_at.into(),
        ],
    )?;
    if changed == 0 {
        return Ok(false);
    }
    if !stored.summary.in_flight {
        crate::seal_scrub::history::scrub_sealed(tx, &stored.summary.environment_id)?;
        if let Some(ended) = stored.summary.ended_at {
            crate::build::settle(tx, &stored.summary.id, ended)?;
        }
    }
    Ok(true)
}

/// The status Deployment `stored`'s row keeps, for a stop of GitHub builds: never one a
/// lapsed runner lease only shows, which a renewal still committing may undo. A claim of
/// `runner` whose lease lapsed is first ended Unknown and saved, as its abandon would, so
/// a stop never waits on a status nothing writes.
pub(crate) fn kept_for_stop(
    tx: &mut dyn Tx,
    mut stored: Stored,
    runner: Option<&RunnerId>,
) -> Result<(DeploymentStatus, Option<RunnerId>), RpcError> {
    let lapsed_ours = runner.is_some_and(|runner| stored.summary.runner.as_ref() == Some(runner))
        && stored.summary.status == DeploymentStatus::Unknown
        && stored.standing.kept().claimed();
    if lapsed_ours {
        stored.standing = stored.standing.lose_runner(now());
        save(tx, &mut stored)?;
    }
    Ok((stored.standing.kept(), stored.standing.runner().cloned()))
}

fn running(stored: &Stored) -> Result<(), RpcError> {
    match stored.summary.status {
        DeploymentStatus::Running | DeploymentStatus::Cancelling => Ok(()),
        DeploymentStatus::Unknown => Err(error::conflict(
            "This Deployment stopped holding its Environment, or a newer one took over, so \
             its outcome stays unknown. Start a new Deployment",
            json!({ "deployment": stored.summary.id }),
        )),
        DeploymentStatus::Applied | DeploymentStatus::Failed | DeploymentStatus::Cancelled => {
            Err(ended(&stored.summary.id))
        }
        DeploymentStatus::WaitingForCi
        | DeploymentStatus::Queued
        | DeploymentStatus::Superseded => Err(error::conflict(
            "Claim this Deployment before recording what it did",
            json!({ "deployment": stored.summary.id }),
        )),
    }
}

#[cfg(test)]
thread_local! {
    /// How many times this thread loaded a Saved revision, for tests that bound it.
    pub(crate) static SAVED_READS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

pub(crate) fn saved_at(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    revision: Revision,
) -> Result<SavedEnvironmentIntent, RpcError> {
    #[cfg(test)]
    SAVED_READS.set(SAVED_READS.get() + 1);
    if revision == NOTHING {
        return scope::load_by_id(tx, environment)
            .map(|loaded| crate::scope::empty(&loaded.working.environment_slug));
    }
    crate::review::saved::intent_at(tx, environment, revision)?
        .ok_or_else(|| error::corrupt("Saved State"))
        .map(canonicalize_environment_intent)
}

fn parse_stored<T: TryFrom<String, Error = RpcError>>(value: &str) -> Result<T, RpcError> {
    T::try_from(value.to_owned()).map_err(|_| error::corrupt("identity"))
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

fn waiting(id: &DeploymentId) -> RpcError {
    error::conflict(
        "This Deployment waits for its commit's CI: it queues once every check passes",
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

#[cfg(test)]
mod runner_lost_tests {
    use super::{lapsed, runner_lost};

    #[test]
    fn a_lost_runner_ended_at_the_earlier_of_its_runner_lease_and_now() {
        // Lapsed: when reads began to show it Unknown.
        assert!(lapsed(90, 100));
        assert_eq!(runner_lost(90, 100), 90);
        // Lapsing now: both are the same moment.
        assert!(lapsed(100, 100));
        assert_eq!(runner_lost(100, 100), 100);
        // Still held: its runner lost it now, before its lease would have.
        assert!(!lapsed(110, 100));
        assert_eq!(runner_lost(110, 100), 100);
    }
}

#[cfg(test)]
mod parity_tests {
    use super::{DeploymentStatus, End, LEASE, Raw, RunnerId, Standing};

    const NOW: i64 = 1_000;

    /// Every column `save` writes from a [`Standing`].
    type Columns = (
        DeploymentStatus,
        Option<u64>,
        Option<RunnerId>,
        Option<i64>,
        i64,
        Option<i64>,
    );

    fn columns(standing: &Standing) -> Columns {
        (
            standing.kept(),
            standing.number(),
            standing.runner().cloned(),
            standing.started(),
            standing.lease(),
            standing.ended(),
        )
    }

    fn raw(standing: &Standing) -> Standing {
        let (status, number, runner, started, lease, ended) = columns(standing);
        Standing::Raw(Raw {
            status,
            number,
            runner,
            started,
            lease,
            ended,
        })
    }

    /// A typed one read again from the columns `save` wrote for it.
    fn again(standing: &Standing) -> Standing {
        let (status, number, runner, started, lease, ended) = columns(standing);
        let again = Standing::decode(status, number, (runner, started, lease), ended);
        assert!(
            !matches!(again, Standing::Raw(_)),
            "{:?}",
            columns(standing)
        );
        again
    }

    fn runner(name: &str) -> RunnerId {
        RunnerId::parse(name).unwrap()
    }

    const STATUSES: [DeploymentStatus; 9] = [
        DeploymentStatus::WaitingForCi,
        DeploymentStatus::Queued,
        DeploymentStatus::Superseded,
        DeploymentStatus::Running,
        DeploymentStatus::Applied,
        DeploymentStatus::Failed,
        DeploymentStatus::Unknown,
        DeploymentStatus::Cancelling,
        DeploymentStatus::Cancelled,
    ];

    /// Every standing `decode` reads typed, over each status and each column
    /// absent or set, with a runner lease lapsed at [`NOW`] or held past it.
    fn typed() -> Vec<Standing> {
        let mut typed = Vec::new();
        for status in STATUSES {
            for number in [None, Some(7)] {
                for runner in [None, Some(runner("runner-a"))] {
                    for started in [None, Some(100)] {
                        for lease in [0, 500, 2_000] {
                            for ended in [None, Some(150)] {
                                let standing = Standing::decode(
                                    status,
                                    number,
                                    (runner.clone(), started, lease),
                                    ended,
                                );
                                if !matches!(standing, Standing::Raw(_)) {
                                    typed.push(standing);
                                }
                            }
                        }
                    }
                }
            }
        }
        typed
    }

    type Edge = (&'static str, fn(Standing) -> Standing);

    const EDGES: [Edge; 13] = [
        ("queue", |standing| standing.queue(8)),
        ("supersede", |standing| standing.supersede(NOW)),
        ("claim_for", |standing| {
            standing.claim_for(&runner("runner-b"), NOW)
        }),
        ("renew", |mut standing| {
            standing.renew(NOW);
            standing
        }),
        ("cancel", |standing| standing.cancel(NOW)),
        ("cancelling", Standing::cancelling),
        ("end applied", |standing| standing.end(End::Applied, NOW)),
        ("end failed", |standing| standing.end(End::Failed, NOW)),
        ("end unknown", |standing| standing.end(End::Unknown, NOW)),
        ("end cancelled", |standing| {
            standing.end(End::Cancelled, NOW)
        }),
        ("lose_runner", |standing| standing.lose_runner(NOW)),
        ("restamp", |mut standing| {
            standing.restamp(NOW);
            standing
        }),
        ("none", |standing| standing),
    ];

    #[test]
    fn a_raw_row_moves_through_every_edge_as_its_typed_twin() {
        let typed = typed();
        for status in STATUSES {
            assert!(
                typed.iter().any(|standing| standing.kept() == status),
                "{status:?} has a typed standing"
            );
        }
        for standing in typed {
            for (first, one) in EDGES {
                for (second, other) in EDGES {
                    let twin = raw(&standing);
                    let before = columns(&standing);
                    let typed = other(one(again(&standing)));
                    let raw = other(one(twin));
                    let path = format!("{before:?} then {first} then {second}");
                    assert_eq!(columns(&raw), columns(&typed), "{path}");
                    for now in [NOW, NOW + 2 * LEASE] {
                        assert_eq!(raw.status(now), typed.status(now), "{path} at {now}");
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod node_pattern_tests {
    use super::{TargetNode, node_pattern};

    #[test]
    fn a_nodes_pattern_is_the_quoted_id_a_targeted_node_serializes_with() {
        let id = "6f1c2a9e-4d3b-4f7a-9c1e-0a5b7d8e2f31";
        let nodes = vec![
            serde_json::from_value::<TargetNode>(serde_json::json!({
                "type": "service", "id": id, "name": "web", "runtime": "web-1",
            }))
            .expect("a Service node"),
        ];
        let text = serde_json::to_string(&nodes).expect("nodes are JSON");
        assert_eq!(node_pattern(id), format!("%\"{id}\"%"));
        assert!(text.contains(node_pattern(id).trim_matches('%')));
        assert!(
            !text.contains(node_pattern("0a5b7d8e-2f31-4f7a-9c1e-6f1c2a9e4d3b").trim_matches('%'))
        );
    }
}
