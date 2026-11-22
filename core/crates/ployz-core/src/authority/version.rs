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
    deploy::{Creation, PayloadDigest, content_addressed},
    keys::{Approver, Purpose, Signed, SignerKey, Statement},
    open::open_approval,
    proposal::{ProposalRef, StepBody, StepKind},
    refusal::{AuthorityRefusal, field, first_mismatch},
    scope::Ask,
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

/// Every signed version that admitted a creation, never empty, and the creation's digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionAdmitted {
    pub by: Vec<Approved>,
    pub digest: PayloadDigest,
}

/// One signed version that admitted a creation. A key that signed several appears once
/// for each, so the Machine can check each time before it keeps the newest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Approved {
    pub by: Approver,
    pub signed_at: Timestamp,
    pub forget_older: bool,
}

/// Check `versions` against the creation `facts` names. Each statement whose signature a
/// key trusted here made, opened as a proposal step or alone, admits when it names this
/// Cluster, Namespace and Service and the digest of the requested spec, the spec runs a
/// content-addressed image, and a hook creation has a hook. Statements are opened in
/// order, each charged to `budget` first, until the next costs more than is left.
///
/// # Errors
/// Returns the first statement's refusal when none admits,
/// [`AuthorityRefusal::Overspent`] when the budget ran out before any was opened, and
/// [`AuthorityRefusal::Unsigned`] when there are none. A statement marked to forget that a
/// key trusted here signed must admit itself: its refusal, or
/// [`AuthorityRefusal::Overspent`] when it was left unopened, refuses the creation even
/// when another statement admits it. One no trusted key signed is ignored, so a removed
/// Device's old forget never blocks a fresh approval.
pub fn verify(
    versions: &[Signed<ServiceVersion>],
    facts: &VersionFacts<'_>,
    budget: &mut Budget,
) -> Result<VersionAdmitted, AuthorityRefusal> {
    let digest = requested_digest(facts.namespace, facts.creation)?;
    let mut by = Vec::new();
    let mut first = None;
    let mut forget_refused = None;
    let mut opened = 0;
    for version in versions.iter().take(MAX_PRESENTED) {
        if let Err(overspent) = budget.reserve(version, facts.trusted) {
            first.get_or_insert(overspent);
            break;
        }
        opened += 1;
        match admits(version, facts, digest) {
            Ok(approved) => by.push(approved),
            Err(refusal) => {
                if claims_forget(version, facts.trusted) {
                    forget_refused.get_or_insert_with(|| refusal.clone());
                }
                first.get_or_insert(refusal);
            }
        }
    }
    if versions
        .iter()
        .skip(opened)
        .any(|version| claims_forget(version, facts.trusted))
    {
        forget_refused.get_or_insert(AuthorityRefusal::Overspent);
    }
    if let Some(refusal) = forget_refused {
        return Err(refusal);
    }
    if by.is_empty() {
        return Err(first.unwrap_or(AuthorityRefusal::Unsigned {
            purpose: Purpose::Workflow,
        }));
    }
    Ok(VersionAdmitted { by, digest })
}

/// Whether `version` reads as a forget and a key trusted here signed it. Only such a
/// forget binds: one no trusted key signed is ignored everywhere.
#[must_use]
pub fn claims_forget(version: &Signed<ServiceVersion>, trusted: &TrustedKeys) -> bool {
    verifications(version, trusted) > 0 && version.open().is_ok_and(|body| body.forget_older)
}

fn admits(
    version: &Signed<ServiceVersion>,
    facts: &VersionFacts<'_>,
    digest: PayloadDigest,
) -> Result<Approved, AuthorityRefusal> {
    let (body, by, cluster) = open_approval(version, facts.trusted, facts.cluster)?;
    let creation = facts.creation;
    first_mismatch([
        (field::CLUSTER, body.cluster == cluster),
        (field::ENVIRONMENT, &body.namespace == facts.namespace),
        (
            field::SYSTEM,
            !body.forget_older || !body.namespace.is_system(),
        ),
        ("a Service", &body.service == creation.service),
        ("a payload", body.digest == digest),
    ])?;
    Ok(Approved {
        by,
        signed_at: body.signed_at,
        forget_older: body.forget_older,
    })
}

/// A signed version that asks every Member to forget the versions of its Service signed
/// before it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct Forget {
    pub service: QualifiedService,
    pub signed_at: Timestamp,
}

/// Open each of `versions` as a [`Forget`] for this Machine's Cluster `cluster`: signed
/// by a key trusted here, alone or as a step of a bundle, marked to forget, and outside
/// the system Namespace. It creates nothing, so no digest is checked. Statements are
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
                (field::CLUSTER, body.cluster == cluster),
                ("a forget", body.forget_older),
                (field::SYSTEM, !body.namespace.is_system()),
            ])?;
            Ok(Forget {
                service: QualifiedService::new(body.namespace, body.service),
                signed_at: body.signed_at,
            })
        })
        .collect()
}

/// The digest of the creation's spec, once its name, hook and image are ones a signed
/// version can cover.
fn requested_digest(
    namespace: &Namespace,
    creation: Creation<'_>,
) -> Result<PayloadDigest, AuthorityRefusal> {
    first_mismatch([
        ("a Service", &creation.spec.name == creation.service),
        (
            "a hook",
            creation.kind != ContainerKind::PreDeployHook || creation.spec.pre_deploy.is_some(),
        ),
        (
            "an image",
            content_addressed(&creation.spec.container.image),
        ),
        (
            "a Volume's Environment",
            creation.spec.volume_graph().born_in(namespace),
        ),
    ])?;
    PayloadDigest::of(&creation.spec.to_requested(), namespace)
        .map_err(|_| AuthorityRefusal::Mismatch { field: "a payload" })
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
