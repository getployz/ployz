//! The opening steps the approval verifiers share: finding which approver signed as the
//! body says, opening a restart or deploy approval alone or as a step of a proposal, and
//! checking a session grant covers it.
//!
//! Each verifier keeps its own order and its request-specific checks. What differs between
//! them stays an argument here: which approvers may sign, and the action family a grant
//! must cover.

use super::{
    canonical::Timestamp,
    keys::{Approver, Signed, SignerKey},
    proposal::{self, StepBody},
    refusal::AuthorityRefusal,
    session::{ActionFamily, GrantScope},
    trusted_key::TrustedKeys,
};
use crate::SealingPublicKey;

/// The approver among `approvers` that `allowed` admits and that signs as `signer`.
///
/// # Errors
/// Returns [`AuthorityRefusal::Mismatch`] naming the signer when there is none.
pub(super) fn the_signer(
    approvers: Vec<Approver>,
    signer: SignerKey,
    allowed: impl Fn(&Approver) -> bool,
) -> Result<Approver, AuthorityRefusal> {
    approvers
        .into_iter()
        .find(|approver| allowed(approver) && approver.signs_as(signer))
        .ok_or(AuthorityRefusal::Mismatch {
            field: "the signer",
        })
}

/// Open `approval` as the body it carries, its approver and this Machine's Cluster key:
/// a single approval signed by a Device or a browser key under a grant, or a step of the
/// proposal bundle it carries. The caller checks the body against its request, the
/// grant's scope ([`covered`]) and the window.
///
/// # Errors
/// Returns the first [`AuthorityRefusal`] found: a signature or grant problem, a malformed
/// body, no Cluster identity, a body that names a proposal it does not ride in, or no
/// approver signing as the body says. A step is refused by [`proposal::open_step`].
pub(super) fn open_approval<T: StepBody>(
    approval: &Signed<T>,
    trusted: &TrustedKeys,
    cluster: Option<SealingPublicKey>,
    now: Timestamp,
) -> Result<(T, Approver, SealingPublicKey), AuthorityRefusal> {
    if let Some(bundle) = &approval.proposal {
        return proposal::open_step(bundle, approval, trusted, cluster, now);
    }
    let approvers = approval.approvers(trusted, now)?;
    let body = approval.open()?;
    let Some(cluster) = cluster else {
        return Err(AuthorityRefusal::NoClusterIdentity);
    };
    if body.place().is_some() {
        return Err(AuthorityRefusal::Mismatch {
            field: "a proposal",
        });
    }
    let by = the_signer(approvers, body.signer(), |_| true)?;
    Ok((body, by, cluster))
}

/// Check that a browser key's session grant covers `body` for `family`. A Device or a
/// passkey needs none.
///
/// # Errors
/// Returns the refusal of [`super::session::GrantBody::covers`].
pub(super) fn covered<T: StepBody>(
    by: &Approver,
    cluster: SealingPublicKey,
    body: &T,
    family: ActionFamily,
) -> Result<(), AuthorityRefusal> {
    if let Approver::Browser { grant, .. } = by {
        let (issued_at, expires_at) = body.window();
        grant.body.covers(&GrantScope {
            cluster,
            environment: body.namespace(),
            machines: body.machines(),
            family,
            issued_at,
            expires_at,
        })?;
    }
    Ok(())
}
