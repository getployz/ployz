//! Operation ledger rows over Polis store primitives.

use std::time::SystemTime;

use crate::error::PrimitiveFailure;
use crate::operation::{
    IdempotencyKey, OperationEvent, OperationEventCursor, OperationEventKind,
    OperationEventSequence, OperationFailureCode, OperationId, OperationKind, OperationLease,
    OperationLeaseEpoch, OperationLedgerPort, OperationLifecycleStatus, OperationOwner,
    OperationPayloadFingerprint, OperationProgressEvent, OperationRecord, OperationStage,
    OperationSubmission, OperationSubmitResult, OperationTerminal, PrincipalId, ScopeId,
};

use super::scalars;

const OPERATIONS_TABLE: &str = "ployz_operations";
const OPERATION_EVENTS_TABLE: &str = "ployz_operation_events";

const OPERATION_COLUMNS: &[polis::StoreTableColumn] = &[
    polis::StoreTableColumn::new(
        "operation",
        "TEXT",
        polis::StorePrimaryKey::order(1),
        Some("length(trim(operation)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "kind",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("length(trim(kind)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "scope",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("length(trim(scope)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "idempotency",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("length(trim(idempotency)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "principal",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("length(trim(principal)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "payload_fingerprint",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("length(trim(payload_fingerprint)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "status",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("status IN ('running', 'succeeded', 'failed', 'interrupted')"),
    ),
    polis::StoreTableColumn::nullable(
        "stage",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("stage IS NULL OR length(trim(stage)) > 0"),
    ),
    polis::StoreTableColumn::nullable(
        "owner",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("owner IS NULL OR length(trim(owner)) > 0"),
    ),
    polis::StoreTableColumn::nullable(
        "lease_epoch",
        "INTEGER",
        polis::StorePrimaryKey::None,
        Some("lease_epoch IS NULL OR lease_epoch > 0"),
    ),
    polis::StoreTableColumn::nullable(
        "lease_renewed_at_secs",
        "INTEGER",
        polis::StorePrimaryKey::None,
        Some("lease_renewed_at_secs IS NULL OR lease_renewed_at_secs >= 0"),
    ),
    polis::StoreTableColumn::nullable(
        "lease_renewed_at_nanos",
        "INTEGER",
        polis::StorePrimaryKey::None,
        Some(
            "lease_renewed_at_nanos IS NULL OR (lease_renewed_at_nanos >= 0 AND lease_renewed_at_nanos < 1000000000)",
        ),
    ),
    polis::StoreTableColumn::nullable(
        "lease_expires_at_secs",
        "INTEGER",
        polis::StorePrimaryKey::None,
        Some("lease_expires_at_secs IS NULL OR lease_expires_at_secs >= 0"),
    ),
    polis::StoreTableColumn::nullable(
        "lease_expires_at_nanos",
        "INTEGER",
        polis::StorePrimaryKey::None,
        Some(
            "lease_expires_at_nanos IS NULL OR (lease_expires_at_nanos >= 0 AND lease_expires_at_nanos < 1000000000)",
        ),
    ),
    polis::StoreTableColumn::nullable(
        "terminal_kind",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("terminal_kind IS NULL OR terminal_kind IN ('succeeded', 'failed', 'interrupted')"),
    ),
    polis::StoreTableColumn::nullable(
        "terminal_code",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("terminal_code IS NULL OR length(trim(terminal_code)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "created_at_secs",
        "INTEGER",
        polis::StorePrimaryKey::None,
        Some("created_at_secs >= 0"),
    ),
    polis::StoreTableColumn::new(
        "created_at_nanos",
        "INTEGER",
        polis::StorePrimaryKey::None,
        Some("created_at_nanos >= 0 AND created_at_nanos < 1000000000"),
    ),
    polis::StoreTableColumn::new(
        "updated_at_secs",
        "INTEGER",
        polis::StorePrimaryKey::None,
        Some("updated_at_secs >= 0"),
    ),
    polis::StoreTableColumn::new(
        "updated_at_nanos",
        "INTEGER",
        polis::StorePrimaryKey::None,
        Some("updated_at_nanos >= 0 AND updated_at_nanos < 1000000000"),
    ),
];

const OPERATION_EVENT_COLUMNS: &[polis::StoreTableColumn] = &[
    polis::StoreTableColumn::new(
        "operation",
        "TEXT",
        polis::StorePrimaryKey::order(1),
        Some("length(trim(operation)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "sequence",
        "INTEGER",
        polis::StorePrimaryKey::order(2),
        Some("sequence > 0"),
    ),
    polis::StoreTableColumn::new(
        "event_kind",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("event_kind IN ('stage', 'terminal')"),
    ),
    polis::StoreTableColumn::nullable(
        "stage",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("stage IS NULL OR length(trim(stage)) > 0"),
    ),
    polis::StoreTableColumn::nullable(
        "terminal_kind",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("terminal_kind IS NULL OR terminal_kind IN ('succeeded', 'failed', 'interrupted')"),
    ),
    polis::StoreTableColumn::nullable(
        "terminal_code",
        "TEXT",
        polis::StorePrimaryKey::None,
        Some("terminal_code IS NULL OR length(trim(terminal_code)) > 0"),
    ),
    polis::StoreTableColumn::new(
        "occurred_at_secs",
        "INTEGER",
        polis::StorePrimaryKey::None,
        Some("occurred_at_secs >= 0"),
    ),
    polis::StoreTableColumn::new(
        "occurred_at_nanos",
        "INTEGER",
        polis::StorePrimaryKey::None,
        Some("occurred_at_nanos >= 0 AND occurred_at_nanos < 1000000000"),
    ),
];

#[derive(Clone)]
pub(crate) struct CorrosionOperationLedger {
    store: polis::CorrosionStore,
}

impl CorrosionOperationLedger {
    #[must_use]
    pub(crate) fn new(store: polis::CorrosionStore) -> Self {
        Self { store }
    }
}

pub(crate) fn start_corrosion_operation_ledger(
    store: polis::CorrosionStore,
) -> impl OperationLedgerPort + Clone + Send + Sync + 'static {
    CorrosionOperationLedger::new(store)
}

pub(crate) fn operation_schema_statements() -> Result<Vec<polis::StoreStatement>, polis::StoreError>
{
    polis::schema_statements(&schema_sql())
}

fn schema_sql() -> String {
    format!(
        "{};

CREATE UNIQUE INDEX IF NOT EXISTS ployz_operations_scope_idempotency
    ON {OPERATIONS_TABLE}(scope, idempotency);

{}",
        polis::create_table_sql(OPERATIONS_TABLE, OPERATION_COLUMNS),
        polis::create_table_sql(OPERATION_EVENTS_TABLE, OPERATION_EVENT_COLUMNS),
    )
}

pub(crate) async fn verify_operation_schema(
    store: &polis::CorrosionStore,
    timeout: polis::StoreTimeout,
) -> Result<(), polis::StoreError> {
    polis::verify_table_schema(store, timeout, OPERATIONS_TABLE, OPERATION_COLUMNS).await?;
    polis::verify_table_schema(
        store,
        timeout,
        OPERATION_EVENTS_TABLE,
        OPERATION_EVENT_COLUMNS,
    )
    .await?;
    verify_operation_idempotency_index(store, timeout).await
}

async fn verify_operation_idempotency_index(
    store: &polis::CorrosionStore,
    timeout: polis::StoreTimeout,
) -> Result<(), polis::StoreError> {
    let statement = polis::StoreStatement::new(format!(
        "SELECT name
        FROM sqlite_schema
        WHERE type = 'index'
          AND tbl_name = '{OPERATIONS_TABLE}'
          AND name = 'ployz_operations_scope_idempotency'"
    ))?;
    let rows = store.query(&statement, timeout).await?;
    if rows.rows().first().and_then(|row| row.text("name").ok())
        == Some("ployz_operations_scope_idempotency")
    {
        Ok(())
    } else {
        Err(polis::StoreError::MalformedPayload)
    }
}

impl OperationLedgerPort for CorrosionOperationLedger {
    async fn submit(
        &self,
        submission: &OperationSubmission,
        lease: &OperationLease,
        initial_stage: &OperationStage,
        now: SystemTime,
    ) -> Result<OperationSubmitResult, PrimitiveFailure> {
        if let Some(existing) = self
            .operation_by_scope_idempotency(submission.scope(), submission.idempotency())
            .await
            .map_err(map_operation_store_error)?
        {
            existing.submission().ensure_replay_matches(submission)?;
            return Ok(OperationSubmitResult::replayed(existing));
        }

        let initial = OperationRecord::running(
            submission.clone(),
            lease.clone(),
            initial_stage.clone(),
            now,
        );
        let initial_event = OperationEvent::new(
            submission.operation().clone(),
            OperationEventKind::stage(initial_stage.clone()),
            now,
        );
        let insert_result = self
            .store
            .execute_transaction(
                &[
                    insert_operation_statement(&initial).map_err(map_operation_store_error)?,
                    insert_event_statement(&initial_event).map_err(map_operation_store_error)?,
                ],
                polis::StoreTimeout::CONTROL_PLANE_DEFAULT,
            )
            .await;
        match insert_result {
            Ok(_) => Ok(OperationSubmitResult::created(initial)),
            Err(error) => {
                if let Some(existing) = self
                    .operation_by_scope_idempotency(submission.scope(), submission.idempotency())
                    .await
                    .map_err(map_operation_store_error)?
                {
                    existing.submission().ensure_replay_matches(submission)?;
                    return Ok(OperationSubmitResult::replayed(existing));
                }
                Err(map_operation_store_error(error))
            }
        }
    }

    async fn get(
        &self,
        operation: &OperationId,
    ) -> Result<Option<OperationRecord>, PrimitiveFailure> {
        self.operation_by_id(operation)
            .await
            .map_err(map_operation_store_error)
    }

    async fn append_event_kind(
        &self,
        operation: &OperationId,
        kind: &OperationProgressEvent,
        owner: &OperationOwner,
        epoch: OperationLeaseEpoch,
        now: SystemTime,
    ) -> Result<OperationEvent, PrimitiveFailure> {
        let record = self
            .operation_by_id(operation)
            .await
            .map_err(map_operation_store_error)?
            .ok_or(PrimitiveFailure::OperationInterrupted)?;
        record.ensure_checkpoint_allowed(owner, epoch, now)?;
        let sequence = self
            .next_event_sequence(operation)
            .await
            .map_err(map_operation_store_error)?;
        let event = OperationEvent::from_sequence(
            operation.clone(),
            sequence,
            kind.clone().into_event_kind(),
            now,
        )?;
        let receipt = self
            .store
            .execute_transaction(
                &[
                    update_operation_from_progress_statement(&event, owner, epoch, now)
                        .map_err(map_operation_store_error)?,
                    insert_event_after_progress_update_statement(&event, owner, epoch, now)
                        .map_err(map_operation_store_error)?,
                ],
                polis::StoreTimeout::CONTROL_PLANE_DEFAULT,
            )
            .await
            .map_err(map_operation_store_error)?;
        ensure_rows_affected(receipt, 2)?;
        Ok(event)
    }

    async fn events_after(
        &self,
        operation: &OperationId,
        cursor: Option<&OperationEventCursor>,
    ) -> Result<Vec<OperationEvent>, PrimitiveFailure> {
        if let Some(cursor) = cursor
            && cursor.operation() != operation
        {
            return Err(PrimitiveFailure::MalformedPayload);
        }
        self.events_after_cursor(operation, cursor)
            .await
            .map_err(map_operation_store_error)
    }

    async fn refresh_lease(
        &self,
        operation: &OperationId,
        owner: &OperationOwner,
        epoch: OperationLeaseEpoch,
        renewed_lease: &OperationLease,
    ) -> Result<(), PrimitiveFailure> {
        let record = self
            .operation_by_id(operation)
            .await
            .map_err(map_operation_store_error)?
            .ok_or(PrimitiveFailure::OperationInterrupted)?;
        record.ensure_checkpoint_allowed(owner, epoch, renewed_lease.renewed_at())?;
        if renewed_lease.owner() != owner || renewed_lease.epoch() != epoch {
            return Err(PrimitiveFailure::OperationInterrupted);
        }
        let receipt = self
            .store
            .execute_transaction(
                &[refresh_lease_statement(operation, renewed_lease)
                    .map_err(map_operation_store_error)?],
                polis::StoreTimeout::CONTROL_PLANE_DEFAULT,
            )
            .await
            .map_err(map_operation_store_error)?;
        ensure_rows_affected(receipt, 1)
    }

    async fn write_terminal(
        &self,
        operation: &OperationId,
        terminal: &OperationTerminal,
        owner: &OperationOwner,
        epoch: OperationLeaseEpoch,
        now: SystemTime,
    ) -> Result<OperationRecord, PrimitiveFailure> {
        let mut record = self
            .operation_by_id(operation)
            .await
            .map_err(map_operation_store_error)?
            .ok_or(PrimitiveFailure::OperationInterrupted)?;
        if record.terminal() == Some(terminal) {
            return Ok(record);
        }
        record.write_terminal(terminal.clone(), owner, epoch, now)?;
        let sequence = self
            .next_event_sequence(operation)
            .await
            .map_err(map_operation_store_error)?;
        let terminal_event = OperationEvent::from_sequence(
            operation.clone(),
            sequence,
            OperationEventKind::terminal(terminal.clone()),
            now,
        )?;
        let receipt = self
            .store
            .execute_transaction(
                &[
                    update_terminal_statement(&record, owner, epoch)
                        .map_err(map_operation_store_error)?,
                    insert_event_after_terminal_update_statement(
                        &terminal_event,
                        terminal,
                        record.status(),
                    )
                    .map_err(map_operation_store_error)?,
                ],
                polis::StoreTimeout::CONTROL_PLANE_DEFAULT,
            )
            .await
            .map_err(map_operation_store_error)?;
        ensure_rows_affected(receipt, 2)?;
        Ok(record)
    }
}

impl CorrosionOperationLedger {
    async fn operation_by_id(
        &self,
        operation: &OperationId,
    ) -> Result<Option<OperationRecord>, polis::StoreError> {
        let statement = polis::StoreStatement::with_params(
            format!(
                "SELECT {}
                FROM {OPERATIONS_TABLE}
                WHERE operation = ?1",
                polis::select_columns(OPERATION_COLUMNS)
            ),
            vec![polis::StoreParam::Text(operation.as_str().to_string())],
        )?;
        self.query_optional_operation(statement).await
    }

    async fn operation_by_scope_idempotency(
        &self,
        scope: &ScopeId,
        idempotency: &IdempotencyKey,
    ) -> Result<Option<OperationRecord>, polis::StoreError> {
        let statement = polis::StoreStatement::with_params(
            format!(
                "SELECT {}
                FROM {OPERATIONS_TABLE}
                WHERE scope = ?1
                  AND idempotency = ?2",
                polis::select_columns(OPERATION_COLUMNS)
            ),
            vec![
                polis::StoreParam::Text(scope.as_str().to_string()),
                polis::StoreParam::Text(idempotency.as_str().to_string()),
            ],
        )?;
        self.query_optional_operation(statement).await
    }

    async fn query_optional_operation(
        &self,
        statement: polis::StoreStatement,
    ) -> Result<Option<OperationRecord>, polis::StoreError> {
        let rows = self
            .store
            .query(&statement, polis::StoreTimeout::CONTROL_PLANE_DEFAULT)
            .await?;
        let Some(row) = rows.rows().first() else {
            return Ok(None);
        };
        decode_operation_record(row).map(Some)
    }

    async fn events_after_cursor(
        &self,
        operation: &OperationId,
        cursor: Option<&OperationEventCursor>,
    ) -> Result<Vec<OperationEvent>, polis::StoreError> {
        let after_sequence = cursor.map_or(0, |cursor| cursor.sequence().value());
        let statement = polis::StoreStatement::with_params(
            format!(
                "SELECT {}
                FROM {OPERATION_EVENTS_TABLE}
                WHERE operation = ?1
                  AND sequence > ?2
                ORDER BY sequence",
                polis::select_columns(OPERATION_EVENT_COLUMNS)
            ),
            vec![
                polis::StoreParam::Text(operation.as_str().to_string()),
                polis::StoreParam::Integer(scalars::i64_from_u64(after_sequence)?),
            ],
        )?;
        let rows = self
            .store
            .query(&statement, polis::StoreTimeout::CONTROL_PLANE_DEFAULT)
            .await?;
        rows.rows().iter().map(decode_operation_event).collect()
    }

    async fn next_event_sequence(
        &self,
        operation: &OperationId,
    ) -> Result<OperationEventSequence, polis::StoreError> {
        let statement = polis::StoreStatement::with_params(
            format!(
                "SELECT COALESCE(MAX(sequence), 0) AS max_sequence
                FROM {OPERATION_EVENTS_TABLE}
                WHERE operation = ?1"
            ),
            vec![polis::StoreParam::Text(operation.as_str().to_string())],
        )?;
        let rows = self
            .store
            .query(&statement, polis::StoreTimeout::CONTROL_PLANE_DEFAULT)
            .await?;
        let Some(row) = rows.rows().first() else {
            return Ok(OperationEventSequence::first());
        };
        let max_sequence = scalars::u64_from_i64(row.integer("max_sequence")?)?;
        if max_sequence == 0 {
            return Ok(OperationEventSequence::first());
        }
        OperationEventSequence::new(max_sequence)
            .and_then(OperationEventSequence::next)
            .map_err(map_primitive_payload_error)
    }
}

fn insert_operation_statement(
    record: &OperationRecord,
) -> Result<polis::StoreStatement, polis::StoreError> {
    let (created_secs, created_nanos) = time_parts(record.created_at())?;
    let (updated_secs, updated_nanos) = time_parts(record.updated_at())?;
    let lease = record.lease();
    let (lease_renewed_secs, lease_renewed_nanos) =
        optional_time_parts(lease.map(OperationLease::renewed_at))?;
    let (lease_expires_secs, lease_expires_nanos) =
        optional_time_parts(lease.map(OperationLease::expires_at))?;
    polis::StoreStatement::with_params(
        format!(
            "INSERT INTO {OPERATIONS_TABLE} (
            operation,
            kind,
            scope,
            idempotency,
            principal,
            payload_fingerprint,
            status,
            stage,
            owner,
            lease_epoch,
            lease_renewed_at_secs,
            lease_renewed_at_nanos,
            lease_expires_at_secs,
            lease_expires_at_nanos,
            terminal_kind,
            terminal_code,
            created_at_secs,
            created_at_nanos,
            updated_at_secs,
            updated_at_nanos
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20)"
        ),
        vec![
            polis::StoreParam::Text(record.operation().as_str().to_string()),
            polis::StoreParam::Text(record.kind().as_str().to_string()),
            polis::StoreParam::Text(record.scope().as_str().to_string()),
            polis::StoreParam::Text(record.idempotency().as_str().to_string()),
            polis::StoreParam::Text(record.principal().as_str().to_string()),
            polis::StoreParam::Text(record.payload_fingerprint().as_str().to_string()),
            polis::StoreParam::Text(status_tag(record.status()).to_string()),
            optional_text_param(record.current_stage().map(OperationStage::as_str)),
            optional_text_param(lease.map(OperationLease::owner).map(OperationOwner::as_str)),
            optional_integer_param(lease.map(|lease| lease.epoch().value()))?,
            optional_i64_param(lease_renewed_secs),
            optional_i64_param(lease_renewed_nanos),
            optional_i64_param(lease_expires_secs),
            optional_i64_param(lease_expires_nanos),
            polis::StoreParam::Null,
            polis::StoreParam::Null,
            polis::StoreParam::Integer(created_secs),
            polis::StoreParam::Integer(created_nanos),
            polis::StoreParam::Integer(updated_secs),
            polis::StoreParam::Integer(updated_nanos),
        ],
    )
}

fn insert_event_statement(
    event: &OperationEvent,
) -> Result<polis::StoreStatement, polis::StoreError> {
    let (occurred_secs, occurred_nanos) = time_parts(event.occurred_at())?;
    let stored = StoredEventKind::from_event(event.kind());
    polis::StoreStatement::with_params(
        format!(
            "INSERT INTO {OPERATION_EVENTS_TABLE} (
            operation,
            sequence,
            event_kind,
            stage,
            terminal_kind,
            terminal_code,
            occurred_at_secs,
            occurred_at_nanos
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
        ),
        vec![
            polis::StoreParam::Text(event.operation().as_str().to_string()),
            polis::StoreParam::Integer(scalars::i64_from_u64(event.sequence().value())?),
            polis::StoreParam::Text(stored.event_kind.to_string()),
            optional_text_param(stored.stage),
            optional_text_param(stored.terminal_kind),
            optional_text_param(stored.terminal_code),
            polis::StoreParam::Integer(occurred_secs),
            polis::StoreParam::Integer(occurred_nanos),
        ],
    )
}

fn update_operation_from_progress_statement(
    event: &OperationEvent,
    owner: &OperationOwner,
    epoch: OperationLeaseEpoch,
    now: SystemTime,
) -> Result<polis::StoreStatement, polis::StoreError> {
    let (updated_secs, updated_nanos) = time_parts(now)?;
    let OperationEventKind::Stage { stage } = event.kind() else {
        return Err(polis::StoreError::MalformedPayload);
    };
    polis::StoreStatement::with_params(
        format!(
            "UPDATE {OPERATIONS_TABLE}
            SET stage = ?2,
                updated_at_secs = ?5,
                updated_at_nanos = ?6
            WHERE operation = ?1
              AND status = 'running'
              AND terminal_kind IS NULL
              AND owner = ?3
              AND lease_epoch = ?4
              AND (
                lease_expires_at_secs > ?5
                OR (lease_expires_at_secs = ?5 AND lease_expires_at_nanos >= ?6)
              )"
        ),
        vec![
            polis::StoreParam::Text(event.operation().as_str().to_string()),
            polis::StoreParam::Text(stage.as_str().to_string()),
            polis::StoreParam::Text(owner.as_str().to_string()),
            polis::StoreParam::Integer(scalars::i64_from_u64(epoch.value())?),
            polis::StoreParam::Integer(updated_secs),
            polis::StoreParam::Integer(updated_nanos),
        ],
    )
}

fn insert_event_after_progress_update_statement(
    event: &OperationEvent,
    owner: &OperationOwner,
    epoch: OperationLeaseEpoch,
    now: SystemTime,
) -> Result<polis::StoreStatement, polis::StoreError> {
    let (updated_secs, updated_nanos) = time_parts(now)?;
    let (occurred_secs, occurred_nanos) = time_parts(event.occurred_at())?;
    let stored = StoredEventKind::from_event(event.kind());
    polis::StoreStatement::with_params(
        format!(
            "INSERT INTO {OPERATION_EVENTS_TABLE} (
                operation,
                sequence,
                event_kind,
                stage,
                terminal_kind,
                terminal_code,
                occurred_at_secs,
                occurred_at_nanos
            )
            SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8
            WHERE EXISTS (
                SELECT 1 FROM {OPERATIONS_TABLE}
                WHERE operation = ?1
                  AND status = 'running'
                  AND terminal_kind IS NULL
                  AND owner = ?9
                  AND lease_epoch = ?10
                  AND updated_at_secs = ?11
                  AND updated_at_nanos = ?12
            )"
        ),
        vec![
            polis::StoreParam::Text(event.operation().as_str().to_string()),
            polis::StoreParam::Integer(scalars::i64_from_u64(event.sequence().value())?),
            polis::StoreParam::Text(stored.event_kind.to_string()),
            optional_text_param(stored.stage),
            optional_text_param(stored.terminal_kind),
            optional_text_param(stored.terminal_code),
            polis::StoreParam::Integer(occurred_secs),
            polis::StoreParam::Integer(occurred_nanos),
            polis::StoreParam::Text(owner.as_str().to_string()),
            polis::StoreParam::Integer(scalars::i64_from_u64(epoch.value())?),
            polis::StoreParam::Integer(updated_secs),
            polis::StoreParam::Integer(updated_nanos),
        ],
    )
}

fn refresh_lease_statement(
    operation: &OperationId,
    lease: &OperationLease,
) -> Result<polis::StoreStatement, polis::StoreError> {
    let (renewed_secs, renewed_nanos) = time_parts(lease.renewed_at())?;
    let (expires_secs, expires_nanos) = time_parts(lease.expires_at())?;
    polis::StoreStatement::with_params(
        format!(
            "UPDATE {OPERATIONS_TABLE}
            SET owner = ?2,
                lease_epoch = ?3,
                lease_renewed_at_secs = ?4,
                lease_renewed_at_nanos = ?5,
                lease_expires_at_secs = ?6,
                lease_expires_at_nanos = ?7,
                updated_at_secs = ?4,
                updated_at_nanos = ?5
            WHERE operation = ?1
              AND status = 'running'
              AND terminal_kind IS NULL
              AND owner = ?2
              AND lease_epoch = ?3
              AND (
                lease_expires_at_secs > ?4
                OR (lease_expires_at_secs = ?4 AND lease_expires_at_nanos >= ?5)
              )"
        ),
        vec![
            polis::StoreParam::Text(operation.as_str().to_string()),
            polis::StoreParam::Text(lease.owner().as_str().to_string()),
            polis::StoreParam::Integer(scalars::i64_from_u64(lease.epoch().value())?),
            polis::StoreParam::Integer(renewed_secs),
            polis::StoreParam::Integer(renewed_nanos),
            polis::StoreParam::Integer(expires_secs),
            polis::StoreParam::Integer(expires_nanos),
        ],
    )
}

fn update_terminal_statement(
    record: &OperationRecord,
    owner: &OperationOwner,
    epoch: OperationLeaseEpoch,
) -> Result<polis::StoreStatement, polis::StoreError> {
    let Some(terminal) = record.terminal() else {
        return Err(polis::StoreError::MalformedPayload);
    };
    let (updated_secs, updated_nanos) = time_parts(record.updated_at())?;
    polis::StoreStatement::with_params(
        format!(
            "UPDATE {OPERATIONS_TABLE}
            SET status = ?2,
                owner = NULL,
                lease_epoch = NULL,
                lease_renewed_at_secs = NULL,
                lease_renewed_at_nanos = NULL,
                lease_expires_at_secs = NULL,
                lease_expires_at_nanos = NULL,
                terminal_kind = ?3,
                terminal_code = ?4,
                updated_at_secs = ?5,
                updated_at_nanos = ?6
            WHERE operation = ?1
              AND status = 'running'
              AND terminal_kind IS NULL
              AND owner = ?7
              AND lease_epoch = ?8
              AND (
                lease_expires_at_secs > ?5
                OR (lease_expires_at_secs = ?5 AND lease_expires_at_nanos >= ?6)
              )"
        ),
        vec![
            polis::StoreParam::Text(record.operation().as_str().to_string()),
            polis::StoreParam::Text(status_tag(record.status()).to_string()),
            polis::StoreParam::Text(terminal_kind_tag(terminal).to_string()),
            optional_text_param(terminal.code().map(OperationFailureCode::as_str)),
            polis::StoreParam::Integer(updated_secs),
            polis::StoreParam::Integer(updated_nanos),
            polis::StoreParam::Text(owner.as_str().to_string()),
            polis::StoreParam::Integer(scalars::i64_from_u64(epoch.value())?),
        ],
    )
}

fn insert_event_after_terminal_update_statement(
    event: &OperationEvent,
    terminal: &OperationTerminal,
    status: OperationLifecycleStatus,
) -> Result<polis::StoreStatement, polis::StoreError> {
    let (occurred_secs, occurred_nanos) = time_parts(event.occurred_at())?;
    let stored = StoredEventKind::from_event(event.kind());
    polis::StoreStatement::with_params(
        format!(
            "INSERT INTO {OPERATION_EVENTS_TABLE} (
                operation,
                sequence,
                event_kind,
                stage,
                terminal_kind,
                terminal_code,
                occurred_at_secs,
                occurred_at_nanos
            )
            SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8
            WHERE EXISTS (
                SELECT 1 FROM {OPERATIONS_TABLE}
                WHERE operation = ?1
                  AND status = ?9
                  AND terminal_kind = ?5
                  AND ((?6 IS NULL AND terminal_code IS NULL) OR terminal_code = ?6)
                  AND updated_at_secs = ?7
                  AND updated_at_nanos = ?8
            )"
        ),
        vec![
            polis::StoreParam::Text(event.operation().as_str().to_string()),
            polis::StoreParam::Integer(scalars::i64_from_u64(event.sequence().value())?),
            polis::StoreParam::Text(stored.event_kind.to_string()),
            optional_text_param(stored.stage),
            polis::StoreParam::Text(terminal_kind_tag(terminal).to_string()),
            optional_text_param(terminal.code().map(OperationFailureCode::as_str)),
            polis::StoreParam::Integer(occurred_secs),
            polis::StoreParam::Integer(occurred_nanos),
            polis::StoreParam::Text(status_tag(status).to_string()),
        ],
    )
}

fn decode_operation_record(row: &polis::StoreRow) -> Result<OperationRecord, polis::StoreError> {
    let submission = OperationSubmission::new(
        OperationId::parse(row.text("operation")?).map_err(map_primitive_payload_error)?,
        OperationKind::parse(row.text("kind")?).map_err(map_primitive_payload_error)?,
        ScopeId::parse(row.text("scope")?).map_err(map_primitive_payload_error)?,
        IdempotencyKey::parse(row.text("idempotency")?).map_err(map_primitive_payload_error)?,
        PrincipalId::parse(row.text("principal")?).map_err(map_primitive_payload_error)?,
        OperationPayloadFingerprint::parse(row.text("payload_fingerprint")?)
            .map_err(map_primitive_payload_error)?,
    );
    let status = status_from_tag(row.text("status")?)?;
    let stage = optional_text(row, "stage")?
        .map(OperationStage::parse)
        .transpose()
        .map_err(map_primitive_payload_error)?;
    let lease = decode_lease(row)?;
    let terminal = decode_terminal(row)?;
    OperationRecord::from_stored(
        submission,
        status,
        stage,
        lease,
        terminal,
        required_time(row, "created_at")?,
        required_time(row, "updated_at")?,
    )
    .map_err(map_primitive_payload_error)
}

fn decode_operation_event(row: &polis::StoreRow) -> Result<OperationEvent, polis::StoreError> {
    let operation =
        OperationId::parse(row.text("operation")?).map_err(map_primitive_payload_error)?;
    let sequence = OperationEventSequence::new(scalars::u64_from_i64(row.integer("sequence")?)?)
        .map_err(map_primitive_payload_error)?;
    let kind = decode_event_kind(row)?;
    OperationEvent::from_sequence(
        operation,
        sequence,
        kind,
        required_time(row, "occurred_at")?,
    )
    .map_err(map_primitive_payload_error)
}

fn decode_lease(row: &polis::StoreRow) -> Result<Option<OperationLease>, polis::StoreError> {
    let owner = optional_text(row, "owner")?;
    let epoch = optional_integer(row, "lease_epoch")?;
    let renewed_at = optional_time(row, "lease_renewed_at")?;
    let expires_at = optional_time(row, "lease_expires_at")?;
    match (owner, epoch, renewed_at, expires_at) {
        (None, None, None, None) => Ok(None),
        (Some(owner), Some(epoch), Some(renewed_at), Some(expires_at)) => {
            let epoch = OperationLeaseEpoch::new(scalars::u64_from_i64(epoch)?)
                .map_err(map_primitive_payload_error)?;
            OperationLease::new(
                OperationOwner::parse(owner).map_err(map_primitive_payload_error)?,
                epoch,
                renewed_at,
                expires_at,
            )
            .map(Some)
            .map_err(map_primitive_payload_error)
        }
        _ => Err(polis::StoreError::MalformedPayload),
    }
}

fn decode_terminal(row: &polis::StoreRow) -> Result<Option<OperationTerminal>, polis::StoreError> {
    let terminal_kind = optional_text(row, "terminal_kind")?;
    let terminal_code = optional_text(row, "terminal_code")?;
    terminal_from_parts(terminal_kind, terminal_code)
}

fn decode_event_kind(row: &polis::StoreRow) -> Result<OperationEventKind, polis::StoreError> {
    match row.text("event_kind")? {
        "stage" => {
            let Some(stage) = optional_text(row, "stage")? else {
                return Err(polis::StoreError::MalformedPayload);
            };
            Ok(OperationEventKind::stage(
                OperationStage::parse(stage).map_err(map_primitive_payload_error)?,
            ))
        }
        "terminal" => {
            let terminal = terminal_from_parts(
                optional_text(row, "terminal_kind")?,
                optional_text(row, "terminal_code")?,
            )?
            .ok_or(polis::StoreError::MalformedPayload)?;
            Ok(OperationEventKind::terminal(terminal))
        }
        _ => Err(polis::StoreError::MalformedPayload),
    }
}

fn terminal_from_parts(
    terminal_kind: Option<&str>,
    terminal_code: Option<&str>,
) -> Result<Option<OperationTerminal>, polis::StoreError> {
    match (terminal_kind, terminal_code) {
        (None, None) => Ok(None),
        (Some("succeeded"), None) => Ok(Some(OperationTerminal::succeeded())),
        (Some("failed"), Some(code)) => Ok(Some(OperationTerminal::failed(
            OperationFailureCode::parse(code).map_err(map_primitive_payload_error)?,
        ))),
        (Some("interrupted"), Some(code)) => Ok(Some(OperationTerminal::interrupted(
            OperationFailureCode::parse(code).map_err(map_primitive_payload_error)?,
        ))),
        _ => Err(polis::StoreError::MalformedPayload),
    }
}

struct StoredEventKind<'a> {
    event_kind: &'static str,
    stage: Option<&'a str>,
    terminal_kind: Option<&'static str>,
    terminal_code: Option<&'a str>,
}

impl<'a> StoredEventKind<'a> {
    fn from_event(kind: &'a OperationEventKind) -> Self {
        match kind {
            OperationEventKind::Stage { stage } => Self {
                event_kind: "stage",
                stage: Some(stage.as_str()),
                terminal_kind: None,
                terminal_code: None,
            },
            OperationEventKind::Terminal { terminal } => Self {
                event_kind: "terminal",
                stage: None,
                terminal_kind: Some(terminal_kind_tag(terminal)),
                terminal_code: terminal.code().map(OperationFailureCode::as_str),
            },
        }
    }
}

fn status_tag(status: OperationLifecycleStatus) -> &'static str {
    match status {
        OperationLifecycleStatus::Running => "running",
        OperationLifecycleStatus::Succeeded => "succeeded",
        OperationLifecycleStatus::Failed => "failed",
        OperationLifecycleStatus::Interrupted => "interrupted",
    }
}

fn status_from_tag(value: &str) -> Result<OperationLifecycleStatus, polis::StoreError> {
    match value {
        "running" => Ok(OperationLifecycleStatus::Running),
        "succeeded" => Ok(OperationLifecycleStatus::Succeeded),
        "failed" => Ok(OperationLifecycleStatus::Failed),
        "interrupted" => Ok(OperationLifecycleStatus::Interrupted),
        _ => Err(polis::StoreError::MalformedPayload),
    }
}

fn terminal_kind_tag(terminal: &OperationTerminal) -> &'static str {
    match terminal {
        OperationTerminal::Succeeded => "succeeded",
        OperationTerminal::Failed { .. } => "failed",
        OperationTerminal::Interrupted { .. } => "interrupted",
    }
}

fn required_time(row: &polis::StoreRow, prefix: &str) -> Result<SystemTime, polis::StoreError> {
    scalars::system_time_from_unix_parts(
        row.integer(&format!("{prefix}_secs"))?,
        row.integer(&format!("{prefix}_nanos"))?,
    )
}

fn optional_time(
    row: &polis::StoreRow,
    prefix: &str,
) -> Result<Option<SystemTime>, polis::StoreError> {
    match (
        optional_integer(row, &format!("{prefix}_secs"))?,
        optional_integer(row, &format!("{prefix}_nanos"))?,
    ) {
        (None, None) => Ok(None),
        (Some(secs), Some(nanos)) => scalars::system_time_from_unix_parts(secs, nanos).map(Some),
        _ => Err(polis::StoreError::MalformedPayload),
    }
}

fn time_parts(value: SystemTime) -> Result<(i64, i64), polis::StoreError> {
    scalars::unix_time_parts(value)
}

fn optional_time_parts(
    value: Option<SystemTime>,
) -> Result<(Option<i64>, Option<i64>), polis::StoreError> {
    value.map_or(Ok((None, None)), |value| {
        let (secs, nanos) = time_parts(value)?;
        Ok((Some(secs), Some(nanos)))
    })
}

fn optional_text<'a>(
    row: &'a polis::StoreRow,
    name: &str,
) -> Result<Option<&'a str>, polis::StoreError> {
    match row.value(name) {
        Some(polis::StoreValue::Null) | None => Ok(None),
        Some(polis::StoreValue::Text(value)) => Ok(Some(value.as_str())),
        Some(
            polis::StoreValue::Integer(_) | polis::StoreValue::Real(_) | polis::StoreValue::Blob(_),
        ) => Err(polis::StoreError::MalformedPayload),
    }
}

