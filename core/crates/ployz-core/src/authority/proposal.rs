//! The bundle a person signs once over an ordered plan of steps, and the check a Machine
//! runs when one step of that plan reaches it.
//!
//! Each step is a body nobody signed: a [`super::ServiceVersion`], a
//! [`super::JoinApproval`], a [`super::KeyStep`] or a [`super::AutoDeployRule`]. What vouches for it is the bundle in
//! its envelope: a signed list of step digests in order. A Machine checks
//! that its step sits in the bundle at the index the step names, with the same kind, and
//! that the body it was handed hashes to that digest. Cloud can carry a step but cannot
//! add or alter one. A Machine sees only its own step, so the order Cloud runs the steps
//! in, and whether it runs them all, stays Cloud's.

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use ts_rs::TS;

use super::{
    canonical::Timestamp,
    keys::{Approver, Purpose, Signed, SignerKey, Statement},
    refusal::{AuthorityRefusal, field, first_mismatch},
    scope::Ask,
    trusted_key::TrustedKeys,
};
use crate::value::hex_newtype;
use crate::{Namespace, OperationId, SealingPublicKey};

hex_newtype!(
    /// SHA-256 of one step's canonical body, written as 64 lowercase hex digits.
    StepDigest, "step digest", 32, "", "64 lowercase hex digits"
);

impl StepDigest {
    #[must_use]
    pub fn of(body: &str) -> Self {
        Self(Sha256::digest(body.as_bytes()).into())
    }
}

/// What one step of a proposal runs, and how long the bundle's signature carries it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum StepKind {
    /// A [`super::ServiceVersion`]. It stands after the bundle's window ends. Written
    /// `deploy`, as 0.4 betas signed it.
    #[serde(rename = "deploy")]
    Version,
    /// A [`super::JoinApproval`]. One-shot: only inside the bundle's window.
    Join,
    /// A [`super::KeyStep`]. A trust is one-shot; a removal stands.
    Key,
    /// A [`super::AutoDeployRule`]. It stands after the bundle's window ends.
    Source,
}

/// One step of a proposal: its place in the order, what it runs, and the digest of the
/// step body a Machine must be handed.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ProposalStep {
    pub index: u32,
    pub kind: StepKind,
    pub digest: StepDigest,
}

/// A person approves an ordered plan of steps in one Cluster and Environment, under one
/// operation with a bounded lifetime.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ProposalApproval {
    pub cluster: SealingPublicKey,
    pub environment: Namespace,
    pub operation: OperationId,
    /// Non-empty, with `steps[i].index == i`.
    #[serde(deserialize_with = "dense_steps")]
    pub steps: Vec<ProposalStep>,
    pub issued_at: Timestamp,
    pub expires_at: Timestamp,
    pub signer: SignerKey,
}

fn dense_steps<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<ProposalStep>, D::Error> {
    let steps = Vec::<ProposalStep>::deserialize(deserializer)?;
    if steps.is_empty() {
        return Err(serde::de::Error::custom("a proposal needs a step"));
    }
    if steps
        .iter()
        .enumerate()
        .any(|(place, step)| u32::try_from(place).ok() != Some(step.index))
    {
        return Err(serde::de::Error::custom(
            "steps must be numbered from 0 in order",
        ));
    }
    Ok(steps)
}

impl Statement for ProposalApproval {
    const PURPOSE: Purpose = Purpose::Workflow;
    const TAKES_ASSERTION: bool = true;
}

/// Where a step body says it sits: which proposal, and at which index.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct ProposalRef {
    pub operation: OperationId,
    pub index: u32,
}

/// A statement a proposal may list as one of its steps.
pub(super) trait StepBody: Statement {
    fn kind(&self) -> StepKind;
    /// Where the body says it sits, absent from a single approval.
    fn place(&self) -> Option<ProposalRef>;
    fn signer(&self) -> SignerKey;
    fn cluster(&self) -> SealingPublicKey;
    /// `None` for a step that changes the whole Cluster, which no Environment bounds.
    fn namespace(&self) -> Option<&Namespace>;
    /// What the step asks of the key that signed it.
    fn ask(&self) -> Ask<'_>;
}

/// Open `step`, an unsigned step body, as the step of `bundle` it names, signed by one of
/// a key this Machine trusts. Returns the body, its approver, and this Machine's Cluster
/// key. The caller checks the body against its request.
///
/// # Errors
/// Returns the first [`AuthorityRefusal`] found: a step that carries its own signatures,
/// a bundle nested in a bundle, a signature problem on the bundle, a
/// malformed body, or a step outside the bundle, of another kind, differing from its
/// digest, or naming another Cluster or Environment than the bundle.
pub(super) fn open_step<T: StepBody>(
    bundle: &Signed<ProposalApproval>,
    step: &Signed<T>,
    trusted: &TrustedKeys,
    cluster: Option<SealingPublicKey>,
) -> Result<(T, Approver, SealingPublicKey), AuthorityRefusal> {
    if !step.signatures.is_empty() || bundle.proposal.is_some() {
        return Err(AuthorityRefusal::Mismatch {
            field: "a proposal",
        });
    }
    let approvers = bundle.approvers(trusted)?;
    let plan = bundle.open()?;
    let body = step.open()?;
    let Some(cluster) = cluster else {
        return Err(AuthorityRefusal::NoClusterIdentity);
    };
    let by = approvers
        .into_iter()
        .find(|approver| approver.signs_as(plan.signer));
    let place = body
        .place()
        .filter(|place| place.operation == plan.operation);
    let listed = place.and_then(|place| plan.steps.get(place.index as usize));
    first_mismatch([
        ("the signer", by.is_some() && body.signer() == plan.signer),
        ("a proposal", place.is_some()),
        (
            "a step",
            listed.is_some_and(|listed| {
                listed.kind == body.kind() && listed.digest == StepDigest::of(&step.body)
            }),
        ),
        (field::CLUSTER, plan.cluster == body.cluster()),
        (
            field::ENVIRONMENT,
            body.namespace()
                .is_none_or(|namespace| &plan.environment == namespace),
        ),
    ])?;
    let by = by.expect("the signer check passed");
    Ok((body, by, cluster))
}

/// A bundle for a test that needs only an envelope to ride one.
#[cfg(test)]
pub(super) fn stub_bundle() -> Box<Signed<ProposalApproval>> {
    let at = |millis| Timestamp::from_millis(millis).unwrap();
    let bundle = ProposalApproval {
        cluster: SealingPublicKey::from_bytes([9; 32]),
        environment: Namespace::parse("app").unwrap(),
        operation: OperationId::parse("0f8fad5bd9cb469fa16570867728950e").unwrap(),
        steps: vec![ProposalStep {
            index: 0,
            kind: StepKind::Version,
            digest: StepDigest::of("a step"),
        }],
        issued_at: at(100_000),
        expires_at: at(1_000_000),
        signer: SignerKey::Device(crate::DeviceKey::from_bytes([1; 32])),
    };
    Box::new(Signed::unsigned(&bundle).unwrap())
}
