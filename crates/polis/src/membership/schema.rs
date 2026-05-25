use super::model::machine_row_from_store_row;
use super::{
    IslandId, MachineRow, MembershipLifecycle, OverlayIp, RowEpoch, StoreMachineId,
    WireGuardPublicKey,
};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::{
    CorrosionStore, IrohEndpointId, StoreParam, StoreQueryRows, StoreResult, StoreStatement,
    StoreTimeout,
};

const MACHINE_COLUMNS: &str = "machine_id,
            island_id,
            iroh_endpoint_id,
            wireguard_public_key,
            overlay_ip,
            lifecycle,
            epoch,
            updated_at";

pub fn membership_schema_statements() -> StoreResult<Vec<StoreStatement>> {
    // Machine rows are owner-written product facts. The machine epoch is an
    // owner-issued row version, not a global conflict clock.
    include_str!("schema.sql")
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .map(StoreStatement::new)
        .collect()
}

pub async fn verify_membership_schema(
    store: &CorrosionStore,
    timeout: StoreTimeout,
) -> StoreResult<()> {
    verify_machine_columns(store, timeout).await?;
    verify_machine_lifecycle_index(store, timeout).await?;
    verify_machine_write_path(store, timeout).await
}

#[must_use]
pub fn membership_startup_schema_sql() -> &'static str {
    // Corrosion v1.0 file-backed startup schemas require defaults on non-null
    // columns for forward compatibility. Keep those defaults out of the
    // canonical Polis schema and use them only for Corrosion startup topology;
    // product writes still provide every column.
    "CREATE TABLE IF NOT EXISTS machines (
    machine_id TEXT NOT NULL CHECK(length(trim(machine_id)) > 0),
    island_id TEXT NOT NULL DEFAULT 'unknown-island' CHECK(length(trim(island_id)) > 0),
    iroh_endpoint_id TEXT NOT NULL DEFAULT 'unknown-endpoint' CHECK(length(trim(iroh_endpoint_id)) > 0),
    wireguard_public_key TEXT NOT NULL DEFAULT 'unknown-wireguard' CHECK(length(trim(wireguard_public_key)) > 0),
    overlay_ip TEXT NOT NULL DEFAULT '0.0.0.0' CHECK(length(trim(overlay_ip)) > 0),
    lifecycle TEXT NOT NULL DEFAULT 'active' CHECK(lifecycle IN ('active', 'removing', 'tombstoned', 'conflicted', 'deleted')),
    epoch INTEGER NOT NULL DEFAULT 1 CHECK(epoch > 0),
    updated_at INTEGER NOT NULL DEFAULT 0 CHECK(updated_at >= 0),
    PRIMARY KEY(machine_id)
);

CREATE INDEX IF NOT EXISTS machines_lifecycle_idx
    ON machines(lifecycle);"
}

pub fn upsert_machine_statement(row: &MachineRow) -> StoreResult<StoreStatement> {
    // Machine rows are idempotent owner records. A conflicting existing row is
    // left untouched so Ployz can report a membership conflict instead of
    // pretending Corrosion gave us compare-and-set semantics.
    StoreStatement::with_params(
        "INSERT INTO machines (
            machine_id,
            island_id,
            iroh_endpoint_id,
            wireguard_public_key,
            overlay_ip,
            lifecycle,
            epoch,
            updated_at
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
        ON CONFLICT(machine_id) DO UPDATE SET
            island_id = excluded.island_id,
            iroh_endpoint_id = excluded.iroh_endpoint_id,
            wireguard_public_key = excluded.wireguard_public_key,
            overlay_ip = excluded.overlay_ip,
            lifecycle = excluded.lifecycle,
            epoch = excluded.epoch,
            updated_at = excluded.updated_at
        WHERE machines.island_id = excluded.island_id
          AND machines.iroh_endpoint_id = excluded.iroh_endpoint_id
          AND machines.wireguard_public_key = excluded.wireguard_public_key
          AND machines.overlay_ip = excluded.overlay_ip
          AND machines.lifecycle = excluded.lifecycle
          AND machines.epoch = excluded.epoch",
        vec![
            StoreParam::Text(row.machine_id.as_str().to_string()),
            StoreParam::Text(row.island_id.as_str().to_string()),
            StoreParam::Text(row.iroh_endpoint_id.as_str().to_string()),
            StoreParam::Text(row.wireguard_public_key.as_str().to_string()),
            StoreParam::Text(row.overlay_ip.as_str().to_string()),
            StoreParam::Text(row.lifecycle.as_str().to_string()),
            StoreParam::Integer(row.epoch.sql_value()),
            StoreParam::Integer(row.updated_at),
        ],
    )
}

