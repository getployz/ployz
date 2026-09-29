//! Deployments. Admission freezes what a Deployment ships: a Saved revision and its
//! target nodes, checked to lower. A runner `claim`s it and receives the Deploy
//! Intent lowered from them, secrets unsealed; it `record`s its evidence, and each
//! confirmed Node Outcome advances Applied State.
//! The Head that reviews compare against comes from here too.

use std::collections::BTreeMap;

use ployz_core::config::{
    CompiledNodeConfig, EncryptedSecretValue, LowerDeploymentInput, LowerDeploymentSnapshot,
    LowerDeploymentVolume, RuntimeOutcomeProjection, SavedEnvironmentIntent, SavedServiceIntent,
    SavedVolumeIntent, ServiceConfig, ServiceImageCredentials, ServiceSource,
    canonicalize_environment_intent, compile_environment_intent, lower_deployment,
    parse_runtime_preview, project_runtime_outcome,
};
use ployz_core::{
    DeployIntent, DeployOutcome, DeployPreview, DockerVolumeId, ExecutionError, Namespace,
    RpcError, ServiceAttempt, ServiceName, VolumeRemoval, VolumeRemovalOutcome,
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
use crate::review::{self, Head};
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
}

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
    /// messages; each node's Node Outcome is on the Deployment's nodes.
    Executed { summary: Value },
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
    pub(crate) environment: EnvironmentId,
    namespace: Namespace,
    cluster_domain: Option<Hostname>,
    pub(crate) nodes: Vec<TargetNode>,
    run: Run,
    /// Until when, in Unix seconds, its runner holds it without recording anything.
    lease: i64,
}

const COLUMNS: &str = "id, environment_id, number, status, saved_revision, services, nodes, \
     namespace, run, upload, cluster_domain, runner, lease, admitted_by, admitted, started, ended";

/// SQL selecting Deployments that may still run: queued, or claimed by a runner
/// whose lease holds.
pub(crate) fn in_flight_sql() -> String {
    format!(
        "(status = '{}' OR (status IN ('{}', '{}') AND lease > {}))",
        name_of(DeploymentStatus::Queued),
        name_of(DeploymentStatus::Running),
        name_of(DeploymentStatus::Cancelling),
        now()
    )
}

/// Freeze a Deployment of `saved`: its target nodes, checked to lower to a Deploy
/// Intent. `services` narrows it; none targets every Service and Volume, including
/// the removal of those Applied State holds and `saved` does not, which deletes the
/// Docker Volumes `losses` names. Generated domains expand under `cluster_domain`;
/// a plan, which has none, checks the rest.
pub(crate) fn freeze(
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    applied: &SavedEnvironmentIntent,
    services: &[ServiceName],
    namespace: Namespace,
    cluster_domain: Option<&Hostname>,
    losses: &[VolumeLoss],
) -> Result<Frozen, RpcError> {
    let mut nodes: Vec<TargetNode> = if services.is_empty() {
        saved
            .services
            .iter()
            .chain(
                applied
                    .services
                    .iter()
                    .filter(|old| !saved.services.iter().any(|new| new.id == old.id)),
            )
            .map(TargetNode::service)
            .collect::<Result<_, _>>()?
    } else {
        services
            .iter()
            .map(|name| {
                saved
                    .services
                    .iter()
                    .find(|service| service.slug == name.as_str())
                    .map(TargetNode::service)
                    .transpose()?
                    .ok_or_else(|| {
                        error::not_found(
                            format!("No Service named {name} to deploy"),
                            json!({ "services": saved.services.iter().map(|service| &service.slug).collect::<Vec<_>>() }),
                        )
                    })
            })
            .collect::<Result<Vec<_>, _>>()?
    };
    // A full Deploy applies every Volume; a narrowed one those its Services mount.
    let mounted = |volume: &SavedVolumeIntent| {
        saved.services.iter().any(|service| {
            nodes.iter().any(|node| node.id() == service.id)
                && service
                    .volume_attachments
                    .iter()
                    .any(|mount| mount.volume_resource_id == volume.resource_id)
        })
    };
    let kept: Vec<TargetNode> = saved
        .volumes
        .iter()
        .filter(|volume| services.is_empty() || mounted(volume))
        .map(|volume| TargetNode::volume(volume, None))
        .collect::<Result<_, _>>()?;
    nodes.extend(kept);
    if services.is_empty() {
        for loss in losses {
            let volume = applied
                .volumes
                .iter()
                .find(|volume| volume.resource_id == loss.volume.id.as_str())
                .ok_or_else(|| error::corrupt("Applied State"))?;
            nodes.push(TargetNode::volume(volume, Some(loss.deletes.clone()))?);
        }
    }
    // Live values and Setup Commands resolve at claim; checking without them is the same.
    lower(
        environment,
        saved,
        services,
        (namespace.clone(), cluster_domain),
        &crate::branch::Lowering::default(),
        None,
    )?;
    Ok(Frozen {
        nodes,
        namespace,
        credentials: BTreeMap::new(),
        cluster_domain: cluster_domain.cloned(),
    })
}

