use ployz_core::ids::{CertId, NodeId, OperationId, OperationOwnerId};
use ployz_core::machine::{IssuedJoinToken, MachineName, RawJoinToken};
use ployz_core::ops::{
    EventSequence, OperationEvent, OperationIdempotencyKey, OperationLeaseExpiresAt,
    OperationOwnerLease, OperationStatus,
};
use ployz_core::roles::FirstNodeGateway;

use super::AsyncNatsOperationRepository;
use crate::operations::events::{OperationEventAppend, OperationEventLogError};
use crate::operations::status_store::{
    OperationStatusStoreError, StoredCertSubmission, StoredDeploySubmission,
    StoredMachineAddSubmission,
};

impl AsyncNatsOperationRepository {
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
        let (operation_id, target) = if stored.duplicate {
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
            (operation_id, target)
        } else {
            (submission.operation_id, submission.target)
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
            operation_id,
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
        let submitted = self
            .status_store
            .put_machine_add_submission_if_absent(
                &submission.idempotency_key,
                &StoredMachineAddSubmission {
                    operation_id: submission.operation_id,
                    start_sequence: None,
                    node_id: submission.node_id,
                    name: submission.name,
                    gateway: submission.gateway,
                    join_token: submission.join_token,
                    raw_join_token: submission.raw_join_token,
                },
            )
            .await
            .map_err(SubmitMachineAddError::StoreStatus)?;
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
                &submission.idempotency_key,
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
            .record_machine_add_submission_sequence(&submission.idempotency_key, stored.sequence)
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
            join_token: submitted.join_token,
            raw_join_token: submitted.raw_join_token,
            lease,
        })
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
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
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
    pub join_token: IssuedJoinToken,
    pub raw_join_token: RawJoinToken,
    pub lease: OperationOwnerLease,
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
}
