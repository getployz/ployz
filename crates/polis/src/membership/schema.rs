use super::model::machine_row_from_store_row;
use super::{IslandId, MachineRow, StoreMachineId};

use crate::{
    CorrosionStore, StoreParam, StorePrimaryKey, StoreQueryRows, StoreResult, StoreStatement,
    StoreTableColumn, StoreTimeout,
};

const MACHINE_TABLE: &str = "machines";
const MACHINE_LIFECYCLE_INDEX: &str = "machines_lifecycle_idx";
const MACHINE_COLUMNS: &[MachineColumn] = &[
    MachineColumn {
        name: "machine_id",
        sql_type: "TEXT",
        primary_key: StorePrimaryKey::order(2),
        check: "length(trim(machine_id)) > 0",
    },
    MachineColumn {
        name: "island_id",
        sql_type: "TEXT",
        primary_key: StorePrimaryKey::order(1),
        check: "length(trim(island_id)) > 0",
    },
    MachineColumn {
        name: "iroh_endpoint_id",
        sql_type: "TEXT",
        primary_key: StorePrimaryKey::None,
        check: "length(trim(iroh_endpoint_id)) > 0",
    },
    MachineColumn {
        name: "wireguard_public_key",
        sql_type: "TEXT",
        primary_key: StorePrimaryKey::None,
        check: "length(trim(wireguard_public_key)) > 0",
    },
    MachineColumn {
        name: "overlay_ip",
        sql_type: "TEXT",
        primary_key: StorePrimaryKey::None,
        check: "length(trim(overlay_ip)) > 0",
    },
    MachineColumn {
        name: "lifecycle",
        sql_type: "TEXT",
        primary_key: StorePrimaryKey::None,
        check: "lifecycle IN ('active', 'removing', 'tombstoned', 'conflicted', 'deleted')",
    },
    MachineColumn {
        name: "epoch",
        sql_type: "INTEGER",
        primary_key: StorePrimaryKey::None,
        check: "epoch > 0",
    },
];

#[derive(Debug, Clone, Copy)]
struct MachineColumn {
    name: &'static str,
    sql_type: &'static str,
    primary_key: StorePrimaryKey,
    check: &'static str,
}

pub fn membership_schema_statements() -> StoreResult<Vec<StoreStatement>> {
    // Machine rows are Corrosion membership substrate rows. The epoch is an
    // owner-issued row version, not a global conflict clock.
    crate::schema_statements(&membership_schema_sql(SchemaProfile::Strict))
}

pub async fn verify_membership_schema(
    store: &CorrosionStore,
    timeout: StoreTimeout,
) -> StoreResult<()> {
    verify_machine_columns(store, timeout, SchemaProfile::Strict).await?;
    verify_machine_lifecycle_index(store, timeout).await
}

#[must_use]
pub fn membership_replication_schema_sql() -> String {
    membership_schema_sql(SchemaProfile::CorrosionReplication)
}

pub async fn verify_membership_replication_schema(
    store: &CorrosionStore,
    timeout: StoreTimeout,
) -> StoreResult<()> {
    verify_machine_columns(store, timeout, SchemaProfile::CorrosionReplication).await?;
    verify_machine_lifecycle_index(store, timeout).await
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SchemaProfile {
    Strict,
    CorrosionReplication,
}

fn membership_schema_sql(profile: SchemaProfile) -> String {
    format!(
        "{}

CREATE INDEX IF NOT EXISTS {MACHINE_LIFECYCLE_INDEX}
    ON {MACHINE_TABLE}(lifecycle);",
        crate::create_table_sql(MACHINE_TABLE, &machine_schema_columns(profile)),
    )
}

fn machine_schema_columns(profile: SchemaProfile) -> Vec<StoreTableColumn> {
    MACHINE_COLUMNS
        .iter()
        .map(|column| {
            if profile == SchemaProfile::CorrosionReplication
                && column.primary_key == StorePrimaryKey::None
            {
                StoreTableColumn::nullable(
                    column.name,
                    column.sql_type,
                    column.primary_key,
                    Some(column.check),
                )
            } else {
                StoreTableColumn::new(
                    column.name,
                    column.sql_type,
                    column.primary_key,
                    Some(column.check),
                )
            }
        })
        .collect()
}

fn machine_select_columns() -> String {
    let columns = machine_schema_columns(SchemaProfile::Strict);
    crate::select_columns(&columns)
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
            epoch
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
        ON CONFLICT(island_id, machine_id) DO UPDATE SET
            iroh_endpoint_id = excluded.iroh_endpoint_id,
            wireguard_public_key = excluded.wireguard_public_key,
            overlay_ip = excluded.overlay_ip,
            lifecycle = excluded.lifecycle,
            epoch = excluded.epoch
        WHERE machines.iroh_endpoint_id = excluded.iroh_endpoint_id
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
        ],
    )
}

async fn verify_machine_columns(
    store: &CorrosionStore,
    timeout: StoreTimeout,
    profile: SchemaProfile,
) -> StoreResult<()> {
    crate::verify_table_schema(
        store,
        timeout,
        MACHINE_TABLE,
        &machine_schema_columns(profile),
    )
    .await
}

async fn verify_machine_lifecycle_index(
    store: &CorrosionStore,
    timeout: StoreTimeout,
) -> StoreResult<()> {
    let statement = StoreStatement::new(format!(
        "SELECT name
        FROM sqlite_schema
        WHERE type = 'index'
          AND tbl_name = '{MACHINE_TABLE}'
          AND name = '{MACHINE_LIFECYCLE_INDEX}'"
    ))?;
    let rows = store.query(&statement, timeout).await?;
    if rows.rows().first().and_then(|row| row.text("name").ok()) == Some(MACHINE_LIFECYCLE_INDEX) {
        Ok(())
    } else {
        Err(crate::StoreError::MalformedPayload)
    }
}

#[derive(Debug, Clone)]
pub struct MachineRowQuery {
    statement: StoreStatement,
}

impl MachineRowQuery {
    pub fn by_island_machine_id(
        island_id: &IslandId,
        machine_id: &StoreMachineId,
    ) -> StoreResult<Self> {
        let columns = machine_select_columns();
        Self::with_sql(
            format!(
                "SELECT {columns}
        FROM {MACHINE_TABLE}
        WHERE island_id = ?1
          AND machine_id = ?2
          AND iroh_endpoint_id IS NOT NULL
          AND wireguard_public_key IS NOT NULL
          AND overlay_ip IS NOT NULL
          AND lifecycle IS NOT NULL
          AND epoch IS NOT NULL"
            ),
            vec![
                StoreParam::Text(island_id.as_str().to_string()),
                StoreParam::Text(machine_id.as_str().to_string()),
            ],
        )
    }

    fn with_sql(sql: impl Into<String>, params: Vec<StoreParam>) -> StoreResult<Self> {
        StoreStatement::with_params(sql, params).map(|statement| Self { statement })
    }

    #[must_use]
    pub fn statement(&self) -> &StoreStatement {
        &self.statement
    }

    pub fn decode_optional(&self, rows: &StoreQueryRows) -> StoreResult<Option<MachineRow>> {
        let Some(row) = rows.rows().first() else {
            return Ok(None);
        };
        machine_row_from_store_row(row).map(Some)
    }
}