fn optional_integer(row: &polis::StoreRow, name: &str) -> Result<Option<i64>, polis::StoreError> {
    match row.value(name) {
        Some(polis::StoreValue::Null) | None => Ok(None),
        Some(polis::StoreValue::Integer(value)) => Ok(Some(*value)),
        Some(
            polis::StoreValue::Text(_) | polis::StoreValue::Real(_) | polis::StoreValue::Blob(_),
        ) => Err(polis::StoreError::MalformedPayload),
    }
}

fn optional_text_param(value: Option<&str>) -> polis::StoreParam {
    value.map_or(polis::StoreParam::Null, |value| {
        polis::StoreParam::Text(value.to_string())
    })
}

fn optional_integer_param(value: Option<u64>) -> Result<polis::StoreParam, polis::StoreError> {
    value.map_or(Ok(polis::StoreParam::Null), |value| {
        scalars::i64_from_u64(value).map(polis::StoreParam::Integer)
    })
}

fn optional_i64_param(value: Option<i64>) -> polis::StoreParam {
    value.map_or(polis::StoreParam::Null, polis::StoreParam::Integer)
}

fn map_primitive_payload_error(_error: PrimitiveFailure) -> polis::StoreError {
    polis::StoreError::MalformedPayload
}

fn map_operation_store_error(error: polis::StoreError) -> PrimitiveFailure {
    match error {
        polis::StoreError::MalformedPayload => PrimitiveFailure::MalformedPayload,
        polis::StoreError::Timeout => PrimitiveFailure::Timeout,
        polis::StoreError::MissedChange { .. }
        | polis::StoreError::Stream { .. }
        | polis::StoreError::Client { .. }
        | polis::StoreError::Response { .. }
        | polis::StoreError::QueryChangedBeforeEndOfQuery
        | polis::StoreError::QueryEndedBeforeEndOfQuery => PrimitiveFailure::NoResponder,
    }
}

