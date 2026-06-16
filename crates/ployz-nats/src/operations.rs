//! NATS-backed operation event and status adapters.

mod events;
mod keys;
mod repository;
mod status_store;

pub use events::{
    AsyncNatsOperationEventLog, OperationEventAppend, OperationEventLogError,
    OperationEventReplayReadError, StoredOperationEvent,
};
pub use keys::{
    backup_submission_key, cert_submission_key, deploy_submission_key, machine_add_join_token_key,
    machine_add_submission_key, operation_owner_lease_key, operation_status_key,
};
pub use ployz_core::ops::{
    OperationEventReplayCursor, OperationEventReplayLimit, OperationEventReplayLimitError,
    OperationEventReplayPage, OperationEventReplayRequest, ReplayedOperationEvent,
};
pub use repository::{
    AcceptedBackupSubmission, AcceptedCertSubmission, AcceptedDeploySubmission,
    AcceptedMachineAddSubmission, AsyncNatsOperationRepository, BackupOperationSubmission,
    CertOperationSubmission, DeployOperationSubmission, MachineAddOperationSubmission,
    MachineJoinRedemption, OperationLeaseClaim, OperationLeaseClaimError, RecordBackupEventError,
    RecordCertEventError, RecordDeployEvidenceError, RecordDeployTransitionError,
    RecordLifecycleEventError, RecordMachineAddEventError, RecordMachineJoinReportError,
    RecordOperationEventError, RecordedMachineJoinReport, RedeemMachineJoinTokenError,
    RedeemedMachineJoin, ReplayOperationEventsError, StoredEventMismatchKind,
    SubmitMachineAddError, SubmitOperationError,
};
pub use status_store::{
    AsyncNatsOperationStatusStore, KvRevision, OperationStatusReadError, OperationStatusStoreError,
    OperationStatusWrite, StoredBackupSubmission, StoredCertSubmission, StoredDeploySubmission,
    StoredMachineAddJoinToken, StoredMachineAddMintClaim, StoredMachineAddSecretDelivery,
    StoredMachineAddSubmission, StoredOperationSubmission,
};

pub const PLZ_OPS_STREAM: &str = "PLZ_OPS";
pub use ployz_core::state::KV_OPS_BUCKET;
