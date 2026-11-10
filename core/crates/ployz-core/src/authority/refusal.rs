//! Why a Machine refuses a signed statement. Each reason names what the caller can fix.

use thiserror::Error;

use super::keys::{Purpose, SigningPublicKey};
use crate::{DeviceCode, OperationId};

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AuthorityRefusal {
    #[error("the {purpose} approval carries no signature")]
    Unsigned { purpose: Purpose },
    #[error("the {purpose} approval is malformed: {problem}")]
    Malformed { purpose: Purpose, problem: String },
    #[error("device {} is not approved on this Machine", .key.code())]
    UnknownKey { key: SigningPublicKey },
    #[error("the signature by device {} does not verify", .key.code())]
    BadSignature { key: SigningPublicKey },
    #[error("the passkey is not approved on this Machine")]
    UnknownPasskey,
    #[error("passkey {code} may approve workflows and rules only, not a {purpose} statement")]
    ApproveOnly { code: DeviceCode, purpose: Purpose },
    #[error("the approval names {field} this request does not match")]
    Mismatch { field: &'static str },
    #[error("the approval expired")]
    Expired,
    #[error("this Machine holds no Sealing Key, so it cannot tell which Cluster was approved")]
    NoClusterIdentity,
    #[error("operation {operation} already ran this step; it needs a new approval")]
    Replayed { operation: OperationId },
    #[error("this GitHub token already minted a Build Grant; a build asks for a new one")]
    ProofReplayed,
}

/// The first of `checks` that does not hold, as a [`AuthorityRefusal::Mismatch`] naming its field.
///
/// # Errors
/// Returns [`AuthorityRefusal::Mismatch`] for the first `false` check, in order.
pub(super) fn first_mismatch<const N: usize>(
    checks: [(&'static str, bool); N],
) -> Result<(), AuthorityRefusal> {
    match checks.into_iter().find(|(_, holds)| !holds) {
        Some((field, _)) => Err(AuthorityRefusal::Mismatch { field }),
        None => Ok(()),
    }
}

/// The names `Mismatch` gives the parts of a request that more than one statement checks.
pub(super) mod field {
    pub const CLUSTER: &str = "a Cluster";
    pub const ENVIRONMENT: &str = "an Environment";
    pub const MACHINES: &str = "Machines";
    pub const WINDOW: &str = "a window";
}
