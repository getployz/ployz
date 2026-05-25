use super::model::machine_row_from_store_row;
use super::{MachineRow, StoreMachineId};
use crate::{StoreParam, StoreQueryRows, StoreResult, StoreStatement};

const MACHINE_COLUMNS: &str = "machine_id,
            island_id,
            iroh_endpoint_id,
            wireguard_public_key,
            overlay_ip,
            lifecycle,
            epoch,
            updated_at";

pub fn membership_schema_statements() -> StoreResult<Vec<StoreStatement>> {
    include_str!("schema.sql")
        .split(';')
        .map(str::trim)
        .filter(|statement| !statement.is_empty())
        .map(StoreStatement::new)
        .collect()
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

#[derive(Debug, Clone)]
pub struct MachineRowQuery {
    statement: StoreStatement,
}

impl MachineRowQuery {
    #[must_use]
    pub fn by_machine_id(machine_id: &StoreMachineId) -> StoreResult<Self> {
        Self::with_sql(
            format!("SELECT {MACHINE_COLUMNS} FROM machines WHERE machine_id = ?1"),
            vec![StoreParam::Text(machine_id.as_str().to_string())],
        )
    }

    #[must_use]
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
        rows.rows()
            .iter()
            .map(|row| machine_row_from_store_row(row))
            .collect()
    }

    pub fn decode_optional(&self, rows: &StoreQueryRows) -> StoreResult<Option<MachineRow>> {
        let Some(row) = rows.rows().first() else {
            return Ok(None);
        };
        machine_row_from_store_row(row).map(Some)
    }
}
