//! Typed requests and outcomes for minting and ending a Build Grant.

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
    /// Handle for [`EndBuildGrantRequest`]; not secret.
    pub id: crate::BuildGrantId,
    /// Secret-bearing grant for the pusher only.
    #[ts(type = "string")]
    pub grant: crate::BuildGrant,
    /// The grant ends by itself this long after minting.
    pub expires_in_seconds: u64,
}

/// End a Build Grant because its Build finished or was cancelled.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EndBuildGrantRequest {
    pub id: crate::BuildGrantId,
}

/// What the Machine received under an ended grant.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct BuildGrantEnded {
    /// `sha256:` digest of the manifest the Machine verified and stored, when the
    /// push completed; absent when nothing was pushed.
    #[serde(default)]
    pub pushed: Option<crate::ImageDigest>,
}
