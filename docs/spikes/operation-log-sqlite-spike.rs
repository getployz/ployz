//! Spike only: a SQLite-shaped replacement for `crates/ployzd/src/operation_log.rs`.
//!
//! Not wired into the build. The point is to show what code disappears if the
//! operation log becomes a small embedded database instead of JSONL plus a
//! hand-maintained working-records index.

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use serde::{de::DeserializeOwned, Serialize};

use ployz_core::ids::{MachineId, OperationId};
use ployz_core::machine::{JoinTokenFingerprint, RawJoinToken};
use ployz_core::ops::{
    project_operation_event, EventSequence, OperationEvent, OperationIdempotencyKey, OperationKind,
    OperationProjection, OperationStatus, ReplayedOperationEvent,
};

const SCHEMA: &str = r#"
pragma foreign_keys = on;
pragma journal_mode = wal;

create table if not exists operations (
  id text primary key,
  kind text not null,
  status_json text not null,
  last_sequence integer not null
);

create table if not exists operation_events (
  operation_id text not null references operations(id) on delete cascade,
  sequence integer not null,
  event_json text not null,
  primary key (operation_id, sequence)
);

create table if not exists idempotency_keys (
  kind text not null,
  key text not null,
  operation_id text not null references operations(id) on delete cascade,
  primary key (kind, key),
  unique (kind, operation_id)
);

create table if not exists machine_add_claims (
  idempotency_key text primary key,
  operation_id text not null unique,
  claim_json text not null
);

create table if not exists machine_add_join_tokens (
  fingerprint text primary key,
  idempotency_key text not null references machine_add_claims(idempotency_key)
);

-- Short-lived escrow, not durable evidence. Delete when the machine-add
-- operation is terminal.
create table if not exists machine_add_secret_escrow (
  idempotency_key text primary key references machine_add_claims(idempotency_key),
  secret_json text not null
);
"#;

pub struct SqliteOperationLog {
    conn: Connection,
}

impl SqliteOperationLog {
    pub fn open(path: impl AsRef<std::path::Path>) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.execute_batch(SCHEMA)?;
        Ok(Self { conn })
    }

    pub fn submit_operation<P>(
        &mut self,
        kind: OperationKind,
        idempotency_key: &OperationIdempotencyKey,
        operation_id: &OperationId,
        submitted_event: &OperationEvent,
        accepted_status: &OperationStatus,
        payload: &P,
    ) -> rusqlite::Result<Submitted<P>>
    where
        P: Clone + Serialize + DeserializeOwned,
    {
        let tx = self.conn.transaction()?;

        if let Some(existing_id) = lookup_idempotency(&tx, kind, idempotency_key)? {
            let (status, start_sequence) = load_operation(&tx, &existing_id)?;
            let payload = load_submission_payload(&tx, &existing_id)?;
            tx.commit()?;
            return Ok(Submitted {
                operation_id: existing_id,
                start_sequence,
                payload,
                should_start_execution: status.last_event_sequence() == start_sequence,
            });
        }

        let sequence = EventSequence::try_new(1).expect("first sequence is one");
        tx.execute(
            "insert into operations (id, kind, status_json, last_sequence) values (?1, ?2, ?3, ?4)",
            params![
                operation_id.as_str(),
                operation_kind_name(kind),
                json(accepted_status)?,
                sequence.get(),
            ],
        )?;
        tx.execute(
            "insert into operation_events (operation_id, sequence, event_json) values (?1, ?2, ?3)",
            params![
                operation_id.as_str(),
                sequence.get(),
                json(submitted_event)?
            ],
        )?;
        tx.execute(
            "insert into idempotency_keys (kind, key, operation_id) values (?1, ?2, ?3)",
            params![
                operation_kind_name(kind),
                idempotency_key.as_str(),
                operation_id.as_str()
            ],
        )?;
        store_submission_payload(&tx, operation_id, payload)?;

        tx.commit()?;
        Ok(Submitted {
            operation_id: operation_id.clone(),
            start_sequence: sequence,
            payload: payload.clone(),
            should_start_execution: true,
        })
    }

    pub fn record_event(
        &mut self,
        operation_id: &OperationId,
        event: &OperationEvent,
    ) -> rusqlite::Result<Option<EventSequence>> {
        let tx = self.conn.transaction()?;
        let (current, _) = load_operation(&tx, operation_id)?;
        let sequence = current.next_event_sequence();

        let OperationProjection::StatusChanged { status } =
            project_operation_event(&current, event.clone(), sequence).map_err(sql_text)?
        else {
            tx.commit()?;
            return Ok(None);
        };

        tx.execute(
            "insert into operation_events (operation_id, sequence, event_json) values (?1, ?2, ?3)",
            params![operation_id.as_str(), sequence.get(), json(event)?],
        )?;
        tx.execute(
            "update operations set status_json = ?2, last_sequence = ?3 where id = ?1",
            params![
                operation_id.as_str(),
                json(status.as_ref())?,
                sequence.get()
            ],
        )?;
        tx.commit()?;
        Ok(Some(sequence))
    }

    pub fn replay(
        &self,
        operation_id: &OperationId,
        start: EventSequence,
        limit: u64,
    ) -> rusqlite::Result<Vec<ReplayedOperationEvent>> {
        let mut stmt = self.conn.prepare(
            "select sequence, event_json
             from operation_events
             where operation_id = ?1 and sequence >= ?2
             order by sequence
             limit ?3",
        )?;
        stmt.query_map(params![operation_id.as_str(), start.get(), limit], |row| {
            Ok(ReplayedOperationEvent {
                sequence: EventSequence::try_new(row.get::<_, u64>(0)?).map_err(sql_text)?,
                event: serde_json::from_str(&row.get::<_, String>(1)?).map_err(sql_text)?,
            })
        })?
        .collect()
    }

    pub fn put_machine_add_claim(
        &mut self,
        idempotency_key: &OperationIdempotencyKey,
        operation_id: &OperationId,
        join_fingerprint: &JoinTokenFingerprint,
        claim: &impl Serialize,
    ) -> rusqlite::Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "insert into machine_add_claims (idempotency_key, operation_id, claim_json)
             values (?1, ?2, ?3)
             on conflict(idempotency_key) do nothing",
            params![
                idempotency_key.as_str(),
                operation_id.as_str(),
                json(claim)?
            ],
        )?;
        tx.execute(
            "insert into machine_add_join_tokens (fingerprint, idempotency_key)
             values (?1, ?2)
             on conflict(fingerprint) do nothing",
            params![join_fingerprint.as_str(), idempotency_key.as_str()],
        )?;
        tx.commit()
    }

    pub fn delete_terminal_machine_add_secrets(
        &mut self,
        idempotency_key: &OperationIdempotencyKey,
    ) -> rusqlite::Result<()> {
        self.conn.execute(
            "delete from machine_add_secret_escrow where idempotency_key = ?1",
            params![idempotency_key.as_str()],
        )?;
        // If raw token redelivery is no longer needed once terminal, this is
        // where the claim row would be rewritten without it.
        Ok(())
    }
}

