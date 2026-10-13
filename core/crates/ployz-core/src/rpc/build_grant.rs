//! Typed requests and outcomes for minting and inspecting a Build Grant.

use serde::{Deserialize, Serialize};
use ts_rs::TS;

/// Mint a Build Grant allowing one image push into this Machine's image ingest.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MintBuildGrantRequest {
    /// The only repository the push may write, as Docker names it (`ployz-build/web`).
    pub repository: crate::BuildGrantRepository,
}

/// A minted grant. The Machine keeps it in memory only; a restart ends it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct BuildGrantMinted {
    /// Handle for [`InspectBuildGrantRequest`]; not secret.
    pub id: crate::BuildGrantId,
    /// Secret-bearing grant for the pusher only.
    #[ts(type = "string")]
    pub grant: crate::BuildGrant,
    /// The grant ends by itself this long after minting.
    pub expires_in_seconds: u64,
}

/// Read what a Build Grant received. Reading neither spends the grant nor shows its
/// secret; the grant ends by expiring. An unknown id, expired or lost to a daemon
/// restart, is `not_found`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct InspectBuildGrantRequest {
    pub id: crate::BuildGrantId,
}

/// What the Machine has received under a live grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct BuildGrantInspected {
    /// `sha256:` digest of the manifest the Machine verified and stored, once the
    /// push completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub pushed: Option<crate::ImageDigest>,
    /// The grant ends by itself this long from now.
    pub expires_in_seconds: u64,
}