async fn verify_machine_columns(store: &CorrosionStore, timeout: StoreTimeout) -> StoreResult<()> {
    let statement = StoreStatement::new(
        "SELECT name, type, \"notnull\" AS not_null, pk
        FROM pragma_table_info('machines')
        ORDER BY cid",
    )?;
    let rows = store.query(&statement, timeout).await?;
    let actual = rows
        .rows()
        .iter()
        .map(|row| {
            Ok((
                row.text("name")?.to_string(),
                row.text("type")?.to_string(),
                row.integer("not_null")?,
                row.integer("pk")?,
            ))
        })
        .collect::<StoreResult<Vec<_>>>()?;
    let expected = [
        ("machine_id", "TEXT", 1, 1),
        ("island_id", "TEXT", 1, 0),
        ("iroh_endpoint_id", "TEXT", 1, 0),
        ("wireguard_public_key", "TEXT", 1, 0),
        ("overlay_ip", "TEXT", 1, 0),
        ("lifecycle", "TEXT", 1, 0),
        ("epoch", "INTEGER", 1, 0),
        ("updated_at", "INTEGER", 1, 0),
    ];
    if actual
        == expected
            .into_iter()
            .map(|(name, ty, not_null, pk)| (name.to_string(), ty.to_string(), not_null, pk))
            .collect::<Vec<_>>()
    {
        Ok(())
    } else {
        Err(crate::StoreError::MalformedPayload)
    }
}

async fn verify_machine_lifecycle_index(
    store: &CorrosionStore,
    timeout: StoreTimeout,
) -> StoreResult<()> {
    let statement = StoreStatement::new(
        "SELECT name
        FROM sqlite_schema
        WHERE type = 'index'
          AND tbl_name = 'machines'
          AND name = 'machines_lifecycle_idx'",
    )?;
    let rows = store.query(&statement, timeout).await?;
    if rows.rows().first().and_then(|row| row.text("name").ok()) == Some("machines_lifecycle_idx") {
        Ok(())
    } else {
        Err(crate::StoreError::MalformedPayload)
    }
}

async fn verify_machine_write_path(
    store: &CorrosionStore,
    timeout: StoreTimeout,
) -> StoreResult<()> {
    let machine_id = schema_probe_machine_id()?;
    let cleanup = StoreStatement::with_params(
        "DELETE FROM machines WHERE machine_id = ?1",
        vec![StoreParam::Text(machine_id.as_str().to_string())],
    )?;
    let row = MachineRow::new(
        machine_id,
        IslandId::parse("schema-probe-island").map_err(|_| crate::StoreError::MalformedPayload)?,
        IrohEndpointId::parse("schema-probe-endpoint")
            .map_err(|_| crate::StoreError::MalformedPayload)?,
        WireGuardPublicKey::parse("schema-probe-wireguard")
            .map_err(|_| crate::StoreError::MalformedPayload)?,
        OverlayIp::parse("127.0.0.1").map_err(|_| crate::StoreError::MalformedPayload)?,
        MembershipLifecycle::Active,
        RowEpoch::new(1).map_err(|_| crate::StoreError::MalformedPayload)?,
        0,
    );
    let insert = upsert_machine_statement(&row)?;
    store
        .execute_transaction(&[cleanup.clone(), insert, cleanup], timeout)
        .await?;
    Ok(())
}

fn schema_probe_machine_id() -> StoreResult<StoreMachineId> {
    let elapsed = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|_| crate::StoreError::MalformedPayload)?;
    StoreMachineId::parse(format!(
        "polis-schema-probe-{}-{}",
        std::process::id(),
        elapsed.as_nanos()
    ))
    .map_err(|_| crate::StoreError::MalformedPayload)
}

#[derive(Debug, Clone)]
pub struct MachineRowQuery {
    statement: StoreStatement,
}

impl MachineRowQuery {
    pub fn by_machine_id(machine_id: &StoreMachineId) -> StoreResult<Self> {
        Self::with_sql(
            format!("SELECT {MACHINE_COLUMNS} FROM machines WHERE machine_id = ?1"),
            vec![StoreParam::Text(machine_id.as_str().to_string())],
        )
    }

    pub fn active_by_machine_id(machine_id: &StoreMachineId) -> StoreResult<Self> {
        Self::with_sql(
            format!(
                "SELECT {MACHINE_COLUMNS}
        FROM machines
        WHERE machine_id = ?1
          AND lifecycle = 'active'"
            ),
            vec![StoreParam::Text(machine_id.as_str().to_string())],
        )
    }

    fn with_sql(sql: impl Into<String>, params: Vec<StoreParam>) -> StoreResult<Self> {
        StoreStatement::with_params(sql, params).map(|statement| Self { statement })
    }

    #[must_use]
    pub fn statement(&self) -> &StoreStatement {
        &self.statement
    }

    pub fn decode_rows(&self, rows: &StoreQueryRows) -> StoreResult<Vec<MachineRow>> {
        rows.rows().iter().map(machine_row_from_store_row).collect()
    }

    pub fn decode_optional(&self, rows: &StoreQueryRows) -> StoreResult<Option<MachineRow>> {
        let Some(row) = rows.rows().first() else {
            return Ok(None);
        };
        machine_row_from_store_row(row).map(Some)
    }
}
