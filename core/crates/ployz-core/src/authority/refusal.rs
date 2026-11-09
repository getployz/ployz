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
    #[error("passkey {code} may approve workflows only, not a {purpose} statement")]
    ApproveOnly { code: DeviceCode, purpose: Purpose },
    #[error("the approval names {field} this request does not match")]
    Mismatch { field: &'static str },
    #[error("the approval expired")]
    Expired,
    #[error("this Machine holds no Sealing Key, so it cannot tell which Cluster was approved")]
    NoClusterIdentity,
    #[error("operation {operation} already ran this step; a new restart signs a new approval")]
    Replayed { operation: OperationId },
    #[error("this GitHub token already minted a Build Grant; a build asks for a new one")]
    ProofReplayed,
}
