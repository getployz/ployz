use ployz_core::ids::{CertId, NodeId, OperationId, OperationOwnerId};
use ployz_core::install::MachineJoinBundle;
use ployz_core::machine::{IssuedJoinToken, MachineName, RawJoinToken};
use ployz_core::ops::{
    EventSequence, OperationEvent, OperationIdempotencyKey, OperationLeaseExpiresAt,
    OperationOwnerLease, OperationStatus,
};
use ployz_core::roles::InstallRolePolicy;

use super::AsyncNatsOperationRepository;
use crate::operations::events::{OperationEventAppend, OperationEventLogError};
use crate::operations::status_store::{
    AsyncNatsOperationStatusStore, OperationStatusStoreError, StoredMachineAddJoinToken,
    StoredMachineAddSubmission, StoredOperationSubmission,
};

/// Per-kind adapter for the shared idempotent submit flow: where the
/// submission record lives, how to build the submitted event, recognize
/// it on duplicate re-read, and build the accepted status.
trait SubmitKind: Sized {
    type Payload: Clone;

    async fn submission(
        store: &AsyncNatsOperationStatusStore,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredOperationSubmission>, OperationStatusStoreError>;

    async fn put_submission_if_absent(
        store: &AsyncNatsOperationStatusStore,
        idempotency_key: &OperationIdempotencyKey,
        submission: &StoredOperationSubmission,
    ) -> Result<StoredOperationSubmission, OperationStatusStoreError>;

    fn submitted_event(
        operation_id: OperationId,
        payload: Self::Payload,
        idempotency_key: &OperationIdempotencyKey,
    ) -> OperationEventAppend;

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)>;

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus;

    /// The payload as recorded by the winning submitted event. Kinds whose
    /// payload is carried on the event re-read it; payload-free kinds skip
    /// the read.
    async fn stored_payload(
        repository: &AsyncNatsOperationRepository,
        expected_operation_id: &OperationId,
        sequence: EventSequence,
    ) -> Result<Self::Payload, SubmitOperationError> {
        repository
            .submitted_event_payload::<Self>(expected_operation_id, sequence)
            .await
    }
}

struct SubmittedOperation<P> {
    operation_id: OperationId,
    start_sequence: EventSequence,
    payload: P,
    lease: OperationOwnerLease,
    should_start_execution: bool,
}

impl SubmitKind for DeployOperationSubmission {
    type Payload = ployz_core::deploy::DeployRequest;

    async fn submission(
        store: &AsyncNatsOperationStatusStore,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredOperationSubmission>, OperationStatusStoreError> {
        store.deploy_submission(idempotency_key).await
    }

    async fn put_submission_if_absent(
        store: &AsyncNatsOperationStatusStore,
        idempotency_key: &OperationIdempotencyKey,
        submission: &StoredOperationSubmission,
    ) -> Result<StoredOperationSubmission, OperationStatusStoreError> {
        store
            .put_deploy_submission_if_absent(idempotency_key, submission)
            .await
    }

    fn submitted_event(
        operation_id: OperationId,
        payload: Self::Payload,
        idempotency_key: &OperationIdempotencyKey,
    ) -> OperationEventAppend {
        OperationEventAppend::deploy_submitted(operation_id, payload, idempotency_key)
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::DeploySubmitted {
            operation_id,
            target,
        } = event
        else {
            return None;
        };
        Some((operation_id, target))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::deploy_accepted(operation_id, payload.service_id.clone(), sequence)
    }
}

impl SubmitKind for CertOperationSubmission {
    type Payload = CertId;

    async fn submission(
        store: &AsyncNatsOperationStatusStore,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredOperationSubmission>, OperationStatusStoreError> {
        store.cert_submission(idempotency_key).await
    }

    async fn put_submission_if_absent(
        store: &AsyncNatsOperationStatusStore,
        idempotency_key: &OperationIdempotencyKey,
        submission: &StoredOperationSubmission,
    ) -> Result<StoredOperationSubmission, OperationStatusStoreError> {
        store
            .put_cert_submission_if_absent(idempotency_key, submission)
            .await
    }

    fn submitted_event(
        operation_id: OperationId,
        payload: Self::Payload,
        idempotency_key: &OperationIdempotencyKey,
    ) -> OperationEventAppend {
        OperationEventAppend::cert_submitted(operation_id, payload, idempotency_key)
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::CertRenewalSubmitted {
            operation_id,
            cert_id,
        } = event
        else {
            return None;
        };
        Some((operation_id, cert_id))
    }

    fn accepted_status(
        operation_id: OperationId,
        payload: &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::cert_accepted(operation_id, payload.clone(), sequence)
    }
}

impl SubmitKind for BackupOperationSubmission {
    type Payload = ();

