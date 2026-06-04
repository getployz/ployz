//! NATS-backed operation event and status adapters.

mod events;
mod keys;
mod projection;
mod repository;
mod status_store;

pub use events::{
    AsyncNatsOperationEventLog, OperationEventAppend, OperationEventLogError, StoredOperationEvent,
};
pub use keys::{deploy_submission_key, operation_status_key};
pub use repository::{
    AsyncNatsOperationRepository, DeployOperationSubmission, RecordDeployEvidenceError,
    RecordDeployTransitionError, SubmitDeployError,
};
pub use status_store::{
    AsyncNatsOperationStatusStore, KvRevision, OperationStatusStoreError, OperationStatusWrite,
    StoredDeploySubmission,
};

pub const PLZ_OPS_STREAM: &str = "PLZ_OPS";
pub const KV_OPS_BUCKET: &str = "KV_OPS";
const NATS_OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
