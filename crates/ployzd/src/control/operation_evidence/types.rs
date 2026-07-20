use ployz_core::build::{BuildAdapter, BuildPlatforms, BuildTarget, GitSourceEvidence};
use ployz_core::deploy::{DeployReservationId, VolumeName};
use ployz_core::ids::{CertId, MachineId, NamespaceId, OperationId, ServiceId};
use ployz_core::ingress::IngressConfiguration;
use ployz_core::install::MachineJoinRuntimeNatsUrl;
use ployz_core::install::{
    HostPortAssurance, InstallArtifactVersion, MachineJoinBundle, MachineJoinSecretDelivery,
};
use ployz_core::machine::MachineLifecycle;
use ployz_core::machine::{
    IssuedJoinToken, JoinTokenRedeemedAt, MachineAddFailure, MachineName, RawJoinToken,
};
use ployz_core::network::MachineEndpointSubnet;
use ployz_core::operation::{
    CredentialGrantAction, EventSequence, MachineAddOperationStateName, ManagedDnsReconcileSubject,
    OperationIdempotencyKey, OperationStatus, StatusProjectionError,
};
use ployz_core::roles::InstallRolePolicy;
use serde::Serialize;

pub(super) struct SubmittedOperation<P> {
    pub(super) operation_id: OperationId,
    pub(super) start_sequence: EventSequence,
    pub(super) payload: P,
    pub(super) should_start_execution: bool,
}