    async fn submission(
        store: &AsyncNatsOperationStatusStore,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredOperationSubmission>, OperationStatusStoreError> {
        store.backup_submission(idempotency_key).await
    }

    async fn put_submission_if_absent(
        store: &AsyncNatsOperationStatusStore,
        idempotency_key: &OperationIdempotencyKey,
        submission: &StoredOperationSubmission,
    ) -> Result<StoredOperationSubmission, OperationStatusStoreError> {
        store
            .put_backup_submission_if_absent(idempotency_key, submission)
            .await
    }

    fn submitted_event(
        operation_id: OperationId,
        (): Self::Payload,
        idempotency_key: &OperationIdempotencyKey,
    ) -> OperationEventAppend {
        OperationEventAppend::backup_submitted(operation_id, idempotency_key)
    }

    fn submitted_event_parts(event: OperationEvent) -> Option<(OperationId, Self::Payload)> {
        let OperationEvent::BackupCreateSubmitted { operation_id } = event else {
            return None;
        };
        Some((operation_id, ()))
    }

    fn accepted_status(
        operation_id: OperationId,
        (): &Self::Payload,
        sequence: EventSequence,
    ) -> OperationStatus {
        OperationStatus::backup_accepted(operation_id, sequence)
    }

    /// Backup submissions carry no payload beyond the operation id, so
    /// there is nothing to re-read from the stored event.
    async fn stored_payload(
        _repository: &AsyncNatsOperationRepository,
        _expected_operation_id: &OperationId,
        _sequence: EventSequence,
    ) -> Result<Self::Payload, SubmitOperationError> {
        Ok(())
    }
}

impl AsyncNatsOperationRepository {
    pub async fn submit_deploy(
        &self,
        submission: DeployOperationSubmission,
        owner: OperationLeaseClaim,
    ) -> Result<AcceptedDeploySubmission, SubmitOperationError> {
        let DeployOperationSubmission {
            operation_id,
            target,
            idempotency_key,
        } = submission;
        let submitted = self
            .submit_operation::<DeployOperationSubmission>(
                operation_id,
                target,
                idempotency_key,
                owner,
            )
            .await?;

        Ok(AcceptedDeploySubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            target: submitted.payload,
            lease: submitted.lease,
            should_start_execution: submitted.should_start_execution,
        })
    }

    pub async fn submit_cert(
        &self,
        submission: CertOperationSubmission,
        owner: OperationLeaseClaim,
    ) -> Result<AcceptedCertSubmission, SubmitOperationError> {
        let CertOperationSubmission {
            operation_id,
            cert_id,
            idempotency_key,
        } = submission;
        let submitted = self
            .submit_operation::<CertOperationSubmission>(
                operation_id,
                cert_id,
                idempotency_key,
                owner,
            )
            .await?;

        Ok(AcceptedCertSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            cert_id: submitted.payload,
            lease: submitted.lease,
        })
    }

    pub async fn submit_backup(
        &self,
        submission: BackupOperationSubmission,
        owner: OperationLeaseClaim,
    ) -> Result<AcceptedBackupSubmission, SubmitOperationError> {
        let BackupOperationSubmission {
            operation_id,
            idempotency_key,
        } = submission;
        let submitted = self
            .submit_operation::<BackupOperationSubmission>(operation_id, (), idempotency_key, owner)
            .await?;

        Ok(AcceptedBackupSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            lease: submitted.lease,
            should_start_execution: submitted.should_start_execution,
        })
    }

