//! Core-local operation evidence and working records, stored in SQLite.
//!
//! The operation projection (`operations`), the append-only event log
//! (`operation_events`), and the machine-add working records (the six index
//! tables) all live in the one core database. Every read is a query and every
//! mutation a transaction — there is no in-memory copy of this state. Business
//! outcomes (a duplicate submit, a stale projection, a fingerprint conflict)
//! travel back inside `Ok(...)`; only a genuine database failure is `Err`.

use crate::core_store::{
    CoreStore, CoreStoreError, from_json, query_json, query_json_list, to_json,
};
use ployz_core::ids::OperationId;
use ployz_core::ops::{
    EventSequence, OperationEvent, OperationEventReplayCursor, OperationEventReplayLimit,
    OperationEventReplayPage, OperationProjection, OperationStatus, StatusProjectionError,
    project_operation_event, validate_fresh_deploy_evidence,
};
use ployz_core::subjects::operation_progress_subject;
use rusqlite::{Connection, ErrorCode, OptionalExtension, params};

mod action;
mod cert;
mod core_replace;
mod credential_grant;
mod dataplane_staging;
mod deploy;
mod ingress_configure;
mod ingress_refresh;
mod machine_add;
mod machine_lifecycle;
mod machine_update;
mod managed_dns_reconcile;
mod namespace_remove;
mod network_repair;
mod operation;
mod service_restart;
mod types;
mod volume_remove;
use action::OperationAction;
use machine_add::{duplicate_machine_add_joined, scrub_failed_machine_add_secrets};
pub use types::*;

#[derive(Debug, Clone)]
pub struct OperationRepository {
    store: CoreStore,
    progress: async_nats::Client,
}

impl OperationRepository {
    #[must_use]
    pub fn open(store: CoreStore, progress: async_nats::Client) -> Self {
        Self { store, progress }
    }

    #[must_use]
    pub(crate) const fn core_store(&self) -> &CoreStore {
        &self.store
    }

    async fn submit_operation<K: OperationAction>(
        &self,
        operation_id: OperationId,
        payload: K::Payload,
    ) -> Result<SubmittedOperation<K::Payload>, SubmitOperationError> {
        let closure_payload = payload.clone();
        let closure_id = operation_id.clone();
        let outcome = self
            .store
            .call(move |conn| submit_operation_txn::<K>(conn, closure_id, closure_payload))
            .await
            .map_err(store_status)?;
        self.finish_submit(operation_id, payload, outcome).await
    }

    async fn finish_submit<P>(
        &self,
        operation_id: OperationId,
        payload: P,
        outcome: SubmitTxn<P>,
    ) -> Result<SubmittedOperation<P>, SubmitOperationError>
    where
        P: Clone,
    {
        match outcome {
            SubmitTxn::DuplicateSequenceMismatch { sequence } => {
                Err(SubmitOperationError::DuplicateSequenceMismatch { sequence })
            }
            SubmitTxn::Existing {
                start_sequence,
                payload,
                should_start_execution,
            } => Ok(SubmittedOperation {
                operation_id,
                start_sequence,
                payload,
                should_start_execution,
            }),
            SubmitTxn::Created {
                start_sequence,
                event,
                status,
            } => {
                publish_progress(&self.progress, event, &status).await;
                Ok(SubmittedOperation {
                    operation_id,
                    start_sequence,
                    payload,
                    should_start_execution: true,
                })
            }
        }
    }

    pub async fn get(
        &self,
        operation_id: &OperationId,
    ) -> Result<Option<OperationStatus>, OperationStatusStoreError> {
        let operation_id = operation_id.clone();
        self.store
            .call(move |conn| select_status(conn, &operation_id))
            .await
            .map_err(|error| index_error(&error))
    }
}

// -------- transaction bodies (run inside CoreStore::call) --------

enum RecordTxn {
    Missing,
    InvalidNextSequence(ployz_core::ops::NextEventSequenceError),
    Projection(StatusProjectionError),
    AlreadySatisfied {
        current_sequence: EventSequence,
        status: Box<OperationStatus>,
    },
    Stored {
        sequence: EventSequence,
        event: Box<OperationEvent>,
        status: Box<OperationStatus>,
    },
}

fn record_operation_event_txn(
    conn: &mut Connection,
    operation_id: &OperationId,
    event: OperationEvent,
) -> Result<RecordTxn, rusqlite::Error> {
    let transaction = conn.transaction()?;
    let outcome = record_operation_event_in_txn(&transaction, operation_id, event)?;
    if matches!(&outcome, RecordTxn::Stored { .. }) {
        transaction.commit()?;
    }
    Ok(outcome)
}