/// The machine-join credential set that travels together through every stage
/// of a machine-add: submission, claim, acceptance. Named once so a new
/// credential field is added in one place, not copied across every struct.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineJoinIdentity {
    pub machine_id: MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    #[serde(default = "HostPortAssurance::keeper")]
    pub host_port_assurance: HostPortAssurance,
    pub endpoint_subnet: MachineEndpointSubnet,
    pub join_bundle: MachineJoinBundle,
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredDeployClaim {
    pub operation_id: OperationId,
    pub reservation_id: DeployReservationId,
    pub target: ployz_core::deploy::DeployRequestEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct DeployOperationPayload {
    pub(super) reservation_id: Option<DeployReservationId>,
    pub(super) target: ployz_core::deploy::DeployRequestEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CertOperationPayload {
    pub(super) cert_id: CertId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredMachineAddSubmission {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub start_sequence: EventSequence,
    pub identity: MachineJoinIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredMachineAddClaim {
    pub operation_id: OperationId,
    pub identity: MachineJoinIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredMachineAddSecretDelivery {
    pub operation_id: OperationId,
    pub secret_delivery: MachineJoinSecretDelivery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredMachineAddMintClaim {
    pub operation_id: OperationId,
    pub nkey_public: ployz_core::nats_config::NatsUserPublicKey,
    pub nkey_seed: ployz_core::nats_config::NatsUserSeed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StoredMachineAddJoinToken {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployOperationSubmission {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
    pub reservation_id: DeployReservationId,
    pub target: ployz_core::deploy::DeployRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuildOperationSubmission {
    pub operation_id: OperationId,
    pub target: BuildTarget,
    pub source: GitSourceEvidence,
    pub adapter: BuildAdapter,
    pub platforms: BuildPlatforms,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct BuildOperationPayload {
    pub(super) target: BuildTarget,
    pub(super) source: GitSourceEvidence,
    pub(super) adapter: BuildAdapter,
    pub(super) platforms: BuildPlatforms,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertOperationSubmission {
    pub operation_id: OperationId,
    pub cert_id: CertId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddOperationSubmission {
    pub operation_id: OperationId,
    pub identity: MachineJoinIdentity,
    pub idempotency_key: OperationIdempotencyKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineUpdateOperationSubmission {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub target_version: InstallArtifactVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MachineUpdatePayload {
    pub(super) machine_id: MachineId,
    pub(super) target_version: InstallArtifactVersion,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineStoragePrepareOperationSubmission {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub requested_pool: Option<ployz_core::deploy::ZfsPoolName>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineBuildCachePruneOperationSubmission {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MachineBuildCachePrunePayload {
    pub(super) machine_id: MachineId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MachineStoragePreparePayload {
    pub(super) machine_id: MachineId,
    pub(super) requested_pool: Option<ployz_core::deploy::ZfsPoolName>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineLifecycleOperationSubmission {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub target: MachineLifecycle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MachineLifecyclePayload {
    pub(super) machine_id: MachineId,
    pub(super) target: MachineLifecycle,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoreReplaceOperationSubmission {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub successor_nats_url: MachineJoinRuntimeNatsUrl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct CoreReplacePayload {
    pub(super) machine_id: MachineId,
    pub(super) successor_nats_url: MachineJoinRuntimeNatsUrl,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServiceRestartOperationSubmission {
    pub operation_id: OperationId,
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CredentialGrantOperationSubmission {
    pub operation_id: OperationId,
    pub action: CredentialGrantAction,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IngressConfigureOperationSubmission {
    pub operation_id: OperationId,
    pub configuration: IngressConfiguration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ServiceRestartPayload {
    pub(super) namespace_id: NamespaceId,
    pub(super) service_id: ServiceId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamespaceRemoveOperationSubmission {
    pub operation_id: OperationId,
    pub namespace_id: NamespaceId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NetworkRepairOperationSubmission {
    pub operation_id: OperationId,
    pub target_machine_id: Option<MachineId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NetworkRepairPayload {
    pub(super) target_machine_id: Option<MachineId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedDnsReconcileOperationSubmission {
    pub operation_id: OperationId,
    pub subject: ManagedDnsReconcileSubject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ManagedDnsReconcilePayload {
    pub(super) subject: ManagedDnsReconcileSubject,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct NamespaceRemovePayload {
    pub(super) namespace_id: NamespaceId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeRemoveOperationSubmission {
    pub operation_id: OperationId,
    pub namespace_id: NamespaceId,
    pub volume_name: VolumeName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VolumeCreateOperationSubmission {
    pub request: ployz_core::operation::VolumeCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VolumeCreatePayload {
    pub(super) request: ployz_core::operation::VolumeCreateRequest,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct VolumeRemovePayload {
    pub(super) namespace_id: NamespaceId,
    pub(super) volume_name: VolumeName,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedDeploySubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub target: ployz_core::deploy::DeployRequest,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedBuildSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub target: BuildTarget,
    pub source: GitSourceEvidence,
    pub adapter: BuildAdapter,
    pub platforms: BuildPlatforms,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedMachineAddSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub identity: MachineJoinIdentity,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedMachineUpdateSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub machine_id: MachineId,
    pub target_version: InstallArtifactVersion,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedMachineStoragePrepareSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub machine_id: MachineId,
    pub requested_pool: Option<ployz_core::deploy::ZfsPoolName>,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedMachineBuildCachePruneSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub machine_id: MachineId,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedMachineLifecycleSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub machine_id: MachineId,
    pub target: MachineLifecycle,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedCoreReplaceSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub machine_id: MachineId,
    pub successor_nats_url: MachineJoinRuntimeNatsUrl,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedServiceRestartSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub namespace_id: NamespaceId,
    pub service_id: ServiceId,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedCredentialGrantSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub action: CredentialGrantAction,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedIngressConfigureSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub configuration: IngressConfiguration,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedNamespaceRemoveSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub namespace_id: NamespaceId,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedNetworkRepairSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub target_machine_id: Option<MachineId>,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedVolumeRemoveSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub namespace_id: NamespaceId,
    pub volume_name: VolumeName,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedVolumeCreateSubmission {
    pub request: ployz_core::operation::VolumeCreateRequest,
    pub start_sequence: EventSequence,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedManagedDnsReconcileSubmission {
    pub operation_id: OperationId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatusWrite {
    Stored,
    AlreadySatisfied { current_sequence: EventSequence },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InterruptedOperationsSummary {
    pub recorded: usize,
}

pub(super) enum RecordOperationEventOutcome {
    AlreadySatisfied {
        current_sequence: EventSequence,
        status: OperationStatus,
    },
    Stored {
        sequence: EventSequence,
        status: OperationStatus,
    },
}

impl RecordOperationEventOutcome {
    pub(super) fn into_status_write(self) -> OperationStatusWrite {
        match self {
            Self::AlreadySatisfied {
                current_sequence, ..
            } => OperationStatusWrite::AlreadySatisfied { current_sequence },
            Self::Stored { .. } => OperationStatusWrite::Stored,
        }
    }

    pub(super) fn sequence(self) -> EventSequence {
        match self {
            Self::AlreadySatisfied {
                current_sequence, ..
            } => current_sequence,
            Self::Stored { sequence, .. } => sequence,
        }
    }

    pub(super) fn status(&self) -> &OperationStatus {
        match self {
            Self::AlreadySatisfied { status, .. } | Self::Stored { status, .. } => status,
        }
    }
}

#[derive(Debug)]
pub enum SubmitOperationError {
    StaleDeployReservation {
        namespace_id: NamespaceId,
        reservation_id: DeployReservationId,
        last_committed_reservation_id: DeployReservationId,
    },
    DeployReservationAlreadyCommitted {
        namespace_id: NamespaceId,
        reservation_id: DeployReservationId,
        owner_operation_id: OperationId,
    },
    StoreStatus(OperationStatusStoreError),
    DuplicateSequenceMismatch {
        sequence: EventSequence,
    },
}

#[derive(Debug)]
pub enum SubmitMachineAddError {
    Operation(SubmitOperationError),
    JoinTokenMismatch,
    DuplicateIdempotencyKey,
}

#[derive(Debug, thiserror::Error)]
pub enum RecordOperationEventError {
    #[error("{0}")]
    StoreStatus(OperationStatusStoreError),
    #[error("operation record corrupt: missing operation {}", .operation_id.as_str())]
    MissingOperation { operation_id: OperationId },
    #[error("operation record corrupt: {0}")]
    InvalidNextSequence(ployz_core::operation::NextEventSequenceError),
    #[error("operation status projection failed: {0}")]
    ProjectStatus(StatusProjectionError),
}

pub type RecordDeployTransitionError = RecordOperationEventError;
pub type RecordBuildTransitionError = RecordOperationEventError;
pub type RecordBuildEvidenceError = RecordOperationEventError;
pub type RecordCertTransitionError = RecordOperationEventError;
pub type RecordDeployEvidenceError = RecordOperationEventError;
pub type RecordServiceRestartTransitionError = RecordOperationEventError;
pub type RecordNamespaceRemoveTransitionError = RecordOperationEventError;
pub type RecordNetworkRepairTransitionError = RecordOperationEventError;
pub type RecordNetworkRepairEvidenceError = RecordOperationEventError;
pub type RecordVolumeRemoveTransitionError = RecordOperationEventError;
pub type RecordManagedDnsReconcileTransitionError = RecordOperationEventError;
pub type RecordIngressConfigureTransitionError = RecordOperationEventError;
pub type RecordLifecycleEventError = RecordOperationEventError;
pub type RecordMachineAddEventError = RecordLifecycleEventError;

#[derive(Debug, thiserror::Error)]
pub enum OperationStatusStoreError {
    #[error("operation working-record conflict: {message}")]
    CasConflict { message: String },
    #[error("operation working records are corrupt: {message}")]
    CorruptRecord { message: String },
    #[error("operation working records: {message}")]
    Index { message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum StageMachineDataplaneError {
    #[error("machine-add operation {} is not joining", .operation_id.as_str())]
    OperationNotJoining { operation_id: OperationId },
    #[error("machine-add operation {} belongs to a different machine", .operation_id.as_str())]
    MachineMismatch { operation_id: OperationId },
    #[error("machine {} is already active", .machine_id.as_str())]
    MachineAlreadyActive { machine_id: MachineId },
    #[error("another machine-add operation owns dataplane staging")]
    StagingOccupied,
    #[error("machine dataplane staging: {0}")]
    Store(OperationStatusStoreError),
}

#[derive(Debug, thiserror::Error)]
pub enum OperationEventLogError {
    #[error("decode operation event: {0}")]
    DecodeEvent(serde_json::Error),
    #[error("read operation event: {message}")]
    ReadEvent { message: String },
    #[error("operation event sequence {sequence} is invalid: {error}")]
    InvalidEventSequence {
        sequence: u64,
        error: ployz_core::operation::EventSequenceError,
    },
    #[error("operation event recorded-at Unix milliseconds {value} is invalid: {error}")]
    InvalidRecordedAtUnixMs {
        value: u64,
        error: ployz_core::operation::OperationEventRecordedAtUnixMsError,
    },
    #[error("operation replay next sequence {sequence} is invalid")]
    InvalidNextReplaySequence { sequence: u64 },
}

#[derive(Debug)]
pub enum ReplayOperationEventsError {
    ReadEvents(OperationEventLogError),
    MissingOperation { operation_id: OperationId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineJoinRedemption {
    Joined(RedeemedMachineJoin),
    AlreadyJoined(RedeemedMachineJoin),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RedeemedMachineJoin {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    pub host_port_assurance: HostPortAssurance,
    pub endpoint_subnet: MachineEndpointSubnet,
    pub join_bundle: MachineJoinBundle,
    pub secret_delivery: MachineJoinSecretDelivery,
    pub joined_at: JoinTokenRedeemedAt,
    pub last_event_sequence: EventSequence,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedMachineJoinReport {
    pub operation_id: OperationId,
    pub machine_id: MachineId,
    pub status_write: OperationStatusWrite,
}

pub(crate) struct MachineJoinReportTarget {
    pub(crate) operation_id: OperationId,
    pub(crate) machine_id: MachineId,
    pub(crate) endpoint_subnet: MachineEndpointSubnet,
}

#[derive(Debug)]
pub enum RedeemMachineJoinTokenError {
    Clock {
        message: String,
    },
    InvalidJoinToken,
    UnknownJoinToken,
    StoreStatus(OperationStatusStoreError),
    RecordMachineAddEvent(RecordMachineAddEventError),
    MissingOperation {
        operation_id: OperationId,
    },
    MissingSecretDelivery {
        operation_id: OperationId,
    },
    WrongOperationKind {
        operation_id: OperationId,
    },
    OperationNotPending {
        operation_id: OperationId,
        current: MachineAddOperationStateName,
    },
    JoinRejected {
        operation_id: OperationId,
        failure: MachineAddFailure,
    },
}

#[derive(Debug)]
pub enum RecordMachineJoinReportError {
    UnknownJoinToken,
    StoreStatus(OperationStatusStoreError),
    RecordMachineAddEvent(RecordMachineAddEventError),
}