    /// The shared idempotent-accept flow: adopt an existing submission
    /// record, otherwise append the submitted event (adopting a duplicate),
    /// project the accepted status, store the submission record, and claim
    /// the owner lease.
    async fn submit_operation<K: SubmitKind>(
        &self,
        operation_id: OperationId,
        payload: K::Payload,
        idempotency_key: OperationIdempotencyKey,
        owner: OperationLeaseClaim,
    ) -> Result<SubmittedOperation<K::Payload>, SubmitOperationError> {
        if let Some(existing) = K::submission(&self.status_store, &idempotency_key)
            .await
            .map_err(SubmitOperationError::StoreStatus)?
        {
            let payload =
                K::stored_payload(self, &existing.operation_id, existing.start_sequence).await?;
            let lease = self
                .claim_submit_lease(&existing.operation_id, &owner)
                .await?;
            return Ok(SubmittedOperation {
                operation_id: existing.operation_id,
                start_sequence: existing.start_sequence,
                payload,
                lease,
                should_start_execution: false,
            });
        }

        let stored = self
            .event_log
            .append(K::submitted_event(
                operation_id.clone(),
                payload.clone(),
                &idempotency_key,
            ))
            .await
            .map_err(SubmitOperationError::AppendEvent)?;
        let (operation_id, payload) = if stored.duplicate {
            let original = self
                .event_log
                .event_at_sequence(stored.sequence)
                .await
                .map_err(SubmitOperationError::AppendEvent)?;
            K::submitted_event_parts(original).ok_or(
                SubmitOperationError::DuplicateSequenceMismatch {
                    sequence: stored.sequence,
                },
            )?
        } else {
            (operation_id, payload)
        };
        let status = K::accepted_status(operation_id.clone(), &payload, stored.sequence);
        self.status_store
            .put_if_newer(&status)
            .await
            .map_err(SubmitOperationError::StoreStatus)?;
        let candidate = StoredOperationSubmission {
            operation_id: operation_id.clone(),
            start_sequence: stored.sequence,
        };

        let submitted =
            K::put_submission_if_absent(&self.status_store, &idempotency_key, &candidate)
                .await
                .map_err(SubmitOperationError::StoreStatus)?;
        let payload =
            K::stored_payload(self, &submitted.operation_id, submitted.start_sequence).await?;
        let should_start_execution = !stored.duplicate
            && submitted.operation_id == operation_id
            && submitted.start_sequence == stored.sequence;
        let lease = self
            .claim_submit_lease(&submitted.operation_id, &owner)
            .await?;

        Ok(SubmittedOperation {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            payload,
            lease,
            should_start_execution,
        })
    }

    pub async fn submit_machine_add(
        &self,
        submission: MachineAddOperationSubmission,
        owner: OperationLeaseClaim,
    ) -> Result<AcceptedMachineAddSubmission, SubmitMachineAddError> {
        validate_machine_add_join_material(&submission.raw_join_token, &submission.join_token)?;
        let idempotency_key = submission.idempotency_key;
        let submitted_candidate = StoredMachineAddSubmission {
            operation_id: submission.operation_id,
            idempotency_key: idempotency_key.clone(),
            start_sequence: None,
            node_id: submission.node_id,
            name: submission.name,
            roles: submission.roles,
            join_bundle: submission.join_bundle,
            join_token: submission.join_token,
            raw_join_token: submission.raw_join_token,
        };

        let submitted = self
            .status_store
            .put_machine_add_submission_if_absent(&idempotency_key, &submitted_candidate)
            .await
            .map_err(submit_machine_add_store_status)?;
        ensure_machine_add_retry_matches(&submitted, &submitted_candidate)?;
        let fingerprint =
            validate_machine_add_join_material(&submitted.raw_join_token, &submitted.join_token)?;
        self.index_machine_add_join_token(&fingerprint, &submitted.operation_id, &idempotency_key)
            .await?;
        if let Some(start_sequence) = submitted.start_sequence {
            let lease = self
                .claim_submit_lease(&submitted.operation_id, &owner)
                .await
                .map_err(SubmitMachineAddError::Operation)?;
            return Ok(AcceptedMachineAddSubmission {
                operation_id: submitted.operation_id,
                start_sequence,
                node_id: submitted.node_id,
                name: submitted.name,
                roles: submitted.roles,
                join_bundle: submitted.join_bundle,
                join_token: submitted.join_token,
                raw_join_token: submitted.raw_join_token,
                lease,
            });
        }

        let stored = self
            .event_log
            .append(OperationEventAppend::machine_add_submitted(
                submitted.operation_id.clone(),
                submitted.node_id.clone(),
                submitted.name.clone(),
                submitted.roles,
                submitted.join_token.clone(),
                &idempotency_key,
            ))
            .await
            .map_err(submit_machine_add_append_event)?;
        let operation_id = if stored.duplicate {
            let original = self
                .event_log
                .event_at_sequence(stored.sequence)
                .await
                .map_err(submit_machine_add_append_event)?;
            let OperationEvent::MachineAddSubmitted {
                operation_id,
                node_id,
                name,
                roles,
                join_token,
            } = original
            else {
                return Err(submit_machine_add_duplicate_mismatch(stored.sequence));
            };
            if node_id != submitted.node_id
                || name != submitted.name
                || roles != submitted.roles
                || join_token != submitted.join_token
            {
                return Err(submit_machine_add_duplicate_mismatch(stored.sequence));
            }
            operation_id
        } else {
            submitted.operation_id.clone()
        };
        let status = OperationStatus::machine_add_pending(
            operation_id.clone(),
            submitted.node_id.clone(),
            submitted.name.clone(),
            submitted.roles,
            submitted.join_token.clone(),
            stored.sequence,
        );
        self.status_store
            .put_if_newer(&status)
            .await
            .map_err(submit_machine_add_store_status)?;
        let submitted = self
            .status_store
            .record_machine_add_submission_sequence(&idempotency_key, stored.sequence)
            .await
            .map_err(submit_machine_add_store_status)?;
        if submitted.operation_id != operation_id {
            return Err(submit_machine_add_duplicate_mismatch(stored.sequence));
        }
        let lease = self
            .claim_submit_lease(&submitted.operation_id, &owner)
            .await
            .map_err(SubmitMachineAddError::Operation)?;

        Ok(AcceptedMachineAddSubmission {
            operation_id: submitted.operation_id,
            start_sequence: stored.sequence,
            node_id: submitted.node_id,
            name: submitted.name,
            roles: submitted.roles,
            join_bundle: submitted.join_bundle,
            join_token: submitted.join_token,
            raw_join_token: submitted.raw_join_token,
            lease,
        })
    }