/// Lower Saved revision `saved` to the Deploy Intent of a Deployment of `services`
/// (none: every Service) into its Namespace, with generated domains under the
/// Cluster Domain. Variables resolve here, a Branch's Live Node references against
/// `branch.live`; secrets, and values that reference one, only with `unseal`, and
/// are left out without it.
fn lower(
    environment: &EnvironmentId,
    saved: &SavedEnvironmentIntent,
    services: &[ServiceName],
    (namespace, cluster_domain): (Namespace, Option<&Hostname>),
    branch: &crate::branch::Lowering,
    unseal: Option<&SealingKey>,
) -> Result<(Value, DeployIntent), RpcError> {
    let mut compiled = compile_environment_intent(
        environment.as_str(),
        crate::domain::expand(saved, cluster_domain),
    );
    // Live values order nothing: what provides them runs elsewhere.
    let lineages: BTreeMap<String, String> = compiled
        .variable_producers
        .iter()
        .map(|producer| (producer.owner_lineage_id.clone(), producer.owner_id.clone()))
        .collect();
    compiled
        .variable_producers
        .extend(branch.live.iter().cloned());
    let mut resolved = variables::resolve(&compiled, unseal)?;
    let targeted: Vec<&str> = saved
        .services
        .iter()
        .filter(|service| services.iter().any(|name| name.as_str() == service.slug))
        .map(|service| service.id.as_str())
        .collect();
    let snapshots: Vec<(ServiceConfig, LowerDeploymentSnapshot)> = compiled
        .node_snapshots
        .into_iter()
        .filter_map(|node| match node.snapshot.0 {
            // A targeted Deployment builds only the Git Services it targets.
            CompiledNodeConfig::Service(config)
                if !services.is_empty()
                    && matches!(config.settings.source, ServiceSource::Git { .. })
                    && !targeted.contains(&node.node_id.as_str()) =>
            {
                None
            }
            CompiledNodeConfig::Service(config) => Some((
                *config,
                LowerDeploymentSnapshot {
                    resolved_env: resolved.remove(&node.node_id).unwrap_or_default(),
                    setup_commands: branch.setup.get(&node.node_id).cloned().unwrap_or_default(),
                    service_id: Some(node.node_id),
                    config: Value::Null,
                    replicas: None,
                },
            )),
            CompiledNodeConfig::Volume(_) => None,
        })
        .collect();
    // Empty reconciles the whole Namespace; names deploy only those Services.
    let selected = saved
        .services
        .iter()
        .filter(|service| services.iter().any(|name| name.as_str() == service.slug))
        .map(|service| ServiceAttempt {
            name: service.config.private_dns.clone(),
        })
        .collect::<Vec<_>>();
    let input = |snapshots: Vec<LowerDeploymentSnapshot>| LowerDeploymentInput {
        namespace: namespace.clone(),
        snapshots,
        volumes: saved
            .volumes
            .iter()
            .map(|volume| LowerDeploymentVolume {
                volume_resource_id: volume.resource_id.clone(),
            })
            .collect(),
        lineages: lineages.clone(),
        selected: Some(selected.clone()),
    };
    let with = |built_later: bool| {
        snapshots
            .iter()
            .map(|(config, snapshot)| LowerDeploymentSnapshot {
                config: json_value(&if built_later {
                    built(config)
                } else {
                    config.clone()
                }),
                ..snapshot.clone()
            })
            .collect()
    };
    let intent = lower_deployment(input(with(true))).map_err(|error| {
        error::invalid(
            format!("This Environment can't deploy: {}", error.message),
            json!({ "path": error.path }),
        )
    })?;
    Ok((json_value(&input(with(false))), intent))
}

/// `config` with a Git source replaced by the image its build will produce, as the
/// SDK's preparation does once it built it.
fn built(config: &ServiceConfig) -> ServiceConfig {
    let mut config = config.clone();
    if matches!(config.settings.source, ServiceSource::Git { .. }) {
        config.settings.source = ServiceSource::Image {
            version: 1,
            image: format!("ployz-build/{}:pending", config.settings.private_dns),
            credentials: ServiceImageCredentials::None,
        };
    }
    config
}

fn json_value(value: &impl Serialize) -> Value {
    serde_json::to_value(value).expect("lowering input is JSON")
}

/// Admit a frozen Deployment of Saved revision `saved`. A Deployment still queued is
/// superseded: the newest admission replaces the pending one.
pub(crate) fn admit(
    tx: &mut dyn Tx,
    who: &Actor,
    (id, services, upload): (&DeploymentId, &[ServiceName], Option<UploadedSource>),
    environment: &EnvironmentId,
    saved: Revision,
    frozen: &Frozen,
) -> Result<DeploymentSummary, RpcError> {
    let environment_id = environment.as_str();
    // Without a new upload, Services without a source keep building from the latest one.
    let upload = match upload {
        Some(upload) => {
            upload.check()?;
            Some(upload)
        }
        None => match tx
            .query(
                "SELECT upload FROM config_deployment \
                 WHERE environment_id = ?1 AND upload <> 'null' ORDER BY number DESC LIMIT 1",
                &[environment_id.into()],
            )?
            .first()
        {
            Some(row) => row.json(0, "Deployment upload")?,
            None => None,
        },
    };
    let number = queue(tx, environment)?;
    let summary = DeploymentSummary {
        id: id.clone(),
        number,
        status: DeploymentStatus::Queued,
        saved,
        services: services.to_vec(),
        runner: None,
        upload,
        remove: saved == NOTHING,
        admitted_by: who.principal.clone(),
        admitted_at: now(),
        started_at: None,
        ended_at: None,
    };
    tx.execute(
        "INSERT INTO config_deployment \
         (id, organization_id, environment_id, number, status, saved_revision, services, nodes, \
          namespace, run, credentials, upload, cluster_domain, admitted, admitted_by) \
         VALUES (?1, ?2, ?3, ?4, 'queued', ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        &[
            id.as_str().into(),
            who.organization.as_str().into(),
            environment_id.into(),
            i64::try_from(number)
                .map_err(|_| error::corrupt("Deployment number"))?
                .into(),
            revision_param(saved)?.into(),
            json_text(&summary.services).as_str().into(),
            json_text(&frozen.nodes).as_str().into(),
            frozen.namespace.as_str().into(),
            json_text(&Run::default()).as_str().into(),
            json_text(&frozen.credentials).as_str().into(),
            json_text(&summary.upload).as_str().into(),
            frozen.cluster_domain.as_ref().map(Hostname::as_str).into(),
            summary.admitted_at.into(),
            who.principal.as_ref().map(Principal::as_str).into(),
        ],
    )?;
    Ok(summary)
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

