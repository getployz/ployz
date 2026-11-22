//! A signed version: a person's standing yes to one spec of one Service, and the check a
//! Machine runs before it creates a Container under one.
//!
//! It names no Machine, no Deployment and no expiry, so the same statement admits the
//! spec on any Server of the Cluster for as long as a key that signed it is trusted there.
//! Cloud can store and replay it, but cannot mint one or change what it covers.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    canonical::Timestamp,
    deploy::{Creation, PayloadDigest, content_addressed, host_reach},
    keys::{Approver, Purpose, Signed, SignerKey, SigningPublicKey, Statement},
    open::open_approval,
    proposal::{ProposalRef, StepBody, StepKind},
    refusal::{AuthorityRefusal, MismatchField, first_mismatch},
    scope::{Ask, Reach},
    trusted_key::TrustedKeys,
};
use crate::{ContainerKind, Namespace, QualifiedService, SealingPublicKey, ServiceName};

/// `service` in `namespace` of the Cluster whose Sealing public key is `cluster` may run
/// the spec whose [`PayloadDigest`] is `digest`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ServiceVersion {
    pub cluster: SealingPublicKey,
    pub namespace: Namespace,
    pub service: ServiceName,
    pub digest: PayloadDigest,
    /// The signer's clock when it signed. A Machine refuses a version signed before its
    /// Service's floor, or too far past the Machine's own clock.
    pub signed_at: Timestamp,
    pub signer: SignerKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub proposal: Option<ProposalRef>,
    /// Every Member refuses each version of this Service signed before this one.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub forget_older: bool,
    /// The digest of the version this one edits: one a person or full CLI signed for the
    /// same Service, which names no parent itself. A CI or agent key's version must name
    /// one, so such a key never signs a Service's first version.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub parent: Option<PayloadDigest>,
}

impl Statement for ServiceVersion {
    const PURPOSE: Purpose = Purpose::Workflow;
}

impl StepBody for ServiceVersion {
    fn kind(&self) -> StepKind {
        StepKind::Version
    }
    fn place(&self) -> Option<ProposalRef> {
        self.proposal
    }
    fn signer(&self) -> SignerKey {
        self.signer
    }
    fn cluster(&self) -> SealingPublicKey {
        self.cluster
    }
    fn namespace(&self) -> Option<&Namespace> {
        Some(&self.namespace)
    }
    fn ask(&self) -> Ask<'_> {
        Ask::Version {
            namespace: &self.namespace,
            service: &self.service,
            forgets: self.forget_older,
            reaches_host: false,
            reach: Reach::none(),
        }
    }
}

/// Most signed versions one request may carry.
pub const MAX_PRESENTED: usize = 32;
/// Most signature verifications one admission may cost, passkey candidates included,
/// across every statement and proof its request carries.
pub const MAX_CHECKED: usize = 8;

/// The signature verifications one admission has left. Every statement is charged
/// before it is opened, whichever verifier opens it, so a request that mixes proofs
/// still costs at most [`MAX_CHECKED`].
#[derive(Debug)]
pub struct Budget {
    left: usize,
}

impl Default for Budget {
    fn default() -> Self {
        Self { left: MAX_CHECKED }
    }
}

impl Budget {
    /// Reserve what opening `signed` can cost on a Machine trusting `trusted`.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Overspent`], reserving nothing, when it costs more
    /// than is left.
    pub fn reserve<T>(
        &mut self,
        signed: &Signed<T>,
        trusted: &TrustedKeys,
    ) -> Result<(), AuthorityRefusal> {
        let cost = verifications(signed, trusted);
        self.left = self
            .left
            .checked_sub(cost)
            .ok_or(AuthorityRefusal::Overspent)?;
        Ok(())
    }

    /// Reserve one verification of a signature that is not a [`Signed`] statement.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Overspent`], reserving nothing, when none is left.
    pub fn reserve_one(&mut self) -> Result<(), AuthorityRefusal> {
        self.left = self
            .left
            .checked_sub(1)
            .ok_or(AuthorityRefusal::Overspent)?;
        Ok(())
    }
}

