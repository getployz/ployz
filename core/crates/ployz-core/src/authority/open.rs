//! The opening steps the approval verifiers share: finding which approver signed as the
//! body says, and opening a deploy approval alone or as a step of a proposal.

use super::{
    keys::{Approver, Signed, SignerKey},
    proposal::{self, StepBody},
    refusal::AuthorityRefusal,
    trusted_key::TrustedKeys,
};
use crate::SealingPublicKey;

/// The approver among `approvers` that signs as `signer`.
///
/// # Errors
/// Returns [`AuthorityRefusal::Mismatch`] naming the signer when there is none.
pub(super) fn the_signer(
    approvers: Vec<Approver>,
    signer: SignerKey,
) -> Result<Approver, AuthorityRefusal> {
    approvers
        .into_iter()
        .find(|approver| approver.signs_as(signer))
        .ok_or(AuthorityRefusal::Mismatch {
            field: "the signer",
        })
}

/// Open `approval` as the body it carries, its approver and this Machine's Cluster key:
/// a single approval signed by a Device, or a step of the proposal bundle it carries. The
/// caller checks the body against its request and the window.
///
/// # Errors
/// Returns the first [`AuthorityRefusal`] found: a signature problem, a malformed
/// body, no Cluster identity, a body that names a proposal it does not ride in, or no
/// approver signing as the body says. A step is refused by [`proposal::open_step`].
pub(super) fn open_approval<T: StepBody>(
    approval: &Signed<T>,
    trusted: &TrustedKeys,
    cluster: Option<SealingPublicKey>,
) -> Result<(T, Approver, SealingPublicKey), AuthorityRefusal> {
    if let Some(bundle) = &approval.proposal {
        return proposal::open_step(bundle, approval, trusted, cluster);
    }
    let approvers = approval.approvers(trusted)?;
    let body = approval.open()?;
    let Some(cluster) = cluster else {
        return Err(AuthorityRefusal::NoClusterIdentity);
    };
    if body.place().is_some() {
        return Err(AuthorityRefusal::Mismatch {
            field: "a proposal",
        });
    }
    let by = the_signer(approvers, body.signer())?;
    Ok((body, by, cluster))
}
