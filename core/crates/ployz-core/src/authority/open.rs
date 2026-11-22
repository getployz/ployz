//! The opening steps the approval verifiers share: finding which approver signed as the
//! body says, and opening a statement alone or as a step of a proposal.

use super::{
    canonical::Timestamp,
    keys::{Approver, Signed, SignerKey},
    proposal::{self, StepBody},
    refusal::AuthorityRefusal,
    trusted_key::TrustedKeys,
};
use crate::SealingPublicKey;
use crate::authority::MismatchField;

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
            field: MismatchField::Signer,
        })
}

/// Open `approval` as the body it carries, its approver and this Machine's Cluster key:
/// a single approval a trusted key signed, or a step of the proposal bundle it carries.
/// The approver must cover what the body asks, so a key whose kind or scope doesn't reach
/// is refused here, before any verifier writes. A step is checked against its bundle's
/// signer. The caller checks the body against its request.
///
/// # Errors
/// Returns the first [`AuthorityRefusal`] found: a signature problem, a malformed
/// body, no Cluster identity, a body that names a proposal it does not ride in, no
/// approver signing as the body says, or one [`TrustedKeys::covers`] refuses. A step is
/// refused by [`proposal::open_step`].
pub(super) fn open_approval<T: StepBody>(
    approval: &Signed<T>,
    trusted: &TrustedKeys,
    cluster: Option<SealingPublicKey>,
) -> Result<(T, Approver, SealingPublicKey), AuthorityRefusal> {
    let (body, by, cluster) = match &approval.proposal {
        Some(bundle) => proposal::open_step(bundle, approval, trusted, cluster)?,
        None => open_alone(approval, trusted, cluster)?,
    };
    trusted.covers(&by, body.ask())?;
    Ok((body, by, cluster))
}

fn open_alone<T: StepBody>(
    approval: &Signed<T>,
    trusted: &TrustedKeys,
    cluster: Option<SealingPublicKey>,
) -> Result<(T, Approver, SealingPublicKey), AuthorityRefusal> {
    let approvers = approval.approvers(trusted)?;
    let body = approval.open()?;
    let Some(cluster) = cluster else {
        return Err(AuthorityRefusal::NoClusterIdentity);
    };
    if body.place().is_some() {
        return Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Proposal,
        });
    }
    let by = the_signer(approvers, body.signer())?;
    Ok((body, by, cluster))
}

/// Open a step that widens what a Machine trusts: [`open_approval`] for a step that rides
/// in a bundle, whose window must hold at `now`. `now` is the Machine's floored clock, so a
/// clock turned back cannot reopen a window.
///
/// # Errors
/// As [`open_approval`], [`AuthorityRefusal::Mismatch`] naming a proposal for a step that
/// rides in none, and the window's refusal from [`super::validity::verify`].
pub(super) fn open_one_shot<T: StepBody>(
    step: &Signed<T>,
    trusted: &TrustedKeys,
    cluster: Option<SealingPublicKey>,
    now: Timestamp,
) -> Result<(T, Approver, SealingPublicKey), AuthorityRefusal> {
    if step.proposal.is_none() {
        return Err(AuthorityRefusal::Mismatch {
            field: MismatchField::Proposal,
        });
    }
    let opened = open_approval(step, trusted, cluster)?;
    bundle_window(step, now)?;
    Ok(opened)
}

/// Refuse a step whose bundle's window does not hold at `now`. A step that rides in no
/// bundle has no window here.
///
/// # Errors
/// Returns the bundle's malformed body or the window's refusal.
pub(super) fn bundle_window<T>(step: &Signed<T>, now: Timestamp) -> Result<(), AuthorityRefusal> {
    let Some(bundle) = &step.proposal else {
        return Ok(());
    };
    let plan = bundle.open()?;
    super::validity::verify(plan.issued_at, plan.expires_at, now)
}