/// What a Machine knows when it checks signed versions against one creation.
#[derive(Clone, Debug)]
pub struct VersionFacts<'a> {
    /// The Sealing public key this Machine holds, or `None` while it holds none.
    pub cluster: Option<SealingPublicKey>,
    pub trusted: &'a TrustedKeys,
    pub namespace: &'a Namespace,
    pub creation: Creation<'a>,
}

/// Every signed version that admitted a creation, never empty, the creation's digest, and
/// what it reaches, which each of their signers covers alone.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionAdmitted {
    pub by: Vec<Approved>,
    pub digest: PayloadDigest,
    pub reach: Reach,
}

/// One signed version that admitted a creation. A key that signed several appears once
/// for each, so the Machine can check each time before it keeps the newest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Approved {
    pub by: Approver,
    /// The signer's own clock, which a Machine refuses when it is too far ahead.
    pub signed_at: Timestamp,
    /// The older of `signed_at` and its parent's, which a Service's floor checks, so a
    /// forget that reaches the parent also reaches every edit of it.
    pub floor_at: Timestamp,
    pub forget_older: bool,
}

/// Check `versions` against the creation `facts` names. Only a statement with the digest
/// of the requested spec may admit it; any other is presented as a parent, and so is a
/// parentless one of that digest that an admitting edit names as its parent. Each statement
/// whose signature a key trusted here made, opened as a proposal step or alone, admits
/// when it names this Cluster, Namespace and Service, the spec runs a content-addressed
/// image, a hook creation has a hook, and its one signer covers the whole spec: its host
/// reach, every Sealed part it opens and every Volume it mounts. A CI or agent key's
/// statement must also name a parent, and one that names a parent must find one among
/// the statements presented with that digest: a parentless version of the same Service
/// that a person or full CLI signed. Statements are opened in order, each charged to
/// `budget` first, until the next costs more than is left.
///
/// # Errors
/// Returns the first statement's refusal when none admits,
/// [`AuthorityRefusal::Overspent`] when the budget ran out before any was opened, and
/// [`AuthorityRefusal::Unsigned`] when there are none. A statement of the requested
/// digest marked to forget that a person or full CLI key trusted here signed must admit
/// itself: its refusal, or [`AuthorityRefusal::Overspent`] when it was left unopened,
/// refuses the creation even when another statement admits it. One no such key signed is
/// ignored, so a removed Device's old forget never blocks a fresh approval.
pub fn verify(
    versions: &[Signed<ServiceVersion>],
    facts: &VersionFacts<'_>,
    budget: &mut Budget,
) -> Result<VersionAdmitted, AuthorityRefusal> {
    let requested = requested(facts.namespace, facts.creation)?;
    let presented = versions.get(..MAX_PRESENTED).unwrap_or(versions);
    let mut candidates: Vec<&Signed<ServiceVersion>> = presented
        .iter()
        .filter(|version| {
            version
                .open()
                .map_or(true, |body| body.digest == requested.digest)
        })
        .collect();
    // With none of this digest nothing admits, so each is opened for its refusal.
    if candidates.is_empty() {
        candidates = presented.iter().collect();
    }
    let mut parents = Parents {
        presented,
        found: Vec::new(),
    };
    let mut by = Vec::new();
    let mut first = None;
    let mut forget_refused = None;
    let mut opened = 0;
    for version in &candidates {
        if let Err(overspent) = budget.reserve(version, facts.trusted) {
            first.get_or_insert(overspent);
            break;
        }
        opened += 1;
        match admits(version, facts, &requested, &mut parents, budget) {
            Ok(approved) => by.push((approved, version.open().ok().and_then(|body| body.parent))),
            Err(refusal) => {
                if claims_forget(version, facts.trusted) {
                    forget_refused.get_or_insert_with(|| refusal.clone());
                }
                first.get_or_insert(refusal);
            }
        }
    }
    if candidates
        .iter()
        .skip(opened)
        .any(|version| claims_forget(version, facts.trusted))
    {
        forget_refused.get_or_insert(AuthorityRefusal::Overspent);
    }
    if let Some(refusal) = forget_refused {
        return Err(refusal);
    }
    // An edit that left the spec as it was presents its parent with the requested digest.
    // That parent's signer approved the parent, not the edit, so it admits nothing here
    // and only its time reaches the edit's floor.
    if by
        .iter()
        .any(|(_, parent)| *parent == Some(requested.digest))
    {
        by.retain(|(_, parent)| parent.is_some());
    }
    if by.is_empty() {
        return Err(first.unwrap_or(AuthorityRefusal::Unsigned {
            purpose: Purpose::Workflow,
        }));
    }
    Ok(VersionAdmitted {
        by: by.into_iter().map(|(approved, _)| approved).collect(),
        digest: requested.digest,
        reach: requested.reach,
    })
}

