use ployz_core::cert::{AcmeHttp01Challenge, ActiveCertState};
use ployz_core::machine::{JoinTokenRedeemedAt, MachineAddFailure};
mod machine_join;
mod submission;

pub use machine_join::{
    MachineJoinRedemption, RecordMachineJoinReportError, RecordedMachineJoinReport,
    RedeemMachineJoinTokenError, RedeemedMachineJoin,
};
pub use submission::{
    AcceptedCertSubmission, AcceptedDeploySubmission, AcceptedMachineAddSubmission,
    CertOperationSubmission, DeployOperationSubmission, MachineAddOperationSubmission,
    OperationLeaseClaim, OperationLeaseClaimError, SubmitCertError, SubmitDeployError,
    SubmitMachineAddError,
};

use ployz_core::ids::{CertId, NodeId, OperationId};
use ployz_core::ops::{
    CertOperationFailure, DeployEvidence, DeployTransition, EventSequence, OperationEvent,
    OperationEventProjection, OperationEventReplayCursor, OperationEventReplayPage,
    OperationEventReplayRequest, OperationLeaseExpiresAt, OperationOwnerLease, OperationStatus,
    OperationStatusSnapshot, StatusProjectionError, project_operation_event,
    validate_fresh_deploy_evidence,
};

use super::events::{
    AsyncNatsOperationEventLog, OperationEventAppend, OperationEventLogError,
    OperationEventReplayReadError, StoredOperationEvent,
};
use super::projection::{next_event_sequence, status_sequence};
use super::status_store::{
    AsyncNatsOperationStatusStore, OperationStatusReadError, OperationStatusStoreError,
    OperationStatusWrite,
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

    pub async fn record_machine_add_joined(
        &self,
        operation_id: &OperationId,
        node_id: &NodeId,
        joined_at: JoinTokenRedeemedAt,
    ) -> Result<OperationStatusWrite, RecordMachineAddEventError> {
        self.record_operation_event_with_validator(
            operation_id,
            OperationEventAppend::machine_add_joined(operation_id, node_id, joined_at),
            validate_stored_machine_add_joined_event,
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
        .map_err(RecordMachineAddEventError::from_event_record)
    }

    pub async fn record_machine_add_failed(
        &self,
        operation_id: &OperationId,
        node_id: &NodeId,
        failure: MachineAddFailure,
    ) -> Result<OperationStatusWrite, RecordMachineAddEventError> {
        self.record_operation_event(
            operation_id,
            OperationEventAppend::machine_add_failed(operation_id, node_id, failure),
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
        .map_err(RecordMachineAddEventError::from_event_record)
    }

    pub async fn record_machine_add_completed(
        &self,
        operation_id: &OperationId,
        node_id: &NodeId,
    ) -> Result<OperationStatusWrite, RecordMachineAddEventError> {
        self.record_operation_event(
            operation_id,
            OperationEventAppend::machine_add_completed(operation_id, node_id),
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
        .map_err(RecordMachineAddEventError::from_event_record)
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
        self.record_operation_event_with_validator(
            operation_id,
            attempted_append,
            validate_stored_operation_event,
        )
        .await
    }

    async fn record_operation_event_with_validator(
        &self,
        operation_id: &OperationId,
        attempted_append: OperationEventAppend,
        validate_stored: impl Fn(
            &OperationId,
            &OperationEvent,
            &OperationEvent,
            EventSequence,
        ) -> Result<(), RecordOperationEventError>,
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
        validate_stored(operation_id, &attempted_event, &event, stored.sequence)?;

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

fn validate_stored_machine_add_joined_event(
    operation_id: &OperationId,
    attempted_event: &OperationEvent,
    stored_event: &OperationEvent,
    sequence: EventSequence,
) -> Result<(), RecordOperationEventError> {
    if attempted_event == stored_event {
        return Ok(());
    }

    if matches!(
        (attempted_event, stored_event),
        (
            OperationEvent::MachineAddJoined {
                operation_id: attempted_operation_id,
                node_id: attempted_node_id,
                ..
            },
            OperationEvent::MachineAddJoined {
                operation_id: stored_operation_id,
                node_id: stored_node_id,
                ..
            },
        ) if attempted_operation_id == operation_id
            && stored_operation_id == operation_id
            && attempted_node_id == stored_node_id
    ) {
        return Ok(());
    }

    Err(RecordOperationEventError::StoredEventMismatch {
        operation_id: operation_id.clone(),
        sequence,
        plan_mismatch: false,
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
        | OperationEvent::MachineAddSubmitted { .. }
        | OperationEvent::MachineAddJoined { .. }
        | OperationEvent::MachineAddCompleted { .. }
        | OperationEvent::MachineAddFailed { .. }
        | OperationEvent::Cancelled { .. } => None,
    }
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

pub type RecordCertEventError = RecordLifecycleEventError;
pub type RecordMachineAddEventError = RecordLifecycleEventError;

#[derive(Debug)]
pub enum RecordLifecycleEventError {
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

impl RecordLifecycleEventError {
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