pub struct Submitted<P> {
    pub operation_id: OperationId,
    pub start_sequence: EventSequence,
    pub payload: P,
    pub should_start_execution: bool,
}

fn lookup_idempotency(
    tx: &Transaction<'_>,
    kind: OperationKind,
    key: &OperationIdempotencyKey,
) -> rusqlite::Result<Option<OperationId>> {
    tx.query_row(
        "select operation_id from idempotency_keys where kind = ?1 and key = ?2",
        params![operation_kind_name(kind), key.as_str()],
        |row| OperationId::try_new(row.get::<_, String>(0)?).map_err(sql_text),
    )
    .optional()
}

fn load_operation(
    tx: &Transaction<'_>,
    operation_id: &OperationId,
) -> rusqlite::Result<(OperationStatus, EventSequence)> {
    tx.query_row(
        "select status_json, last_sequence from operations where id = ?1",
        params![operation_id.as_str()],
        |row| {
            Ok((
                serde_json::from_str(&row.get::<_, String>(0)?).map_err(sql_text)?,
                EventSequence::try_new(row.get::<_, u64>(1)?).map_err(sql_text)?,
            ))
        },
    )
}

fn load_submission_payload<P: DeserializeOwned>(
    _tx: &Transaction<'_>,
    _operation_id: &OperationId,
) -> rusqlite::Result<P> {
    todo!("one small table per operation kind, or store submitted payload JSON with idempotency")
}

fn store_submission_payload<P: Serialize>(
    _tx: &Transaction<'_>,
    _operation_id: &OperationId,
    _payload: &P,
) -> rusqlite::Result<()> {
    todo!("one insert; shown as a stub because this is not the interesting part")
}

fn json(value: &impl Serialize) -> rusqlite::Result<String> {
    serde_json::to_string(value).map_err(sql_text)
}

fn sql_text(error: impl ToString) -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(error.to_string())))
}

fn operation_kind_name(kind: OperationKind) -> &'static str {
    match kind {
        OperationKind::Deploy => "deploy",
        OperationKind::MachineAdd => "machine_add",
        OperationKind::MachineUpdate => "machine_update",
        OperationKind::MachineLifecycle => "machine_lifecycle",
        OperationKind::Cert => "cert",
    }
}
