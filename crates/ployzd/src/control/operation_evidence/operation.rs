#[cfg(test)]
use super::select_all_statuses;
use super::{
    InterruptedOperationsSummary, OperationRepository, RecordOperationEventError,
    RecordOperationEventOutcome, RecordTxn, ReplayOperationEventsError, ReplayTxn, from_json,
    index_error, publish_progress, read_event_error, record_operation_event_in_txn,
    record_operation_event_txn, replay_operation_events_txn, select_all_statuses_newest_first,
    select_status, select_statuses_before,
};
use ployz_core::ids::OperationId;
use ployz_core::operation::{
    OperationEvent, OperationEventReplayPage, OperationEventReplayRequest,
    OperationInterruptionCause, OperationStatus, OperationStatusSnapshot,
};

impl OperationRepository {
    pub async fn record_interrupted_operation(
        &self,
        operation_id: &OperationId,
        cause: OperationInterruptionCause,
    ) -> Result<bool, super::OperationStatusStoreError> {
        let operation_id = operation_id.clone();
        let stored = self
            .store
            .call(move |conn| record_interrupted_operation_txn(conn, &operation_id, cause))
            .await
            .map_err(|error| index_error(&error))?;
        let Some((event, status)) = stored else {
            return Ok(false);
        };
        publish_progress(&self.progress, event, &status).await;
        Ok(true)
    }

    pub async fn record_interrupted_operations(
        &self,
        cause: OperationInterruptionCause,
    ) -> Result<InterruptedOperationsSummary, super::OperationStatusStoreError> {
        let stored = self
            .store
            .call(move |conn| record_interrupted_operations_txn(conn, cause))
            .await
            .map_err(|error| index_error(&error))?;
        let recorded = stored.len();
        for (event, status) in stored {
            publish_progress(&self.progress, event, &status).await;
        }
        Ok(InterruptedOperationsSummary { recorded })
    }

    pub(super) async fn record_operation_event(
        &self,
        operation_id: &OperationId,
        event: OperationEvent,
    ) -> Result<RecordOperationEventOutcome, RecordOperationEventError> {
        let closure_id = operation_id.clone();
        let outcome = self
            .store
            .call(move |conn| record_operation_event_txn(conn, &closure_id, event))
            .await
            .map_err(|error| RecordOperationEventError::StoreStatus(index_error(&error)))?;
        match outcome {
            RecordTxn::Missing => Err(RecordOperationEventError::MissingOperation {
                operation_id: operation_id.clone(),
            }),
            RecordTxn::InvalidNextSequence(error) => {
                Err(RecordOperationEventError::InvalidNextSequence(error))
            }
            RecordTxn::Projection(error) => Err(RecordOperationEventError::ProjectStatus(error)),
            RecordTxn::AlreadySatisfied {
                current_sequence,
                status,
            } => Ok(RecordOperationEventOutcome::AlreadySatisfied {
                current_sequence,
                status: *status,
            }),
            RecordTxn::Stored {
                sequence,
                event,
                status,
            } => {
                publish_progress(&self.progress, *event, &status).await;
                Ok(RecordOperationEventOutcome::Stored {
                    sequence,
                    status: *status,
                })
            }
        }
    }

