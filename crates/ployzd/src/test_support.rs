use std::collections::BTreeMap;
use std::future::Future;
use std::sync::{Arc, Mutex};

use ployz::operation::{
    OperationEvent, OperationEventCursor, OperationId, OperationLease, OperationLeaseEpoch,
    OperationLedgerPort, OperationOwner, OperationProgressEvent, OperationRecord, OperationStage,
    OperationSubmission, OperationSubmitResult, OperationTerminal,
};

#[derive(Clone, Default)]
pub(crate) struct MemoryOperationLedger {
    inner: Arc<Mutex<MemoryOperationLedgerState>>,
}

#[derive(Default)]
struct MemoryOperationLedgerState {
    records: BTreeMap<OperationId, OperationRecord>,
    events: BTreeMap<OperationId, Vec<OperationEvent>>,
}

impl OperationLedgerPort for MemoryOperationLedger {
    fn submit<'a>(
        &'a self,
        submission: &'a OperationSubmission,
        lease: &'a OperationLease,
        initial_stage: &'a OperationStage,
        now: std::time::SystemTime,
    ) -> impl Future<Output = Result<OperationSubmitResult, ployz::PrimitiveFailure>> + Send + 'a
    {
        async move {
            let mut state = self
                .inner
                .lock()
                .map_err(|_| ployz::PrimitiveFailure::OperationStateConflict)?;
            if let Some(existing) = state.records.get(submission.operation()) {
                submission.ensure_replay_matches(existing.submission())?;
                return Ok(OperationSubmitResult::replayed(existing.clone()));
            }
            let record = OperationRecord::running(
                submission.clone(),
                lease.clone(),
                initial_stage.clone(),
                now,
            );
            let event = OperationEvent::new(
                submission.operation().clone(),
                ployz::operation::OperationEventKind::stage(initial_stage.clone()),
                now,
            );
            state
                .events
                .insert(submission.operation().clone(), vec![event]);
            state
                .records
                .insert(submission.operation().clone(), record.clone());
            Ok(OperationSubmitResult::created(record))
        }
    }

    fn get<'a>(
        &'a self,
        operation: &'a OperationId,
    ) -> impl Future<Output = Result<Option<OperationRecord>, ployz::PrimitiveFailure>> + Send + 'a
    {
        async move {
            let state = self
                .inner
                .lock()
                .map_err(|_| ployz::PrimitiveFailure::OperationStateConflict)?;
            Ok(state.records.get(operation).cloned())
        }
    }

    fn append_event_kind<'a>(
        &'a self,
        operation: &'a OperationId,
        kind: &'a OperationProgressEvent,
        owner: &'a OperationOwner,
        epoch: OperationLeaseEpoch,
        now: std::time::SystemTime,
    ) -> impl Future<Output = Result<OperationEvent, ployz::PrimitiveFailure>> + Send + 'a {
        async move {
            let mut state = self
                .inner
                .lock()
                .map_err(|_| ployz::PrimitiveFailure::OperationStateConflict)?;
            let record = state
                .records
                .get(operation)
                .ok_or(ployz::PrimitiveFailure::OperationStateConflict)?;
            record.ensure_checkpoint_allowed(owner, epoch, now)?;
            let events = state.events.entry(operation.clone()).or_default();
            let event = events
                .last()
                .ok_or(ployz::PrimitiveFailure::OperationStateConflict)?
                .next(kind.clone().into_event_kind(), now)?;
            events.push(event.clone());
            Ok(event)
        }
    }

    fn events_after<'a>(
        &'a self,
        operation: &'a OperationId,
        cursor: Option<&'a OperationEventCursor>,
    ) -> impl Future<Output = Result<Vec<OperationEvent>, ployz::PrimitiveFailure>> + Send + 'a
    {
        async move {
            let state = self
                .inner
                .lock()
                .map_err(|_| ployz::PrimitiveFailure::OperationStateConflict)?;
            let events = state.events.get(operation).cloned().unwrap_or_default();
            Ok(match cursor {
                Some(cursor) => events
                    .into_iter()
                    .filter(|event| event.sequence() > cursor.sequence())
                    .collect(),
                None => events,
            })
        }
    }

    fn refresh_lease<'a>(
        &'a self,
        operation: &'a OperationId,
        owner: &'a OperationOwner,
        epoch: OperationLeaseEpoch,
        _renewed_lease: &'a OperationLease,
    ) -> impl Future<Output = Result<(), ployz::PrimitiveFailure>> + Send + 'a {
        async move {
            let state = self
                .inner
                .lock()
                .map_err(|_| ployz::PrimitiveFailure::OperationStateConflict)?;
            let record = state
                .records
                .get(operation)
                .ok_or(ployz::PrimitiveFailure::OperationStateConflict)?;
            record.ensure_checkpoint_allowed(owner, epoch, std::time::SystemTime::now())
        }
    }

    fn write_terminal<'a>(
        &'a self,
        operation: &'a OperationId,
        terminal: &'a OperationTerminal,
        owner: &'a OperationOwner,
        epoch: OperationLeaseEpoch,
        now: std::time::SystemTime,
    ) -> impl Future<Output = Result<OperationRecord, ployz::PrimitiveFailure>> + Send + 'a {
        async move {
            let mut state = self
                .inner
                .lock()
                .map_err(|_| ployz::PrimitiveFailure::OperationStateConflict)?;
            let event = state
                .events
                .get(operation)
                .and_then(|events| events.last())
                .ok_or(ployz::PrimitiveFailure::OperationStateConflict)?
                .next(
                    ployz::operation::OperationEventKind::terminal(terminal.clone()),
                    now,
                )?;
            let record = state
                .records
                .get_mut(operation)
                .ok_or(ployz::PrimitiveFailure::OperationStateConflict)?;
            record.write_terminal(terminal.clone(), owner, epoch, now)?;
            let updated = record.clone();
            state
                .events
                .entry(operation.clone())
                .or_default()
                .push(event);
            Ok(updated)
        }
    }
}
