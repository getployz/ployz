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

pub use crate::authority::{
    session::MAX_GRANT_LIFETIME_MILLIS,
    validity::{CLOCK_SKEW_MILLIS, MAX_LIFETIME_MILLIS},
};

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

/// The `protocolVersion` of a Server's check-in request.
pub const CHECKIN_PROTOCOL_VERSION: u8 = 1;
/// How far a Server's clock and Cloud's may differ, in seconds, before Cloud refuses a check-in.
pub const MAX_SKEW_SECONDS: i64 = 300;
/// How long Cloud asks a waiting Server to wait between check-ins, in seconds.
pub const CHECKIN_WAIT_SECONDS: u64 = 10;
/// The optional enroll instruction fields this CLI carries out, so Cloud never sends one an
/// older CLI would drop.
pub const ENROLL_OPTIONS: &[&str] = &["ingressClosed"];

/// How long Cloud's reachability probe holds a window open, in milliseconds.
pub const PROBE_WINDOW_MS: u64 = 10_000;
/// How long after a window opens Cloud's first TCP attempt comes, in milliseconds.
pub const PROBE_FIRST_ATTEMPT_MS: u64 = 1_000;
/// How long Cloud waits between TCP attempts inside a window, in milliseconds.
pub const PROBE_ATTEMPT_EVERY_MS: u64 = 1_500;
/// How long one TCP attempt may take, in milliseconds.
pub const PROBE_ATTEMPT_TIMEOUT_MS: u64 = 3_000;
/// How long Cloud reads a connection for the greeting, in milliseconds.
pub const PROBE_GREETING_READ_MS: u64 = 500;
/// How many characters of the keyed digest a probe's TCP greeting holds.
pub const PROBE_GREETING_LENGTH: usize = 22;

/// The smallest `--size` of a Managed Volume, in bytes (0.1 GB): a much smaller Volume is
/// full when created (#2995). A Volume already recorded smaller still reads and resizes up.
pub const MIN_MANAGED_VOLUME_BYTES: u64 = 100_000_000;

#[cfg(test)]
#[path = "cloud_contract_tests.rs"]
mod tests;
