//! What Ployz Cloud chooses and this CLI must match, in one place.
//!
//! Cloud's TypeScript reads these through `@ployz/sdk/cloud-contract`, generated from this
//! module by `ployz::sdk::generated_files`, so neither side keeps a copy. Each constant is
//! frozen by release: an older CLI and a newer Cloud, or the reverse, meet on these values,
//! so changing one is a wire change that needs a capability, never an edit.
//!
//! The lifetimes a Machine enforces without asking Cloud stay in [`crate::authority`], where
//! the Machine checks them. They are re-exported here so Cloud reads the same numbers, and
//! Cloud's own choices under them (a session's length, an approval's length) stay Cloud's.

pub use crate::authority::validity::{CLOCK_SKEW_MILLIS, MAX_LIFETIME_MILLIS};
/// The paths every ingress Server answers with its Machine id, which Cloud probes.
pub use crate::domain::{HOSTNAME_VERIFY_PATH, INGRESS_VERIFY_PATH};

/// The most compressed source one upload may hold, the same as Cloud's cap. It stays under
/// the 100 MB request body Cloudflare passes, in front of ployz.dev or a Tunnel.
pub const UPLOAD_LIMIT: usize = 95 * 1024 * 1024;

/// The reply header that names Cloud's manifest revision.
pub const REVISION_HEADER: &str = "x-ployz-manifest";
/// The request header that marks a call made through a coding agent.
pub const VIA_HEADER: &str = "x-ployz-via";
/// The request header that carries a Device's approval for the call.
pub const APPROVAL_HEADER: &str = "x-ployz-approval";
/// The request header that lets Cloud answer a retried call once.
pub const REQUEST_KEY_HEADER: &str = "x-ployz-request-key";
/// The request header that names a detected coding agent, for telemetry only.
pub const AGENT_HEADER: &str = "x-ployz-agent";
/// The reply header that carries Cloud's machine-readable code for a refusal.
pub const ERROR_CODE_HEADER: &str = "Ployz-Error-Code";

/// Cloud's device-authorization client id for this CLI.
pub const CLIENT_ID: &str = "ployz-cli";

/// The `format` of the command manifest Cloud serves at `/api/cli/manifest`.
pub const MANIFEST_FORMAT: u32 = 1;

/// The smallest `--size` of a Managed Volume, in bytes (0.1 GB): a much smaller Volume is
/// full when created (#2995). A Volume already recorded smaller still reads and resizes up.
pub const MIN_MANAGED_VOLUME_BYTES: u64 = 100_000_000;

#[cfg(test)]
#[path = "cloud_contract_tests.rs"]
mod tests;
