//! Deployments. Admission freezes what a Deployment ships: a Saved revision and its
//! target nodes, checked to lower. A runner `claim`s it and receives the Deploy
//! Intent lowered from them, secrets still sealed; it `record`s its evidence, and each
//! confirmed Node Outcome advances Applied State.
//! The Head that reviews compare against comes from here too.

pub(crate) mod admit;
mod history;
mod landing;
mod lifecycle;
mod lowering;
pub(crate) mod query;
#[cfg(test)]
mod reference_tests;
#[cfg(test)]
mod retire_tests;
mod rows;
pub(crate) mod uploads;
pub(crate) use history::*;
pub(crate) use landing::*;
pub(crate) use lifecycle::*;
pub(crate) use lowering::*;
pub(crate) use rows::servers;
pub use rows::{
    FailedAt, Failure, RowPhase, RowState, RowTracker, RowTransition, ServerProgress, ServerRow,
    StoppedBy, starts_container,
};

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::config::{
    CompiledNodeConfig, EncryptedSecretValue, EnvironmentNodeType, LowerDeploymentConfig,
    LowerDeploymentConfigFile, LowerDeploymentInput, LowerDeploymentSnapshot,
    LowerDeploymentVolume, RuntimeOutcomeProjection, SavedConfigIntent, SavedEnvironmentIntent,
    SavedServiceIntent, SavedVolumeIntent, SealedFor, ServiceConfig, ServiceImageCredentials,
    ServiceSource, ValuePartOwner, canonicalize_environment_intent, compile_environment_intent,
    lower_deployment, parse_runtime_preview, project_runtime_outcome, redacted_runtime_preview,
};
use ployz_core::{
    ConfigFileName, ConfigName, DeployIntent, DeployOutcome, DeployPreview, DeployWarning,
    DockerVolumeId, ExecutionError, FailedOperation, MachineId, Namespace, PruneScope, RpcError,
    ServiceAttempt, ServiceName, ServiceOutcome, ServiceRemovals, VolumeRemoval,
    VolumeRemovalOutcome,
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
use crate::registry;
use crate::removal::VolumeLoss;
use crate::review::{Head, InFlight};
use crate::scope::{self, Environment, EnvironmentSummary, revision_param};
use crate::sealing::SealingKey;
use crate::storage::{Row, Tx, name_of};
use crate::variables;

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
    /// When it ended, in Unix seconds: its outcome recorded, or cancelled before
    /// it ran.
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
            (None, Some(push)) => push.head.as_str().get(..7).unwrap_or_default().to_owned(),
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

/// How long a runner holds a claimed Deployment without recording anything. Every
/// record renews it; once it lapses the runner is gone and the outcome is unknown.
pub(crate) const LEASE: i64 = 10 * 60;

/// A queued Deployment no runner claimed: its dispatch may have been lost.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Unclaimed {
    pub organization: OrganizationId,
    pub environment: EnvironmentId,
    pub deployment: DeploymentId,
    /// Its place in its Environment's queue: a newer Deployment has a higher number.
    pub number: u64,
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
        /// The runtime Services it tears down beside `runtime`, which it ran as and
        /// no longer does; for a Service the Deployment retires, every runtime
        /// Service it tears down, `runtime` among them.
        #[serde(default, skip_serializing_if = "ServiceRemovals::is_empty")]
        removals: ServiceRemovals,
    },
    Volume {
        id: VolumeId,
        name: VolumeName,
        /// For a Volume the Deployment removes: the Docker Volumes it deletes.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        deletes: Option<Vec<DockerVolumeId>>,
    },
    Config {
        id: ConfigId,
        name: ConfigName,
    },
}

/// The Services among `nodes`.
pub(crate) fn shipped(nodes: &[TargetNode]) -> Vec<ServiceName> {
    nodes
        .iter()
        .filter_map(|node| match node {
            TargetNode::Service { name, .. } => Some(name.clone()),
            TargetNode::Volume { .. } | TargetNode::Config { .. } => None,
        })
        .collect()
}

impl TargetNode {
    fn service(service: &SavedServiceIntent) -> Result<Self, RpcError> {
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
        Ok(Self::Service {
            id: retire.id.clone(),
            name: retire.name.clone(),
            runtime: runtime.clone(),
            removals: retire.runtimes.clone(),
        })
    }