fn record_operation_event_in_txn(
    conn: &Connection,
    operation_id: &OperationId,
    event: OperationEvent,
) -> Result<RecordTxn, rusqlite::Error> {
    let Some(current) = select_status(conn, operation_id)? else {
        return Ok(RecordTxn::Missing);
    };
    // The next sequence is the projection's own: status is authoritative and
    // the transaction holds the write, so the insert lands at this sequence.
    let sequence = match current.next_event_sequence() {
        Ok(sequence) => sequence,
        Err(error) => return Ok(RecordTxn::InvalidNextSequence(error)),
    };
    // Deploy evidence recorded after a terminal deploy must be fresh.
    if let Some(evidence) = event.deploy_evidence()
        && current.is_terminal()
        && let Err(error) = validate_fresh_deploy_evidence(&current, &evidence)
    {
        return Ok(RecordTxn::Projection(error));
    }
    if duplicate_machine_add_joined(&current, &event) {
        return Ok(RecordTxn::AlreadySatisfied {
            current_sequence: current.last_event_sequence(),
            status: Box::new(current),
        });
    }
    let projection = match project_operation_event(&current, event.clone(), sequence) {
        Ok(projection) => projection,
        Err(error) => return Ok(RecordTxn::Projection(error)),
    };
    let OperationProjection::StatusChanged { status } = projection else {
        return Ok(RecordTxn::AlreadySatisfied {
            current_sequence: current.last_event_sequence(),
            status: Box::new(current),
        });
    };
    let status = *status;
    let subject = event.singleton_subject();
    if let Err(error) = insert_event(conn, &event, sequence) {
        if is_unique_constraint(&error)
            && singleton_event_exists(conn, event.operation_id(), subject)?
        {
            return Ok(RecordTxn::AlreadySatisfied {
                current_sequence: current.last_event_sequence(),
                status: Box::new(current),
            });
        }
        return Err(error);
    }
    upsert_status(conn, operation_id, &status)?;
    scrub_failed_machine_add_secrets(conn, &status)?;
    Ok(RecordTxn::Stored {
        sequence,
        event: Box::new(event),
        status: Box::new(status),
    })
}

// A transient result enum: built then immediately matched inside one transaction,
// never held in bulk, so the variant-size spread the lint optimizes for is moot here.
#[allow(clippy::large_enum_variant)]
enum SubmitTxn<P> {
    DuplicateSequenceMismatch {
        sequence: EventSequence,
    },
    Existing {
        start_sequence: EventSequence,
        payload: P,
        should_start_execution: bool,
    },
    Created {
        start_sequence: EventSequence,
        event: OperationEvent,
        status: OperationStatus,
    },
}

fn submit_operation_txn<K: OperationAction>(
    conn: &mut Connection,
    operation_id: OperationId,
    payload: K::Payload,
) -> Result<SubmitTxn<K::Payload>, rusqlite::Error> {
    let transaction = conn.transaction()?;
    if let Some(existing) = existing_submit::<K>(&transaction, &operation_id)? {
        return Ok(existing);
    }
    let created = create_submit::<K>(&transaction, operation_id, payload)?;
    transaction.commit()?;
    Ok(created)
}

fn existing_submit<K: OperationAction>(
    conn: &Connection,
    operation_id: &OperationId,
) -> Result<Option<SubmitTxn<K::Payload>>, rusqlite::Error> {
    let Some(current) = select_status(conn, operation_id)? else {
        return Ok(None);
    };
    if current.kind() != K::KIND {
        return Ok(Some(SubmitTxn::DuplicateSequenceMismatch {
            sequence: current.last_event_sequence(),
        }));
    }
    let Some((first_event, first_sequence)) = select_first_event(conn, operation_id)? else {
        return Ok(Some(SubmitTxn::DuplicateSequenceMismatch {
            sequence: EventSequence::first(),
        }));
    };
    let Some((stored_operation_id, payload)) = K::submitted_event_parts(first_event) else {
        return Ok(Some(SubmitTxn::DuplicateSequenceMismatch {
            sequence: first_sequence,
        }));
    };
    if stored_operation_id != *operation_id {
        return Ok(Some(SubmitTxn::DuplicateSequenceMismatch {
            sequence: first_sequence,
        }));
    }
    Ok(Some(SubmitTxn::Existing {
        start_sequence: first_sequence,
        payload,
        should_start_execution: current.last_event_sequence() == first_sequence,
    }))
}

fn create_submit<K: OperationAction>(
    conn: &Connection,
    operation_id: OperationId,
    payload: K::Payload,
) -> Result<SubmitTxn<K::Payload>, rusqlite::Error> {
    let sequence = EventSequence::first();
    let event = K::submitted_event(operation_id.clone(), payload.clone());
    let status = K::accepted_status(operation_id.clone(), &payload, sequence);
    insert_event(conn, &event, sequence)?;
    upsert_status(conn, &operation_id, &status)?;
    Ok(SubmitTxn::Created {
        start_sequence: sequence,
        event,
        status,
    })
}

