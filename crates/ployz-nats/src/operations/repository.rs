use ployz_core::cert::{AcmeHttp01Challenge, ActiveCertState};
use ployz_core::ids::{CertId, OperationId, OperationOwnerId};
use ployz_core::ops::{
    CertOperationFailure, DeployEvidence, DeployTransition, EventSequence, OperationEvent,
    OperationEventProjection, OperationEventReplayCursor, OperationEventReplayPage,
    OperationEventReplayRequest, OperationIdempotencyKey, OperationLeaseExpiresAt,
    OperationOwnerLease, OperationStatus, OperationStatusSnapshot, StatusProjectionError,
    project_operation_event, validate_fresh_deploy_evidence,
};

use super::events::{
    AsyncNatsOperationEventLog, OperationEventAppend, OperationEventLogError,
    OperationEventReplayReadError, StoredOperationEvent,
};
use super::projection::{next_event_sequence, status_sequence};
use super::status_store::{
    AsyncNatsOperationStatusStore, OperationStatusReadError, OperationStatusStoreError,
    OperationStatusWrite, StoredCertSubmission, StoredDeploySubmission,
};

#[derive(Debug, Clone)]
pub struct AsyncNatsOperationRepository {
    event_log: AsyncNatsOperationEventLog,
    status_store: AsyncNatsOperationStatusStore,
}

