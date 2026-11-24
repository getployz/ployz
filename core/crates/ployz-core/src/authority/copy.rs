//! A person's approval to make one new Volume from a restore point of another, on the
//! Machine that holds the source's Writer.
//!
//! A Copy step widens what a Machine holds, so it is one-shot: it rides in a bundle, and
//! it starts a copy only while the bundle's window holds on the Machine's floored clock.
//! The Machine records the step before the copy starts, so the same step sent again may
//! finish a copy it began after the window closed, and never begins a second one. The
//! step names the new Volume's birth, which the copy carries as its origin once it lands,
//! so only a version that expects that birth mounts it. The step starts no Container; a
//! version in the same bundle does.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    canonical::Timestamp,
    keys::{Purpose, Signed, SignerKey, Statement},
    open::{bundle_window, open_approval},
    proposal::{ProposalRef, StepBody, StepDigest, StepKind, listed_step},
    refusal::{AuthorityRefusal, MismatchField, first_mismatch},
    scope::Ask,
    trusted_key::TrustedKeys,
    validity::CLOCK_SKEW_MILLIS,
};
use crate::{
    DockerVolumeName, MachineId, Namespace, SealingPublicKey, SeedVolumeRequest, SnapshotGuid,
    VolumeBirth, VolumeOrigin,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct CopyStep {
    pub cluster: SealingPublicKey,
    pub namespace: Namespace,
    pub proposal: ProposalRef,
    pub signer: SignerKey,
    /// The Machine that holds the source's Writer, where the copy is made.
    pub machine: MachineId,
    /// The source Volume's physical name.
    pub source: DockerVolumeName,
    /// The origin the source's point carries: its birth's, or [`VolumeOrigin::legacy`] of
    /// `source` for data from before births.
    pub origin: VolumeOrigin,
    pub point: SnapshotGuid,
    /// When the point was taken, to the second, as the source's snapshot records it.
    pub taken_at: Timestamp,
    /// The new Volume's physical name.
    pub into: DockerVolumeName,
    pub birth: VolumeBirth,
}

impl Statement for CopyStep {
    const PURPOSE: Purpose = Purpose::Workflow;
}

impl StepBody for CopyStep {
    fn kind(&self) -> StepKind {
        StepKind::Copy
    }
    fn place(&self) -> Option<ProposalRef> {
        Some(self.proposal)
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
        Ask::Copy
    }
}

/// What a Machine holds itself when a Copy step reaches it.
#[derive(Clone, Copy, Debug)]
pub struct CopyFacts<'a> {
    pub trusted: &'a TrustedKeys,
    /// The Sealing public key this Machine holds, or `None` while it holds none.
    pub cluster: Option<SealingPublicKey>,
    pub machine: MachineId,
    /// This Machine's floored clock.
    pub now: Timestamp,
}

/// Whether the step's bundle window holds on the Machine's floored clock.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CopyWindow {
    Open,
    Closed,
}

/// A Copy step this Machine accepted for the seed it rides on.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CopyVerified {
    pub step: CopyStep,
    /// The digest the bundle lists for the step, which the Machine records it under.
    pub digest: StepDigest,
    pub window: CopyWindow,
    /// The last instant the window holds: the bundle's expiry plus the clock skew a
    /// Machine allows. Past it, a copy may finish but none starts.
    pub closes: Timestamp,
}

impl CopyVerified {
    /// The origin the new Volume carries once the copy lands.
    ///
    /// # Errors
    /// Returns a [`super::CanonicalError`] when the birth has no canonical form.
    pub fn origin(&self) -> Result<VolumeOrigin, super::CanonicalError> {
        VolumeOrigin::of(&self.step.birth)
    }
}

/// Check a Copy step against the seed it rides on, on the Machine `facts` describes. It
/// opens as a step of a bundle a person's key or a full CLI key signed, never alone and
/// never by a CI or agent key, names this Machine and Cluster, and copies `seed`'s point
/// of `seed`'s Volume into `seed`'s new Volume, both in the step's Namespace. The window
/// is reported, not refused: whether a closed window still lets the copy finish is the
/// Machine's record to say. The origin and time of the point are the source's dataset's
/// to say.
///
/// # Errors
/// Returns the opening's refusal, a bundle window that is malformed or not yet open, or a
/// mismatch naming the proposal, the Cluster, this Machine, the Namespace, the source,
/// the restore point or the new Volume.
pub fn verify(
    step: &Signed<CopyStep>,
    seed: &SeedVolumeRequest,
    facts: &CopyFacts<'_>,
) -> Result<CopyVerified, AuthorityRefusal> {
    let Some(bundle) = step.proposal.as_deref() else {
        return Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Proposal,
        });
    };
    let (body, _, cluster) = open_approval(step, facts.trusted, facts.cluster)?;
    let closes = bundle.open()?.expires_at.plus(CLOCK_SKEW_MILLIS);
    let window = match bundle_window(step, facts.now) {
        Ok(()) => CopyWindow::Open,
        Err(AuthorityRefusal::Expired) => CopyWindow::Closed,
        Err(refusal) => return Err(refusal),
    };
    first_mismatch([
        (MismatchField::Cluster, body.cluster == cluster),
        (MismatchField::ThisMachine, body.machine == facts.machine),
        (
            MismatchField::Namespace,
            in_namespace(&body.namespace, &body.source)
                && in_namespace(&body.namespace, &body.into)
                && body.birth.namespace == body.namespace,
        ),
        (MismatchField::CopySource, body.source == seed.name),
        (MismatchField::RestorePoint, body.point == seed.guid),
        (
            MismatchField::CopyDestination,
            body.into == seed.into && body.into != body.source,
        ),
    ])?;
    let digest = bundle
        .open()?
        .steps
        .get(body.proposal.index as usize)
        .map(|listed| listed.digest)
        .ok_or(AuthorityRefusal::Mismatch {
            field: MismatchField::Step,
        })?;
    Ok(CopyVerified {
        step: body,
        digest,
        window,
        closes,
    })
}

/// Open `step` as the step of its bundle it names, as [`verify`] does on a Machine, but
/// with no key to check a signature against: a step that rides alone, carries its own
/// signature, or isn't the bundle's step at the place it names is refused. The Store
/// records a Copy's birth only from a step that opens this way, so a Copy no Server would
/// take never claims a new Volume first.
///
/// # Errors
/// Returns [`AuthorityRefusal::Mismatch`] naming the proposal, the signer, the step, the
/// Cluster or the Environment, or a malformed bundle or body.
pub fn open_listed(step: &Signed<CopyStep>) -> Result<CopyStep, AuthorityRefusal> {
    let Some(bundle) = step.proposal.as_deref() else {
        return Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Proposal,
        });
    };
    listed_step(bundle, step)
}

/// Whether `physical` is a Volume's physical name in `namespace`.
fn in_namespace(namespace: &Namespace, physical: &DockerVolumeName) -> bool {
    Namespace::declared_volume_name(physical)
        .and_then(|declared| DockerVolumeName::parse(declared).ok())
        .is_some_and(|declared| namespace.volume_name(&declared) == *physical)
}

#[cfg(test)]
#[path = "copy_tests.rs"]
mod tests;
