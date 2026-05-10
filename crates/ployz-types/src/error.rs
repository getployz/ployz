use thiserror::Error;

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum Error {
    #[error(transparent)]
    Deploy(#[from] DeployError),
    #[error(transparent)]
    Image(#[from] ImageError),
    #[error(transparent)]
    Build(#[from] BuildError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Storage(#[from] StorageError),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Coordination(#[from] CoordinationError),
    #[error(transparent)]
    Certificate(#[from] CertificateError),
    #[error("invite '{invite_id}' already exists")]
    InviteAlreadyExists { invite_id: String },
    #[error("invite '{invite_id}' not found")]
    InviteNotFound { invite_id: String },
    #[error("invite '{invite_id}' is revoked")]
    InviteRevoked { invite_id: String },
    #[error("invite '{invite_id}' is expired")]
    InviteExpired { invite_id: String },
    #[error("invite '{invite_id}' is already consumed")]
    InviteConsumed { invite_id: String },
    #[error("{stream} subscriber lagged by {skipped} events")]
    SubscriptionLagged {
        stream: SubscriptionStream,
        skipped: u64,
    },
    #[error("routing event '{event_id}' ack receiver closed")]
    RoutingEventAckReceiverClosed { event_id: String },
    #[error("{operation}: {message}")]
    Operation {
        operation: &'static str,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ImageError {
    #[error("invalid image digest '{digest}': {message}")]
    InvalidDigest { digest: String, message: String },
    #[error("image digest '{digest}' is missing on machine '{machine_id}'")]
    AvailabilityMissing { digest: String, machine_id: String },
    #[error("image digest mismatch: expected '{expected}', got '{actual}'")]
    DigestMismatch { expected: String, actual: String },
    #[error("image transfer '{operation_id}' failed: {message}")]
    TransferFailed {
        operation_id: String,
        message: String,
    },
    #[error("machine '{machine_id}' has insufficient disk for image '{digest}'")]
    InsufficientDisk { machine_id: String, digest: String },
    #[error("image target machine '{machine_id}' is unreachable")]
    TargetUnreachable { machine_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BuildError {
    #[error("unsupported build method '{method}'")]
    UnsupportedMethod { method: String },
    #[error("build '{operation_id}' failed during {stage}: {message}")]
    Failed {
        operation_id: String,
        stage: String,
        message: String,
    },
    #[error("build produced no image digest")]
    MissingDigest,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum DeployError {
    #[error("invalid deploy manifest: {message}")]
    ManifestInvalid { message: String },
    #[error("service '{service}' requested zero replicas")]
    ZeroReplicas { service: String },
    #[error("volume '{volume}' is bound to unavailable machine '{machine_id}'")]
    VolumeBoundToUnavailableMachine { volume: String, machine_id: String },
    #[error("no eligible machines are available for new deploy placement")]
    NoEligiblePlacementTargets,
    #[error("service '{service}' attaches volumes bound to different machines")]
    ServiceVolumesOnDifferentMachines { service: String },
    #[error("volume '{volume}' cannot change scope")]
    VolumeScopeChange { volume: String },
    #[error("volume '{volume}' cannot change mode after creation")]
    VolumeModeChange { volume: String },
    #[error("volume '{volume}' cannot change owner after creation")]
    VolumeOwnerChange { volume: String },
    #[error("volume '{volume}' has invalid {quota_kind} quota: {message}")]
    VolumeQuotaInvalid {
        volume: String,
        quota_kind: &'static str,
        message: String,
    },
    #[error("volume '{volume}' quota cannot shrink in v1")]
    VolumeQuotaShrink { volume: String },
    #[error("volume '{volume}' move requires an existing committed volume")]
    VolumeMoveMissingSource { volume: String },
    #[error(
        "volume '{volume}' move expected source machine '{expected_machine}' but current owner is '{actual_machine}'"
    )]
    VolumeMoveSourceMismatch {
        volume: String,
        expected_machine: String,
        actual_machine: String,
    },
    #[error("volume '{volume}' move requires scope=single")]
    VolumeMoveRequiresSingleScope { volume: String },
    #[error("volume '{volume}' move target machine '{machine_id}' is missing")]
    VolumeMoveTargetMissing { volume: String, machine_id: String },
    #[error("volume '{volume}' move target machine '{machine_id}' is not deployable")]
    VolumeMoveTargetIneligible { volume: String, machine_id: String },
    #[error("volume '{volume}' move target machine '{machine_id}' is not storage-capable")]
    VolumeMoveTargetNotStorageCapable { volume: String, machine_id: String },
    #[error("volume '{volume}' move execution is not supported yet")]
    VolumeMoveExecutionUnsupported { volume: String },
    #[error("volume '{volume}' clone source {source_namespace}/{source_volume} is missing")]
    VolumeCloneSourceMissing {
        volume: String,
        source_namespace: String,
        source_volume: String,
    },
    #[error("volume '{volume}' clone target already has a committed volume")]
    VolumeCloneTargetExists { volume: String },
    #[error("volume '{volume}' clone cannot reference the target volume itself")]
    VolumeCloneSourceIsTarget { volume: String },
    #[error("volume '{volume}' clone requires source and target scope=single")]
    VolumeCloneRequiresSingleScope { volume: String },
    #[error("volume '{volume}' clone source machine '{machine_id}' is missing")]
    VolumeCloneSourceMachineMissing { volume: String, machine_id: String },
    #[error("volume '{volume}' clone source machine '{machine_id}' is not deployable")]
    VolumeCloneSourceMachineIneligible { volume: String, machine_id: String },
    #[error("volume '{volume}' clone source machine '{machine_id}' is not storage-capable")]
    VolumeCloneSourceMachineNotStorageCapable { volume: String, machine_id: String },
    #[error("volume '{volume}' clone execution is not supported yet")]
    VolumeCloneExecutionUnsupported { volume: String },
    #[error("participant '{machine_id}' is missing from machine inventory")]
    ParticipantMissing { machine_id: String },
    #[error(
        "deploy blocked: {unreachable_count} of {participant_count} participants unreachable: {machine_ids:?}"
    )]
    ParticipantsUnreachable {
        unreachable_count: usize,
        participant_count: usize,
        machine_ids: Vec<String>,
    },
    #[error("participant set changed after lock acquisition; retry deploy")]
    ParticipantSetChanged,
    #[error("resolved execution plan changed after lock acquisition; retry deploy")]
    ExecutionPlanChanged,
    #[error(
        "hostname '{hostname}' is declared by both {first_namespace}/{first_service} and {second_namespace}/{second_service}"
    )]
    HostnameDeclaredByMultipleServices {
        hostname: String,
        first_namespace: String,
        first_service: String,
        second_namespace: String,
        second_service: String,
    },
    #[error(
        "hostname '{hostname}' is already owned by {owner_namespace}/{owner_service} and cannot be used by {request_namespace}/{request_service}"
    )]
    HostnameAlreadyOwned {
        hostname: String,
        owner_namespace: String,
        owner_service: String,
        request_namespace: String,
        request_service: String,
    },
    #[error(
        "missing active revision '{revision_hash}' for committed service {namespace}/{service}"
    )]
    CommittedServiceMissingActiveRevision {
        namespace: String,
        service: String,
        revision_hash: String,
    },
    #[error("could not decode committed service spec for {namespace}/{service}: {message}")]
    CommittedServiceSpecDecode {
        namespace: String,
        service: String,
        message: String,
    },
    #[error("branch source {namespace}/{service} has no committed release")]
    BranchSourceMissingRelease { namespace: String, service: String },
    #[error("branch source for {namespace}/{service} cannot reference the target service itself")]
    BranchSourceIsTarget { namespace: String, service: String },
    #[error("branch source {namespace}/{service} references missing revision '{revision_hash}'")]
    BranchSourceMissingRevision {
        namespace: String,
        service: String,
        revision_hash: String,
    },
    #[error("could not decode branch source spec for {namespace}/{service}: {message}")]
    BranchSourceSpecDecode {
        namespace: String,
        service: String,
        message: String,
    },
    #[error("empty machine start queue")]
    EmptyMachineStartQueue,
    #[error("volume '{volume}' changed but no attached service was restarted")]
    VolumeChangedWithoutRestart { volume: String },
    #[error("deploy node response missing {payload} payload")]
    MissingNodePayload { payload: &'static str },
    #[error("{operation} remote daemon error [{code}]: {message}")]
    RemoteNodeError {
        operation: &'static str,
        code: String,
        message: String,
    },
    #[error("{transition} deploy state transition rejected ({code}): {message}")]
    StateTransitionRejected {
        transition: &'static str,
        code: &'static str,
        message: String,
    },
    #[error("missing current slot for unchanged service '{service}' slot '{slot}'")]
    MissingCurrentSlot { service: String, slot: String },
    #[error("missing started instance for service '{service}' slot '{slot}'")]
    MissingStartedInstance { service: String, slot: String },
    #[error(
        "current release for service '{service}' referenced missing revision '{revision_hash}'"
    )]
    StoredReleaseMissingRevision {
        service: String,
        revision_hash: String,
    },
    #[error(
        "stored spec service '{stored_service}' did not match release service '{release_service}'"
    )]
    StoredSpecServiceMismatch {
        stored_service: String,
        release_service: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RuntimeError {
    #[error("missing required observed field {field}")]
    RequiredObservationMissing { field: String },
    #[error("malformed observed field {field}: {message}")]
    RequiredObservationMalformed { field: String, message: String },
    #[error("unknown required observed field {field}")]
    RequiredObservationUnknown { field: String },
    #[error("probe did not become ready before retries were exhausted")]
    ProbeRetriesExhausted,
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StorageError {
    #[error("overcommit_ratio must be a positive finite number, got {value}")]
    InvalidOvercommitRatio { value: String },
    #[error("dataset '{dataset}' has mountpoint '{actual}', expected '{expected}'")]
    DatasetMountpointMismatch {
        dataset: String,
        actual: String,
        expected: String,
    },
    #[error("dataset '{dataset}' does not exist")]
    DatasetMissing { dataset: String },
    #[error("dataset '{dataset}' has no parent")]
    DatasetMissingParent { dataset: String },
    #[error("stat output missing {field} in '{raw}'")]
    StatFieldMissing { field: &'static str, raw: String },
    #[error("ZFS parse error in {field}: {message}")]
    ZfsParse {
        field: &'static str,
        message: String,
    },
    #[error(
        "requested quota {requested} is below current used bytes {used} for dataset '{dataset}'"
    )]
    QuotaBelowUsed {
        dataset: String,
        requested: String,
        used: u64,
    },
    #[error(
        "quota for '{dataset}' would overcommit pool '{pool}': declared total {total} bytes exceeds budget {budget} bytes"
    )]
    QuotaWouldOvercommit {
        dataset: String,
        pool: String,
        total: u64,
        budget: u64,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum StoreError {
    #[error("{record} key {key} does not match payload key {payload_key}")]
    KeyMismatch {
        record: StoreRecordKind,
        key: String,
        payload_key: String,
    },
    #[error("deploy '{deploy_id}' commit persisted but routing publication failed: {message}")]
    DeployCommitRoutingPublishFailed { deploy_id: String, message: String },
    #[error("NATS watch '{watch}' failed: {message}")]
    WatchFailed {
        watch: &'static str,
        message: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CoordinationError {
    #[error("lock '{key}' is already held")]
    LockAlreadyHeld { key: String },
    #[error("lock '{key}' contention: {message}")]
    LockContention { key: String, message: String },
    #[error("lock '{key}' changed before renewal completed")]
    LockChangedBeforeRenewal { key: String },
    #[error("lock '{key}' is held by another lease; refusing stale release")]
    StaleLockRelease { key: String },
    #[error("certificate renewal subject '{subject}' did not match hostname '{hostname}'")]
    CertRenewalSubjectMismatch { subject: String, hostname: String },
    #[error("invalid certificate renewal schedule timestamp {timestamp}: {message}")]
    CertRenewalInvalidSchedule { timestamp: String, message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum CertificateError {
    #[error("no ACME issuer is configured for this orchestrator")]
    AcmeDisabled,
    #[error("no HTTP-01 challenge found for {hostname}")]
    AcmeHttp01ChallengeMissing { hostname: String },
    #[error("authorization for {hostname} reached unexpected status {status}")]
    AcmeAuthorizationUnexpectedStatus { hostname: String, status: String },
    #[error("order for {hostname} reached unexpected status {status}")]
    AcmeOrderUnexpectedStatus { hostname: String, status: String },
    #[error("ACME order URL {order_url} does not share an origin with directory {directory_url}")]
    AcmeOrderUrlOriginMismatch {
        directory_url: String,
        order_url: String,
    },
    #[error("ACME account creation deferred for {issuer_url}: {message}")]
    AcmeAccountCreationDeferred { issuer_url: String, message: String },
    #[error("could not acquire ACME account lock for {issuer_url}: {message}")]
    AcmeAccountLockAcquireFailed { issuer_url: String, message: String },
    #[error("could not acquire certificate lock for {hostname} during {phase}: {message}")]
    CertificateLockAcquireFailed {
        phase: &'static str,
        hostname: String,
        message: String,
    },
    #[error("HTTP-01 challenge for {hostname} token {token} was not visible in local store")]
    Http01LocalChallengeNotVisible { hostname: String, token: String },
    #[error("hostname {hostname} is not in the current advertised routing state")]
    Http01HostnameNotAdvertised { hostname: String },
    #[error("no active advertised gateway is eligible for HTTP-01 challenge {hostname}")]
    Http01NoEligibleGateway {
        hostname: String,
        excluded: Vec<String>,
    },
    #[error("HTTP-01 challenge for {hostname} token {token} is missing readiness observations")]
    Http01MissingReadiness {
        hostname: String,
        token: String,
        missing_machine_ids: Vec<String>,
        excluded: Vec<String>,
    },
    #[error("invalid service revision {namespace}/{service}@{revision_hash}: {message}")]
    Http01InvalidServiceRevision {
        namespace: String,
        service: String,
        revision_hash: String,
        message: String,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoreRecordKind {
    AcmeAccount,
    AcmeChallenge,
    AcmeChallengeReadiness,
    Certificate,
    DeployPhase,
    DeployStatus,
    ImageAvailability,
    Instance,
    Invite,
    Machine,
}

impl std::fmt::Display for StoreRecordKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::AcmeAccount => "ACME account",
            Self::AcmeChallenge => "ACME challenge",
            Self::AcmeChallengeReadiness => "ACME readiness",
            Self::Certificate => "certificate",
            Self::DeployPhase => "deploy phase",
            Self::DeployStatus => "deploy status",
            Self::ImageAvailability => "image availability",
            Self::Instance => "instance",
            Self::Invite => "invite",
            Self::Machine => "machine",
        };
        f.write_str(name)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubscriptionStream {
    Machine,
    Routing,
    Certificate,
    AcmeChallenge,
}

impl std::fmt::Display for SubscriptionStream {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let name = match self {
            Self::Machine => "machine",
            Self::Routing => "routing",
            Self::Certificate => "certificate",
            Self::AcmeChallenge => "acme challenge",
        };
        f.write_str(name)
    }
}

impl Error {
    #[must_use]
    pub fn operation(operation: &'static str, message: impl Into<String>) -> Self {
        Self::Operation {
            operation,
            message: message.into(),
        }
    }

    #[must_use]
    pub fn invite_already_exists(invite_id: impl Into<String>) -> Self {
        Self::InviteAlreadyExists {
            invite_id: invite_id.into(),
        }
    }

    #[must_use]
    pub fn invite_not_found(invite_id: impl Into<String>) -> Self {
        Self::InviteNotFound {
            invite_id: invite_id.into(),
        }
    }

    #[must_use]
    pub fn invite_revoked(invite_id: impl Into<String>) -> Self {
        Self::InviteRevoked {
            invite_id: invite_id.into(),
        }
    }

    #[must_use]
    pub fn invite_expired(invite_id: impl Into<String>) -> Self {
        Self::InviteExpired {
            invite_id: invite_id.into(),
        }
    }

    #[must_use]
    pub fn invite_consumed(invite_id: impl Into<String>) -> Self {
        Self::InviteConsumed {
            invite_id: invite_id.into(),
        }
    }

    #[must_use]
    pub fn subscription_lagged(stream: SubscriptionStream, skipped: u64) -> Self {
        Self::SubscriptionLagged { stream, skipped }
    }

    #[must_use]
    pub fn routing_event_ack_receiver_closed(event_id: impl Into<String>) -> Self {
        Self::RoutingEventAckReceiverClosed {
            event_id: event_id.into(),
        }
    }

    #[must_use]
    pub fn store_key_mismatch(
        record: StoreRecordKind,
        key: impl Into<String>,
        payload_key: impl Into<String>,
    ) -> Self {
        Self::Store(StoreError::KeyMismatch {
            record,
            key: key.into(),
            payload_key: payload_key.into(),
        })
    }
}