/// Whether `version` reads as a forget and a key trusted here that may forget, a
/// person's or a full CLI's, signed it. Only such a forget binds: one no such key signed
/// is ignored everywhere.
#[must_use]
pub fn claims_forget(version: &Signed<ServiceVersion>, trusted: &TrustedKeys) -> bool {
    signed_whole_cluster(version, trusted) && version.open().is_ok_and(|body| body.forget_older)
}

/// Whether a person's or full CLI's key trusted here signed `signed` or the bundle it
/// rides in. Nothing is verified.
fn signed_whole_cluster<T>(signed: &Signed<T>, trusted: &TrustedKeys) -> bool {
    whole_cluster_envelope(signed, trusted)
        || signed
            .proposal
            .as_deref()
            .is_some_and(|bundle| whole_cluster_envelope(bundle, trusted))
}

fn whole_cluster_envelope<T>(signed: &Signed<T>, trusted: &TrustedKeys) -> bool {
    signed.signatures.iter().any(|signature| {
        trusted
            .devices()
            .any(|device| SigningPublicKey::from(device) == signature.key)
    }) || signed.assertions.iter().any(|assertion| {
        trusted
            .passkeys()
            .any(|passkey| passkey.passkey.credential_id == assertion.credential_id)
    })
}

/// Whether `version` admits the creation `requested` describes. Its signer alone must
/// cover the spec: [`Cap::HostReach`] when it reaches its host, and a place and Volume
/// list that hold all of its reach. A version that names a parent takes the older of its
/// own time and the parent's as the time a floor checks.
///
/// [`Cap::HostReach`]: super::scope::Cap::HostReach
fn admits(
    version: &Signed<ServiceVersion>,
    facts: &VersionFacts<'_>,
    requested: &Requested,
    parents: &mut Parents<'_>,
    budget: &mut Budget,
) -> Result<Approved, AuthorityRefusal> {
    let (body, by, cluster) = open_approval(version, facts.trusted, facts.cluster)?;
    facts.trusted.covers(
        &by,
        Ask::Version {
            namespace: &body.namespace,
            service: &body.service,
            forgets: body.forget_older,
            reaches_host: requested.reaches_host,
            reach: &requested.reach,
        },
    )?;
    let scoped = facts
        .trusted
        .of(&by)
        .is_some_and(|key| !key.kind().whole_cluster());
    let creation = facts.creation;
    first_mismatch([
        (MismatchField::Cluster, body.cluster == cluster),
        (
            MismatchField::Environment,
            &body.namespace == facts.namespace,
        ),
        (
            MismatchField::SystemNamespace,
            !body.forget_older || !body.namespace.is_system(),
        ),
        (MismatchField::Service, &body.service == creation.service),
        (MismatchField::Payload, body.digest == requested.digest),
        (
            MismatchField::ForgetParent,
            !body.forget_older || body.parent.is_none(),
        ),
        (MismatchField::Parent, !scoped || body.parent.is_some()),
    ])?;
    let floor_at = match body.parent {
        Some(parent) => parents
            .signed_at(parent, &body, facts.trusted, budget)?
            .min(body.signed_at),
        None => body.signed_at,
    };
    Ok(Approved {
        by,
        signed_at: body.signed_at,
        floor_at,
        forget_older: body.forget_older,
    })
}

