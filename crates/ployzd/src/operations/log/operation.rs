use super::{
    OperationRepository, RecordOperationEventError, RecordOperationEventOutcome, RecordTxn,
    ReplayOperationEventsError, ReplayTxn, index_error, publish_progress, read_event_error,
    record_operation_event_txn, replay_operation_events_txn, select_all_statuses, select_status,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{
    OperationEvent, OperationEventReplayPage, OperationEventReplayRequest, OperationStatus,
    OperationStatusSnapshot,
};

impl OperationRepository {
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
            RecordTxn::Projection(error) => Err(RecordOperationEventError::ProjectStatus(error)),
            RecordTxn::AlreadySatisfied {
                current_sequence,
                status,
            } => Ok(RecordOperationEventOutcome::AlreadySatisfied {
                current_sequence,
                status,
            }),
            RecordTxn::Stored {
                sequence,
                event,
                status,
            } => {
                publish_progress(&self.progress, event).await;
                Ok(RecordOperationEventOutcome::Stored { sequence, status })
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
