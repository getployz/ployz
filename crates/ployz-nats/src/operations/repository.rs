use ployz_core::ids::OperationId;
use ployz_core::ops::{
    DeployEvidence, DeployProjection, DeployTransition, EventSequence, OperationEvent,
    OperationEventProjection, OperationIdempotencyKey, OperationStatus, StatusProjectionError,
    project_deploy_transition, project_operation_event, validate_fresh_deploy_evidence,
};

use super::events::{
    AsyncNatsOperationEventLog, OperationEventAppend, OperationEventLogError, StoredOperationEvent,
};
use super::projection::{next_event_sequence, status_sequence};
use super::status_store::{
    AsyncNatsOperationStatusStore, OperationStatusStoreError, OperationStatusWrite,
    StoredDeploySubmission,
};

#[derive(Debug, Clone)]
pub struct AsyncNatsOperationRepository {
    event_log: AsyncNatsOperationEventLog,
    status_store: AsyncNatsOperationStatusStore,
}

enum DeployEvidenceAdmission {
    DurableRetry {
        stored: StoredOperationEvent,
        event: OperationEvent,
    },
    Fresh(Box<FreshDeployEvidenceAdmission>),
}

struct FreshDeployEvidenceAdmission {
    current: OperationStatus,
    append: OperationEventAppend,
    evidence: DeployEvidence,
    event: OperationEvent,
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
    ) -> Result<StoredDeploySubmission, SubmitDeployError> {
        if let Some(existing) = self
            .status_store
            .deploy_submission(&submission.idempotency_key)
            .await
            .map_err(SubmitDeployError::StoreStatus)?
        {
            return Ok(existing);
        }

        let stored = self
            .event_log
            .append(OperationEventAppend::deploy_submitted(
                submission.operation_id.clone(),
                submission.service_id.clone(),
                &submission.idempotency_key,
            ))
            .await
            .map_err(SubmitDeployError::AppendEvent)?;
        let (operation_id, service_id) = if stored.duplicate {
            let original = self
                .event_log
                .event_at_sequence(stored.sequence)
                .await
                .map_err(SubmitDeployError::AppendEvent)?;
            let OperationEvent::DeploySubmitted {
                operation_id,
                service_id,
            } = original
            else {
                return Err(SubmitDeployError::DuplicateSequenceMismatch {
                    sequence: stored.sequence,
                });
            };
            (operation_id, service_id)
        } else {
            (submission.operation_id, submission.service_id)
        };
        let status =
            OperationStatus::deploy_accepted(operation_id.clone(), service_id, stored.sequence);
        self.status_store
            .put_if_newer(&status)
            .await
            .map_err(SubmitDeployError::StoreStatus)?;
        let submitted = StoredDeploySubmission {
            operation_id,
            start_sequence: stored.sequence,
        };

        self.status_store
            .put_deploy_submission_if_absent(&submission.idempotency_key, &submitted)
            .await
            .map_err(SubmitDeployError::StoreStatus)
    }

    pub async fn record_deploy_transition(
        &self,
        operation_id: &OperationId,
        transition: DeployTransition,
    ) -> Result<OperationStatusWrite, RecordDeployTransitionError> {
        let Some(existing) = self
            .status_store
            .get(operation_id)
            .await
            .map_err(RecordDeployTransitionError::LoadStatus)?
        else {
            return Err(RecordDeployTransitionError::ProjectStatus(
                StatusProjectionError::MissingOperation {
                    operation_id: operation_id.clone(),
                },
            ));
        };
        let preview_sequence = next_event_sequence(&existing);
        let attempted_append = OperationEventAppend::deploy_transition(operation_id, &transition);
        let attempted_event = attempted_append.payload().clone();
        let preview = project_deploy_transition(&existing, transition.clone(), preview_sequence)
            .map_err(RecordDeployTransitionError::ProjectStatus)?;
        if matches!(preview, DeployProjection::AlreadySatisfied) {
            return Ok(OperationStatusWrite::AlreadySatisfied {
                current_sequence: status_sequence(&existing),
            });
        }

        let stored = self
            .event_log
            .append(attempted_append)
            .await
            .map_err(RecordDeployTransitionError::AppendEvent)?;
        if stored.duplicate {
            let event = self
                .event_log
                .event_at_sequence(stored.sequence)
                .await
                .map_err(RecordDeployTransitionError::AppendEvent)?;
            validate_stored_transition_event(
                operation_id,
                &attempted_event,
                &event,
                stored.sequence,
            )?;
        }
        let current = self
            .status_store
            .get(operation_id)
            .await
            .map_err(RecordDeployTransitionError::LoadStatus)?
            .unwrap_or(existing);
        let projection = project_deploy_transition(&current, transition, stored.sequence)
            .map_err(RecordDeployTransitionError::ProjectStatus)?;
        match projection {
            DeployProjection::Updated { status } => self.put_status_with_retry(&status).await,
            DeployProjection::AlreadySatisfied => Ok(OperationStatusWrite::AlreadySatisfied {
                current_sequence: status_sequence(&current),
            }),
        }
    }

    pub async fn record_deploy_evidence(
        &self,
        operation_id: &OperationId,
        evidence: DeployEvidence,
    ) -> Result<StoredOperationEvent, RecordDeployEvidenceError> {
        self.record_deploy_evidence_event(
            operation_id,
            OperationEventAppend::deploy_evidence(operation_id, &evidence),
            evidence,
        )
        .await
    }

    pub async fn operation_status(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationStatus>, OperationStatusStoreError> {
        self.status_store.get(operation_id).await
    }

    async fn put_status_with_retry(
        &self,
        status: &OperationStatus,
    ) -> Result<OperationStatusWrite, RecordDeployTransitionError> {
        const MAX_STATUS_PROJECTION_ATTEMPTS: usize = 3;

        for _ in 0..MAX_STATUS_PROJECTION_ATTEMPTS {
            match self
                .status_store
                .put_if_newer(status)
                .await
                .map_err(RecordDeployTransitionError::StoreStatus)?
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

        Err(RecordDeployTransitionError::StatusProjectionContended)
    }

    async fn record_deploy_evidence_event(
        &self,
        operation_id: &OperationId,
        attempted_append: OperationEventAppend,
        evidence: DeployEvidence,
    ) -> Result<StoredOperationEvent, RecordDeployEvidenceError> {
        match self
            .classify_deploy_evidence(operation_id, attempted_append, evidence)
            .await?
        {
            DeployEvidenceAdmission::DurableRetry { stored, event } => {
                self.project_evidence_event(operation_id, event, stored.sequence)
                    .await?;
                Ok(stored)
            }
            DeployEvidenceAdmission::Fresh(fresh) => {
                self.append_fresh_deploy_evidence(operation_id, *fresh)
                    .await
            }
        }
    }

    async fn classify_deploy_evidence(
        &self,
        operation_id: &OperationId,
        attempted_append: OperationEventAppend,
        evidence: DeployEvidence,
    ) -> Result<DeployEvidenceAdmission, RecordDeployEvidenceError> {
        let stored = self
            .event_log
            .event_at_subject(attempted_append.subject())
            .await
            .map_err(RecordDeployEvidenceError::AppendEvent)?;
        if let Some((stored, event)) = stored {
            validate_stored_evidence_event(operation_id, &evidence, &event)?;
            return Ok(DeployEvidenceAdmission::DurableRetry { stored, event });
        }

        let Some(current) = self
            .status_store
            .get(operation_id)
            .await
            .map_err(RecordDeployEvidenceError::LoadStatus)?
        else {
            return Err(RecordDeployEvidenceError::MissingOperation {
                operation_id: operation_id.clone(),
            });
        };
        validate_fresh_deploy_evidence(&current, &evidence)
            .map_err(RecordDeployEvidenceError::ProjectStatus)?;
        Ok(DeployEvidenceAdmission::Fresh(Box::new(
            FreshDeployEvidenceAdmission {
                current,
                evidence,
                event: attempted_append.payload().clone(),
                append: attempted_append,
            },
        )))
    }

    async fn append_fresh_deploy_evidence(
        &self,
        operation_id: &OperationId,
        fresh: FreshDeployEvidenceAdmission,
    ) -> Result<StoredOperationEvent, RecordDeployEvidenceError> {
        let FreshDeployEvidenceAdmission {
            current,
            append,
            evidence,
            event: attempted_event,
        } = fresh;
        let preview_sequence = next_event_sequence(&current);
        let preview = project_operation_event(&current, attempted_event.clone(), preview_sequence)
            .map_err(RecordDeployEvidenceError::ProjectStatus)?;
        let OperationEventProjection::StatusChanged { .. } = preview else {
            return Ok(StoredOperationEvent {
                sequence: status_sequence(&current),
                duplicate: true,
            });
        };

        let stored = self
            .event_log
            .append(append)
            .await
            .map_err(RecordDeployEvidenceError::AppendEvent)?;
        let event = if stored.duplicate {
            self.event_log
                .event_at_sequence(stored.sequence)
                .await
                .map_err(RecordDeployEvidenceError::AppendEvent)?
        } else {
            attempted_event
        };
        validate_stored_evidence_event(operation_id, &evidence, &event)?;
        self.project_evidence_event(operation_id, event, stored.sequence)
            .await
            .map(|()| stored)
    }

    async fn put_evidence_status(
        &self,
        status: &OperationStatus,
    ) -> Result<(), RecordDeployEvidenceError> {
        const MAX_STATUS_CURSOR_ATTEMPTS: usize = 3;

        for _ in 0..MAX_STATUS_CURSOR_ATTEMPTS {
            match self
                .status_store
                .put_if_newer(status)
                .await
                .map_err(RecordDeployEvidenceError::StoreStatus)?
            {
                OperationStatusWrite::Stored { .. }
                | OperationStatusWrite::AlreadySatisfied { .. } => return Ok(()),
                OperationStatusWrite::Stale {
                    current_sequence,
                    attempted_sequence,
                } if current_sequence >= attempted_sequence => return Ok(()),
                OperationStatusWrite::Stale { .. } | OperationStatusWrite::Contended { .. } => {
                    continue;
                }
            }
        }

        Err(RecordDeployEvidenceError::StatusCursorContended)
    }

    async fn project_evidence_event(
        &self,
        operation_id: &OperationId,
        event: OperationEvent,
        sequence: EventSequence,
    ) -> Result<(), RecordDeployEvidenceError> {
        let Some(current) = self
            .status_store
            .get(operation_id)
            .await
            .map_err(RecordDeployEvidenceError::LoadStatus)?
        else {
            return Err(RecordDeployEvidenceError::MissingOperation {
                operation_id: operation_id.clone(),
            });
        };
        let projection = project_operation_event(&current, event, sequence)
            .map_err(RecordDeployEvidenceError::ProjectStatus)?;
        match projection {
            OperationEventProjection::StatusChanged { status } => {
                self.put_evidence_status(&status).await
            }
            OperationEventProjection::AlreadySatisfied => Ok(()),
        }
    }
}

fn validate_stored_transition_event(
    operation_id: &OperationId,
    attempted_event: &OperationEvent,
    stored_event: &OperationEvent,
    sequence: EventSequence,
) -> Result<(), RecordDeployTransitionError> {
    if attempted_event == stored_event {
        return Ok(());
    }

    Err(RecordDeployTransitionError::StoredTransitionMismatch {
        operation_id: operation_id.clone(),
        sequence,
    })
}

fn validate_stored_evidence_event(
    operation_id: &OperationId,
    evidence: &DeployEvidence,
    stored_event: &OperationEvent,
) -> Result<(), RecordDeployEvidenceError> {
    match (evidence, stored_event) {
        (
            DeployEvidence::PlanCreated { plan: attempted },
            OperationEvent::DeployPlanCreated {
                operation_id: stored_operation_id,
                plan,
            },
        ) if stored_operation_id == operation_id && plan == attempted => Ok(()),
        (
            DeployEvidence::PlanCreated { .. },
            OperationEvent::DeployPlanCreated {
                operation_id: stored_operation_id,
                ..
            },
        ) if stored_operation_id == operation_id => Err(RecordDeployEvidenceError::PlanMismatch {
            operation_id: operation_id.clone(),
        }),
        (
            DeployEvidence::ContainerStarted {
                node_id: attempted_node,
                container_id: attempted_container,
            },
            OperationEvent::DeployContainerStarted {
                operation_id: stored_operation_id,
                node_id,
                container_id,
            },
        ) if stored_operation_id == operation_id
            && node_id == attempted_node
            && container_id == attempted_container =>
        {
            Ok(())
        }
        (
            DeployEvidence::HealthCheckStarted,
            OperationEvent::DeployHealthCheckStarted {
                operation_id: stored_operation_id,
            },
        ) if stored_operation_id == operation_id => Ok(()),
        (
            DeployEvidence::PlanCreated { .. }
            | DeployEvidence::ContainerStarted { .. }
            | DeployEvidence::HealthCheckStarted,
            OperationEvent::DeploySubmitted { .. }
            | OperationEvent::DeployPlanningStarted { .. }
            | OperationEvent::DeployPlanCreated { .. }
            | OperationEvent::DeployRunning { .. }
            | OperationEvent::DeployContainerStarted { .. }
            | OperationEvent::DeployHealthCheckStarted { .. }
            | OperationEvent::DeployCompleted { .. }
            | OperationEvent::DeployFailed { .. }
            | OperationEvent::Cancelled { .. },
        ) => Err(RecordDeployEvidenceError::StoredEventMismatch {
            operation_id: operation_id.clone(),
        }),
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeployOperationSubmission {
    pub operation_id: OperationId,
    pub service_id: ployz_core::ids::ServiceId,
    pub idempotency_key: OperationIdempotencyKey,
}

#[derive(Debug)]
pub enum SubmitDeployError {
    AppendEvent(OperationEventLogError),
    StoreStatus(OperationStatusStoreError),
    DuplicateSequenceMismatch { sequence: EventSequence },
}

#[derive(Debug)]
pub enum RecordDeployTransitionError {
    LoadStatus(OperationStatusStoreError),
    AppendEvent(OperationEventLogError),
    ProjectStatus(StatusProjectionError),
    StoreStatus(OperationStatusStoreError),
    StoredTransitionMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
    StatusProjectionContended,
}

#[derive(Debug)]
pub enum RecordDeployEvidenceError {
    LoadStatus(OperationStatusStoreError),
    StoreStatus(OperationStatusStoreError),
    MissingOperation { operation_id: OperationId },
    ProjectStatus(StatusProjectionError),
    AppendEvent(OperationEventLogError),
    PlanMismatch { operation_id: OperationId },
    StoredEventMismatch { operation_id: OperationId },
    StatusCursorContended,
}