/// The statements presented beside a creation, as parents of the versions that admit it.
/// Each parent digest is looked up once, whichever version names it.
struct Parents<'a> {
    presented: &'a [Signed<ServiceVersion>],
    found: Vec<(PayloadDigest, Result<Timestamp, AuthorityRefusal>)>,
}

impl Parents<'_> {
    /// When the parent `digest` of `child` was signed: the first statement presented with
    /// that digest that opens under a key trusted here, each charged to `budget` first,
    /// and is a parentless version of `child`'s Cluster, Namespace and Service that a
    /// person or full CLI signed.
    ///
    /// # Errors
    /// Returns [`AuthorityRefusal::Overspent`] when the budget ran out before one was
    /// found, and [`AuthorityRefusal::Mismatch`] naming [`MismatchField::Parent`] when none is.
    fn signed_at(
        &mut self,
        digest: PayloadDigest,
        child: &ServiceVersion,
        trusted: &TrustedKeys,
        budget: &mut Budget,
    ) -> Result<Timestamp, AuthorityRefusal> {
        if let Some((_, found)) = self.found.iter().find(|(held, _)| *held == digest) {
            return found.clone();
        }
        let found = self.find(digest, child, trusted, budget);
        self.found.push((digest, found.clone()));
        found
    }

    fn find(
        &self,
        digest: PayloadDigest,
        child: &ServiceVersion,
        trusted: &TrustedKeys,
        budget: &mut Budget,
    ) -> Result<Timestamp, AuthorityRefusal> {
        let with_digest = self
            .presented
            .iter()
            .filter(|statement| statement.open().is_ok_and(|body| body.digest == digest));
        for statement in with_digest {
            budget.reserve(statement, trusted)?;
            if let Ok(body) = open_root(statement, trusted, child.cluster)
                && body.namespace == child.namespace
                && body.service == child.service
            {
                return Ok(body.signed_at);
            }
        }
        Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Parent,
        })
    }
}

/// Open `version` as a root, the way a Server opens an edit's parent: signed for Cluster
/// `cluster` by a person's or full CLI's key trusted here, alone or as a step of a bundle,
/// and naming no parent of its own. The caller checks its Namespace, Service and digest.
///
/// # Errors
/// Returns the opening's refusal, and [`AuthorityRefusal::Mismatch`] naming the Cluster
/// for another Cluster's, or [`MismatchField::Parent`] for one a CI or agent key signed or one
/// that names a parent.
pub fn open_root(
    version: &Signed<ServiceVersion>,
    trusted: &TrustedKeys,
    cluster: SealingPublicKey,
) -> Result<ServiceVersion, AuthorityRefusal> {
    let (body, by, _) = open_approval(version, trusted, Some(cluster))?;
    let human = trusted
        .of(&by)
        .is_some_and(|key| key.kind().whole_cluster());
    first_mismatch([
        (MismatchField::Cluster, body.cluster == cluster),
        (MismatchField::Parent, human && body.parent.is_none()),
    ])?;
    Ok(body)
}

/// A signed version that asks every Member to forget the versions of its Service signed
/// before it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Forget {
    pub service: QualifiedService,
    pub signed_at: Timestamp,
}

