//! Signed versions the Store keeps for its runners and drains, and what each Deployment
//! ran.
//!
//! A client posts each Service's signed version with the image it pinned the Service's
//! tag to. The Store keeps the statement under the Namespace, Service and digest its own
//! body names, and records the pin as what that Service of the Deployment runs. It checks
//! no signature: every Server does, against the keys it trusts.

use std::collections::{BTreeMap, BTreeSet};

use ployz_core::authority::{
    AutoDeployRule, Births, BuiltImageDigest, CopyStep, KeptRule, PayloadDigest, ProposalRef,
    Purpose, RuleProof, ServiceVersion, Signed, SignerKey, Statement, Timestamp,
    deploy::content_addressed, version::MAX_PRESENTED,
};
use ployz_core::{
    MachineId, Namespace, OperationId, RequestedServiceSpec, RpcError, SealingPublicKey,
    ServiceName,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use ts_rs::TS;

use super::{TargetNode, organization};
use crate::error;
use crate::id::DeploymentId;
use crate::scope::EnvironmentRef;
use crate::storage::Tx;
use crate::storage::now;

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
    /// Services whose running Containers its runner replaces even when their spec is
    /// unchanged.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    recreate: BTreeSet<ServiceName>,
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

/// Record the birth each of `copies` mints for the Volume it restores into, then keep
/// each of `posted` and record it as what its Service of Deployment `id` runs, over what
/// `id` already records for that Service, and record the births it minted. The Copies
/// come first, so a version that mounts a restored Volume records the Copy's birth again.
/// A statement already kept stays as it is.
///
/// # Errors
/// Returns `invalid_argument` for a statement whose body doesn't parse, names another
/// Namespace or a Service `id` doesn't deploy, or names a Service an earlier statement
/// already named, or a pin that isn't content-addressed; what recording a birth refuses;
/// or a storage error.
pub(crate) fn post(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    (posted, copies): (&[PostedVersion], &[Signed<CopyStep>]),
) -> Result<(), RpcError> {
    if posted.is_empty() && copies.is_empty() {
        return Ok(());
    }
    let stored = super::load(tx, id)?;
    if !copies.is_empty() {
        let copies = copies
            .iter()
            .map(|copy| {
                ployz_core::authority::copy::open_listed(copy).map_err(|refusal| {
                    error::invalid(
                        format!(
                            "A copy isn't a step of the approval it rides in: {refusal}. \
                             Approve the restore again."
                        ),
                        json!({}),
                    )
                })
            })
            .collect::<Result<Vec<_>, _>>()?;
        let saved = super::held_revision(tx, &stored)?;
        crate::volume::birth::record_copies(
            tx,
            (&stored.summary.environment_id, stored.namespace()),
            &saved,
            &copies,
        )?;
    }
    if posted.is_empty() {
        return Ok(());
    }
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
        let parent = body.parent.map(|parent| parent.to_string());
        tx.execute(
            "INSERT INTO config_service_version \
             (organization_id, namespace, service, digest, statement_hash, statement, created_at, \
              parent) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8) \
             ON CONFLICT (organization_id, namespace, service, digest, statement_hash) DO NOTHING",
            &[
                organization.as_str().into(),
                body.namespace.as_str().into(),
                body.service.as_str().into(),
                body.digest.to_string().as_str().into(),
                hash.as_str().into(),
                text.as_str().into(),
                now().into(),
                parent.as_deref().into(),
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

/// What a retry or rollback of `source` that ships the runtime Services `ships` records
/// before it is claimed, as JSON for its `ran` column: its source, and which of `ships`
/// the source recreates. Its claim reads what `source` ran, after what the retry itself
/// posted. A retry of a retry copies its source's copy.
///
/// # Errors
/// Returns `corrupt` when the source's record doesn't read or this one doesn't
/// serialize, or a storage error.
pub(super) fn replaying(
    tx: &mut dyn Tx,
    source: &DeploymentId,
    ships: &BTreeSet<&ServiceName>,
) -> Result<String, RpcError> {
    let recreate = record_of(tx, source)?
        .recreate
        .into_iter()
        .filter(|service| ships.contains(service))
        .collect();
    json_text(&Record {
        origin: Origin::Replay {
            source: source.clone(),
        },
        services: Ran::new(),
        recreate,
    })
}

/// Have Deployment `id`'s runner replace each running Container of `services`, even one
/// whose spec is unchanged.
///
/// # Errors
/// Returns a storage error, or `corrupt` for a record that doesn't read.
pub(crate) fn recreate(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    services: &[ServiceName],
) -> Result<(), RpcError> {
    if services.is_empty() {
        return Ok(());
    }
    let mut record = record_of(tx, id)?;
    record.recreate.extend(services.iter().cloned());
    write_record(tx, id, &record)
}

/// The Services whose running Containers Deployment `id`'s runner replaces even when
/// their spec is unchanged.
///
/// # Errors
/// Returns a storage error, or `corrupt` for a record that doesn't read.
pub(crate) fn recreates(
    tx: &mut dyn Tx,
    id: &DeploymentId,
) -> Result<BTreeSet<ServiceName>, RpcError> {
    Ok(record_of(tx, id)?.recreate)
}

/// Every signed version kept for `service` in `namespace` at `digest`, newest first, each
/// followed by the parentless versions kept for the digest it names as its parent, which
/// a Server checks it against, at most as many as one request carries.
///
/// # Errors
/// Returns a storage error, or `corrupt` for a row that doesn't read.
pub(crate) fn versions_for(
    tx: &mut dyn Tx,
    organization: &str,
    service: (&Namespace, &ServiceName),
    digest: PayloadDigest,
) -> Result<Vec<Signed<ServiceVersion>>, RpcError> {
    let mut presented = Vec::new();
    let mut parents = Vec::new();
    for statement in kept(tx, organization, service, digest)? {
        let parent = statement.open().ok().and_then(|body| body.parent);
        push_new(&mut presented, statement);
        if let Some(parent) = parent
            && !parents.contains(&parent)
        {
            parents.push(parent);
            for root in roots(tx, organization, service, parent)? {
                push_new(&mut presented, root);
            }
        }
    }
    presented.truncate(MAX_PRESENTED);
    Ok(presented)
}

fn push_new(presented: &mut Vec<Signed<ServiceVersion>>, statement: Signed<ServiceVersion>) {
    if !presented.contains(&statement) {
        presented.push(statement);
    }
}

/// Every signed version kept for `service` in `namespace` at `digest`, newest first, at
/// most as many as one request carries.
fn kept(
    tx: &mut dyn Tx,
    organization: &str,
    service: (&Namespace, &ServiceName),
    digest: PayloadDigest,
) -> Result<Vec<Signed<ServiceVersion>>, RpcError> {
    newest(tx, organization, service, digest, "")
}

/// The versions kept for `service` at `digest` that name no parent, newest first, at most
/// as many as one request carries: those a Server can take as the parent of an edit.
/// Edits of the same digest never crowd them out.
fn roots(
    tx: &mut dyn Tx,
    organization: &str,
    service: (&Namespace, &ServiceName),
    digest: PayloadDigest,
) -> Result<Vec<Signed<ServiceVersion>>, RpcError> {
    Ok(
        newest(tx, organization, service, digest, "AND parent IS NULL ")?
            .into_iter()
            .filter(|statement| statement.open().is_ok_and(|body| body.parent.is_none()))
            .collect(),
    )
}

fn newest(
    tx: &mut dyn Tx,
    organization: &str,
    (namespace, service): (&Namespace, &ServiceName),
    digest: PayloadDigest,
    only: &str,
) -> Result<Vec<Signed<ServiceVersion>>, RpcError> {
    tx.query(
        &format!(
            "SELECT statement FROM config_service_version \
             WHERE organization_id = ?1 AND namespace = ?2 AND service = ?3 AND digest = ?4 \
             {only}ORDER BY created_at DESC, statement_hash LIMIT ?5"
        ),
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

/// The last version a person approved of each of `services`, for a key that signs
/// without a person to edit.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ParentQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// By the name its spec runs under.
    pub services: Vec<ServiceName>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct ParentView {
    pub namespace: Namespace,
    pub services: BTreeMap<ServiceName, Parent>,
}

/// What a Service's newest parentless signed version is.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "parent", rename_all = "snake_case")]
pub enum Parent {
    /// The newest a Deployment ran, with the spec it ran under it, its tag pinned. The
    /// Store checks neither signature nor digest: the signer recomputes the digest.
    Signed {
        statement: Signed<ServiceVersion>,
        spec: Box<RequestedServiceSpec>,
        /// The tag its Saved image names and what it was pinned to. None when the Saved
        /// State names the image by digest.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        pin: Option<TagPin>,
    },
    /// No Deployment ran one. `deployed` when the Service runs anyway: from a build, an
    /// automatic deploy rule, or a Deploy from before signed versions.
    Missing { deployed: bool },
}

/// What `organization` keeps as the parent of each Service `query` names: the newest
/// Deployment of its Environment that applied that Service itself, not only another beside
/// it, and ran a version of it that names no parent, that version, and the spec that
/// Deployment ran it with. A Deployment whose spec no longer
/// lowers to the digest it ran is passed over.
///
/// # Errors
/// Returns a storage error, `corrupt` for a record that doesn't read, and what reading
/// the Environment refuses, write access included: the spec carries its Sealed values.
pub(crate) fn parents(
    tx: &mut dyn Tx,
    who: &crate::Actor,
    trusted: &crate::Trusted,
    query: &ParentQuery,
) -> Result<ParentView, RpcError> {
    let environment = crate::scope::environment(tx, who, &query.environment)?;
    trusted.writes(&environment.summary.name)?;
    let namespace = super::namespace(tx, who, &environment.summary)?;
    let organization = who.organization.as_str();
    let records = ran_records(tx, &environment.summary)?;
    let applied = super::head(tx, &environment)?.applied;
    let mut services = BTreeMap::new();
    for service in &query.services {
        let parent = match signed_parent(tx, organization, (&namespace, service), &records)? {
            Some(parent) => parent,
            None => Parent::Missing {
                deployed: applied
                    .services
                    .iter()
                    .any(|applied| applied.config.private_dns == *service),
            },
        };
        services.insert(service.clone(), parent);
    }
    Ok(ParentView {
        namespace,
        services,
    })
}

/// What each Deployment of `environment` that put anything into Applied State ran,
/// newest first.
fn ran_records(
    tx: &mut dyn Tx,
    environment: &crate::EnvironmentSummary,
) -> Result<Vec<(DeploymentId, Record)>, RpcError> {
    tx.query(
        "SELECT id, ran FROM config_deployment \
         WHERE environment_id = ?1 AND ran IS NOT NULL AND (status = 'applied' OR id IN \
         (SELECT deployment_id FROM config_applied WHERE environment_id = ?1)) \
         ORDER BY number DESC",
        &[environment.id.as_str().into()],
    )?
    .iter()
    .map(|row| Ok((row.parse(0, "Deployment")?, parse(row.text(1)?)?)))
    .collect()
}

/// What each Service a key rotation carries runs now, so it can sign it again under the
/// new key.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RunningQuery {
    #[serde(default)]
    pub environment: EnvironmentRef,
    /// By the name its spec runs under.
    pub services: Vec<ServiceName>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct RunningView {
    pub namespace: Namespace,
    /// Each named Service a Deployment applied. One none applied is absent.
    pub services: BTreeMap<ServiceName, Running>,
}

/// What the newest Deployment that applied a Service ran it with, and every signed
/// version kept for that spec. The Store checks no signature or digest: the reader does.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct Running {
    pub spec: Box<RequestedServiceSpec>,
    /// The tag its Saved image names and what it was pinned to. None when the Saved
    /// State names the image by digest.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub pin: Option<TagPin>,
    /// Newest first, at most as many as one request carries.
    pub versions: Vec<Signed<ServiceVersion>>,
}

/// What `organization` keeps of what each Service `query` names runs now: the newest
/// Deployment of its Environment that applied it, the spec it ran, and the signed
/// versions kept for that spec.
///
/// # Errors
/// Returns a storage error, `corrupt` for a record that doesn't read, and what reading
/// the Environment refuses, write access included: the spec carries its Sealed values.
pub(crate) fn running(
    tx: &mut dyn Tx,
    who: &crate::Actor,
    trusted: &crate::Trusted,
    query: &RunningQuery,
) -> Result<RunningView, RpcError> {
    let environment = crate::scope::environment(tx, who, &query.environment)?;
    trusted.writes(&environment.summary.name)?;
    let namespace = super::namespace(tx, who, &environment.summary)?;
    let organization = who.organization.as_str();
    let records = ran_records(tx, &environment.summary)?;
    let mut services = BTreeMap::new();
    for service in &query.services {
        let found = records.iter().find_map(|(id, record)| {
            let ran = record.services.get(service)?;
            Some((ran, ran_spec(tx, id, service, ran)?))
        });
        if let Some((ran, spec)) = found {
            let versions = kept(tx, organization, (&namespace, service), ran.digest)?;
            services.insert(
                service.clone(),
                Running {
                    spec: Box::new(spec),
                    pin: ran.pin.clone(),
                    versions,
                },
            );
        }
    }
    Ok(RunningView {
        namespace,
        services,
    })
}

/// The newest of `records` that ran a parentless version of `service`, as [`parents`]
/// reads it.
fn signed_parent(
    tx: &mut dyn Tx,
    organization: &str,
    service: (&Namespace, &ServiceName),
    records: &[(DeploymentId, Record)],
) -> Result<Option<Parent>, RpcError> {
    let mut tried = Vec::new();
    for (id, record) in records {
        let Some(ran) = record.services.get(service.1) else {
            continue;
        };
        if tried.contains(&ran.digest) {
            continue;
        }
        let Some(statement) = roots(tx, organization, service, ran.digest)?
            .into_iter()
            .next()
        else {
            tried.push(ran.digest);
            continue;
        };
        if let Some(spec) = ran_spec(tx, id, service.1, ran) {
            return Ok(Some(Parent::Signed {
                statement,
                spec: Box::new(spec),
                pin: ran.pin.clone(),
            }));
        }
    }
    Ok(None)
}

/// The spec Deployment `id` ran `service` with, its tag pinned as `ran` says, when it
/// applied `service` and the spec still lowers to the digest `ran` names.
fn ran_spec(
    tx: &mut dyn Tx,
    id: &DeploymentId,
    service: &ServiceName,
    ran: &RanService,
) -> Option<RequestedServiceSpec> {
    let stored = super::load(tx, id).ok()?;
    if !applied(&stored, service) {
        return None;
    }
    let saved = super::held_revision(tx, &stored).ok()?;
    let lowered = super::lowered(tx, &stored, &saved).ok()?;
    let mut spec = lowered
        .intent
        .target
        .into_iter()
        .find(|spec| spec.name == *service)?;
    if let Some(pin) = &ran.pin {
        if pin.tag != spec.container.image {
            return None;
        }
        spec.container.image.clone_from(&pin.image);
    }
    (PayloadDigest::of(&spec, stored.namespace()).ok() == Some(ran.digest)).then_some(spec)
}

/// Whether `stored` put the Service that runs as `service` into Applied State. A
/// Deployment that applied a Service beside it, then failed or was cancelled in it, never
/// ran its version.
fn applied(stored: &super::Stored, service: &ServiceName) -> bool {
    let succeeded = stored.summary.status == super::DeploymentStatus::Applied;
    stored.nodes.iter().any(|node| {
        matches!(node, super::TargetNode::Service { runtime, .. } if runtime == service)
            && stored
                .run
                .nodes
                .get(node.id())
                .is_some_and(|status| status.applied(succeeded))
    })
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
#[ts(as = "VersionsFields")]
pub struct VersionsView {
    /// Newest first, and at most as many as one request carries.
    pub versions: Vec<Signed<ServiceVersion>>,
    /// The newest verified build kept for `image` under the Environment's rule, which a
    /// Server takes in place of a signed version, with the login of what's applied when
    /// it is a registry rule: its Server pulls the image itself.
    #[serde(default, flatten)]
    pub rule: Option<ployz_core::authority::KeptRule>,
    /// The Free receipt a free Build's Server gave for `image`, which an unsigned
    /// creation of it carries.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub free: Option<ployz_core::FreeReceipt>,
}

/// [`VersionsView`] as TypeScript reads it: ts-rs flattens no `Option`, so the kept
/// rule's two fields stand beside the versions, absent when no rule is kept.
#[derive(TS)]
#[ts(rename = "VersionsView")]
pub struct VersionsFields {
    pub versions: Vec<Signed<ServiceVersion>>,
    #[ts(optional)]
    pub rule: Option<Box<ployz_core::authority::RuleProof>>,
    #[ts(optional)]
    pub registry_login: Option<ployz_core::SealedLogin>,
    /// The Free receipt a free Build's Server gave for `image`, which an unsigned
    /// creation of it carries.
    #[ts(optional)]
    pub free: Option<ployz_core::FreeReceipt>,
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
        Some(image) => match crate::build::verified_proof(tx, organization, service, image)? {
            Some(proof) => Some(KeptRule::built(proof)),
            None => match registry_proof(tx, organization, service)? {
                Some(proof) => Some(KeptRule {
                    proof: Box::new(proof),
                    login: applied_login(tx, organization, service)?,
                }),
                None => None,
            },
        },
        None => None,
    };
    let free = match query.image {
        Some(image) => free_receipt(tx, organization, service, image)?,
        None => None,
    };
    Ok(VersionsView {
        versions,
        rule,
        free,
    })
}

/// The sealed login of what's applied for `service` in `namespace`: the one the Deployment
/// that confirmed its Applied State row froze for it, none when that one froze none. A
/// Deployment that ran without advancing that row, queued, cancelled or taken over, never
/// counts. A Move and a drain read it here; a Deploy pulls with the login its own
/// Deployment froze, which becomes this one when it confirms the row.
fn applied_login(
    tx: &mut dyn Tx,
    organization: &str,
    (namespace, service): (&Namespace, &ServiceName),
) -> Result<Option<ployz_core::SealedLogin>, RpcError> {
    let rows = tx.query(
        "SELECT a.node_id, d.nodes, d.logins FROM config_applied a \
         JOIN config_deployment d ON d.id = a.deployment_id \
         WHERE a.organization_id = ?1 AND d.namespace = ?2 AND a.node_type = 'service'",
        &[organization.into(), namespace.as_str().into()],
    )?;
    for row in &rows {
        let node = row.text(0)?;
        let nodes: Vec<super::TargetNode> = row.json(1, "Deployment nodes")?;
        let confirms = nodes.iter().any(|target| {
            matches!(target, super::TargetNode::Service { id, runtime, .. }
                if id.as_str() == node && runtime == service)
        });
        if confirms {
            let mut logins: BTreeMap<ServiceName, ployz_core::SealedLogin> =
                row.json(2, "Deployment logins")?;
            return Ok(logins.remove(service));
        }
    }
    Ok(None)
}

/// The proof a copy of `service` in `namespace` takes under its Environment's registry
/// rule while that rule is on and names it: the signed rule alone. A drain sends it with a
/// copy the rule made, and the Server pulls the copy's digest from the registry itself.
fn registry_proof(
    tx: &mut dyn Tx,
    organization: &str,
    (namespace, service): (&Namespace, &ServiceName),
) -> Result<Option<RuleProof>, RpcError> {
    let rows = tx.query(
        "SELECT DISTINCT r.rule FROM config_auto_deploy_rule r \
         JOIN config_deployment d ON d.environment_id = r.environment_id \
         WHERE d.organization_id = ?1 AND d.namespace = ?2",
        &[organization.into(), namespace.as_str().into()],
    )?;
    for row in &rows {
        let signed: Signed<AutoDeployRule> = row.json(0, "Automatic deploy rule")?;
        let covers = signed.open().is_ok_and(|rule| {
            !rule.ended && rule.source.registry().is_some() && rule.service(service).is_some()
        });
        if covers {
            return Ok(Some(RuleProof {
                rule: signed,
                evidence: ployz_core::authority::RuleEvidence::Registry,
                claim: None,
                reader: None,
            }));
        }
    }
    Ok(None)
}

/// Keep the Free receipt each of `receipts` carries under `namespace` and its Service, so
/// a later copy of a Container running its image carries it too.
///
/// # Errors
/// Returns `invalid_argument` for a Free receipt that doesn't read, or a storage error.
pub(crate) fn keep_free_receipts(
    tx: &mut dyn Tx,
    (organization, namespace): (&str, &Namespace),
    receipts: &BTreeMap<ServiceName, serde_json::Value>,
) -> Result<(), RpcError> {
    for (service, receipt) in receipts {
        let Some(free) = receipt.get("free") else {
            continue;
        };
        let free: ployz_core::FreeReceipt = serde_json::from_value(free.clone())
            .map_err(|_| error::invalid("A build's Free receipt doesn't read", json!({})))?;
        tx.execute(
            "INSERT INTO config_free_receipt (organization_id, namespace, service, image, receipt) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT (organization_id, namespace, service, image) \
             DO UPDATE SET receipt = excluded.receipt",
            &[
                organization.into(),
                namespace.as_str().into(),
                service.as_str().into(),
                free.image.as_str().into(),
                json_text(&free)?.as_str().into(),
            ],
        )?;
    }
    Ok(())
}

/// The Free receipt kept for `image` of `service` in `namespace`.
fn free_receipt(
    tx: &mut dyn Tx,
    organization: &str,
    (namespace, service): (&Namespace, &ServiceName),
    image: BuiltImageDigest,
) -> Result<Option<ployz_core::FreeReceipt>, RpcError> {
    let image = image.to_string();
    let rows = tx.query(
        "SELECT receipt FROM config_free_receipt \
         WHERE organization_id = ?1 AND namespace = ?2 AND service = ?3 AND image = ?4",
        &[
            organization.into(),
            namespace.as_str().into(),
            service.as_str().into(),
            image.as_str().into(),
        ],
    )?;
    rows.first()
        .map(|row| row.json(0, "Free receipt"))
        .transpose()
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
        let ran = match &record.origin {
            Origin::Own => would_run(tx, environment, (nodes, spec), namespace)?,
            Origin::Legacy => written(spec, namespace),
            Origin::Replay { .. } => replays
                .get(&spec.name)
                .cloned()
                .or_else(|| written(spec, namespace)),
        };
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

/// What a Deploy of its own Saved revision runs for `spec`: what the Deployment that last
/// applied its Service ran, while only run fields changed since, else `spec` as written.
/// The claim and the plan both resolve it here, so a plan asks to sign what a claim runs.
fn would_run(
    tx: &mut dyn Tx,
    environment: &str,
    (nodes, spec): (&[TargetNode], &RequestedServiceSpec),
    namespace: &Namespace,
) -> Result<Option<RanService>, RpcError> {
    Ok(reused(tx, environment, (nodes, spec), namespace)?.or_else(|| written(spec, namespace)))
}

fn written(spec: &RequestedServiceSpec, namespace: &Namespace) -> Option<RanService> {
    PayloadDigest::of(spec, namespace)
        .ok()
        .map(|digest| RanService { digest, pin: None })
}

/// Each Service of `target` that reaches data and that no signed version kept for what a
/// claim would run covers, with what it needs before a person's Deploy can run it.
///
/// A built Service is left to its automatic deploy rule while the rule is live, tracks
/// the Service's branch and approves its spec. So is an image Service a live registry rule
/// approves, once its spec names the image by digest. Its first Deploy still needs a build under
/// the rule, which a push or the Deploy makes; once a Server verified that build under
/// the rule's current generation, a Deploy reuses its image while only run fields
/// change, and carries its proof.
///
/// # Errors
/// Returns a storage error, or `corrupt` for a row that doesn't read.
pub(crate) fn unsigned(
    tx: &mut dyn Tx,
    (environment, organization, namespace): (&crate::id::EnvironmentId, &str, &Namespace),
    saved: &ployz_core::config::SavedEnvironmentIntent,
    (nodes, target): (&[TargetNode], &[RequestedServiceSpec]),
) -> Result<Vec<crate::Unsigned>, RpcError> {
    use crate::Needs;
    use ployz_core::config::ServiceSource;
    let stored = crate::auto_rule::stored(tx, environment)?
        .as_ref()
        .and_then(crate::auto_rule::opened);
    let ruled = ruled(stored.as_ref());
    let ended = stored.as_ref().is_some_and(|rule| rule.ended);
    let approved: Vec<ServiceName> = match stored.filter(|rule| !rule.ended) {
        Some(rule) => crate::auto_rule::approved(&rule, namespace, saved, target),
        None => Vec::new(),
    };
    let mut unsigned = Vec::new();
    for spec in target {
        let service = saved_service(saved, nodes, spec);
        let login = service.is_some_and(crate::registry::uses_login);
        if runs_unsigned(
            tx,
            (environment, organization, namespace),
            &ruled,
            (spec, login),
        )? {
            continue;
        }
        let reused = reused(tx, environment.as_str(), (nodes, spec), namespace)?;
        if let Some(ran) = reused.clone().or_else(|| written(spec, namespace))
            && !versions_for(tx, organization, (namespace, &spec.name), ran.digest)?.is_empty()
        {
            continue;
        }
        let built = service
            .is_some_and(|service| !matches!(service.config.source, ServiceSource::Image { .. }));
        // An image the live registry rule approves runs under it once the spec names its
        // digest: each Server pulls that digest from the registry itself.
        if !built && approved.contains(&spec.name) && content_addressed(&spec.container.image) {
            continue;
        }
        let needs = match (built, approved.contains(&spec.name)) {
            (true, true)
                if reused.is_some()
                    && proven(tx, (environment, organization, namespace), &spec.name)? =>
            {
                continue;
            }
            (true, true) => Needs::Rebuild,
            (true, false) => Needs::Rule,
            (false, _) if login && !content_addressed(&spec.container.image) => Needs::Digest,
            (false, _) => Needs::Version,
        };
        unsigned.push(crate::Unsigned {
            service: spec.name.clone(),
            needs,
            ended: needs == Needs::Rule && ended,
        });
    }
    Ok(unsigned)
}

/// Each Service a rule names, with each image a preview rule pins. A rule names each
/// Service it ever deployed, so one that ended still says which ran signed.
fn ruled(rule: Option<&ployz_core::authority::AutoDeployRule>) -> Vec<ServiceName> {
    let pinned = (rule.iter())
        .filter_map(|rule| rule.preview.as_ref())
        .flat_map(|preview| &preview.pinned)
        .map(|pinned| &pinned.name);
    rule.iter()
        .flat_map(|rule| &rule.services)
        .map(|service| &service.name)
        .chain(pinned)
        .cloned()
        .collect()
}

/// Whether `spec` runs unsigned: it reaches no data, no rule in `ruled` names it, and it
/// never ran signed. A Service that ran signed keeps its name on every Server for a signed
/// Container, which then refuses it unsigned.
fn runs_unsigned(
    tx: &mut dyn Tx,
    keys: (&crate::id::EnvironmentId, &str, &Namespace),
    ruled: &[ServiceName],
    (spec, login): (&RequestedServiceSpec, bool),
) -> Result<bool, RpcError> {
    Ok(ployz_core::authority::reach(spec, login).is_none()
        && !ruled.contains(&spec.name)
        && !signed_before(tx, keys, &spec.name)?)
}

/// Each Service of `target`, by the name it runs under, that a Server builds, from Git or
/// an upload, and that runs unsigned: its build is a free Build, whose image an unsigned
/// creation runs by its receipt, and needs nobody's approval.
///
/// # Errors
/// Returns a storage error, or `corrupt` for a row that doesn't read.
pub(crate) fn free_builds(
    tx: &mut dyn Tx,
    keys: (&crate::id::EnvironmentId, &str, &Namespace),
    saved: &ployz_core::config::SavedEnvironmentIntent,
    (nodes, target): (&[TargetNode], &[RequestedServiceSpec]),
) -> Result<BTreeSet<ServiceName>, RpcError> {
    use ployz_core::config::ServiceSource;
    let rule = crate::auto_rule::stored(tx, keys.0)?
        .as_ref()
        .and_then(crate::auto_rule::opened);
    let ruled = ruled(rule.as_ref());
    let mut free = BTreeSet::new();
    for spec in target {
        let Some(service) = saved_service(saved, nodes, spec) else {
            continue;
        };
        let login = crate::registry::uses_login(service);
        if !matches!(service.config.source, ServiceSource::Image { .. })
            && runs_unsigned(tx, keys, &ruled, (spec, login))?
        {
            free.insert(spec.name.clone());
        }
    }
    Ok(free)
}

/// The Saved Service `nodes` lowered into `spec`.
fn saved_service<'a>(
    saved: &'a ployz_core::config::SavedEnvironmentIntent,
    nodes: &[TargetNode],
    spec: &RequestedServiceSpec,
) -> Option<&'a ployz_core::config::SavedServiceIntent> {
    nodes
        .iter()
        .find_map(|node| match node {
            TargetNode::Service { id, runtime, .. } if *runtime == spec.name => Some(id),
            TargetNode::Service { .. }
            | TargetNode::Retire { .. }
            | TargetNode::Volume { .. }
            | TargetNode::Config { .. } => None,
        })
        .and_then(|id| {
            saved
                .services
                .iter()
                .find(|service| service.id == id.as_str())
        })
}

/// Each Service of `target`, by the name it runs under, that builds from a Git repository
/// and that the live automatic deploy rule doesn't approve as it stands, whether or not it
/// reaches data: only a Source step signed with the Deploy builds it. None while the
/// rule is ended, since a person turns automatic deploys back on first.
///
/// # Errors
/// Returns a storage error, or `corrupt` for a row that doesn't read.
pub(crate) fn unsourced(
    tx: &mut dyn Tx,
    (environment, organization, namespace): (&crate::id::EnvironmentId, &str, &Namespace),
    saved: &ployz_core::config::SavedEnvironmentIntent,
    (nodes, target): (&[TargetNode], &[RequestedServiceSpec]),
) -> Result<Vec<ServiceName>, RpcError> {
    use ployz_core::config::ServiceSource;
    let stored = crate::auto_rule::stored(tx, environment)?
        .as_ref()
        .and_then(crate::auto_rule::opened);
    if stored.as_ref().is_some_and(|rule| rule.ended) {
        return Ok(Vec::new());
    }
    let approved = stored.map_or_else(Vec::new, |rule| {
        crate::auto_rule::approved(&rule, namespace, saved, target)
    });
    let free = free_builds(
        tx,
        (environment, organization, namespace),
        saved,
        (nodes, target),
    )?;
    Ok(target
        .iter()
        .filter(|spec| !approved.contains(&spec.name) && !free.contains(&spec.name))
        .filter(|spec| {
            saved_service(saved, nodes, spec)
                .is_some_and(|service| matches!(service.config.source, ServiceSource::Git { .. }))
        })
        .map(|spec| spec.name.clone())
        .collect())
}

/// Whether a person signed a version of `service`, or a Server verified a build of it a
/// rule deployed in this Environment.
fn signed_before(
    tx: &mut dyn Tx,
    (environment, organization, namespace): (&crate::id::EnvironmentId, &str, &Namespace),
    service: &ServiceName,
) -> Result<bool, RpcError> {
    let versions = tx.query(
        "SELECT 1 AS signed FROM config_service_version \
         WHERE organization_id = ?1 AND namespace = ?2 AND service = ?3 LIMIT 1",
        &[
            organization.into(),
            namespace.as_str().into(),
            service.as_str().into(),
        ],
    )?;
    if !versions.is_empty() {
        return Ok(true);
    }
    let verified = tx.query(
        "SELECT 1 AS signed FROM config_build b \
         JOIN config_deployment d ON d.id = b.deployment_id \
         WHERE d.environment_id = ?1 AND b.service = ?2 AND b.verified IS NOT NULL LIMIT 1",
        &[environment.as_str().into(), service.as_str().into()],
    )?;
    Ok(!verified.is_empty())
}

/// Whether a Server verified, under the rule and generation `environment` keeps now, the
/// image a Deploy of `service` reuses: its latest build, the receipt preparation reads
/// first. The runner carries the same lookup's proof.
fn proven(
    tx: &mut dyn Tx,
    (environment, organization, namespace): (&crate::id::EnvironmentId, &str, &Namespace),
    service: &ServiceName,
) -> Result<bool, RpcError> {
    let image = crate::deployment::receipts(tx, environment)?
        .remove(service)
        .and_then(|receipts| receipts.into_iter().next())
        .and_then(|receipt| {
            let reference = receipt.pointer("/image/reference")?.as_str()?;
            ployz_core::authority::BuiltImageDigest::parse(reference).ok()
        });
    let Some(image) = image else {
        return Ok(false);
    };
    Ok(crate::build::verified_proof(tx, organization, (namespace, service), image)?.is_some())
}

/// What the Deployment that last applied `spec`'s Service node ran, when `spec` with
/// that pin has the digest it ran: the same tag, and only run fields changed. For a Service
/// that mounts a Provisioned Volume, a change of region alone keeps the pin too, at the new
/// digest, so the Deploy that lets it run in another region signs the image a move of its
/// Volume there starts.
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
    let Some((deployment, last)) = last_applied(tx, environment, node.as_str(), &spec.name)? else {
        return Ok(None);
    };
    let mut pinned = spec.clone();
    if let Some(pin) = &last.pin {
        if pin.tag != spec.container.image {
            return Ok(None);
        }
        pinned.container.image.clone_from(&pin.image);
    }
    let Ok(digest) = PayloadDigest::of(&pinned, namespace) else {
        return Ok(None);
    };
    if digest == last.digest {
        return Ok(Some(last));
    }
    if pinned
        .volume_graph()
        .mounted_provisioned_volumes()
        .next()
        .is_none()
    {
        return Ok(None);
    }
    let unplaced = |spec: &RequestedServiceSpec| {
        let mut spec = spec.clone();
        ployz_core::Region::keep(None, &mut spec.placement, &mut spec.regions);
        PayloadDigest::of(&spec, namespace).ok()
    };
    let moved = ran_spec(tx, &deployment, &spec.name, &last)
        .is_some_and(|ran| unplaced(&ran).is_some() && unplaced(&ran) == unplaced(&pinned));
    Ok(moved.then_some(RanService {
        digest,
        pin: last.pin,
    }))
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

/// The Deployment that last applied Service node `node`, and what `service` ran in it.
fn last_applied(
    tx: &mut dyn Tx,
    environment: &str,
    node: &str,
    service: &ServiceName,
) -> Result<Option<(DeploymentId, RanService)>, RpcError> {
    let rows = tx.query(
        "SELECT d.id, d.ran FROM config_applied a JOIN config_deployment d ON d.id = a.deployment_id \
         WHERE a.environment_id = ?1 AND a.node_id = ?2",
        &[environment.into(), node.into()],
    )?;
    let Some(row) = rows.first() else {
        return Ok(None);
    };
    let Some(text) = row.optional_text(1)? else {
        return Ok(None);
    };
    let deployment = row.parse::<DeploymentId>(0, "Deployment")?;
    Ok(parse(text)?
        .services
        .remove(service)
        .map(|ran| (deployment, ran)))
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
