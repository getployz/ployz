use ployz_core::machine::{JoinTokenRedeemedAt, MachineAddFailure};
mod machine_join;
mod submission;

pub use machine_join::{
    MachineJoinRedemption, RecordMachineJoinReportError, RecordedMachineJoinReport,
    RedeemMachineJoinTokenError, RedeemedMachineJoin,
};
pub use submission::{
    AcceptedCertSubmission, AcceptedDeploySubmission, AcceptedMachineAddSubmission,
    AcceptedMachineUpdateSubmission, CertOperationSubmission, DeployOperationSubmission,
    MachineAddOperationSubmission, MachineUpdateOperationSubmission, SubmitMachineAddError,
    SubmitOperationError,
};

use ployz_core::ids::{MachineId, OperationId};
use ployz_core::ops::{
    DeployEvidence, DeployTransition, EventSequence, MachineUpdateTransition, OperationEvent,
    OperationEventReplayCursor, OperationEventReplayPage, OperationEventReplayRequest,
    OperationProjection, OperationStatusSnapshot, StatusProjectionError, project_operation_event,
    validate_fresh_deploy_evidence,
};

use super::events::{
    AsyncNatsOperationEventLog, OperationEventAppend, OperationEventLogError,
    OperationEventReplayReadError, StoredOperationEvent,
};
use super::status_store::{
    AsyncNatsOperationStatusStore, KvRevision, OperationStatusReadError, OperationStatusStoreError,
    StatusStoreWrite,
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

    /// Direct access to the underlying operation-memory records for status
    /// reads and machine-add join material that need no event projection.
    #[must_use]
    pub fn records(&self) -> &AsyncNatsOperationStatusStore {
        &self.status_store
    }

    pub async fn record_deploy_transition(
        &self,
        operation_id: &OperationId,
        transition: DeployTransition,
    ) -> Result<OperationStatusWrite, RecordDeployTransitionError> {
        let attempted_append = OperationEventAppend::from_event(transition.event(operation_id));
        self.record_deploy_event(operation_id, attempted_append)
            .await
            .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_deploy_evidence(
        &self,
        operation_id: &OperationId,
        evidence: DeployEvidence,
    ) -> Result<StoredOperationEvent, RecordDeployEvidenceError> {
        self.record_deploy_event(
            operation_id,
            OperationEventAppend::from_event(evidence.event(operation_id)),
        )
        .await
        .map(RecordOperationEventOutcome::stored_event)
    }

    pub async fn record_machine_add_joined(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        joined_at: JoinTokenRedeemedAt,
    ) -> Result<OperationStatusWrite, RecordMachineAddEventError> {
        self.record_operation_event_with_validator(
            operation_id,
            OperationEventAppend::from_event(OperationEvent::MachineAddJoined {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                joined_at,
            }),
            PreCheck::None,
            validate_stored_machine_add_joined_event,
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_machine_add_credential_provisioned(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        step: ployz_core::machine::MachineCredentialProvisioningStep,
    ) -> Result<OperationStatusWrite, RecordMachineAddEventError> {
        self.record_operation_event(
            operation_id,
            OperationEventAppend::from_event(OperationEvent::MachineAddCredentialProvisioned {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                step,
            }),
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_machine_add_failed(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        failure: MachineAddFailure,
    ) -> Result<OperationStatusWrite, RecordMachineAddEventError> {
        self.record_operation_event(
            operation_id,
            OperationEventAppend::from_event(OperationEvent::MachineAddFailed {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
                failure,
            }),
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_machine_add_completed(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
    ) -> Result<OperationStatusWrite, RecordMachineAddEventError> {
        self.record_operation_event(
            operation_id,
            OperationEventAppend::from_event(OperationEvent::MachineAddCompleted {
                operation_id: operation_id.clone(),
                machine_id: machine_id.clone(),
            }),
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn record_machine_update_transition(
        &self,
        operation_id: &OperationId,
        machine_id: &MachineId,
        transition: MachineUpdateTransition,
    ) -> Result<OperationStatusWrite, RecordOperationEventError> {
        self.record_operation_event(
            operation_id,
            OperationEventAppend::from_event(transition.event(operation_id, machine_id)),
        )
        .await
        .map(RecordOperationEventOutcome::into_status_write)
    }

    pub async fn operation_status_snapshot(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationStatusSnapshot>, OperationStatusReadError> {
        let Some(status) = self.status_store.get(operation_id).await? else {
            return Ok(None);
        };
        Ok(Some(OperationStatusSnapshot::new(status)))
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

    async fn project_recorded_operation_event(
        &self,
        operation_id: &OperationId,
        event: OperationEvent,
        stored: StoredOperationEvent,
    ) -> Result<RecordOperationEventOutcome, RecordOperationEventError> {
        const MAX_STATUS_PROJECTION_ATTEMPTS: usize = 3;

        for _ in 0..MAX_STATUS_PROJECTION_ATTEMPTS {
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

            if current.is_terminal()
                && stored.sequence > current.last_event_sequence()
                && let Some(evidence) = deploy_evidence_from_event(&event)
            {
                validate_fresh_deploy_evidence(&current, &evidence)
                    .map_err(RecordOperationEventError::ProjectStatus)?;
            }

            let projection = project_operation_event(&current, event.clone(), stored.sequence)
                .map_err(RecordOperationEventError::ProjectStatus)?;
            let OperationProjection::StatusChanged { status } = projection else {
                return Ok(RecordOperationEventOutcome::Stored {
                    stored,
                    status_write: OperationStatusWrite::AlreadySatisfied {
                        current_sequence: current.last_event_sequence(),
                    },
                });
            };

            match self
                .status_store
                .put_if_newer(&status)
                .await
                .map_err(RecordOperationEventError::StoreStatus)?
            {
                StatusStoreWrite::Stored { revision } => {
                    return Ok(RecordOperationEventOutcome::Stored {
                        stored,
                        status_write: OperationStatusWrite::Stored { revision },
                    });
                }
                StatusStoreWrite::Stale {
                    current_sequence,
                    attempted_sequence,
                } if current_sequence >= attempted_sequence => {
                    return Ok(RecordOperationEventOutcome::Stored {
                        stored,
                        status_write: OperationStatusWrite::Stale {
                            current_sequence,
                            attempted_sequence,
                        },
                    });
                }
                StatusStoreWrite::Stale { .. } | StatusStoreWrite::Contended { .. } => {
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
            PreCheck::None,
            validate_stored_operation_event,
        )
        .await
    }

    async fn record_deploy_event(
        &self,
        operation_id: &OperationId,
        attempted_append: OperationEventAppend,
    ) -> Result<RecordOperationEventOutcome, RecordOperationEventError> {
        let evidence = deploy_evidence_from_event(attempted_append.payload());
        self.record_operation_event_with_validator(
            operation_id,
            attempted_append,
            PreCheck::DeployEvidence(evidence),
            validate_stored_operation_event,
        )
        .await
    }

    async fn record_operation_event_with_validator(
        &self,
        operation_id: &OperationId,
        attempted_append: OperationEventAppend,
        pre_check: PreCheck,
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
        if let PreCheck::DeployEvidence(Some(evidence)) = &pre_check
            && current.is_terminal()
        {
            if let Some((stored, event)) = self
                .event_log
                .event_at_subject(attempted_append.subject())
                .await
                .map_err(RecordOperationEventError::AppendEvent)?
            {
                validate_stored(operation_id, &attempted_event, &event, stored.sequence)?;
                return self
                    .project_recorded_operation_event(operation_id, event, stored)
                    .await;
            }

            validate_fresh_deploy_evidence(&current, evidence)
                .map_err(RecordOperationEventError::ProjectStatus)?;
        }

        match project_operation_event(
            &current,
            attempted_event.clone(),
            current.next_event_sequence(),
        )
        .map_err(RecordOperationEventError::ProjectStatus)?
        {
            OperationProjection::StatusChanged { .. } => {}
            OperationProjection::AlreadySatisfied => {
                if let PreCheck::DeployEvidence(Some(evidence)) = &pre_check {
                    validate_fresh_deploy_evidence(&current, evidence)
                        .map_err(RecordOperationEventError::ProjectStatus)?;
                }

                return Ok(RecordOperationEventOutcome::AlreadySatisfied {
                    current_sequence: current.last_event_sequence(),
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

        self.project_recorded_operation_event(operation_id, event, stored)
            .await
    }
}

/// Extra validation a deploy event needs before projection: terminal
/// operations may still adopt or validate fresh deploy evidence.
enum PreCheck {
    None,
    DeployEvidence(Option<DeployEvidence>),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OperationStatusWrite {
    Stored {
        revision: KvRevision,
    },
    AlreadySatisfied {
        current_sequence: EventSequence,
    },
    Stale {
        current_sequence: EventSequence,
        attempted_sequence: EventSequence,
    },
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

#[derive(Debug, thiserror::Error)]
pub enum RecordOperationEventError {
    #[error("{0}")]
    LoadStatus(OperationStatusReadError),
    #[error("{0}")]
    StoreStatus(OperationStatusStoreError),
    #[error("operation record corrupt: missing operation {}", .operation_id.as_str())]
    MissingOperation { operation_id: OperationId },
    #[error("operation status projection failed: {0}")]
    ProjectStatus(StatusProjectionError),
    #[error("{0}")]
    AppendEvent(OperationEventLogError),
    #[error(
        "stored {} mismatch for {} at sequence {}",
        .kind.noun(),
        .operation_id.as_str(),
        .sequence.get()
    )]
    StoredEventMismatch {
        operation_id: OperationId,
        sequence: EventSequence,
        kind: StoredEventMismatchKind,
    },
    #[error("operation status projection contended")]
    StatusProjectionContended,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StoredEventMismatchKind {
    Generic,
    DeployPlan,
}

impl StoredEventMismatchKind {
    #[must_use]
    pub const fn noun(self) -> &'static str {
        match self {
            Self::Generic => "operation event",
            Self::DeployPlan => "deploy plan",
        }
    }
}

pub type RecordDeployTransitionError = RecordOperationEventError;
pub type RecordDeployEvidenceError = RecordOperationEventError;
pub type RecordLifecycleEventError = RecordOperationEventError;
pub type RecordMachineAddEventError = RecordLifecycleEventError;
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
        kind: stored_event_mismatch_kind(operation_id, attempted_event, stored_event),
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
                machine_id: attempted_machine_id,
                ..
            },
            OperationEvent::MachineAddJoined {
                operation_id: stored_operation_id,
                machine_id: stored_machine_id,
                ..
            },
        ) if attempted_operation_id == operation_id
            && stored_operation_id == operation_id
            && attempted_machine_id == stored_machine_id
    ) {
        return Ok(());
    }

    Err(RecordOperationEventError::StoredEventMismatch {
        operation_id: operation_id.clone(),
        sequence,
        kind: StoredEventMismatchKind::Generic,
    })
}

fn stored_event_mismatch_kind(
    operation_id: &OperationId,
    attempted_event: &OperationEvent,
    stored_event: &OperationEvent,
) -> StoredEventMismatchKind {
    if matches!(
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
    ) {
        StoredEventMismatchKind::DeployPlan
    } else {
        StoredEventMismatchKind::Generic
    }
}

fn deploy_evidence_from_event(event: &OperationEvent) -> Option<DeployEvidence> {
    match event {
        OperationEvent::DeployPlanCreated { plan, .. } => {
            Some(DeployEvidence::PlanCreated { plan: plan.clone() })
        }
        OperationEvent::DeployDataplanePrepared { report, .. } => {
            Some(DeployEvidence::DataplanePrepared {
                report: report.clone(),
            })
        }
        OperationEvent::DeployContainerStarted {
            machine_id,
            container_id,
            ..
        } => Some(DeployEvidence::ContainerStarted {
            machine_id: machine_id.clone(),
            container_id: container_id.clone(),
        }),
        OperationEvent::DeployHealthCheckStarted { .. } => Some(DeployEvidence::HealthCheckStarted),
        OperationEvent::DeployCleanupFinished {
            removed, failed, ..
        } => Some(DeployEvidence::CleanupFinished {
            removed: removed.clone(),
            failed: failed.clone(),
        }),
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
        | OperationEvent::MachineAddCredentialProvisioned { .. }
        | OperationEvent::MachineAddCompleted { .. }
        | OperationEvent::MachineAddFailed { .. }
        | OperationEvent::MachineUpdateSubmitted { .. }
        | OperationEvent::MachineUpdateRunning { .. }
        | OperationEvent::MachineUpdateCompleted { .. }
        | OperationEvent::MachineUpdateFailed { .. }
        | OperationEvent::Cancelled { .. } => None,
    }
}

#[derive(Debug)]
pub enum ReplayOperationEventsError {
    LoadStatus(OperationStatusReadError),
    ReadEvents(OperationEventReplayReadError),
    MissingOperation { operation_id: OperationId },
}
