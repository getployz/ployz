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
    trusted_key::TrustedKeys,
};
use crate::{ContainerKind, Namespace, SealingPublicKey, ServiceName};

/// `service` in `namespace` of the Cluster whose Sealing public key is `cluster` may run
/// the spec whose [`PayloadDigest`] is `digest`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ServiceVersion {
    pub cluster: SealingPublicKey,
    pub namespace: Namespace,
    pub service: ServiceName,
    pub digest: PayloadDigest,
    /// The signer's clock. No Machine checks it.
    pub signed_at: Timestamp,
    pub signer: SignerKey,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub proposal: Option<ProposalRef>,
}

impl Statement for ServiceVersion {
    const PURPOSE: Purpose = Purpose::Workflow;
}

impl StepBody for ServiceVersion {
    fn kind(&self) -> StepKind {
        StepKind::Deploy
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
    fn namespace(&self) -> &Namespace {
        &self.namespace
    }
    fn window(&self) -> Option<(Timestamp, Timestamp)> {
        None
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

/// Every key whose signed version admitted a creation, never empty, and the creation's
/// digest.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VersionAdmitted {
    pub by: Vec<Approver>,
    pub digest: PayloadDigest,
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
/// [`AuthorityRefusal::Unsigned`] when there are none.
pub fn verify(
    versions: &[Signed<ServiceVersion>],
    facts: &VersionFacts<'_>,
    budget: &mut Budget,
) -> Result<VersionAdmitted, AuthorityRefusal> {
    let digest = requested_digest(facts.namespace, facts.creation)?;
    let mut by = Vec::new();
    let mut first = None;
    for version in versions.iter().take(MAX_PRESENTED) {
        if let Err(overspent) = budget.reserve(version, facts.trusted) {
            first.get_or_insert(overspent);
            break;
        }
        match admits(version, facts, digest) {
            Ok(approver) if !by.contains(&approver) => by.push(approver),
            Ok(_) => {}
            Err(refusal) => {
                first.get_or_insert(refusal);
            }
        }
    }
    if by.is_empty() {
        return Err(first.unwrap_or(AuthorityRefusal::Unsigned {
            purpose: Purpose::Workflow,
        }));
    }
    Ok(VersionAdmitted { by, digest })
}

fn admits(
    version: &Signed<ServiceVersion>,
    facts: &VersionFacts<'_>,
    digest: PayloadDigest,
) -> Result<Approver, AuthorityRefusal> {
    let (body, by, cluster) = open_approval(version, facts.trusted, facts.cluster)?;
    let creation = facts.creation;
    first_mismatch([
        (field::CLUSTER, body.cluster == cluster),
        (field::ENVIRONMENT, &body.namespace == facts.namespace),
        ("a Service", &body.service == creation.service),
        ("a payload", body.digest == digest),
    ])?;
    Ok(by)
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
                .passkeys
                .iter()
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
