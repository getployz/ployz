//! NATS-backed operation event and status adapters.

mod events;
mod keys;
mod repository;
mod status_store;

pub use events::{
    AsyncNatsOperationEventLog, OperationEventAppend, OperationEventLogError,
    OperationEventReplayReadError, StoredOperationEvent,
};
pub use keys::{machine_add_join_token_key, machine_add_submission_key, operation_status_key};
pub use ployz_core::ops::{
    OperationEventReplayCursor, OperationEventReplayLimit, OperationEventReplayLimitError,
    OperationEventReplayPage, OperationEventReplayRequest, ReplayedOperationEvent,
};
pub use repository::{
    AcceptedCertSubmission, AcceptedDeploySubmission, AcceptedMachineAddSubmission,
    AcceptedMachineUpdateSubmission, AsyncNatsOperationRepository, CertOperationSubmission,
    DeployOperationSubmission, MachineAddOperationSubmission, MachineJoinRedemption,
    MachineUpdateOperationSubmission, OperationStatusWrite, RecordCertEventError,
    RecordDeployEvidenceError, RecordDeployTransitionError, RecordLifecycleEventError,
    RecordMachineAddEventError, RecordMachineJoinReportError, RecordOperationEventError,
    RecordedMachineJoinReport, RedeemMachineJoinTokenError, RedeemedMachineJoin,
    ReplayOperationEventsError, StoredEventMismatchKind, SubmitMachineAddError,
    SubmitOperationError,
};
pub use status_store::{
    AsyncNatsOperationStatusStore, KvRevision, OperationStatusReadError, OperationStatusStoreError,
    StoredMachineAddClaim, StoredMachineAddJoinToken, StoredMachineAddMintClaim,
    StoredMachineAddSecretDelivery, StoredMachineAddSubmission,
};

pub const PLZ_OPS_STREAM: &str = "PLZ_OPS";
pub use ployz_core::state::KV_OPS_BUCKET;
