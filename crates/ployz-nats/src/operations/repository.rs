use ployz_core::ids::OperationId;
use ployz_core::ops::{
    DeployProjection, DeployTransition, EventSequence, OperationEvent, OperationIdempotencyKey,
    OperationStatus, StatusProjectionError, project_deploy_transition,
};

use super::events::{AsyncNatsOperationEventLog, OperationEventAppend, OperationEventLogError};
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
        let preview = project_deploy_transition(&existing, transition.clone(), preview_sequence)
            .map_err(RecordDeployTransitionError::ProjectStatus)?;
        let DeployProjection::Updated { .. } = preview else {
            return Ok(OperationStatusWrite::AlreadySatisfied {
                current_sequence: status_sequence(&existing),
            });
        };
        let attempted_append = OperationEventAppend::deploy_transition(operation_id, &transition);
        let attempted_event = attempted_append.payload().clone();

        let stored = self
            .event_log
            .append(attempted_append)
            .await
            .map_err(RecordDeployTransitionError::AppendEvent)?;
        let event = if stored.duplicate {
            self.event_log
                .event_at_sequence(stored.sequence)
                .await
                .map_err(RecordDeployTransitionError::AppendEvent)?
        } else {
            attempted_event
        };
        let transition = deploy_transition_from_event(operation_id, event, stored.sequence)?;
        let current = self
            .status_store
            .get(operation_id)
            .await
            .map_err(RecordDeployTransitionError::LoadStatus)?
            .unwrap_or(existing);
        let projection = project_deploy_transition(&current, transition, stored.sequence)
            .map_err(RecordDeployTransitionError::ProjectStatus)?;
        let DeployProjection::Updated { status } = projection else {
            return Ok(OperationStatusWrite::AlreadySatisfied {
                current_sequence: status_sequence(&current),
            });
        };

        self.put_status_with_retry(&status).await
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
    StoredEventMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
    },
    StatusProjectionContended,
}

fn deploy_transition_from_event(
    operation_id: &OperationId,
    event: OperationEvent,
    sequence: EventSequence,
) -> Result<DeployTransition, RecordDeployTransitionError> {
    match event {
        OperationEvent::DeployPlanningStarted {
            operation_id: event_operation_id,
        } if &event_operation_id == operation_id => Ok(DeployTransition::Planning),
        OperationEvent::DeployRunning {
            operation_id: event_operation_id,
            stage,
        } if &event_operation_id == operation_id => Ok(DeployTransition::Running { stage }),
        OperationEvent::DeployCompleted {
            operation_id: event_operation_id,
        } if &event_operation_id == operation_id => Ok(DeployTransition::Completed),
        OperationEvent::DeployFailed {
            operation_id: event_operation_id,
            failure,
        } if &event_operation_id == operation_id => Ok(DeployTransition::Failed { failure }),
        OperationEvent::Cancelled {
            operation_id: event_operation_id,
            reason,
        } if &event_operation_id == operation_id => Ok(DeployTransition::Cancelled { reason }),
        OperationEvent::DeploySubmitted { .. }
        | OperationEvent::DeployPlanningStarted { .. }
        | OperationEvent::DeployRunning { .. }
        | OperationEvent::DeployContainerStarted { .. }
        | OperationEvent::DeployCompleted { .. }
        | OperationEvent::DeployFailed { .. }
        | OperationEvent::Cancelled { .. } => {
            Err(RecordDeployTransitionError::StoredEventMismatch {
                operation_id: operation_id.clone(),
                sequence,
            })
        }
    }
}
