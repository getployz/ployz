//! NATS-backed operation event and status adapters.

mod events;
mod keys;
mod projection;
mod repository;
mod status_store;

pub use events::{
    AsyncNatsOperationEventLog, OperationEventAppend, OperationEventLogError,
    OperationEventReplayReadError, StoredOperationEvent,
};
pub use keys::{
    cert_submission_key, deploy_submission_key, machine_add_submission_key,
    operation_owner_lease_key, operation_status_key,
};
pub use ployz_core::ops::{
    OperationEventReplayCursor, OperationEventReplayLimit, OperationEventReplayLimitError,
    OperationEventReplayPage, OperationEventReplayRequest, ReplayedOperationEvent,
};
pub use repository::{
    AcceptedCertSubmission, AcceptedDeploySubmission, AcceptedMachineAddSubmission,
    AsyncNatsOperationRepository, CertOperationSubmission, DeployOperationSubmission,
    MachineAddOperationSubmission, OperationLeaseClaim, OperationLeaseClaimError,
    RecordCertEventError, RecordDeployEvidenceError, RecordDeployTransitionError,
    RecordLifecycleEventError, RecordMachineAddEventError, ReplayOperationEventsError,
    SubmitCertError, SubmitDeployError, SubmitMachineAddError,
};
pub use status_store::{
    AsyncNatsOperationStatusStore, KvRevision, OperationStatusReadError, OperationStatusStoreError,
    OperationStatusWrite, StoredCertSubmission, StoredDeploySubmission, StoredMachineAddSubmission,
};

pub const PLZ_OPS_STREAM: &str = "PLZ_OPS";
pub const KV_OPS_BUCKET: &str = "KV_OPS";
const NATS_OPERATION_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);