impl AsyncNatsOperationRepository {
    #[must_use]
    pub fn new(
        event_log: AsyncNatsOperationEventLog,
        status_store: AsyncNatsOperationStatusStore,
    ) -> Self {
        Self {
            event_log,
            status_store,
        }
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

    pub async fn record_deploy_transition(
        &self,
        operation_id: &OperationId,
        transition: DeployTransition,
    ) -> Result<OperationStatusWrite, RecordDeployTransitionError> {
        let attempted_append = OperationEventAppend::deploy_transition(operation_id, &transition);
        self.record_deploy_event(operation_id, attempted_append)
            .await
            .map(RecordOperationEventOutcome::into_status_write)
            .map_err(RecordDeployTransitionError::from_event_record)
    }

    pub async fn record_deploy_evidence(
        &self,
        operation_id: &OperationId,
        evidence: DeployEvidence,
    ) -> Result<StoredOperationEvent, RecordDeployEvidenceError> {
        self.record_deploy_event(
            operation_id,
            OperationEventAppend::deploy_evidence(operation_id, &evidence),
        )
        .await
        .map(RecordOperationEventOutcome::stored_event)
        .map_err(RecordDeployEvidenceError::from_event_record)
    }

    pub async fn record_cert_challenge_published(
        &self,
        operation_id: &OperationId,
        cert_id: &CertId,
        challenge: AcmeHttp01Challenge,
    ) -> Result<OperationStatusWrite, RecordCertEventError> {
        self.record_operation_event(
            operation_id,
            OperationEventAppend::cert_challenge_published(operation_id, cert_id, challenge),
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
        .map_err(RecordCertEventError::from_event_record)
    }

    pub async fn record_cert_validation_started(
        &self,
        operation_id: &OperationId,
        cert_id: &CertId,
    ) -> Result<OperationStatusWrite, RecordCertEventError> {
        self.record_operation_event(
            operation_id,
            OperationEventAppend::cert_validation_started(operation_id, cert_id),
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
        .map_err(RecordCertEventError::from_event_record)
    }

    pub async fn record_cert_completed(
        &self,
        operation_id: &OperationId,
        active_cert: ActiveCertState,
    ) -> Result<OperationStatusWrite, RecordCertEventError> {
        self.record_operation_event(
            operation_id,
            OperationEventAppend::cert_completed(operation_id, active_cert),
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
        .map_err(RecordCertEventError::from_event_record)
    }

    pub async fn record_cert_failed(
        &self,
        operation_id: &OperationId,
        failure: CertOperationFailure,
    ) -> Result<OperationStatusWrite, RecordCertEventError> {
        self.record_operation_event(
            operation_id,
            OperationEventAppend::cert_failed(operation_id, failure),
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
        .map_err(RecordCertEventError::from_event_record)
    }

    pub async fn operation_status(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationStatus>, OperationStatusReadError> {
        self.status_store.get(operation_id).await
    }

    pub async fn operation_status_snapshot(
        &self,
        operation_id: &OperationId,
        now: OperationLeaseExpiresAt,
    ) -> Result<Option<OperationStatusSnapshot>, OperationStatusStoreError> {
        let Some(status) = self
            .status_store
            .get(operation_id)
            .await
            .map_err(OperationStatusStoreError::from_status_read)?
        else {
            return Ok(None);
        };
        let ownership = self
            .status_store
            .operation_ownership(operation_id, now)
            .await?;
        Ok(Some(OperationStatusSnapshot::new(status, ownership)))
    }

    pub async fn renew_owner_lease(
        &self,
        operation_id: &OperationId,
        owner: OperationLeaseClaim,
    ) -> Result<Option<OperationOwnerLease>, OperationStatusStoreError> {
        self.status_store
            .renew_owner_lease(
                operation_id,
                owner.owner_id(),
                owner.now(),
                owner.expires_at(),
            )
            .await
    }

    pub async fn replay_operation_events(
        &self,
        request: OperationEventReplayRequest,
    ) -> Result<OperationEventReplayPage, ReplayOperationEventsError> {
        let Some(status) = self
            .status_store
            .get(&request.operation_id)
            .await
            .map_err(ReplayOperationEventsError::LoadStatus)?
        else {
            return Err(ReplayOperationEventsError::MissingOperation {
                operation_id: request.operation_id,
            });
        };

        let page = self
            .event_log
            .replay_operation(&request.operation_id, request.start_sequence, request.limit)
            .await
            .map_err(ReplayOperationEventsError::ReadEvents)?;

        match (page.cursor, status.is_terminal()) {
            (OperationEventReplayCursor::CaughtUp, true) => {
                Ok(OperationEventReplayPage::terminal(page.events))
            }
            (cursor, _) => Ok(OperationEventReplayPage {
                events: page.events,
                cursor,
            }),
        }
    }

    async fn put_projected_status(
        &self,
        status: &OperationStatus,
    ) -> Result<OperationStatusWrite, RecordOperationEventError> {
        const MAX_STATUS_PROJECTION_ATTEMPTS: usize = 3;

        for _ in 0..MAX_STATUS_PROJECTION_ATTEMPTS {
            match self
                .status_store
                .put_if_newer(status)
                .await
                .map_err(RecordOperationEventError::StoreStatus)?
            {
                OperationStatusWrite::Stored { revision } => {
                    return Ok(OperationStatusWrite::Stored { revision });
                }
                OperationStatusWrite::AlreadySatisfied { current_sequence } => {
                    return Ok(OperationStatusWrite::AlreadySatisfied { current_sequence });
                }
                OperationStatusWrite::Stale {
                    current_sequence,
                    attempted_sequence,
                } if current_sequence >= attempted_sequence => {
                    return Ok(OperationStatusWrite::Stale {
                        current_sequence,
                        attempted_sequence,
                    });
                }
                OperationStatusWrite::Stale { .. } | OperationStatusWrite::Contended { .. } => {
                    continue;
                }
            }
        }

        Err(RecordOperationEventError::StatusProjectionContended)
    }

    async fn record_operation_event(
        &self,
        operation_id: &OperationId,
        attempted_append: OperationEventAppend,
    ) -> Result<RecordOperationEventOutcome, RecordOperationEventError> {
        let Some(current) = self
            .status_store
            .get(operation_id)
            .await
            .map_err(RecordOperationEventError::LoadStatus)?
        else {
            return Err(RecordOperationEventError::MissingOperation {
                operation_id: operation_id.clone(),
            });
        };

        let attempted_event = attempted_append.payload().clone();
        match project_operation_event(
            &current,
            attempted_event.clone(),
            next_event_sequence(&current),
        )
        .map_err(RecordOperationEventError::ProjectStatus)?
        {
            OperationEventProjection::StatusChanged { .. } => {}
            OperationEventProjection::AlreadySatisfied => {
                return Ok(RecordOperationEventOutcome::AlreadySatisfied {
                    current_sequence: status_sequence(&current),
                });
            }
        }

        let stored = self
            .event_log
            .append(attempted_append)
            .await
            .map_err(RecordOperationEventError::AppendEvent)?;
        let event = if stored.duplicate {
            self.event_log
                .event_at_sequence(stored.sequence)
                .await
                .map_err(RecordOperationEventError::AppendEvent)?
        } else {
            attempted_event.clone()
        };
        validate_stored_operation_event(operation_id, &attempted_event, &event, stored.sequence)?;

        let current = self
            .status_store
            .get(operation_id)
            .await
            .map_err(RecordOperationEventError::LoadStatus)?
            .unwrap_or(current);
        self.project_recorded_operation_event(event, stored, current)
            .await
    }

    async fn record_deploy_event(
        &self,
        operation_id: &OperationId,
        attempted_append: OperationEventAppend,
    ) -> Result<RecordOperationEventOutcome, RecordOperationEventError> {
        let Some(current) = self
            .status_store
            .get(operation_id)
            .await
            .map_err(RecordOperationEventError::LoadStatus)?
        else {
            return Err(RecordOperationEventError::MissingOperation {
                operation_id: operation_id.clone(),
            });
        };

        let attempted_event = attempted_append.payload().clone();
        if current.is_terminal()
            && let Some(evidence) = deploy_evidence_from_event(&attempted_event)
        {
            if let Some((stored, event)) = self
                .event_log
                .event_at_subject(attempted_append.subject())
                .await
                .map_err(RecordOperationEventError::AppendEvent)?
            {
                validate_stored_operation_event(
                    operation_id,
                    &attempted_event,
                    &event,
                    stored.sequence,
                )?;
                return self
                    .project_recorded_operation_event(event, stored, current)
                    .await;
            }

            validate_fresh_deploy_evidence(&current, &evidence)
                .map_err(RecordOperationEventError::ProjectStatus)?;
        }

        match project_operation_event(
            &current,
            attempted_event.clone(),
            next_event_sequence(&current),
        )
        .map_err(RecordOperationEventError::ProjectStatus)?
        {
            OperationEventProjection::StatusChanged { .. } => {}
            OperationEventProjection::AlreadySatisfied => {
                if let Some(evidence) = deploy_evidence_from_event(&attempted_event) {
                    validate_fresh_deploy_evidence(&current, &evidence)
                        .map_err(RecordOperationEventError::ProjectStatus)?;
                }

                return Ok(RecordOperationEventOutcome::AlreadySatisfied {
                    current_sequence: status_sequence(&current),
                });
            }
        }

        let stored = self
            .event_log
            .append(attempted_append)
            .await
            .map_err(RecordOperationEventError::AppendEvent)?;
        let event = if stored.duplicate {
            self.event_log
                .event_at_sequence(stored.sequence)
                .await
                .map_err(RecordOperationEventError::AppendEvent)?
        } else {
            attempted_event.clone()
        };
        validate_stored_operation_event(operation_id, &attempted_event, &event, stored.sequence)?;

        let current = self
            .status_store
            .get(operation_id)
            .await
            .map_err(RecordOperationEventError::LoadStatus)?
            .unwrap_or(current);
        self.project_recorded_operation_event(event, stored, current)
            .await
    }

    async fn project_recorded_operation_event(
        &self,
        event: OperationEvent,
        stored: StoredOperationEvent,
        current: OperationStatus,
    ) -> Result<RecordOperationEventOutcome, RecordOperationEventError> {
        if current.is_terminal()
            && stored.sequence > status_sequence(&current)
            && let Some(evidence) = deploy_evidence_from_event(&event)
        {
            validate_fresh_deploy_evidence(&current, &evidence)
                .map_err(RecordOperationEventError::ProjectStatus)?;
        }

        let projection = project_operation_event(&current, event, stored.sequence)
            .map_err(RecordOperationEventError::ProjectStatus)?;
        match projection {
            OperationEventProjection::StatusChanged { status } => {
                let status_write = self.put_projected_status(&status).await?;
                Ok(RecordOperationEventOutcome::Stored {
                    stored,
                    status_write,
                })
            }
            OperationEventProjection::AlreadySatisfied => Ok(RecordOperationEventOutcome::Stored {
                stored,
                status_write: OperationStatusWrite::AlreadySatisfied {
                    current_sequence: status_sequence(&current),
                },
            }),
        }
    }
}

enum RecordOperationEventOutcome {
    AlreadySatisfied {
        current_sequence: EventSequence,
    },
    Stored {
        stored: StoredOperationEvent,
        status_write: OperationStatusWrite,
    },
}

impl RecordOperationEventOutcome {
    fn into_status_write(self) -> OperationStatusWrite {
        match self {
            Self::AlreadySatisfied { current_sequence } => {
                OperationStatusWrite::AlreadySatisfied { current_sequence }
            }
            Self::Stored { status_write, .. } => status_write,
        }
    }

    fn stored_event(self) -> StoredOperationEvent {
        match self {
            Self::AlreadySatisfied { current_sequence } => StoredOperationEvent {
                sequence: current_sequence,
                duplicate: true,
            },
            Self::Stored { stored, .. } => stored,
        }
    }
}

#[derive(Debug)]
enum RecordOperationEventError {
    LoadStatus(OperationStatusReadError),
    StoreStatus(OperationStatusStoreError),
    MissingOperation {
        operation_id: OperationId,
    },
    ProjectStatus(StatusProjectionError),
    AppendEvent(OperationEventLogError),
    StoredEventMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
        plan_mismatch: bool,
    },
    StatusProjectionContended,
}

fn validate_stored_operation_event(
    operation_id: &OperationId,
    attempted_event: &OperationEvent,
    stored_event: &OperationEvent,
    sequence: EventSequence,
) -> Result<(), RecordOperationEventError> {
    if attempted_event == stored_event {
        return Ok(());
    }

    Err(RecordOperationEventError::StoredEventMismatch {
        operation_id: operation_id.clone(),
        sequence,
        plan_mismatch: deploy_plan_mismatch(operation_id, attempted_event, stored_event),
    })
}

fn deploy_plan_mismatch(
    operation_id: &OperationId,
    attempted_event: &OperationEvent,
    stored_event: &OperationEvent,
) -> bool {
    matches!(
        (attempted_event, stored_event),
        (
            OperationEvent::DeployPlanCreated {
                operation_id: attempted_operation_id,
                ..
            },
            OperationEvent::DeployPlanCreated {
                operation_id: stored_operation_id,
                ..
            },
        ) if attempted_operation_id == operation_id && stored_operation_id == operation_id
    )
}

fn deploy_evidence_from_event(event: &OperationEvent) -> Option<DeployEvidence> {
    match event {
        OperationEvent::DeployPlanCreated { plan, .. } => {
            Some(DeployEvidence::PlanCreated { plan: plan.clone() })
        }
        OperationEvent::DeployContainerStarted {
            node_id,
            container_id,
            ..
        } => Some(DeployEvidence::ContainerStarted {
            node_id: node_id.clone(),
            container_id: container_id.clone(),
        }),
        OperationEvent::DeployHealthCheckStarted { .. } => Some(DeployEvidence::HealthCheckStarted),
        OperationEvent::DeploySubmitted { .. }
        | OperationEvent::DeployPlanningStarted { .. }
        | OperationEvent::DeployRunning { .. }
        | OperationEvent::DeployCompleted { .. }
        | OperationEvent::DeployFailed { .. }
        | OperationEvent::CertRenewalSubmitted { .. }
        | OperationEvent::CertChallengePublished { .. }
        | OperationEvent::CertValidationStarted { .. }
        | OperationEvent::CertCompleted { .. }
        | OperationEvent::CertFailed { .. }
        | OperationEvent::Cancelled { .. } => None,
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
pub enum RecordDeployTransitionError {
    LoadStatus(OperationStatusReadError),
    AppendEvent(OperationEventLogError),
    ProjectStatus(StatusProjectionError),
    StoreStatus(OperationStatusStoreError),
    StoredTransitionMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
    StatusProjectionContended,
}

impl RecordDeployTransitionError {
    fn from_event_record(error: RecordOperationEventError) -> Self {
        match error {
            RecordOperationEventError::LoadStatus(error) => Self::LoadStatus(error),
            RecordOperationEventError::StoreStatus(error) => Self::StoreStatus(error),
            RecordOperationEventError::MissingOperation { operation_id } => {
                Self::ProjectStatus(StatusProjectionError::MissingOperation { operation_id })
            }
            RecordOperationEventError::ProjectStatus(error) => Self::ProjectStatus(error),
            RecordOperationEventError::AppendEvent(error) => Self::AppendEvent(error),
            RecordOperationEventError::StoredEventMismatch {
                operation_id,
                sequence,
                ..
            } => Self::StoredTransitionMismatch {
                operation_id,
                sequence,
            },
            RecordOperationEventError::StatusProjectionContended => Self::StatusProjectionContended,
        }
    }
}

#[derive(Debug)]
pub enum RecordDeployEvidenceError {
    LoadStatus(OperationStatusReadError),
    StoreStatus(OperationStatusStoreError),
    MissingOperation { operation_id: OperationId },
    ProjectStatus(StatusProjectionError),
    AppendEvent(OperationEventLogError),
    PlanMismatch { operation_id: OperationId },
    StoredEventMismatch { operation_id: OperationId },
    StatusCursorContended,
}

#[derive(Debug)]
pub enum RecordCertEventError {
    LoadStatus(OperationStatusReadError),
    StoreStatus(OperationStatusStoreError),
    MissingOperation { operation_id: OperationId },
    ProjectStatus(StatusProjectionError),
    AppendEvent(OperationEventLogError),
    StoredEventMismatch { operation_id: OperationId },
    StatusCursorContended,
}

#[derive(Debug)]
pub enum ReplayOperationEventsError {
    LoadStatus(OperationStatusReadError),
    ReadEvents(OperationEventReplayReadError),
    MissingOperation { operation_id: OperationId },
}

impl RecordCertEventError {
    fn from_event_record(error: RecordOperationEventError) -> Self {
        match error {
            RecordOperationEventError::LoadStatus(error) => Self::LoadStatus(error),
            RecordOperationEventError::StoreStatus(error) => Self::StoreStatus(error),
            RecordOperationEventError::MissingOperation { operation_id } => {
                Self::MissingOperation { operation_id }
            }
            RecordOperationEventError::ProjectStatus(error) => Self::ProjectStatus(error),
            RecordOperationEventError::AppendEvent(error) => Self::AppendEvent(error),
            RecordOperationEventError::StoredEventMismatch { operation_id, .. } => {
                Self::StoredEventMismatch { operation_id }
            }
            RecordOperationEventError::StatusProjectionContended => Self::StatusCursorContended,
        }
    }
}

impl RecordDeployEvidenceError {
    fn from_event_record(error: RecordOperationEventError) -> Self {
        match error {
            RecordOperationEventError::LoadStatus(error) => Self::LoadStatus(error),
            RecordOperationEventError::StoreStatus(error) => Self::StoreStatus(error),
            RecordOperationEventError::MissingOperation { operation_id } => {
                Self::MissingOperation { operation_id }
            }
            RecordOperationEventError::ProjectStatus(error) => Self::ProjectStatus(error),
            RecordOperationEventError::AppendEvent(error) => Self::AppendEvent(error),
            RecordOperationEventError::StoredEventMismatch {
                operation_id,
                plan_mismatch: true,
                ..
            } => Self::PlanMismatch { operation_id },
            RecordOperationEventError::StoredEventMismatch { operation_id, .. } => {
                Self::StoredEventMismatch { operation_id }
            }
            RecordOperationEventError::StatusProjectionContended => Self::StatusCursorContended,
        }
    }
}