/// Open each of `versions` as a [`Forget`] for this Machine's Cluster `cluster`: signed
/// by a key trusted here, alone or as a step of a bundle, marked to forget, naming no
/// parent, and outside the system Namespace. It creates nothing, so no digest is checked. Statements are
/// charged to one [`Budget`] in order; one past it is refused as overspent.
#[must_use]
pub fn verify_forget(
    versions: &[Signed<ServiceVersion>],
    trusted: &TrustedKeys,
    cluster: Option<SealingPublicKey>,
) -> Vec<Result<Forget, AuthorityRefusal>> {
    let mut budget = Budget::default();
    versions
        .iter()
        .take(MAX_PRESENTED)
        .map(|version| {
            budget.reserve(version, trusted)?;
            let (body, _, cluster) = open_approval(version, trusted, cluster)?;
            first_mismatch([
                (MismatchField::Cluster, body.cluster == cluster),
                (MismatchField::Forget, body.forget_older),
                (MismatchField::SystemNamespace, !body.namespace.is_system()),
                (MismatchField::ForgetParent, body.parent.is_none()),
            ])?;
            Ok(Forget {
                service: QualifiedService::new(body.namespace, body.service),
                signed_at: body.signed_at,
            })
        })
        .collect()
}

/// What a signed version must cover of a creation's spec.
struct Requested {
    digest: PayloadDigest,
    reaches_host: bool,
    reach: Reach,
}

/// The digest of the creation's spec, once its name, hook and image are ones a signed
/// version can cover, whether it reaches its Server's host, and what else it reaches.
fn requested(namespace: &Namespace, creation: Creation<'_>) -> Result<Requested, AuthorityRefusal> {
    first_mismatch([
        (
            MismatchField::Service,
            &creation.spec.name == creation.service,
        ),
        (
            MismatchField::Hook,
            creation.kind != ContainerKind::PreDeployHook || creation.spec.pre_deploy.is_some(),
        ),
        (
            MismatchField::Image,
            content_addressed(&creation.spec.container.image),
        ),
        (
            MismatchField::VolumesEnvironment,
            creation.spec.volume_graph().born_in(namespace),
        ),
    ])?;
    let spec = creation.spec.to_requested();
    let digest = PayloadDigest::of(&spec, namespace).map_err(|_| AuthorityRefusal::Mismatch {
        field: MismatchField::Payload,
    })?;
    let reach =
        Reach::of(creation.kind, creation.spec).map_err(|_| AuthorityRefusal::Mismatch {
            field: MismatchField::Payload,
        })?;
    Ok(Requested {
        digest,
        reaches_host: !host_reach(&spec).is_empty(),
        reach,
    })
}

/// How many signature verifications opening `signed` can cost here: one per signature
/// by a trusted Device, and per passkey answer one per trusted passkey under its
/// credential ID, on the statement and on the bundle it rides in. A signature by a key
/// this Machine doesn't trust is refused before it is verified, so it costs nothing.
fn verifications<T>(signed: &Signed<T>, trusted: &TrustedKeys) -> usize {
    envelope_verifications(signed, trusted)
        + signed
            .proposal
            .as_deref()
            .map_or(0, |bundle| envelope_verifications(bundle, trusted))
}

fn envelope_verifications<T>(signed: &Signed<T>, trusted: &TrustedKeys) -> usize {
    let devices = signed
        .signatures
        .iter()
        .filter(|signature| trusted.signing_keys().any(|key| key == signature.key))
        .count();
    let passkeys = signed
        .assertions
        .iter()
        .map(|assertion| {
            trusted
                .passkeys()
                .filter(|passkey| passkey.passkey.credential_id == assertion.credential_id)
                .count()
        })
        .sum::<usize>();
    devices + passkeys
}

/// Read at most [`MAX_PRESENTED`] signed versions.
///
/// # Errors
/// Returns the deserializer's error for more, or for a list that doesn't parse.
pub fn at_most_presented<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<Signed<ServiceVersion>>, D::Error> {
    let versions = Vec::<Signed<ServiceVersion>>::deserialize(deserializer)?;
    if versions.len() > MAX_PRESENTED {
        return Err(serde::de::Error::custom(format!(
            "a request carries at most {MAX_PRESENTED} signed versions"
        )));
    }
    Ok(versions)
}

#[cfg(test)]
#[path = "version_tests.rs"]
mod tests;
