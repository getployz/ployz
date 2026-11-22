//! Signed versions the Store keeps for its runners and drains, and what each Deployment
//! ran.
//!
//! A client posts each Service's signed version with the image it pinned the Service's
//! tag to. The Store keeps the statement under the Namespace, Service and digest its own
//! body names, and records the pin as what that Service of the Deployment runs. It checks
//! no signature: every Server does, against the keys it trusts.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::authority::{
    Births, BuiltImageDigest, PayloadDigest, ProposalRef, Purpose, RuleProof, ServiceVersion,
    Signed, SignerKey, Statement, Timestamp, deploy::content_addressed, version::MAX_PRESENTED,
};
use ployz_core::{
    MachineId, Namespace, OperationId, RequestedServiceSpec, RpcError, SealingPublicKey,
    ServiceName,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use super::{TargetNode, now, organization};
use crate::error;
use crate::id::DeploymentId;
use crate::storage::Tx;

/// A signed version a client posts with a Deploy, and the image it pinned for it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PostedVersion {
    pub statement: Signed<ServiceVersion>,
    /// None when the Saved State names the Service's image by digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pin: Option<TagPin>,
    /// The births the approval minted for Volumes this Service's spec mounts first, by the
    /// name the spec declares each under. The Store records them, so every later spec of
    /// those Volumes carries them.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    #[ts(optional, as = "Option<Births>")]
    pub births: Births,
}

/// A Saved image reference that names a tag, and the content-addressed image a client
/// resolved it to.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct TagPin {
    /// The whole Saved image reference, repository included, so another repository is
    /// another tag.
    pub tag: String,
    pub image: String,
}

/// What one Service of a Deployment runs.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct RanService {
    /// The digest of the Service's spec as its runner sends it.
    pub digest: PayloadDigest,
    /// The image its tag was pinned to. None when it runs its Saved image as written.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pin: Option<TagPin>,
}

/// What a runner sends for one Service it creates: the pin to write into its spec, the
/// digest that spec has, and every signed version kept for that digest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Pinned {
    pub ran: RanService,
    pub versions: Vec<Signed<ServiceVersion>>,
}

type Ran = BTreeMap<ServiceName, RanService>;

/// What a Deployment runs, as `config_deployment.ran` keeps it.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
struct Record {
    #[serde(flatten)]
    origin: Origin,
    services: Ran,
}

/// Where a Service a Deployment names no pin for takes one from when it is claimed.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
enum Origin {
    /// A Deploy of its own Saved revision: from the Deployment that last applied the
    /// Service, while only run fields changed since.
    #[default]
    Own,
    /// A Deployment admitted before the Store kept what each one ran. It runs its Saved
    /// image as written, never a pin another Deployment ran, so a Server refuses a
    /// signed Service whose image is a tag.
    Legacy,
    /// A retry or rollback, which runs what `source` ran and takes nothing from any
    /// later Deployment.
    Replay { source: DeploymentId },
}

/// Keep each of `posted` and record it as what its Service of Deployment `id` runs, over
/// what `id` already records for that Service, and record the births it minted. A
/// statement already kept stays as it is.
///
/// # Errors
/// Returns `invalid_argument` for a statement whose body doesn't parse, names another
/// Namespace or a Service `id` doesn't deploy, or names a Service an earlier statement
/// already named, or a pin that isn't content-addressed; what recording a birth refuses;
/// or a storage error.
pub(crate) fn post(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    posted: &[PostedVersion],
) -> Result<(), RpcError> {
    if posted.is_empty() {
        return Ok(());
    }
    let stored = super::load(tx, id)?;
    let services: BTreeSet<&ServiceName> = stored
        .nodes
        .iter()
        .filter_map(|node| match node {
            TargetNode::Service { runtime, .. } => Some(runtime),
            TargetNode::Retire { .. } | TargetNode::Volume { .. } | TargetNode::Config { .. } => {
                None
            }
        })
        .collect();
    let organization = organization(tx, id)?;
    let mut record = record_of(tx, id)?;
    let mut named = BTreeSet::new();
    let mut births = Births::new();
    for PostedVersion {
        statement,
        pin,
        births: minted,
    } in posted
    {
        let body = statement.open().map_err(|refusal| {
            error::invalid(
                format!("A signed version doesn't read: {refusal}"),
                json!({}),
            )
        })?;
        if body.namespace != *stored.namespace() || !services.contains(&body.service) {
            return Err(error::invalid(
                format!(
                    "A signed version is for {} in {}, which this deploy doesn't deploy",
                    body.service, body.namespace
                ),
                json!({ "service": body.service, "namespace": body.namespace }),
            ));
        }
        if !named.insert(body.service.clone()) {
            return Err(error::invalid(
                format!(
                    "This deploy carries more than one signed version of {}",
                    body.service
                ),
                json!({ "service": body.service }),
            ));
        }
        if let Some(pin) = pin
            && !content_addressed(&pin.image)
        {
            return Err(error::invalid(
                format!(
                    "{}'s pinned image doesn't name its digest: pin it as name@sha256:…",
                    body.service
                ),
                json!({ "service": body.service, "image": pin.image }),
            ));
        }
        let text = json_text(statement)?;
        let hash = hex::encode(ring::digest::digest(&ring::digest::SHA256, text.as_bytes()));
        tx.execute(
            "INSERT INTO config_service_version \
             (organization_id, namespace, service, digest, statement_hash, statement, created_at) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7) \
             ON CONFLICT (organization_id, namespace, service, digest, statement_hash) DO NOTHING",
            &[
                organization.as_str().into(),
                body.namespace.as_str().into(),
                body.service.as_str().into(),
                body.digest.to_string().as_str().into(),
                hash.as_str().into(),
                text.as_str().into(),
                now().into(),
            ],
        )?;
        record.services.insert(
            body.service,
            RanService {
                digest: body.digest,
                pin: pin.clone(),
            },
        );
        births.extend(
            minted
                .iter()
                .map(|(name, birth)| (name.clone(), birth.clone())),
        );
    }
    if !births.is_empty() {
        let saved = super::held_revision(tx, &stored)?;
        crate::volume::birth::record(
            tx,
            (&stored.summary.environment_id, stored.namespace()),
            &saved,
            &births,
        )?;
    }
    write_record(tx, id, &record)
}