enum ReplayTxn {
    Missing,
    Invalid(OperationEventLogError),
    Page(OperationEventReplayPage),
}

fn replay_operation_events_txn(
    conn: &mut Connection,
    operation_id: &OperationId,
    start_sequence: EventSequence,
    limit: OperationEventReplayLimit,
) -> Result<ReplayTxn, rusqlite::Error> {
    let Some(status) = select_status(conn, operation_id)? else {
        return Ok(ReplayTxn::Missing);
    };
    let page_limit = limit.as_usize();
    let mut statement = conn.prepare(
        "SELECT sequence, event_json FROM operation_events
         WHERE operation_id = ?1 AND sequence >= ?2 ORDER BY sequence LIMIT ?3",
    )?;
    let rows = statement.query_map(
        params![
            operation_id.as_str(),
            start_sequence.get(),
            page_limit as i64
        ],
        |row| Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?)),
    )?;
    let mut events = Vec::new();
    for row in rows {
        let (sequence, event_json) = row?;
        let sequence = match EventSequence::try_new(sequence) {
            Ok(sequence) => sequence,
            Err(error) => {
                return Ok(ReplayTxn::Invalid(
                    OperationEventLogError::InvalidEventSequence { sequence, error },
                ));
            }
        };
        let event = match serde_json::from_str(&event_json) {
            Ok(event) => event,
            Err(error) => {
                return Ok(ReplayTxn::Invalid(OperationEventLogError::DecodeEvent(
                    error,
                )));
            }
        };
        events.push(ployz_core::ops::ReplayedOperationEvent { sequence, event });
    }
    if events.len() < page_limit {
        return Ok(finish_replay_page(
            OperationEventReplayPage::caught_up(events),
            status.is_terminal(),
        ));
    }
    let Some(next) = events
        .last()
        .and_then(|event| event.sequence.get().checked_add(1))
    else {
        return Ok(ReplayTxn::Invalid(
            OperationEventLogError::InvalidNextReplaySequence { sequence: u64::MAX },
        ));
    };
    let next = match EventSequence::try_new(next) {
        Ok(next) => next,
        Err(error) => {
            return Ok(ReplayTxn::Invalid(
                OperationEventLogError::InvalidEventSequence {
                    sequence: next,
                    error,
                },
            ));
        }
    };
    Ok(finish_replay_page(
        OperationEventReplayPage::more(events, next),
        status.is_terminal(),
    ))
}

fn finish_replay_page(page: OperationEventReplayPage, terminal: bool) -> ReplayTxn {
    let page = match (page.cursor, terminal) {
        (OperationEventReplayCursor::CaughtUp, true) => {
            OperationEventReplayPage::terminal(page.events)
        }
        (cursor, _) => OperationEventReplayPage {
            events: page.events,
            cursor,
        },
    };
    ReplayTxn::Page(page)
}

fn select_status(
    conn: &Connection,
    operation_id: &OperationId,
) -> Result<Option<OperationStatus>, rusqlite::Error> {
    query_json(
        conn,
        "SELECT status_json FROM operations WHERE operation_id = ?1",
        operation_id.as_str(),
    )
}

const OPS_LIST_READ_LIMIT: usize = 101;

fn select_all_statuses_newest_first(
    conn: &mut Connection,
    active_only: bool,
) -> Result<Vec<OperationStatus>, rusqlite::Error> {
    let mut statement =
        conn.prepare("SELECT status_json FROM operations ORDER BY created_order DESC")?;
    let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
    collect_list_statuses(rows, active_only)
}

fn select_statuses_before(
    conn: &mut Connection,
    before: &OperationId,
    active_only: bool,
) -> Result<Option<Vec<OperationStatus>>, rusqlite::Error> {
    let created_order = conn
        .query_row(
            "SELECT created_order FROM operations WHERE operation_id = ?1",
            [before.as_str()],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let Some(created_order) = created_order else {
        return Ok(None);
    };
    let mut statement = conn.prepare(
        "SELECT status_json FROM operations
         WHERE created_order < ?1
         ORDER BY created_order DESC",
    )?;
    let rows = statement.query_map([created_order], |row| row.get::<_, String>(0))?;
    Ok(Some(collect_list_statuses(rows, active_only)?))
}

fn collect_list_statuses<F>(
    rows: rusqlite::MappedRows<'_, F>,
    active_only: bool,
) -> Result<Vec<OperationStatus>, rusqlite::Error>
where
    F: FnMut(&rusqlite::Row<'_>) -> Result<String, rusqlite::Error>,
{
    let mut statuses = Vec::new();
    for row in rows {
        let status: OperationStatus = from_json(&row?)?;
        if active_only && status.is_terminal() {
            continue;
        }
        statuses.push(status);
        if statuses.len() == OPS_LIST_READ_LIMIT {
            break;
        }
    }
    Ok(statuses)
}

fn select_all_statuses(conn: &mut Connection) -> Result<Vec<OperationStatus>, rusqlite::Error> {
    query_json_list(
        conn,
        "SELECT status_json FROM operations ORDER BY operation_id",
    )
}

fn upsert_status(
    conn: &Connection,
    operation_id: &OperationId,
    status: &OperationStatus,
) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO operations (operation_id, status_json) VALUES (?1, ?2)
         ON CONFLICT(operation_id) DO UPDATE SET status_json = excluded.status_json",
        params![operation_id.as_str(), to_json(status)?],
    )?;
    Ok(())
}

