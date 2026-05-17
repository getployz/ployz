mod actor;
mod error;
mod model;

pub use actor::ServingActorHandle;
pub use error::{ServingError, ServingResult};
pub use model::{
    ServingFailure, ServingFailureKind, ServingFreshness, ServingRevisions, ServingSnapshotBatch,
    ServingSnapshotKind, ServingSnapshotPaths, ServingStatus,
};

#[cfg(test)]
mod tests;