    /// Whether it is a Service the Deployment tears down.
    pub(crate) fn retires(&self) -> bool {
        matches!(self, Self::Service { runtime, removals, .. } if removals.contains(runtime))
    }

    /// The runtime Services it ships or tears down, whose rows are its own.
    pub(crate) fn runtimes(&self) -> Vec<&ServiceName> {
        match self {
            Self::Service { removals, .. } if self.retires() => removals.iter().collect(),
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
            Self::Service { id, .. } => id.as_str(),
            Self::Volume { id, .. } => id.as_str(),
            Self::Config { id, .. } => id.as_str(),
        }
    }

    /// Where views list it: Services, then Volumes, then Configs, each by name, as the
    /// review does.
    pub(crate) fn listed(&self) -> (EnvironmentNodeType, &str) {
        match self {
            Self::Service { name, .. } => (EnvironmentNodeType::Service, name.as_str()),
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
            Self::Service { .. } => {
                !self.retires() && saved.services.iter().any(|service| service.id == id)
            }
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
            Self::Service { id, name, .. } => DeployedNode::Service {
                id: id.clone(),
                name: name.clone(),
            },
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
    /// Deployment tears down what Saved dropped too.
    pub(crate) full: bool,
    /// Whether it also removes every Service of the Namespace this Store never held.
    pub(crate) prune: bool,
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

    /// Whether this Deployment ships Service `name`, named or referenced.
    pub(crate) fn ships(&self, name: &ServiceName) -> bool {
        self.nodes.iter().any(
            |node| matches!(node, TargetNode::Service { name: service, .. } if service == name),
        )
    }
}

/// The runner's evidence so far, stored as one document.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
struct Run {
    /// Parsed and redacted once, when recorded.
    preview: Option<DeployPreview>,
    outcome: Option<Outcome>,
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
    /// Until when, in Unix seconds, its runner holds it without recording anything.
    lease: i64,
    /// Where it lists among its Environment's Deployments: taken once, at insert,
    /// and never moved, so a page cursor can hold it.
    pub(crate) listed: i64,
    /// The digest of the upload each target Service without a source of its own
    /// builds from, by lineage; none for one admitted before it was kept, whose
    /// every such Service builds from its upload.
    pub(crate) service_uploads: Option<uploads::ServiceUploads>,
}

impl Stored {
    /// What it lowers: the Saved Services it ships and the runtime Services it
    /// tears down, beside each shipped one those it no longer runs as. A full
    /// Deploy ships every Service of `saved`, its revision, and tears down each
    /// node `saved` lacks: one frozen before teardowns were, as the runtime
    /// Service it ran as then, unless a Service of `saved` runs as it now and so
    /// replaces it.
    pub(crate) fn lowering(&self, saved: &SavedEnvironmentIntent) -> Ships {
        let full = self.summary.services.is_empty();
        let mut shipped = Vec::new();
        let mut removals = ServiceRemovals::default();
        for node in &self.nodes {
            if let TargetNode::Service {
                id,
                name,
                runtime,
                removals: gone,
            } = node
            {
                if node.retires() {
                    removals.extend(gone.iter().cloned());
                } else if full
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
        }
        if full {
            return Ships::All {
                removals,
                prune: self.run.prune,
            };
        }
        let services = self
            .summary
            .services
            .iter()
            .filter(|service| shipped.contains(service))
            .cloned()
            .collect();
        Ships::Named { services, removals }
    }
}

/// What a Deployment row reads as, from an unaliased `config_deployment`. The
/// second-last column is the [`CiVerdict`] of one waiting for CI: failed once a
/// check suite of its commit completed without passing, by the rule that lets it go.
/// The last is where it lists among its Environment's Deployments.
static COLUMNS: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    let passing = crate::automation::CheckConclusion::PASSING
        .iter()
        .map(|conclusion| format!("'{}'", name_of(conclusion)))
        .collect::<Vec<_>>()
        .join(", ");
    let completed = name_of(crate::automation::CheckStatus::Completed);
    format!(
        "id, environment_id, number, status, saved_revision, services, nodes, namespace, run, \
         upload, cluster_domain, runner, lease, admitted_by, admitted, started, ended, message, \
         push_repository_id, push_branch, push_head, \
         CASE WHEN status = 'waiting_for_ci' THEN \
           CASE WHEN EXISTS (SELECT 1 FROM config_check_suite s \
             WHERE s.organization_id = config_deployment.organization_id \
             AND s.repository_id = config_deployment.push_repository_id \
             AND s.head = config_deployment.push_head AND s.status = '{completed}' \
             AND COALESCE(s.conclusion, '') NOT IN ({passing})) \
           THEN 'failed' ELSE 'pending' END \
         END, listed, service_uploads"
    )
});

/// SQL selecting Deployments that may still run: queued, or claimed by a runner
/// whose lease holds. The inverse of [`lapsed`] for claimed ones. The status set
/// comes first, spelled as the `config_deployment_in_flight` index's predicate is,
/// so SQLite's planner takes the index.
pub(crate) fn in_flight_sql() -> String {
    format!(
        "(status IN ('queued', 'running', 'cancelling') AND (status = 'queued' OR lease > {}))",
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
pub(crate) fn owned(tx: &mut dyn Tx, who: &Actor, id: &DeploymentId) -> Result<Stored, RpcError> {
    let columns = COLUMNS.as_str();
    let rows = tx.query(
        &format!("SELECT {columns} FROM config_deployment WHERE id = ?1 AND organization_id = ?2"),
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
    let columns = COLUMNS.as_str();
    let rows = tx.query(
        &format!("SELECT {columns} FROM config_deployment WHERE id = ?1"),
        &[id.as_str().into()],
    )?;
    stored(rows.first().ok_or_else(|| missing(id))?)
}

fn stored(row: &Row) -> Result<Stored, RpcError> {
    let status: DeploymentStatus = row.variant(3, "Deployment status")?;
    let lease = row.int(12)?;
    let run: Run = row.json(8, "Deployment run")?;
    let status = match lapsed(status, lease) {
        true => DeploymentStatus::Unknown,
        false => status,
    };
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
    let service_uploads: Option<uploads::ServiceUploads> = row
        .optional_text(23)?
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| error::corrupt("Deployment uploads"))?;
    Ok(Stored {
        summary: DeploymentSummary {
            id: row.parse(0, "identity")?,
            environment_id: row.parse(1, "identity")?,
            number: row
                .optional_int(2)?
                .map(u64::try_from)
                .transpose()
                .map_err(|_| error::corrupt("Deployment number"))?,
            status,
            saved: row.number(4, "revision")?,
            services: row.json(5, "Deployment")?,
            ships: shipped(&nodes),
            runner: row.parse_optional(11, "identity")?,
            upload: row
                .optional_text(9)?
                .map(serde_json::from_str)
                .transpose()
                .map_err(|_| error::corrupt("Deployment upload"))?,
            chooses_upload_at_start: chooses_upload_at_start(status, service_uploads.as_ref()),
            preparing: preparing(status, &run),
            remove: row.number::<Revision>(4, "revision")? == NOTHING,
            prune: run.prune,
            admitted_by: row.parse_optional(13, "identity")?,
            admitted_at: row.int(14)?,
            started_at: row.optional_int(15)?,
            ended_at: row.optional_int(16)?,
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
        cluster_domain: row.parse_optional(10, "identity")?,
        lease,
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
    status == DeploymentStatus::Queued && uploads.is_some_and(|uploads| uploads.provisional)
}

/// Whether a Deployment in `status` that recorded `run` prepares: it runs, without
/// a Deploy Preview yet.
fn preparing(status: DeploymentStatus, run: &Run) -> bool {
    status == DeploymentStatus::Running && run.preview.is_none()
}

/// Write `stored`'s status and run, keeping `in_flight`, `chooses_upload_at_start`
/// and `preparing` in step with them.
fn save(tx: &mut dyn Tx, stored: &mut Stored) -> Result<(), RpcError> {
    stored.summary.in_flight = stored.summary.status.in_flight();
    stored.summary.chooses_upload_at_start =
        chooses_upload_at_start(stored.summary.status, stored.service_uploads.as_ref());
    stored.summary.preparing = preparing(stored.summary.status, &stored.run);
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
    if !stored.summary.in_flight {
        crate::review::scrub_sealed(tx, &stored.summary.environment_id)?;
    }
    Ok(())
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

/// Seconds since the Unix epoch: when a Deployment was admitted, which the idle
/// rule for Branches reads.
pub(crate) fn now() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| {
            i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX)
        })
}
