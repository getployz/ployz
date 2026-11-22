//! Why a Machine refuses a signed statement. Each reason names what the caller can fix.

use thiserror::Error;

use super::admission::DataReach;
use super::canonical::Timestamp;
use super::keys::{Purpose, SigningPublicKey};
use super::scope::KeyKind;
use crate::{DeviceCode, DockerVolumeName};

#[derive(Clone, Debug, Eq, Error, PartialEq)]
pub enum AuthorityRefusal {
    #[error("the {purpose} approval carries no signature")]
    Unsigned { purpose: Purpose },
    #[error("the {purpose} approval is malformed: {problem}")]
    Malformed { purpose: Purpose, problem: String },
    #[error("key {} is not trusted on this Machine", .key.code())]
    UnknownKey { key: SigningPublicKey },
    #[error("the signature by device {} does not verify", .key.code())]
    BadSignature { key: SigningPublicKey },
    #[error("the passkey is not approved on this Machine")]
    UnknownPasskey,
    /// The key is trusted here, but its kind or scope doesn't reach what it signed.
    #[error("the {kind} key {code} may not approve {what} here")]
    OutOfScope {
        code: DeviceCode,
        kind: KeyKind,
        what: &'static str,
    },
    /// The key is a grant whose Volume list names no entry for a Volume the spec mounts.
    #[error("the {kind} key {code} may not mount Volume {volume} here")]
    VolumeNotGranted {
        code: DeviceCode,
        kind: KeyKind,
        volume: DockerVolumeName,
    },
    #[error("the spec {reach}, and nothing signed it")]
    NeedsSignature { reach: DataReach },
    #[error("the approval names {field} this request does not match")]
    Mismatch { field: &'static str },
    #[error(
        "the rule pins getployz/build {sha}, which installs a ployz no Ployz release built; approve the rule again to pin the current builder"
    )]
    RetiredBuilder { sha: String },
    #[error("the approval expired")]
    Expired,
    #[error("this Machine holds no Sealing Key, so it cannot tell which Cluster was approved")]
    NoClusterIdentity,
    #[error("this GitHub token already minted a Build Grant; a build asks for a new one")]
    ProofReplayed,
    #[error("the request carries more signatures than this Machine checks for one request")]
    Overspent,
    /// A forget reached this Machine for the Service, signed at `floor`, after the approval.
    #[error("the approval was signed before {}, when this Service's older versions were forgotten", .floor.utc())]
    Forgotten { floor: Timestamp },
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
pub mod field {
    pub const CLUSTER: &str = "a Cluster";
    pub const ENVIRONMENT: &str = "an Environment";
    /// A forget names the system Namespace, whose Containers the daemon itself keeps.
    pub const SYSTEM: &str = "the system Namespace";
    /// Signed more than ten minutes past this Machine's clock.
    pub const AHEAD: &str = "a signing time";
    /// A Container a CI or agent key admitted before this Machine kept what it reaches.
    pub const KEPT_REACH: &str = "a kept reach";
    /// A kept Container whose every admitting key was rotated away or removed here.
    pub const KEPT_KEY: &str = "a kept key";
    /// A CI or agent key's version names no parent, or none of the statements presented
    /// with that digest is a parentless version a person or full CLI signed for the same
    /// Service.
    pub const PARENT: &str = "a parent version";
    /// A version that edits a parent also asks to forget older ones.
    pub const FORGET_PARENT: &str = "a forget on an edit";
}
