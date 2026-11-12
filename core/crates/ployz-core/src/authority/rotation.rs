//! A person's approval of the recipient key a Cluster seals new values to after activation.
//!
//! The Cluster's Sealing public key stays its identity: statements, sealed values and pins
//! keep naming it. Any Machine that was ever a Member may hold its private half, so
//! activation adds a recipient key beside it. A Device's CLI generates that key, or on the
//! dashboard path the lowest-ID activated Machine does, and a person names its public half
//! here. A Machine installs, and re-seals to, only the recipient a verified rotation names.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::{
    Approver, AuthorityRefusal, Purpose, Signed, Statement, Timestamp,
    refusal::{field, first_mismatch},
    restart::SignerKey,
    rule::{RuleStatement, verify_signature},
    starting_set::Member,
    trusted_key::TrustedKeys,
};
use crate::{OperationId, SealingPublicKey};

/// A person's approval that `cluster` seals new values to `recipient`, which only the
/// Members named here receive.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct SealingRotation {
    /// The Cluster's identity: its first Sealing public key, which rotation keeps.
    pub cluster: SealingPublicKey,
    /// The activation this rotation finishes. A resend under it converges on one key.
    pub operation: OperationId,
    pub recipient: SealingPublicKey,
    /// The Members of the signed starting set: the only Machines sealed the private half.
    pub members: Vec<Member>,
    pub signer: SignerKey,
}

impl Statement for SealingRotation {
    const PURPOSE: Purpose = Purpose::Enforce;
}

impl RuleStatement for SealingRotation {
    fn signer(&self) -> SignerKey {
        self.signer
    }
}

/// Open a rotation for `cluster` that names `own`, signed by a Device in `trusted` or
/// answered by a trusted passkey's own prompt.
///
/// # Errors
/// Returns the refusal of [`verify_signature`], and [`AuthorityRefusal::Mismatch`] naming a
/// Cluster for a rotation of another one, or this Machine when it is not a named Member.
pub fn verify(
    signed: &Signed<SealingRotation>,
    trusted: &TrustedKeys,
    cluster: SealingPublicKey,
    own: &Member,
    now: Timestamp,
) -> Result<(SealingRotation, Approver), AuthorityRefusal> {
    let (body, approver) = verify_signature(signed, trusted, now)?;
    first_mismatch([
        (field::CLUSTER, body.cluster == cluster),
        ("this Machine", body.members.contains(own)),
    ])?;
    Ok((body, approver))
}

/// HPKE associated data binding a recipient key's sealed private half to one rotation, so a
/// blob sealed for one activation never installs under another.
#[must_use]
pub fn sealed_key_binding(cluster: &SealingPublicKey, operation: &OperationId) -> Vec<u8> {
    let mut aad = b"ployz-recipient-key-v1".to_vec();
    aad.extend(cluster.as_bytes());
    aad.extend(operation.to_string().as_bytes());
    aad
}