/// What a retry or rollback of `source` records before it is claimed, as JSON for its
/// `ran` column: only its source. Its claim reads what `source` ran, after what the
/// retry itself posted.
///
/// # Errors
/// Returns `corrupt` when the record doesn't serialize.
pub(super) fn replaying(source: &DeploymentId) -> Result<String, RpcError> {
    json_text(&Record {
        origin: Origin::Replay {
            source: source.clone(),
        },
        services: Ran::new(),
    })
}

/// Every signed version kept for `service` in `namespace` at `digest`, newest first, at
/// most as many as one request carries.
///
/// # Errors
/// Returns a storage error, or `corrupt` for a row that doesn't read.
pub(crate) fn versions_for(
    tx: &mut dyn Tx,
    organization: &str,
    (namespace, service): (&Namespace, &ServiceName),
    digest: PayloadDigest,
) -> Result<Vec<Signed<ServiceVersion>>, RpcError> {
    tx.query(
        "SELECT statement FROM config_service_version \
         WHERE organization_id = ?1 AND namespace = ?2 AND service = ?3 AND digest = ?4 \
         ORDER BY created_at DESC, statement_hash LIMIT ?5",
        &[
            organization.into(),
            namespace.as_str().into(),
            service.as_str().into(),
            digest.to_string().as_str().into(),
            i64::try_from(MAX_PRESENTED).unwrap_or(i64::MAX).into(),
        ],
    )?
    .iter()
    .map(|row| row.json(0, "Signed version"))
    .collect()
}

/// The signed versions kept for one spec of a Service, by the digest a Server checks.
/// Each is a signature Cloud can't make, so a drain that reads them from Cloud gets
/// nothing a Server would take that a trusted key didn't sign.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct VersionsQuery {
    pub namespace: Namespace,
    pub service: ServiceName,
    pub digest: PayloadDigest,
    /// The image the spec runs, when it names its content: a rule's build of it may
    /// stand in for a signed version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional, type = "string")]
    pub image: Option<BuiltImageDigest>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct VersionsView {
    /// Newest first, and at most as many as one request carries.
    pub versions: Vec<Signed<ServiceVersion>>,
    /// The newest verified build kept for `image` under the Environment's rule, which a
    /// Server takes in place of a signed version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rule: Option<Box<RuleProof>>,
}

/// What `organization` keeps for the spec `query` names: its signed versions and, for an
/// image a rule built, the newest verified build under the Environment's rule now.
///
/// # Errors
/// Returns a storage error, or `corrupt` for a row that doesn't read.
pub(crate) fn versions(
    tx: &mut dyn Tx,
    organization: &str,
    query: &VersionsQuery,
) -> Result<VersionsView, RpcError> {
    let service = (&query.namespace, &query.service);
    let versions = versions_for(tx, organization, service, query.digest)?;
    let rule = match query.image {
        Some(image) => crate::build::verified_proof(tx, organization, service, image)?,
        None => None,
    };
    Ok(VersionsView {
        versions,
        rule: rule.map(Box::new),
    })
}

