//! Shared validated values used by bounded operations and runtime adapters.

mod build;
mod routes;
mod text;

pub use build::{
    BuildAdapterToolchainEvidence, BuildCachePruneEvidence, BuildLogChunk, BuildLogChunkError,
    BuildPlatformFailure, BuildToolchainEvidence, MAX_BUILD_LOG_CHUNK_BYTES,
};
pub use routes::{RouteHostname, RouteHostnameError, RoutePort, RoutePortError, RouteTarget};
pub use text::{CancellationReason, FailureMessage, NonEmptyTextError};
