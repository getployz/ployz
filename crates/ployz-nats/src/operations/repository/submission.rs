use ployz_core::ids::{CertId, NodeId, OperationId, OperationOwnerId};
use ployz_core::install::MachineJoinBundle;
use ployz_core::machine::{IssuedJoinToken, MachineName, RawJoinToken};
use ployz_core::ops::{
    EventSequence, OperationEvent, OperationIdempotencyKey, OperationLeaseExpiresAt,
    OperationOwnerLease, OperationStatus,
};
use ployz_core::roles::FirstNodeGateway;

use super::AsyncNatsOperationRepository;
use crate::operations::events::{OperationEventAppend, OperationEventLogError};
use crate::operations::status_store::{
    OperationStatusStoreError, StoredBackupSubmission, StoredCertSubmission,
    StoredDeploySubmission, StoredMachineAddJoinToken, StoredMachineAddSecretDelivery,
    StoredMachineAddSubmission,
};

impl AsyncNatsOperationRepository {
    pub async fn machine_add_submission(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredMachineAddSubmission>, OperationStatusStoreError> {
        self.status_store
            .machine_add_submission(idempotency_key)
            .await
    }

    pub async fn machine_add_secret_delivery(
        &self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> Result<Option<StoredMachineAddSecretDelivery>, OperationStatusStoreError> {
        self.status_store
            .machine_add_secret_delivery(idempotency_key)
            .await
    }

    /// Write-once store of the minted per-machine secret. The mint worker
    /// writes it after the credential verifies; a replayed mint converges
    /// on the first stored record instead of minting twice.
    pub async fn put_machine_add_secret_delivery_if_absent(
        &self,
        idempotency_key: &OperationIdempotencyKey,
        record: &StoredMachineAddSecretDelivery,
    ) -> Result<StoredMachineAddSecretDelivery, OperationStatusStoreError> {
        self.status_store
            .put_machine_add_secret_delivery_if_absent(idempotency_key, record)
            .await
    }

    pub async fn submit_deploy(
        &self,
        submission: DeployOperationSubmission,
        owner: OperationLeaseClaim,
    ) -> Result<AcceptedDeploySubmission, SubmitDeployError> {
        if let Some(existing) = self
            .status_store
            .deploy_submission(&submission.idempotency_key)
            .await
            .map_err(SubmitDeployError::StoreStatus)?
        {
            let target = self
                .submitted_deploy_target(&existing.operation_id, existing.start_sequence)
                .await?;
            let lease = self
                .status_store
                .claim_owner_lease(
                    &existing.operation_id,
                    owner.owner_id(),
                    owner.now(),
                    owner.expires_at(),
                )
                .await
                .map_err(SubmitDeployError::StoreStatus)?;
            return Ok(AcceptedDeploySubmission {
                operation_id: existing.operation_id,
                start_sequence: existing.start_sequence,
                target,
                lease,
                should_start_execution: false,
            });
        }

        let stored = self
            .event_log
            .append(OperationEventAppend::deploy_submitted(
                submission.operation_id.clone(),
                submission.target.clone(),
                &submission.idempotency_key,
            ))
            .await
            .map_err(SubmitDeployError::AppendEvent)?;
        let (operation_id, target, should_start_execution) = if stored.duplicate {
            let original = self
                .event_log
                .event_at_sequence(stored.sequence)
                .await
                .map_err(SubmitDeployError::AppendEvent)?;
            let OperationEvent::DeploySubmitted {
                operation_id,
                target,
            } = original
            else {
                return Err(SubmitDeployError::DuplicateSequenceMismatch {
                    sequence: stored.sequence,
                });
            };
            (operation_id, target, false)
        } else {
            (submission.operation_id, submission.target, true)
        };
        let status = OperationStatus::deploy_accepted(
            operation_id.clone(),
            target.service_id.clone(),
            stored.sequence,
        );
        self.status_store
            .put_if_newer(&status)
            .await
            .map_err(SubmitDeployError::StoreStatus)?;
        let submitted = StoredDeploySubmission {
            operation_id: operation_id.clone(),
            start_sequence: stored.sequence,
        };

        let submitted = self
            .status_store
            .put_deploy_submission_if_absent(&submission.idempotency_key, &submitted)
            .await
            .map_err(SubmitDeployError::StoreStatus)?;
        let target = self
            .submitted_deploy_target(&submitted.operation_id, submitted.start_sequence)
            .await?;
        let should_start_execution = should_start_execution
            && submitted.operation_id == operation_id
            && submitted.start_sequence == stored.sequence;

        let lease = self
            .status_store
            .claim_owner_lease(
                &submitted.operation_id,
                owner.owner_id(),
                owner.now(),
                owner.expires_at(),
            )
            .await
            .map_err(SubmitDeployError::StoreStatus)?;

        Ok(AcceptedDeploySubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            target,
            lease,
            should_start_execution,
        })
    }

    pub async fn submit_cert(
        &self,
        submission: CertOperationSubmission,
        owner: OperationLeaseClaim,
    ) -> Result<AcceptedCertSubmission, SubmitCertError> {
        if let Some(existing) = self
            .status_store
            .cert_submission(&submission.idempotency_key)
            .await
            .map_err(SubmitCertError::StoreStatus)?
        {
            let cert_id = self
                .submitted_cert_id(&existing.operation_id, existing.start_sequence)
                .await?;
            let lease = self
                .status_store
                .claim_owner_lease(
                    &existing.operation_id,
                    owner.owner_id(),
                    owner.now(),
                    owner.expires_at(),
                )
                .await
                .map_err(SubmitCertError::StoreStatus)?;
            return Ok(AcceptedCertSubmission {
                operation_id: existing.operation_id,
                start_sequence: existing.start_sequence,
                cert_id,
                lease,
            });
        }

        let stored = self
            .event_log
            .append(OperationEventAppend::cert_submitted(
                submission.operation_id.clone(),
                submission.cert_id.clone(),
                &submission.idempotency_key,
            ))
            .await
            .map_err(SubmitCertError::AppendEvent)?;
        let (operation_id, cert_id) = if stored.duplicate {
            let original = self
                .event_log
                .event_at_sequence(stored.sequence)
                .await
                .map_err(SubmitCertError::AppendEvent)?;
            let OperationEvent::CertRenewalSubmitted {
                operation_id,
                cert_id,
            } = original
            else {
                return Err(SubmitCertError::DuplicateSequenceMismatch {
                    sequence: stored.sequence,
                });
            };
            (operation_id, cert_id)
        } else {
            (submission.operation_id, submission.cert_id)
        };
        let status =
            OperationStatus::cert_accepted(operation_id.clone(), cert_id.clone(), stored.sequence);
        self.status_store
            .put_if_newer(&status)
            .await
            .map_err(SubmitCertError::StoreStatus)?;
        let submitted = StoredCertSubmission {
            operation_id,
            start_sequence: stored.sequence,
        };

        let submitted = self
            .status_store
            .put_cert_submission_if_absent(&submission.idempotency_key, &submitted)
            .await
            .map_err(SubmitCertError::StoreStatus)?;
        let cert_id = self
            .submitted_cert_id(&submitted.operation_id, submitted.start_sequence)
            .await?;
        let lease = self
            .status_store
            .claim_owner_lease(
                &submitted.operation_id,
                owner.owner_id(),
                owner.now(),
                owner.expires_at(),
            )
            .await
            .map_err(SubmitCertError::StoreStatus)?;

        Ok(AcceptedCertSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            cert_id,
            lease,
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
            gateway: submission.gateway,
            join_bundle: submission.join_bundle,
            join_token: submission.join_token,
            raw_join_token: submission.raw_join_token,
        };

        let submitted = self
            .status_store
            .put_machine_add_submission_if_absent(&idempotency_key, &submitted_candidate)
            .await
            .map_err(SubmitMachineAddError::StoreStatus)?;
        ensure_machine_add_retry_matches(&submitted, &submitted_candidate)?;
        let fingerprint =
            validate_machine_add_join_material(&submitted.raw_join_token, &submitted.join_token)?;
        self.index_machine_add_join_token(&fingerprint, &submitted.operation_id, &idempotency_key)
            .await?;
        if let Some(start_sequence) = submitted.start_sequence {
            let lease = self
                .status_store
                .claim_owner_lease(
                    &submitted.operation_id,
                    owner.owner_id(),
                    owner.now(),
                    owner.expires_at(),
                )
                .await
                .map_err(SubmitMachineAddError::StoreStatus)?;
            return Ok(AcceptedMachineAddSubmission {
                operation_id: submitted.operation_id,
                start_sequence,
                node_id: submitted.node_id,
                name: submitted.name,
                gateway: submitted.gateway,
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
                submitted.gateway,
                submitted.join_token.clone(),
                &idempotency_key,
            ))
            .await
            .map_err(SubmitMachineAddError::AppendEvent)?;
        let operation_id = if stored.duplicate {
            let original = self
                .event_log
                .event_at_sequence(stored.sequence)
                .await
                .map_err(SubmitMachineAddError::AppendEvent)?;
            let OperationEvent::MachineAddSubmitted {
                operation_id,
                node_id,
                name,
                gateway,
                join_token,
            } = original
            else {
                return Err(SubmitMachineAddError::DuplicateSequenceMismatch {
                    sequence: stored.sequence,
                });
            };
            if node_id != submitted.node_id
                || name != submitted.name
                || gateway != submitted.gateway
                || join_token != submitted.join_token
            {
                return Err(SubmitMachineAddError::DuplicateSequenceMismatch {
                    sequence: stored.sequence,
                });
            }
            operation_id
        } else {
            submitted.operation_id.clone()
        };
        let status = OperationStatus::machine_add_pending(
            operation_id.clone(),
            submitted.node_id.clone(),
            submitted.name.clone(),
            submitted.gateway,
            submitted.join_token.clone(),
            stored.sequence,
        );
        self.status_store
            .put_if_newer(&status)
            .await
            .map_err(SubmitMachineAddError::StoreStatus)?;
        let submitted = self
            .status_store
            .record_machine_add_submission_sequence(&idempotency_key, stored.sequence)
            .await
            .map_err(SubmitMachineAddError::StoreStatus)?;
        if submitted.operation_id != operation_id {
            return Err(SubmitMachineAddError::DuplicateSequenceMismatch {
                sequence: stored.sequence,
            });
        }
        let lease = self
            .status_store
            .claim_owner_lease(
                &submitted.operation_id,
                owner.owner_id(),
                owner.now(),
                owner.expires_at(),
            )
            .await
            .map_err(SubmitMachineAddError::StoreStatus)?;

        Ok(AcceptedMachineAddSubmission {
            operation_id: submitted.operation_id,
            start_sequence: stored.sequence,
            node_id: submitted.node_id,
            name: submitted.name,
            gateway: submitted.gateway,
            join_bundle: submitted.join_bundle,
            join_token: submitted.join_token,
            raw_join_token: submitted.raw_join_token,
            lease,
        })
    }

    pub async fn submit_backup(
        &self,
        submission: BackupOperationSubmission,
        owner: OperationLeaseClaim,
    ) -> Result<AcceptedBackupSubmission, SubmitBackupError> {
        if let Some(existing) = self
            .status_store
            .backup_submission(&submission.idempotency_key)
            .await
            .map_err(SubmitBackupError::StoreStatus)?
        {
            let lease = self
                .status_store
                .claim_owner_lease(
                    &existing.operation_id,
                    owner.owner_id(),
                    owner.now(),
                    owner.expires_at(),
                )
                .await
                .map_err(SubmitBackupError::StoreStatus)?;
            return Ok(AcceptedBackupSubmission {
                operation_id: existing.operation_id,
                start_sequence: existing.start_sequence,
                lease,
                should_start_execution: false,
            });
        }

        let stored = self
            .event_log
            .append(OperationEventAppend::backup_submitted(
                submission.operation_id.clone(),
                &submission.idempotency_key,
            ))
            .await
            .map_err(SubmitBackupError::AppendEvent)?;
        let operation_id = if stored.duplicate {
            let original = self
                .event_log
                .event_at_sequence(stored.sequence)
                .await
                .map_err(SubmitBackupError::AppendEvent)?;
            let OperationEvent::BackupCreateSubmitted { operation_id } = original else {
                return Err(SubmitBackupError::DuplicateSequenceMismatch {
                    sequence: stored.sequence,
                });
            };
            operation_id
        } else {
            submission.operation_id
        };
        let status = OperationStatus::backup_accepted(operation_id.clone(), stored.sequence);
        self.status_store
            .put_if_newer(&status)
            .await
            .map_err(SubmitBackupError::StoreStatus)?;
        let submitted = StoredBackupSubmission {
            operation_id: operation_id.clone(),
            start_sequence: stored.sequence,
        };

        let submitted = self
            .status_store
            .put_backup_submission_if_absent(&submission.idempotency_key, &submitted)
            .await
            .map_err(SubmitBackupError::StoreStatus)?;
        let lease = self
            .status_store
            .claim_owner_lease(
                &submitted.operation_id,
                owner.owner_id(),
                owner.now(),
                owner.expires_at(),
            )
            .await
            .map_err(SubmitBackupError::StoreStatus)?;
        let should_start_execution = !stored.duplicate
            && submitted.operation_id == operation_id
            && submitted.start_sequence == stored.sequence;

        Ok(AcceptedBackupSubmission {
            operation_id: submitted.operation_id,
            start_sequence: submitted.start_sequence,
            lease,
            should_start_execution,
        })
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
            .map_err(SubmitMachineAddError::StoreStatus)
    }

    async fn submitted_deploy_target(
        &self,
        expected_operation_id: &OperationId,
        sequence: EventSequence,
    ) -> Result<ployz_core::deploy::DeployRequest, SubmitDeployError> {
        let event = self
            .event_log
            .event_at_sequence(sequence)
            .await
            .map_err(SubmitDeployError::AppendEvent)?;
        let OperationEvent::DeploySubmitted {
            operation_id,
            target,
        } = event
        else {
            return Err(SubmitDeployError::DuplicateSequenceMismatch { sequence });
        };
        if &operation_id != expected_operation_id {
            return Err(SubmitDeployError::DuplicateSequenceMismatch { sequence });
        }

        Ok(target)
    }

    async fn submitted_cert_id(
        &self,
        expected_operation_id: &OperationId,
        sequence: EventSequence,
    ) -> Result<CertId, SubmitCertError> {
        let event = self
            .event_log
            .event_at_sequence(sequence)
            .await
            .map_err(SubmitCertError::AppendEvent)?;
        let OperationEvent::CertRenewalSubmitted {
            operation_id,
            cert_id,
        } = event
        else {
            return Err(SubmitCertError::DuplicateSequenceMismatch { sequence });
        };
        if &operation_id != expected_operation_id {
            return Err(SubmitCertError::DuplicateSequenceMismatch { sequence });
        }

        Ok(cert_id)
    }
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
        && existing.gateway == candidate.gateway
        && existing.join_bundle == candidate.join_bundle
        && existing.join_token == candidate.join_token
        && existing.raw_join_token == candidate.raw_join_token
    {
        return Ok(());
    }

    Err(SubmitMachineAddError::StoreStatus(
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
    pub gateway: FirstNodeGateway,
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
    pub gateway: FirstNodeGateway,
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

#[derive(Debug)]
pub enum SubmitDeployError {
    AppendEvent(OperationEventLogError),
    StoreStatus(OperationStatusStoreError),
    Clock { message: String },
    DuplicateSequenceMismatch { sequence: EventSequence },
}

#[derive(Debug)]
pub enum SubmitCertError {
    AppendEvent(OperationEventLogError),
    StoreStatus(OperationStatusStoreError),
    DuplicateSequenceMismatch { sequence: EventSequence },
}

#[derive(Debug)]
pub enum SubmitMachineAddError {
    AppendEvent(OperationEventLogError),
    StoreStatus(OperationStatusStoreError),
    Clock { message: String },
    DuplicateSequenceMismatch { sequence: EventSequence },
    JoinTokenMismatch,
}

#[derive(Debug)]
pub enum SubmitBackupError {
    AppendEvent(OperationEventLogError),
    StoreStatus(OperationStatusStoreError),
    Clock { message: String },
    DuplicateSequenceMismatch { sequence: EventSequence },
}