/// What each Service of `target` runs in Deployment `id`, written back so a retry of `id`
/// runs the same, with the versions kept for each digest. A Service runs, in order: what
/// `id` already records, which is the version posted with it or what an earlier claim
/// resolved; for a Deploy, what the Deployment that last applied it ran, when the spec
/// differs from that one only in run fields; for a retry or rollback, what its source
/// ran; else its Saved image as written, which is all a Deployment from before records
/// ever gets.
///
/// # Errors
/// Returns a storage error, or `corrupt` for a record that doesn't read.
pub(super) fn pinned(
    tx: &mut dyn Tx,
    (id, environment, namespace): (&DeploymentId, &str, &Namespace),
    (nodes, target): (&[TargetNode], &[RequestedServiceSpec]),
) -> Result<BTreeMap<ServiceName, Pinned>, RpcError> {
    let recorded = record_of(tx, id)?;
    let mut record = recorded.clone();
    let replays = match &record.origin {
        Origin::Replay { source } => {
            let pending: Vec<&RequestedServiceSpec> = target
                .iter()
                .filter(|spec| !record.services.contains_key(&spec.name))
                .collect();
            replayed(tx, source.clone(), pending)?
        }
        Origin::Own | Origin::Legacy => BTreeMap::new(),
    };
    for spec in target {
        if record.services.contains_key(&spec.name) {
            continue;
        }
        let from = match &record.origin {
            Origin::Own => reused(tx, environment, (nodes, spec), namespace)?,
            Origin::Legacy => None,
            Origin::Replay { .. } => replays.get(&spec.name).cloned(),
        };
        let ran = from.or_else(|| {
            PayloadDigest::of(spec, namespace)
                .ok()
                .map(|digest| RanService { digest, pin: None })
        });
        if let Some(ran) = ran {
            record.services.insert(spec.name.clone(), ran);
        }
    }
    if record != recorded {
        write_record(tx, id, &record)?;
    }
    let organization = organization(tx, id)?;
    record
        .services
        .into_iter()
        .filter(|(service, _)| target.iter().any(|spec| spec.name == *service))
        .map(|(service, ran)| {
            let versions =
                versions_for(tx, organization.as_str(), (namespace, &service), ran.digest)?;
            Ok((service, Pinned { ran, versions }))
        })
        .collect()
}

/// A Device's deploy approval as `config_deployment.deploy_approval` kept it before signed
/// versions, decoded as the claim then decoded it: a canonical body with every field, and
/// Machines and Services sorted and distinct.
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct KeptApproval {
    cluster: SealingPublicKey,
    operation: OperationId,
    namespace: Namespace,
    machines: Vec<MachineId>,
    services: Vec<ApprovedService>,
    issued_at: Timestamp,
    expires_at: Timestamp,
    signer: SignerKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    proposal: Option<ProposalRef>,
}