    pub async fn operation_status_snapshot(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationStatusSnapshot>, super::OperationStatusStoreError> {
        let operation_id = operation_id.clone();
        self.store
            .call(move |conn| select_status(conn, &operation_id))
            .await
            .map_err(|error| index_error(&error))
            .map(|status| status.map(OperationStatusSnapshot::new))
    }

    pub async fn operation_statuses_newest_first(
        &self,
        active_only: bool,
    ) -> Result<Vec<OperationStatus>, super::OperationStatusStoreError> {
        self.store
            .call(move |conn| select_all_statuses_newest_first(conn, active_only))
            .await
            .map_err(|error| index_error(&error))
    }

    pub async fn operation_statuses_before(
        &self,
        before: &OperationId,
        active_only: bool,
    ) -> Result<Option<Vec<OperationStatus>>, super::OperationStatusStoreError> {
        let before = before.clone();
        self.store
            .call(move |conn| select_statuses_before(conn, &before, active_only))
            .await
            .map_err(|error| index_error(&error))
    }

    #[cfg(test)]
    pub async fn operation_statuses(
        &self,
    ) -> Result<Vec<OperationStatus>, super::OperationStatusStoreError> {
        self.store
            .call(select_all_statuses)
            .await
            .map_err(|error| index_error(&error))
    }

    pub async fn replay_operation_events(
        &self,
        request: OperationEventReplayRequest,
    ) -> Result<OperationEventReplayPage, ReplayOperationEventsError> {
        let OperationEventReplayRequest {
            operation_id,
            start_sequence,
            limit,
        } = request;
        let closure_id = operation_id.clone();
        let outcome = self
            .store
            .call(move |conn| replay_operation_events_txn(conn, &closure_id, start_sequence, limit))
            .await
            .map_err(|error| ReplayOperationEventsError::ReadEvents(read_event_error(&error)))?;
        match outcome {
            ReplayTxn::Missing => {
                Err(ReplayOperationEventsError::MissingOperation { operation_id })
            }
            ReplayTxn::Invalid(error) => Err(ReplayOperationEventsError::ReadEvents(error)),
            ReplayTxn::Page(page) => Ok(page),
        }
    }
}

fn record_interrupted_operation_txn(
    conn: &mut rusqlite::Connection,
    operation_id: &OperationId,
    cause: OperationInterruptionCause,
) -> Result<Option<(OperationEvent, OperationStatus)>, rusqlite::Error> {
    let transaction = conn.transaction()?;
    let Some(status) = select_status(&transaction, operation_id)? else {
        return Err(interruption_record_error(
            "operation disappeared during interruption recording",
        ));
    };
    let Some(evidence) = status.interruption_evidence(cause) else {
        return Ok(None);
    };
    let event = OperationEvent::OperationInterrupted {
        operation_id: operation_id.clone(),
        evidence,
    };
    let stored = match record_operation_event_in_txn(&transaction, operation_id, event.clone())? {
        RecordTxn::Stored { status, .. } => Some((event, *status)),
        RecordTxn::AlreadySatisfied { .. } => None,
        RecordTxn::Missing => {
            return Err(interruption_record_error(
                "operation disappeared during interruption recording",
            ));
        }
        RecordTxn::InvalidNextSequence(error) => {
            return Err(interruption_record_error(&error.to_string()));
        }
        RecordTxn::Projection(error) => {
            return Err(interruption_record_error(&error.to_string()));
        }
    };
    transaction.commit()?;
    Ok(stored)
}

fn record_interrupted_operations_txn(
    conn: &mut rusqlite::Connection,
    cause: OperationInterruptionCause,
) -> Result<Vec<(OperationEvent, OperationStatus)>, rusqlite::Error> {
    let transaction = conn.transaction()?;
    let statuses = {
        let mut statement = transaction.prepare("SELECT status_json FROM operations")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut statuses = Vec::new();
        for row in rows {
            statuses.push(from_json::<OperationStatus>(&row?)?);
        }
        statuses
    };
    let mut stored = Vec::new();
    for status in statuses {
        let Some(evidence) = status.interruption_evidence(cause) else {
            continue;
        };
        let event = OperationEvent::OperationInterrupted {
            operation_id: status.id().clone(),
            evidence,
        };
        match record_operation_event_in_txn(&transaction, status.id(), event.clone())? {
            RecordTxn::Stored { status, .. } => stored.push((event, *status)),
            RecordTxn::AlreadySatisfied { .. } => {}
            RecordTxn::Missing => {
                return Err(interruption_record_error(
                    "operation disappeared during recovery",
                ));
            }
            RecordTxn::InvalidNextSequence(error) => {
                return Err(interruption_record_error(&error.to_string()));
            }
            RecordTxn::Projection(error) => {
                return Err(interruption_record_error(&error.to_string()));
            }
        }
    }
    transaction.commit()?;
    Ok(stored)
}

fn interruption_record_error(message: &str) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(
        std::io::Error::other(format!("record interrupted operation: {message}")).into(),
    )
}