/// Supersede `environment`'s queued Deployment, if any, and number the next one.
fn queue(tx: &mut dyn Tx, environment: &EnvironmentId) -> Result<u64, RpcError> {
    tx.execute(
        "UPDATE config_deployment SET status = 'superseded' \
         WHERE environment_id = ?1 AND status = 'queued'",
        &[environment.as_str().into()],
    )?;
    let number = tx
        .query(
            "SELECT COALESCE(MAX(number), 0) FROM config_deployment WHERE environment_id = ?1",
            &[environment.as_str().into()],
        )?
        .first()
        .ok_or_else(|| error::corrupt("Deployment number"))?
        .int(0)?;
    u64::try_from(number + 1).map_err(|_| error::corrupt("Deployment number"))
}

/// Queue Deployment `id` shipping exactly what `source` froze: its Saved revision,
/// targets, Namespace, registry credentials and upload, whatever changed since. Only a Deployment that ended
/// without applying can be retried: failed, unknown or cancelled.
pub(crate) fn retry(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &DeploymentId,
    source: &DeploymentId,
) -> Result<DeploymentSummary, RpcError> {
    owned(tx, who, source)?;
    let stored = locked(tx, source)?;
    match stored.summary.status {
        DeploymentStatus::Failed | DeploymentStatus::Unknown | DeploymentStatus::Cancelled => {}
        DeploymentStatus::Applied => {
            return Err(error::conflict(
                "This Deployment applied, so there is nothing to retry",
                json!({ "deployment": source }),
            ));
        }
        DeploymentStatus::Queued | DeploymentStatus::Running | DeploymentStatus::Cancelling => {
            return Err(error::conflict(
                "This Deployment hasn't ended: cancel it or wait before retrying it",
                json!({ "deployment": source }),
            ));
        }
        DeploymentStatus::Superseded => return Err(superseded(source)),
    }
    if stored.summary.remove {
        let environment = scope::load_by_id(tx, &stored.environment)?;
        crate::teardown::guard_removal(tx, &environment)?;
    }
    let number = queue(tx, &stored.environment)?;
    let admitted_at = now();
    // Every frozen column comes from the source, so a retry never re-reads authored state.
    tx.execute(
        "INSERT INTO config_deployment \
         (id, organization_id, environment_id, number, status, saved_revision, services, nodes, \
          namespace, run, credentials, upload, cluster_domain, admitted, admitted_by) \
         SELECT ?1, organization_id, environment_id, ?2, 'queued', saved_revision, services, \
          nodes, namespace, ?3, credentials, upload, cluster_domain, ?5, ?6 \
         FROM config_deployment WHERE id = ?4",
        &[
            id.as_str().into(),
            i64::try_from(number)
                .map_err(|_| error::corrupt("Deployment number"))?
                .into(),
            json_text(&Run::default()).as_str().into(),
            source.as_str().into(),
            admitted_at.into(),
            who.principal.as_ref().map(Principal::as_str).into(),
        ],
    )?;
    // A retry builds the commits its source pinned.
    tx.execute(
        "INSERT INTO config_build \
         (deployment_id, service, organization_id, commit_sha, status, message, log) \
         SELECT ?1, service, organization_id, commit_sha, 'pending', NULL, '' \
         FROM config_build WHERE deployment_id = ?2",
        &[id.as_str().into(), source.as_str().into()],
    )?;
    Ok(DeploymentSummary {
        id: id.clone(),
        number,
        status: DeploymentStatus::Queued,
        runner: None,
        admitted_by: who.principal.clone(),
        admitted_at,
        started_at: None,
        ended_at: None,
        ..stored.summary
    })
}