impl Statement for KeptApproval {
    const PURPOSE: Purpose = Purpose::Workflow;
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ApprovedService {
    name: ServiceName,
    digest: PayloadDigest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    image: Option<String>,
}

impl KeptApproval {
    fn read(text: &str) -> Option<Self> {
        let approval = serde_json::from_str::<Signed<Self>>(text)
            .ok()?
            .open()
            .ok()?;
        let sorted = approval.machines.is_sorted_by(|left, right| left < right)
            && approval
                .services
                .is_sorted_by(|left, right| left.name < right.name);
        sorted.then_some(approval)
    }
}

/// Record in `ran`, for each Deployment a Device approved before the Store kept what
/// each one ran, the digest and pin its approval named for each Service it ships, as a
/// claim read them from the approval: a retry or rollback of it then runs that pin, not
/// whatever its tag names now. A Service `ran` already names keeps what it names, so a
/// second run changes nothing. A Deployment whose approval doesn't read or whose Saved
/// revision no longer lowers keeps none, and runs its tag as before.
///
/// # Errors
/// Returns a storage error.
pub(crate) fn record_approved(tx: &mut dyn Tx) -> Result<(), RpcError> {
    let rows = tx.query(
        "SELECT id, deploy_approval FROM config_deployment WHERE deploy_approval IS NOT NULL",
        &[],
    )?;
    for row in &rows {
        let id = row.parse::<DeploymentId>(0, "Deployment")?;
        let Some(approved) = KeptApproval::read(row.text(1)?) else {
            continue;
        };
        let Some(target) = super::load(tx, &id).ok().and_then(|stored| {
            let saved = super::held_revision(tx, &stored).ok()?;
            super::lowered(tx, &stored, &saved).ok()
        }) else {
            continue;
        };
        let recorded = record_of(tx, &id)?;
        let mut record = recorded.clone();
        for spec in &target.intent.target {
            let Some(approved) = approved
                .services
                .iter()
                .find(|service| service.name == spec.name)
            else {
                continue;
            };
            record
                .services
                .entry(spec.name.clone())
                .or_insert_with(|| RanService {
                    digest: approved.digest,
                    pin: approved.image.clone().map(|image| TagPin {
                        tag: spec.container.image.clone(),
                        image,
                    }),
                });
        }
        if record != recorded {
            write_record(tx, &id, &record)?;
        }
    }
    Ok(())
}

/// What the Deployment that last applied `spec`'s Service node ran, when `spec` with
/// that pin has the digest it ran: the same tag, and only run fields changed.
fn reused(
    tx: &mut dyn Tx,
    environment: &str,
    (nodes, spec): (&[TargetNode], &RequestedServiceSpec),
    namespace: &Namespace,
) -> Result<Option<RanService>, RpcError> {
    let Some(node) = nodes.iter().find_map(|node| match node {
        TargetNode::Service { id, runtime, .. } if *runtime == spec.name => Some(id),
        TargetNode::Service { .. }
        | TargetNode::Retire { .. }
        | TargetNode::Volume { .. }
        | TargetNode::Config { .. } => None,
    }) else {
        return Ok(None);
    };
    let Some(last) = last_applied(tx, environment, node.as_str(), &spec.name)? else {
        return Ok(None);
    };
    let mut pinned = spec.clone();
    if let Some(pin) = &last.pin {
        if pin.tag != spec.container.image {
            return Ok(None);
        }
        pinned.container.image.clone_from(&pin.image);
    }
    let same = PayloadDigest::of(&pinned, namespace).ok() == Some(last.digest);
    Ok(same.then_some(last))
}

/// What `source` ran for each of `pending`'s Services: its record, else what the
/// Deployment it replays ran, read in one walk back through the chain. A Service none of
/// them names is left out, so it runs its Saved image as written.
///
/// # Errors
/// Returns a storage error, or `corrupt` for a chain that reaches a Deployment twice.
fn replayed(
    tx: &mut dyn Tx,
    mut source: DeploymentId,
    mut pending: Vec<&RequestedServiceSpec>,
) -> Result<BTreeMap<ServiceName, RanService>, RpcError> {
    let mut found = BTreeMap::new();
    let mut walked = BTreeSet::new();
    while !pending.is_empty() {
        if !walked.insert(source.as_str().to_owned()) {
            return Err(error::corrupt("Deployment ran"));
        }
        let record = record_of(tx, &source)?;
        pending.retain(|spec| {
            let ran = record.services.get(&spec.name).cloned();
            ran.map(|ran| found.insert(spec.name.clone(), ran))
                .is_none()
        });
        match record.origin {
            Origin::Replay { source: earlier } => source = earlier,
            Origin::Own | Origin::Legacy => break,
        }
    }
    Ok(found)
}

/// What `service` ran in the Deployment that last applied Service node `node`.
fn last_applied(
    tx: &mut dyn Tx,
    environment: &str,
    node: &str,
    service: &ServiceName,
) -> Result<Option<RanService>, RpcError> {
    let rows = tx.query(
        "SELECT d.ran FROM config_applied a JOIN config_deployment d ON d.id = a.deployment_id \
         WHERE a.environment_id = ?1 AND a.node_id = ?2",
        &[environment.into(), node.into()],
    )?;
    let Some(text) = rows
        .first()
        .map(|row| row.optional_text(0))
        .transpose()?
        .flatten()
    else {
        return Ok(None);
    };
    Ok(parse(text)?.services.remove(service))
}

fn record_of(tx: &mut dyn Tx, id: &DeploymentId) -> Result<Record, RpcError> {
    let rows = tx.query(
        "SELECT ran FROM config_deployment WHERE id = ?1",
        &[id.as_str().into()],
    )?;
    rows.first()
        .map(|row| row.optional_text(0))
        .transpose()?
        .flatten()
        .map_or(Ok(Record::default()), parse)
}

fn parse(text: &str) -> Result<Record, RpcError> {
    serde_json::from_str(text).map_err(|_| error::corrupt("Deployment ran"))
}

fn write_record(tx: &mut dyn Tx, id: &DeploymentId, record: &Record) -> Result<(), RpcError> {
    tx.execute(
        "UPDATE config_deployment SET ran = ?2 WHERE id = ?1",
        &[id.as_str().into(), json_text(record)?.as_str().into()],
    )
    .map(drop)
}

fn json_text(value: &impl Serialize) -> Result<String, RpcError> {
    serde_json::to_string(value).map_err(|_| error::corrupt("Signed version"))
}