fn insert_event(
    conn: &Connection,
    event: &OperationEvent,
    sequence: EventSequence,
) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO operation_events (operation_id, sequence, subject, event_json)
         VALUES (?1, ?2, ?3, ?4)",
        params![
            event.operation_id().as_str(),
            sequence.get(),
            event.singleton_subject(),
            to_json(event)?
        ],
    )?;
    Ok(())
}

fn singleton_event_exists(
    conn: &Connection,
    operation_id: &OperationId,
    subject: Option<&'static str>,
) -> Result<bool, rusqlite::Error> {
    let Some(subject) = subject else {
        return Ok(false);
    };
    conn.query_row(
        "SELECT EXISTS(
            SELECT 1 FROM operation_events
            WHERE operation_id = ?1 AND subject = ?2
         )",
        params![operation_id.as_str(), subject],
        |row| row.get(0),
    )
}

fn is_unique_constraint(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(sqlite, _)
            if sqlite.code == ErrorCode::ConstraintViolation
                && sqlite.extended_code == rusqlite::ffi::SQLITE_CONSTRAINT_UNIQUE
    )
}

fn select_first_event(
    conn: &Connection,
    operation_id: &OperationId,
) -> Result<Option<(OperationEvent, EventSequence)>, rusqlite::Error> {
    let Some((sequence, event_json)) = conn
        .query_row(
            "SELECT sequence, event_json FROM operation_events
             WHERE operation_id = ?1 ORDER BY sequence LIMIT 1",
            [operation_id.as_str()],
            |row| Ok((row.get::<_, u64>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()?
    else {
        return Ok(None);
    };
    let sequence = EventSequence::try_new(sequence).map_err(sequence_conversion)?;
    Ok(Some((from_json(&event_json)?, sequence)))
}

enum AdoptResult<V> {
    Value(V),
    Conflict(&'static str),
}

impl<V> AdoptResult<V> {
    fn into_value(self) -> Result<V, OperationStatusStoreError> {
        match self {
            Self::Value(value) => Ok(value),
            Self::Conflict(message) => Err(OperationStatusStoreError::CasConflict {
                message: message.to_owned(),
            }),
        }
    }
}

fn create_or_adopt_deploy_claim(
    conn: &Connection,
    key: &str,
    value: &StoredDeployClaim,
) -> Result<AdoptResult<StoredDeployClaim>, rusqlite::Error> {
    if let Some(existing) =
        query_json::<StoredDeployClaim>(conn, "SELECT json FROM deploy_claims WHERE key = ?1", key)?
    {
        return Ok(AdoptResult::Value(existing));
    }
    conn.execute(
        "INSERT INTO deploy_claims (key, json) VALUES (?1, ?2)",
        params![key, to_json(value)?],
    )?;
    Ok(AdoptResult::Value(value.clone()))
}

fn sequence_conversion(error: ployz_core::ops::EventSequenceError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Integer,
        format!("invalid event sequence: {error}").into(),
    )
}

pub(super) fn subject_token_conversion(
    error: ployz_core::ids::SubjectTokenError,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        format!("invalid subject token: {error}").into(),
    )
}

fn index_error(error: &CoreStoreError) -> OperationStatusStoreError {
    OperationStatusStoreError::Index {
        message: error.to_string(),
    }
}

fn read_event_error(error: &CoreStoreError) -> OperationEventLogError {
    OperationEventLogError::ReadEvent {
        message: error.to_string(),
    }
}

fn store_status(error: CoreStoreError) -> SubmitOperationError {
    SubmitOperationError::StoreStatus(index_error(&error))
}

async fn publish_progress(
    client: &async_nats::Client,
    event: OperationEvent,
    status: &OperationStatus,
) {
    let Ok(payload) = serde_json::to_vec(&event) else {
        return;
    };
    let subject = operation_progress_subject(
        &status.progress_scope(),
        event.operation_id(),
        &event.subject_suffix(),
    );
    let _ = client.publish(subject, payload.into()).await;
}

#[cfg(test)]
mod operation_query_tests;