    async fn claim_submit_lease(
        &self,
        operation_id: &OperationId,
        owner: &OperationLeaseClaim,
    ) -> Result<OperationOwnerLease, SubmitOperationError> {
        self.status_store
            .claim_owner_lease(
                operation_id,
                owner.owner_id(),
                owner.now(),
                owner.expires_at(),
            )
            .await
            .map_err(SubmitOperationError::StoreStatus)
    }

    async fn submitted_event_payload<K: SubmitKind>(
        &self,
        expected_operation_id: &OperationId,
        sequence: EventSequence,
    ) -> Result<K::Payload, SubmitOperationError> {
        let event = self
            .event_log
            .event_at_sequence(sequence)
            .await
            .map_err(SubmitOperationError::AppendEvent)?;
        let Some((operation_id, payload)) = K::submitted_event_parts(event) else {
            return Err(SubmitOperationError::DuplicateSequenceMismatch { sequence });
        };
        if &operation_id != expected_operation_id {
            return Err(SubmitOperationError::DuplicateSequenceMismatch { sequence });
        }

        Ok(payload)
    }

    async fn index_machine_add_join_token(
        &self,
        fingerprint: &ployz_core::machine::JoinTokenFingerprint,
        operation_id: &OperationId,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<(), SubmitMachineAddError> {
        self.status_store
            .put_machine_add_join_token_if_absent(
                fingerprint,
                &StoredMachineAddJoinToken {
                    operation_id: operation_id.clone(),
                    idempotency_key: idempotency_key.clone(),
                },
            )
            .await
            .map(|_| ())
            .map_err(submit_machine_add_store_status)
    }
}

fn submit_machine_add_store_status(error: OperationStatusStoreError) -> SubmitMachineAddError {
    SubmitMachineAddError::Operation(SubmitOperationError::StoreStatus(error))
}

fn submit_machine_add_append_event(error: OperationEventLogError) -> SubmitMachineAddError {
    SubmitMachineAddError::Operation(SubmitOperationError::AppendEvent(error))
}

const fn submit_machine_add_duplicate_mismatch(sequence: EventSequence) -> SubmitMachineAddError {
    SubmitMachineAddError::Operation(SubmitOperationError::DuplicateSequenceMismatch { sequence })
}

fn validate_machine_add_join_material(
    raw_join_token: &RawJoinToken,
    join_token: &IssuedJoinToken,
) -> Result<ployz_core::machine::JoinTokenFingerprint, SubmitMachineAddError> {
    let fingerprint = raw_join_token
        .fingerprint()
        .map_err(|_| SubmitMachineAddError::JoinTokenMismatch)?;
    if join_token.matches(&fingerprint) {
        Ok(fingerprint)
    } else {
        Err(SubmitMachineAddError::JoinTokenMismatch)
    }
}

fn ensure_machine_add_retry_matches(
    existing: &StoredMachineAddSubmission,
    candidate: &StoredMachineAddSubmission,
) -> Result<(), SubmitMachineAddError> {
    if existing.idempotency_key == candidate.idempotency_key
        && existing.node_id == candidate.node_id
        && existing.name == candidate.name
        && existing.roles == candidate.roles
        && existing.join_bundle == candidate.join_bundle
        && existing.join_token == candidate.join_token
        && existing.raw_join_token == candidate.raw_join_token
    {
        return Ok(());
    }

    Err(submit_machine_add_store_status(
        OperationStatusStoreError::CasConflict {
            message: "machine add idempotency key is already assigned to different join material"
                .to_owned(),
        },
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployOperationSubmission {
    pub operation_id: OperationId,
    pub target: ployz_core::deploy::DeployRequest,
    pub idempotency_key: OperationIdempotencyKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertOperationSubmission {
    pub operation_id: OperationId,
    pub cert_id: CertId,
    pub idempotency_key: OperationIdempotencyKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineAddOperationSubmission {
    pub operation_id: OperationId,
    pub node_id: NodeId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    pub join_bundle: MachineJoinBundle,
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
    pub idempotency_key: OperationIdempotencyKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupOperationSubmission {
    pub operation_id: OperationId,
    pub idempotency_key: OperationIdempotencyKey,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationLeaseClaim {
    owner_id: OperationOwnerId,
    now: OperationLeaseExpiresAt,
    expires_at: OperationLeaseExpiresAt,
}

impl OperationLeaseClaim {
    pub fn try_new(
        owner_id: OperationOwnerId,
        now: OperationLeaseExpiresAt,
        expires_at: OperationLeaseExpiresAt,
    ) -> Result<Self, OperationLeaseClaimError> {
        if expires_at <= now {
            return Err(OperationLeaseClaimError::AlreadyExpired { now, expires_at });
        }

        Ok(Self {
            owner_id,
            now,
            expires_at,
        })
    }

    #[must_use]
    pub const fn now(&self) -> OperationLeaseExpiresAt {
        self.now
    }

    #[must_use]
    pub const fn expires_at(&self) -> OperationLeaseExpiresAt {
        self.expires_at
    }

    #[must_use]
    pub fn owner_id(&self) -> &OperationOwnerId {
        &self.owner_id
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OperationLeaseClaimError {
    AlreadyExpired {
        now: OperationLeaseExpiresAt,
        expires_at: OperationLeaseExpiresAt,
    },
}

impl std::fmt::Display for OperationLeaseClaimError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::AlreadyExpired { now, expires_at } => write!(
                formatter,
                "operation lease expires at {} but now is {}",
                expires_at.unix_seconds(),
                now.unix_seconds(),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedDeploySubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub target: ployz_core::deploy::DeployRequest,
    pub lease: OperationOwnerLease,
    pub should_start_execution: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedCertSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub cert_id: CertId,
    pub lease: OperationOwnerLease,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedMachineAddSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub node_id: NodeId,
    pub name: MachineName,
    pub roles: InstallRolePolicy,
    pub join_bundle: MachineJoinBundle,
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
    pub lease: OperationOwnerLease,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcceptedBackupSubmission {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub lease: OperationOwnerLease,
    pub should_start_execution: bool,
}

/// How a deploy, cert, or backup submission fails inside the repository.
/// The lease-clock failure lives with the caller that reads the clock
/// (`ployzd` controllers); this crate never constructs it.
#[derive(Debug)]
pub enum SubmitOperationError {
    AppendEvent(OperationEventLogError),
    StoreStatus(OperationStatusStoreError),
    DuplicateSequenceMismatch { sequence: EventSequence },
}

/// Machine-add extends the shared submit failure with join-token
/// validation.
#[derive(Debug)]
pub enum SubmitMachineAddError {
    Operation(SubmitOperationError),
    JoinTokenMismatch,
}