fn ensure_rows_affected(
    receipt: polis::TransactionReceipt,
    expected: usize,
) -> Result<(), PrimitiveFailure> {
    if receipt.rows_affected() == expected {
        Ok(())
    } else {
        Err(PrimitiveFailure::OperationInterrupted)
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, UNIX_EPOCH};

    use super::*;
    use crate::operation::{OperationLiveness, OperationProgressEvent, OperationSubmitOutcome};

    #[test]
    fn schema_creates_operation_tables_only() {
        let conn = rusqlite::Connection::open_in_memory().expect("sqlite");
        for statement in operation_schema_statements().expect("schema") {
            conn.execute(statement.sql(), []).expect("schema statement");
        }

        let mut query = conn
            .prepare(
                "SELECT name FROM sqlite_schema
                WHERE type = 'table'
                  AND name LIKE 'ployz_operation%'
                ORDER BY name",
            )
            .expect("prepare");
        let tables = query
            .query_map([], |row| row.get::<_, String>(0))
            .expect("query")
            .collect::<Result<Vec<_>, _>>()
            .expect("tables");

        assert_eq!(
            tables,
            vec![
                "ployz_operation_events".to_string(),
                "ployz_operations".to_string()
            ]
        );

        let index = conn
            .query_row(
                "SELECT name FROM sqlite_schema
                WHERE type = 'index'
                  AND tbl_name = 'ployz_operations'
                  AND name = 'ployz_operations_scope_idempotency'",
                [],
                |row| row.get::<_, String>(0),
            )
            .expect("idempotency index");
        assert_eq!(index, "ployz_operations_scope_idempotency");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn submit_creates_record_and_initial_event() {
        let (_agent, ledger) = ledger_fixture().await;
        let submission = submission("op-1", "deploy.apply", "scope-a", "idem-a", "payload-a");
        let lease = lease("owner-a", 1, 10, 70);

        let submit = ledger
            .submit(&submission, &lease, &stage("submitted"), timestamp(10))
            .await
            .expect("submit");
        let record = submit.record();
        let events = ledger
            .events_after(submission.operation(), None)
            .await
            .expect("events");

        assert_eq!(submit.outcome(), OperationSubmitOutcome::Created);
        assert_eq!(record.operation(), submission.operation());
        assert_eq!(record.lease(), Some(&lease));
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sequence().value(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn duplicate_submit_replays_matching_operation_and_rejects_conflict() {
        let (_agent, ledger) = ledger_fixture().await;
        let submit = submission("op-1", "deploy.apply", "scope-a", "idem-a", "payload-a");
        let lease = lease("owner-a", 1, 10, 70);
        ledger
            .submit(&submit, &lease, &stage("submitted"), timestamp(10))
            .await
            .expect("submit");

        let replay = ledger
            .submit(&submit, &lease, &stage("submitted"), timestamp(11))
            .await
            .expect("replay");
        let conflict = submission("op-2", "deploy.apply", "scope-a", "idem-a", "payload-b");

        assert_eq!(replay.outcome(), OperationSubmitOutcome::Replayed);
        assert_eq!(replay.record().operation(), submit.operation());
        assert_eq!(
            ledger
                .submit(&conflict, &lease, &stage("submitted"), timestamp(12))
                .await,
            Err(PrimitiveFailure::OperationStateConflict)
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn events_after_cursor_returns_later_events_only() {
        let (_agent, ledger) = ledger_fixture().await;
        let submission = submission("op-1", "deploy.apply", "scope-a", "idem-a", "payload-a");
        let current_owner = owner("owner-a");
        let epoch = OperationLeaseEpoch::new(1).expect("epoch");
        ledger
            .submit(
                &submission,
                &lease("owner-a", 1, 10, 70),
                &stage("submitted"),
                timestamp(10),
            )
            .await
            .expect("submit");
        let first = ledger
            .events_after(submission.operation(), None)
            .await
            .expect("events")
            .remove(0);
        ledger
            .append_event_kind(
                submission.operation(),
                &OperationProgressEvent::stage(stage("running")),
                &current_owner,
                epoch,
                timestamp(20),
            )
            .await
            .expect("append");

        let events = ledger
            .events_after(submission.operation(), Some(&first.cursor()))
            .await
            .expect("events");

        assert_eq!(events.len(), 1);
        assert_eq!(events[0].sequence().value(), 2);
        assert_eq!(events[0].cursor().operation(), submission.operation());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lease_refresh_and_terminal_write_enforce_owner_epoch() {
        let (_agent, ledger) = ledger_fixture().await;
        let submission = submission("op-1", "deploy.apply", "scope-a", "idem-a", "payload-a");
        let current_owner = owner("owner-a");
        let epoch = OperationLeaseEpoch::new(1).expect("epoch");
        ledger
            .submit(
                &submission,
                &lease("owner-a", 1, 10, 70),
                &stage("submitted"),
                timestamp(10),
            )
            .await
            .expect("submit");
        ledger
            .refresh_lease(
                submission.operation(),
                &current_owner,
                epoch,
                &lease("owner-a", 1, 20, 80),
            )
            .await
            .expect("refresh");

        let record = ledger
            .get(submission.operation())
            .await
            .expect("get")
            .expect("record");
        assert_eq!(
            record.liveness_at(timestamp(81)),
            OperationLiveness::Expired {
                owner: current_owner.clone(),
                epoch,
                expired_at: timestamp(80),
            }
        );
        assert_eq!(
            ledger
                .write_terminal(
                    submission.operation(),
                    &OperationTerminal::succeeded(),
                    &owner("other"),
                    epoch,
                    timestamp(21),
                )
                .await,
            Err(PrimitiveFailure::OperationInterrupted)
        );

        let terminal = ledger
            .write_terminal(
                submission.operation(),
                &OperationTerminal::succeeded(),
                &current_owner,
                epoch,
                timestamp(21),
            )
            .await
            .expect("terminal");
        assert_eq!(terminal.terminal(), Some(&OperationTerminal::Succeeded));

        let event_count = ledger
            .events_after(submission.operation(), None)
            .await
            .expect("events")
            .len();
        let replay = ledger
            .write_terminal(
                submission.operation(),
                &OperationTerminal::succeeded(),
                &current_owner,
                epoch,
                timestamp(22),
            )
            .await
            .expect("terminal replay");
        let replay_event_count = ledger
            .events_after(submission.operation(), None)
            .await
            .expect("events")
            .len();

        assert_eq!(replay.terminal(), Some(&OperationTerminal::Succeeded));
        assert_eq!(replay_event_count, event_count);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn lease_refresh_rejects_stale_or_mismatched_driver() {
        let (_agent, ledger) = ledger_fixture().await;
        let submission = submission("op-1", "deploy.apply", "scope-a", "idem-a", "payload-a");
        let current_owner = owner("owner-a");
        let epoch = OperationLeaseEpoch::new(1).expect("epoch");
        ledger
            .submit(
                &submission,
                &lease("owner-a", 1, 10, 70),
                &stage("submitted"),
                timestamp(10),
            )
            .await
            .expect("submit");

        assert_eq!(
            ledger
                .refresh_lease(
                    submission.operation(),
                    &owner("owner-b"),
                    epoch,
                    &lease("owner-b", 1, 20, 80),
                )
                .await,
            Err(PrimitiveFailure::OperationInterrupted)
        );
        assert_eq!(
            ledger
                .refresh_lease(
                    submission.operation(),
                    &current_owner,
                    OperationLeaseEpoch::new(2).expect("epoch"),
                    &lease("owner-a", 2, 20, 80),
                )
                .await,
            Err(PrimitiveFailure::OperationInterrupted)
        );
        assert_eq!(
            ledger
                .refresh_lease(
                    submission.operation(),
                    &current_owner,
                    epoch,
                    &lease("owner-a", 1, 71, 90),
                )
                .await,
            Err(PrimitiveFailure::OperationInterrupted)
        );
    }

    async fn ledger_fixture() -> (polis::LocalCorrosionAgent, CorrosionOperationLedger) {
        let agent = polis::test_support::CorrosionAgentFixtureConfig::isolated()
            .expect("fixture")
            .start()
            .await
            .expect("corrosion");
        let store = agent.store().expect("store");
        store
            .execute_transaction(
                &operation_schema_statements().expect("schema"),
                polis::StoreTimeout::seconds(2).expect("timeout"),
            )
            .await
            .expect("schema");
        verify_operation_schema(&store, polis::StoreTimeout::seconds(2).expect("timeout"))
            .await
            .expect("verify schema");
        (agent, CorrosionOperationLedger::new(store))
    }

    fn submission(
        operation: &str,
        kind: &str,
        scope: &str,
        idempotency: &str,
        fingerprint: &str,
    ) -> OperationSubmission {
        OperationSubmission::new(
            OperationId::parse(operation).expect("operation"),
            OperationKind::parse(kind).expect("kind"),
            ScopeId::parse(scope).expect("scope"),
            IdempotencyKey::parse(idempotency).expect("idempotency"),
            PrincipalId::parse("principal-a").expect("principal"),
            OperationPayloadFingerprint::parse(fingerprint).expect("fingerprint"),
        )
    }

    fn stage(value: &str) -> OperationStage {
        OperationStage::parse(value).expect("stage")
    }

    fn owner(value: &str) -> OperationOwner {
        OperationOwner::parse(value).expect("owner")
    }

    fn lease(owner: &str, epoch: u64, renewed_at: u64, expires_at: u64) -> OperationLease {
        OperationLease::new(
            OperationOwner::parse(owner).expect("owner"),
            OperationLeaseEpoch::new(epoch).expect("epoch"),
            timestamp(renewed_at),
            timestamp(expires_at),
        )
        .expect("lease")
    }

    fn timestamp(seconds: u64) -> SystemTime {
        UNIX_EPOCH + Duration::from_secs(seconds)
    }
}
