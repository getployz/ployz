mod actor;
mod error;
mod model;
mod wire;

pub use actor::ServingActorHandle;
pub use error::{ServingError, ServingResult};
pub use model::{
    ServingFailure, ServingFailureKind, ServingFreshness, ServingRevisions, ServingSnapshotBatch,
    ServingSnapshotKind, ServingSnapshotPaths, ServingStatus,
};
pub use wire::{WireRoleMetrics, WireRoleStatus, WireServingState};

#[cfg(test)]
mod tests;