/// A queued Deployment of `who`'s Organization, checked that a runner may still
/// claim it. Nothing changes: the caller hands it to a runner.
pub(crate) fn start(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &DeploymentId,
) -> Result<DeploymentSummary, RpcError> {
    owned(tx, who, id)?;
    let stored = locked(tx, id)?;
    match stored.summary.status {
        DeploymentStatus::Queued => Ok(stored.summary),
        DeploymentStatus::Running | DeploymentStatus::Cancelling => Err(error::conflict(
            "This Deployment is already running",
            json!({ "deployment": id }),
        )),
        DeploymentStatus::Superseded => Err(superseded(id)),
        DeploymentStatus::Applied
        | DeploymentStatus::Failed
        | DeploymentStatus::Unknown
        | DeploymentStatus::Cancelled => Err(ended(id)),
    }
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

/// Bind a queued Deployment to `runner` and hand over its frozen Deploy Intent.
/// Claiming again as the same runner returns the same until it records a Deploy
/// Preview; after that it may have executed, so a second claim leaves the Deployment
/// `unknown` and refuses: nothing replays silently. A Deployment another runner still
/// holds without an outcome reads `unknown` from here on.
///
/// The outer error rolls back; the inner one is a refusal that keeps what it recorded.
pub(crate) fn claim(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    runner: &RunnerId,
    sealing: &SealingKey,
) -> Result<Result<Claimed, RpcError>, RpcError> {
    let mut stored = locked(tx, id)?;
    match stored.summary.status {
        DeploymentStatus::Running | DeploymentStatus::Cancelling
            if stored.summary.runner.as_ref() == Some(runner) =>
        {
            if stored.run.preview.is_some() {
                stored.summary.status = DeploymentStatus::Unknown;
                save(tx, &stored)?;
                return Ok(Err(error::conflict(
                    "This Deployment's runner lost track of it after preparing it, so what \
                     ran is unknown. Start a new Deployment",
                    json!({ "deployment": id }),
                )));
            }
            stored.lease = now() + LEASE;
            save(tx, &stored)?;
        }
        DeploymentStatus::Running | DeploymentStatus::Cancelling => {
            return Ok(Err(owned_elsewhere(id)));
        }
        DeploymentStatus::Superseded => return Ok(Err(superseded(id))),
        DeploymentStatus::Applied
        | DeploymentStatus::Failed
        | DeploymentStatus::Unknown
        | DeploymentStatus::Cancelled => return Ok(Err(ended(id))),
        DeploymentStatus::Queued => {
            tx.execute(
                "UPDATE config_deployment SET status = 'unknown' \
                 WHERE environment_id = ?1 AND status IN ('running', 'cancelling')",
                &[stored.environment.as_str().into()],
            )?;
            let started = now();
            stored.summary.status = DeploymentStatus::Running;
            stored.summary.runner = Some(runner.clone());
            stored.summary.started_at = Some(started);
            stored.lease = started + LEASE;
            save(tx, &stored)?;
        }
    }
    let sources = build::sources_of(tx, &stored)?;
    let uploads = build::uploads_of(tx, &stored)?;
    let saved = saved_at(tx, &stored.environment, stored.summary.saved)?;
    let branch = crate::branch::lowering(tx, &stored.environment, &saved)?;
    let (input, mut intent) = lower(
        &stored.environment,
        &saved,
        &stored.summary.services,
        (stored.namespace, stored.cluster_domain.as_ref()),
        &branch,
        Some(sealing),
    )?;
    let credentials = tx.query(
        "SELECT credentials FROM config_deployment WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    let credentials = credentials
        .first()
        .ok_or_else(|| missing(id))?
        .text(0)
        .and_then(|text| {
            serde_json::from_str(text).map_err(|_| error::corrupt("Deployment credentials"))
        })?;
    intent.registry_auth = registry::unseal(credentials, sealing)?;
    let receipts = receipts(tx, &stored.environment)?;
    let deletes = stored
        .nodes
        .iter()
        .filter_map(|node| match node {
            TargetNode::Volume { deletes, .. } => deletes.clone(),
            TargetNode::Service { .. } => None,
        })
        .flatten()
        .collect();
    let organization = build::organization(tx, id)?;
    let build_order = crate::builders::order(tx, organization.as_str())?;
    Ok(Ok(Claimed {
        deployment: stored.summary,
        intent,
        input,
        receipts,
        sources,
        deletes,
        uploads,
        build_order,
    }))
}

/// The latest build receipt of each Service of `environment`, by runtime name.
pub(crate) fn receipts(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<BTreeMap<ServiceName, Value>, RpcError> {
    tx.query(
        "SELECT service, receipt FROM config_build_receipt WHERE environment_id = ?1",
        &[environment.as_str().into()],
    )?
    .iter()
    .map(|row| {
        Ok((
            row.parse::<ServiceName>(0, "receipt")?,
            row.json(1, "receipt")?,
        ))
    })
    .collect()
}

/// Replace `service`'s latest build receipt in `environment`.
pub(crate) fn save_receipt(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    service: &ServiceName,
    receipt: &Value,
) -> Result<(), RpcError> {
    if !receipt.is_object() {
        return Err(invalid_evidence("build receipt"));
    }
    tx.execute(
        "INSERT INTO config_build_receipt (environment_id, service, organization_id, receipt) \
         SELECT id, ?2, organization_id, ?3 FROM config_environment WHERE id = ?1 \
         ON CONFLICT (environment_id, service) DO UPDATE SET receipt = excluded.receipt",
        &[
            environment.as_str().into(),
            service.as_str().into(),
            json_text(receipt).as_str().into(),
        ],
    )?;
    Ok(())
}

/// The lowering input of `stored`, secrets unsealed: what a build of it takes.
/// In-process only.
pub(crate) fn input(
    tx: &mut dyn Tx,
    stored: &Stored,
    sealing: &SealingKey,
) -> Result<Value, RpcError> {
    let saved = saved_at(tx, &stored.environment, stored.summary.saved)?;
    let branch = crate::branch::lowering(tx, &stored.environment, &saved)?;
    let (input, _) = lower(
        &stored.environment,
        &saved,
        &stored.summary.services,
        (stored.namespace.clone(), stored.cluster_domain.as_ref()),
        &branch,
        Some(sealing),
    )?;
    Ok(input)
}

/// Cancel a Deployment of `who`'s Organization. A queued one never runs; a running
/// one reads `cancelling` until its runner stops it and records what ran. Cancelling
/// again changes nothing.
pub(crate) fn cancel(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &DeploymentId,
) -> Result<DeploymentSummary, RpcError> {
    owned(tx, who, id)?;
    let mut stored = locked(tx, id)?;
    stored.summary.status = match stored.summary.status {
        DeploymentStatus::Queued => {
            stored.summary.ended_at = Some(now());
            DeploymentStatus::Cancelled
        }
        DeploymentStatus::Running => DeploymentStatus::Cancelling,
        DeploymentStatus::Cancelling | DeploymentStatus::Cancelled => return Ok(stored.summary),
        DeploymentStatus::Superseded
        | DeploymentStatus::Applied
        | DeploymentStatus::Failed
        | DeploymentStatus::Unknown => return Err(ended(id)),
    };
    save(tx, &stored)?;
    Ok(stored.summary)
}

/// Record `runner`'s evidence. Only the runner that claimed the Deployment may, and
/// recording the same evidence again changes nothing.
pub(crate) fn record(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    runner: &RunnerId,
    evidence: RunEvidence,
) -> Result<DeploymentSummary, RpcError> {
    let mut stored = locked(tx, id)?;
    if stored.summary.runner.as_ref() != Some(runner) {
        return Err(owned_elsewhere(id));
    }
    // Any record while it runs shows the runner is still there.
    if matches!(
        stored.summary.status,
        DeploymentStatus::Running | DeploymentStatus::Cancelling
    ) {
        stored.lease = now() + LEASE;
    }
    match evidence {
        RunEvidence::Alive => {
            running(&stored)?;
            save(tx, &stored)?;
            Ok(stored.summary)
        }
        RunEvidence::Prepared(preview) => {
            let preview = serde_json::to_value(preview)
                .ok()
                .and_then(|preview| parse_runtime_preview(preview).ok())
                .ok_or_else(|| invalid_evidence("Deploy Preview"))?;
            let preview = serde_json::to_value(preview).expect("a Deploy Preview is JSON");
            match &stored.run.preview {
                Some(recorded) if *recorded == preview => return Ok(stored.summary),
                Some(_) => {
                    return Err(error::conflict(
                        "This Deployment already recorded a different Deploy Preview",
                        json!({ "deployment": id }),
                    ));
                }
                None => {}
            }
            running(&stored)?;
            stored.run.preview = Some(preview);
            save(tx, &stored)?;
            Ok(stored.summary)
        }
        RunEvidence::Executed { outcome, removed } => {
            let Some(preview) = stored.run.preview.clone() else {
                return Err(error::conflict(
                    "Record the Deploy Preview before what executing it did",
                    json!({ "deployment": id }),
                ));
            };
            let success = matches!(*outcome, DeployOutcome::Success { .. });
            let projection =
                project_runtime_outcome(preview, json!({ "version": 1, "outcome": outcome }))
                    .map_err(|_| invalid_evidence("Deploy Outcome"))?;
            let saved = saved_at(tx, &stored.environment, stored.summary.saved)?;
            let applied = applied_state(tx, &stored.environment, &saved)?;
            let nodes = node_outcomes(&stored.nodes, &saved, &applied, &projection, &removed);
            // A Deploy that left a Volume's data behind didn't finish: a retry removes it.
            let status = if success && nodes.values().all(|status| *status != NodeStatus::Failed) {
                DeploymentStatus::Applied
            } else {
                DeploymentStatus::Failed
            };
            let outcome = Outcome::Executed {
                summary: serde_json::to_value(projection.summary).expect("a summary is JSON"),
            };
            stored.run.nodes = nodes;
            finish(tx, stored, outcome, status)
        }
        RunEvidence::Built(receipts) => {
            running(&stored)?;
            for (service, receipt) in &receipts {
                save_receipt(tx, &stored.environment, service, receipt)?;
            }
            Ok(stored.summary)
        }
        RunEvidence::Build(report) => {
            running(&stored)?;
            build::record(tx, &stored, &report)?;
            Ok(stored.summary)
        }
        RunEvidence::NotExecuted(reason) => {
            let reason = reason.chars().take(500).collect();
            finish(
                tx,
                stored,
                Outcome::NotExecuted {
                    reason,
                    needs_upload: Vec::new(),
                },
                DeploymentStatus::Failed,
            )
        }
        RunEvidence::UploadNeeded(services) => {
            let names = services
                .iter()
                .map(ServiceName::as_str)
                .collect::<Vec<_>>()
                .join(", ");
            finish(
                tx,
                stored,
                Outcome::NotExecuted {
                    reason: format!(
                        "Upload the source again: {names} has no upload to build from and no \
                         usable image to reuse"
                    ),
                    needs_upload: services,
                },
                DeploymentStatus::Failed,
            )
        }
        RunEvidence::Abandoned => {
            if stored.run.outcome.is_some() || stored.summary.status == DeploymentStatus::Unknown {
                return Ok(stored.summary);
            }
            running(&stored)?;
            if stored.run.preview.is_none() {
                let reason = "Its runner stopped before executing anything".to_owned();
                return finish(
                    tx,
                    stored,
                    Outcome::NotExecuted {
                        reason,
                        needs_upload: Vec::new(),
                    },
                    DeploymentStatus::Failed,
                );
            }
            stored.summary.status = DeploymentStatus::Unknown;
            save(tx, &stored)?;
            Ok(stored.summary)
        }
    }
}

/// Each target node's Node Outcome. A Service's comes from its operations: all
/// completed is Deployed (Removed once it left Saved State), some ran is Failed,
/// none ran is Not attempted, none planned is Unchanged. A kept Volume follows the
/// targeted Services mounting it, and is Unchanged when Applied State already holds
/// it as saved. A removed Volume is Removed once the Deploy succeeded and every
/// Docker Volume it deletes is gone; Failed when one wasn't deleted, and Not
/// attempted when the Deploy failed first.
fn node_outcomes(
    nodes: &[TargetNode],
    saved: &SavedEnvironmentIntent,
    applied: &SavedEnvironmentIntent,
    projection: &RuntimeOutcomeProjection,
    removed: &[VolumeRemoval],
) -> BTreeMap<String, NodeStatus> {
    let success = matches!(
        projection.summary,
        ployz_core::config::RuntimeOutcomeSummary::Success { .. }
    );
    let service = |name: &ServiceName, kept: bool| {
        if projection.confirmed_services.contains(name) {
            if kept {
                NodeStatus::Deployed
            } else {
                NodeStatus::Removed
            }
        } else if projection.failed_services.contains(name) {
            NodeStatus::Failed
        } else if projection.unattempted_services.contains(name) {
            NodeStatus::NotAttempted
        } else {
            NodeStatus::Unchanged
        }
    };
    let gone = |id: &DockerVolumeId| {
        removed.iter().any(|removal| {
            removal.id == *id && matches!(removal.outcome, VolumeRemovalOutcome::Removed)
        })
    };
    let kept_volume = |volume: &SavedVolumeIntent| {
        let mounting: Vec<NodeStatus> = saved
            .services
            .iter()
            .filter(|service| {
                nodes.iter().any(|node| node.id() == service.id)
                    && service
                        .volume_attachments
                        .iter()
                        .any(|mount| mount.volume_resource_id == volume.resource_id)
            })
            .map(|mounting| service(&mounting.config.private_dns, true))
            .collect();
        if mounting.contains(&NodeStatus::Failed) {
            NodeStatus::Failed
        } else if mounting.contains(&NodeStatus::NotAttempted) {
            NodeStatus::NotAttempted
        } else if applied.volumes.contains(volume) {
            NodeStatus::Unchanged
        } else {
            NodeStatus::Deployed
        }
    };
    nodes
        .iter()
        .map(|node| {
            let status = match node {
                TargetNode::Service { id, runtime, .. } => service(
                    runtime,
                    saved.services.iter().any(|kept| kept.id == id.as_str()),
                ),
                TargetNode::Volume {
                    deletes: Some(_), ..
                } if !success => NodeStatus::NotAttempted,
                TargetNode::Volume {
                    deletes: Some(deletes),
                    ..
                } if deletes.iter().all(gone) => NodeStatus::Removed,
                TargetNode::Volume {
                    deletes: Some(_), ..
                } => NodeStatus::Failed,
                TargetNode::Volume {
                    id, deletes: None, ..
                } => saved
                    .volumes
                    .iter()
                    .find(|volume| volume.resource_id == id.as_str())
                    .map_or(NodeStatus::Unchanged, kept_volume),
            };
            (node.id().to_owned(), status)
        })
        .collect()
}

fn finish(
    tx: &mut dyn Tx,
    mut stored: Stored,
    outcome: Outcome,
    status: DeploymentStatus,
) -> Result<DeploymentSummary, RpcError> {
    match &stored.run.outcome {
        Some(recorded) if *recorded == outcome => return Ok(stored.summary),
        Some(_) => {
            return Err(error::conflict(
                "This Deployment already recorded a different outcome",
                json!({ "deployment": stored.summary.id }),
            ));
        }
        None => running(&stored)?,
    }
    let advanced: Vec<&TargetNode> = stored
        .nodes
        .iter()
        .filter(|node| {
            stored
                .run
                .nodes
                .get(node.id())
                .is_some_and(|status| status.advances())
        })
        .collect();
    if !advanced.is_empty() {
        let saved = saved_at(tx, &stored.environment, stored.summary.saved)?;
        for node in advanced {
            let applied = match node {
                TargetNode::Service { .. } => saved
                    .services
                    .iter()
                    .find(|service| service.id == node.id())
                    .map(scope::Node::Service),
                TargetNode::Volume { .. } => saved
                    .volumes
                    .iter()
                    .find(|volume| volume.resource_id == node.id())
                    .map(scope::Node::Volume),
            };
            match applied {
                Some(applied) => tx.execute(
                    "INSERT INTO config_applied \
                     (environment_id, node_id, organization_id, deployment_id, node, node_type) \
                     SELECT environment_id, ?2, organization_id, id, ?3, ?4 \
                     FROM config_deployment WHERE id = ?1 \
                     ON CONFLICT (environment_id, node_id) \
                     DO UPDATE SET deployment_id = excluded.deployment_id, node = excluded.node",
                    &[
                        stored.summary.id.as_str().into(),
                        node.id().into(),
                        applied.document().as_str().into(),
                        applied.node_type().into(),
                    ],
                )?,
                None => tx.execute(
                    "DELETE FROM config_applied WHERE environment_id = ?1 AND node_id = ?2",
                    &[stored.environment.as_str().into(), node.id().into()],
                )?,
            };
        }
    }
    // A cancelled Deployment that stopped short reads cancelled, not failed.
    stored.summary.status = match (stored.summary.status, status) {
        (DeploymentStatus::Cancelling, DeploymentStatus::Failed) => DeploymentStatus::Cancelled,
        _ => status,
    };
    stored.summary.ended_at = Some(now());
    stored.run.outcome = Some(outcome);
    save(tx, &stored)?;
    Ok(stored.summary)
}

/// `environment`'s latest `limit` Deployments that weren't superseded, newest first.
pub(crate) fn history(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    limit: i64,
) -> Result<Vec<DeploymentSummary>, RpcError> {
    tx.query(
        &format!(
            "SELECT {COLUMNS} FROM config_deployment \
             WHERE environment_id = ?1 AND status <> 'superseded' ORDER BY number DESC LIMIT ?2"
        ),
        &[environment.as_str().into(), limit.into()],
    )?
    .iter()
    .map(|row| stored(row).map(|stored| stored.summary))
    .collect()
}

/// Applied State: each node as its latest confirmed Deployment applied it, as one
/// document shaped like `like`.
pub(crate) fn applied_state(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    like: &SavedEnvironmentIntent,
) -> Result<SavedEnvironmentIntent, RpcError> {
    let rows = tx.query(
        "SELECT node, node_type FROM config_applied WHERE environment_id = ?1 ORDER BY node_id",
        &[environment.as_str().into()],
    )?;
    scope::nodes(&rows, like, "Applied State")
}

/// What reviews compare Working State against: Applied State, overlaid with the
/// target nodes of the Deployment in flight, if any. Its token changes whenever a
/// Deployment is admitted or ends.
pub(crate) fn head(tx: &mut dyn Tx, environment: &Environment) -> Result<Head, RpcError> {
    let id = &environment.summary.id;
    let applied = applied_state(tx, id, &environment.working)?;
    let in_flight = in_flight_sql();
    let ended = tx
        .query(
            &format!(
                "SELECT COUNT(*) FROM config_deployment \
                 WHERE environment_id = ?1 AND status <> 'superseded' AND NOT {in_flight}"
            ),
            &[id.as_str().into()],
        )?
        .first()
        .ok_or_else(|| error::corrupt("Deployment count"))?
        .int(0)?;
    let in_flight = tx.query(
        &format!(
            "SELECT number, saved_revision, nodes FROM config_deployment \
             WHERE environment_id = ?1 AND {in_flight} ORDER BY number DESC LIMIT 1"
        ),
        &[id.as_str().into()],
    )?;
    let Some(row) = in_flight.first() else {
        return Ok(Head {
            token: format!("0.{ended}"),
            intent: applied.clone(),
            applied,
        });
    };
    let saved = saved_at(tx, id, revision(row.int(1)?)?)?;
    let nodes: Vec<TargetNode> = row.json(2, "Deployment")?;
    let mut intent = applied.clone();
    for node in nodes {
        intent.services.retain(|service| service.id != node.id());
        intent
            .volumes
            .retain(|volume| volume.resource_id != node.id());
        match node {
            TargetNode::Service { .. } => intent.services.extend(
                saved
                    .services
                    .iter()
                    .filter(|service| service.id == node.id())
                    .cloned(),
            ),
            // A Volume the Deployment removes isn't in Head.
            TargetNode::Volume {
                deletes: Some(_), ..
            } => {}
            TargetNode::Volume { deletes: None, .. } => intent.volumes.extend(
                saved
                    .volumes
                    .iter()
                    .filter(|volume| volume.resource_id == node.id())
                    .cloned(),
            ),
        }
    }
    Ok(Head {
        token: format!("{}.{ended}", row.int(0)?),
        intent,
        applied,
    })
}

/// `environment`'s Deployment that may still run, if any: queued, or claimed by a
/// runner whose lease holds.
pub(crate) fn in_flight(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<DeploymentSummary>, RpcError> {
    latest_where(tx, environment, &in_flight_sql())
}

/// `environment`'s latest Deployment a runner claimed: what may run on the Servers.
pub(crate) fn last_ran(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
) -> Result<Option<DeploymentSummary>, RpcError> {
    latest_where(tx, environment, "runner IS NOT NULL")
}

fn latest_where(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    condition: &str,
) -> Result<Option<DeploymentSummary>, RpcError> {
    tx.query(
        &format!(
            "SELECT {COLUMNS} FROM config_deployment \
             WHERE environment_id = ?1 AND {condition} ORDER BY number DESC LIMIT 1"
        ),
        &[environment.as_str().into()],
    )?
    .first()
    .map(|row| stored(row).map(|stored| stored.summary))
    .transpose()
}

/// Every queued Deployment no runner claimed that was admitted before `before`
/// (Unix seconds), oldest first: those whose dispatch may have been lost.
pub(crate) fn unclaimed(tx: &mut dyn Tx, before: i64) -> Result<Vec<Unclaimed>, RpcError> {
    tx.query(
        "SELECT organization_id, environment_id, id, admitted FROM config_deployment \
         WHERE status = 'queued' AND runner IS NULL AND admitted < ?1 ORDER BY admitted, id",
        &[before.into()],
    )?
    .iter()
    .map(|row| {
        Ok(Unclaimed {
            organization: row.parse(0, "identity")?,
            environment: row.parse(1, "identity")?,
            deployment: row.parse(2, "identity")?,
            admitted_at: row.int(3)?,
        })
    })
    .collect()
}

/// One Deployment in `who`'s Organization, with its Node Outcomes.
pub(crate) fn view(
    tx: &mut dyn Tx,
    who: &Actor,
    id: &DeploymentId,
) -> Result<DeploymentView, RpcError> {
    let stored = owned(tx, who, id)?;
    let environment = scope::load_by_id(tx, &stored.environment)?;
    let nodes = stored
        .nodes
        .iter()
        .map(|node| NodeOutcome {
            node: node.shown(),
            outcome: match (stored.run.nodes.get(node.id()), stored.summary.status) {
                (Some(status), _) => *status,
                (None, DeploymentStatus::Unknown) => NodeStatus::Unknown,
                (
                    None,
                    DeploymentStatus::Queued
                    | DeploymentStatus::Running
                    | DeploymentStatus::Cancelling,
                ) => NodeStatus::Pending,
                (
                    None,
                    DeploymentStatus::Superseded
                    | DeploymentStatus::Applied
                    | DeploymentStatus::Failed
                    | DeploymentStatus::Cancelled,
                ) => NodeStatus::NotAttempted,
            },
        })
        .collect();
    let builds = build::views(tx, &stored)?;
    Ok(DeploymentView {
        builds,
        deployment: stored.summary,
        environment: environment.summary,
        namespace: stored.namespace,
        nodes,
        preview: stored.run.preview,
        outcome: stored.run.outcome,
    })
}

/// One Git build of a Deployment in `who`'s Organization, with its log.
pub(crate) fn build_log(
    tx: &mut dyn Tx,
    who: &Actor,
    query: &build::BuildLogQuery,
) -> Result<build::BuildLogView, RpcError> {
    let stored = owned(tx, who, &query.deployment)?;
    build::log(tx, &stored, &query.service)
}

/// Deployment `id` of `who`'s Organization.
fn owned(tx: &mut dyn Tx, who: &Actor, id: &DeploymentId) -> Result<Stored, RpcError> {
    let rows = tx.query(
        &format!("SELECT {COLUMNS} FROM config_deployment WHERE id = ?1 AND organization_id = ?2"),
        &[id.as_str().into(), who.organization.as_str().into()],
    )?;
    stored(rows.first().ok_or_else(|| missing(id))?)
}

/// One page of `environment`'s Deployments, newest first, before `cursor`.
pub(crate) fn page(
    tx: &mut dyn Tx,
    environment: &EnvironmentId,
    limit: usize,
    before: Option<u64>,
) -> Result<(Vec<DeploymentSummary>, Option<u64>), RpcError> {
    let before = i64::try_from(before.unwrap_or(u64::MAX >> 1)).unwrap_or(i64::MAX);
    let rows = tx.query(
        &format!(
            "SELECT {COLUMNS} FROM config_deployment WHERE environment_id = ?1 AND number < ?2 \
             ORDER BY number DESC LIMIT ?3"
        ),
        &[
            environment.as_str().into(),
            before.into(),
            i64::try_from(limit + 1).unwrap_or(i64::MAX).into(),
        ],
    )?;
    let mut page = rows
        .iter()
        .map(|row| stored(row).map(|stored| stored.summary))
        .collect::<Result<Vec<_>, _>>()?;
    let next = (page.len() > limit).then(|| {
        page.truncate(limit);
        page.last().map_or(0, |last| last.number)
    });
    Ok((page, next))
}

/// Load a Deployment with its Environment locked, so admissions, claims and records
/// of one Environment apply in turn.
pub(crate) fn locked(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Stored, RpcError> {
    let environment = load(tx, id)?.environment;
    scope::lock_all(tx, [environment])?;
    // A lapsed lease reads unknown; writing it down keeps the rows saying so too.
    tx.execute(
        "UPDATE config_deployment SET status = 'unknown' \
         WHERE id = ?1 AND status IN ('running', 'cancelling') AND lease <= ?2",
        &[id.as_str().into(), now().into()],
    )?;
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
    // A runner whose lease lapsed is gone: what ran is unknown.
    let lapsed = matches!(
        status,
        DeploymentStatus::Running | DeploymentStatus::Cancelling
    ) && lease <= now();
    let status = if lapsed {
        DeploymentStatus::Unknown
    } else {
        status
    };
    Ok(Stored {
        summary: DeploymentSummary {
            id: row.parse(0, "identity")?,
            number: row.number(2, "Deployment")?,
            status,
            saved: revision(row.int(4)?)?,
            services: row.json(5, "Deployment")?,
            runner: row.parse_optional(11, "identity")?,
            upload: row.json(9, "Deployment upload")?,
            remove: revision(row.int(4)?)? == NOTHING,
            admitted_by: row.parse_optional(13, "identity")?,
            admitted_at: row.int(14)?,
            started_at: row.optional_int(15)?,
            ended_at: row.optional_int(16)?,
        },
        environment: row.parse(1, "identity")?,
        nodes: row.json(6, "Deployment")?,
        namespace: row.parse::<Namespace>(7, "Namespace")?,
        run: row.json(8, "Deployment run")?,
        cluster_domain: row.parse_optional(10, "identity")?,
        lease,
    })
}

fn save(tx: &mut dyn Tx, stored: &Stored) -> Result<(), RpcError> {
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
            .map(|loaded| review::empty(&loaded.working.environment_slug));
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

fn revision(value: i64) -> Result<Revision, RpcError> {
    u64::try_from(value)
        .map(Revision)
        .map_err(|_| error::corrupt("revision"))
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
