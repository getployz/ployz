//! Core-local machine roster, stored in SQLite.
//!
//! One `machines` row per active machine holds the whole `ActiveMachineState`,
//! lifecycle included — so a machine's lifecycle (drain/resume operator intent)
//! lives in exactly one place, read straight from the roster with no separate
//! overlay.

use crate::core_store::{CoreStore, CoreStoreError, query_json, query_json_list, to_json};
use ployz_core::ids::MachineId;
use ployz_core::state::{ActiveMachineState, MachineLifecycle};
use rusqlite::{Connection, params};

#[derive(Debug, Clone)]
pub struct MachineRosterStore {
    store: CoreStore,
}

/// Outcome of a lifecycle transition: the machine may not exist, the intent may
/// already hold, or the row changed.
pub enum MachineLifecycleUpdate {
    NoSuchMachine,
    Unchanged,
    Changed,
}

impl MachineRosterStore {
    #[must_use]
    pub fn new(store: CoreStore) -> Self {
        Self { store }
    }

    pub async fn replace_active_machine(
        &self,
        state: &ActiveMachineState,
    ) -> Result<(), MachineRosterStoreError> {
        let state = state.clone();
        self.store
            .call(move |conn| put_machine(conn, &state))
            .await
            .map_err(store_error)
    }

    pub async fn active_machine(
        &self,
        machine_id: &MachineId,
    ) -> Result<Option<ActiveMachineState>, MachineRosterStoreError> {
        let machine_id = machine_id.clone();
        self.store
            .call(move |conn| get_machine(conn, &machine_id))
            .await
            .map_err(store_error)
    }

    pub async fn active_machines(
        &self,
    ) -> Result<Vec<ActiveMachineState>, MachineRosterStoreError> {
        self.store
            .call(|conn| query_json_list(conn, "SELECT json FROM machines ORDER BY machine_id"))
            .await
            .map_err(store_error)
    }

    /// Commit a machine's lifecycle intent. The read-modify-write runs in one
    /// transaction, so lifecycle stays consistent with the rest of the record.
    pub async fn set_lifecycle(
        &self,
        machine_id: &MachineId,
        lifecycle: MachineLifecycle,
    ) -> Result<MachineLifecycleUpdate, MachineRosterStoreError> {
        let machine_id = machine_id.clone();
        self.store
            .call(move |conn| set_lifecycle_txn(conn, &machine_id, lifecycle))
            .await
            .map_err(store_error)
    }
}

fn get_machine(
    conn: &Connection,
    machine_id: &MachineId,
) -> Result<Option<ActiveMachineState>, rusqlite::Error> {
    query_json(
        conn,
        "SELECT json FROM machines WHERE machine_id = ?1",
        machine_id.as_str(),
    )
}

fn put_machine(conn: &Connection, state: &ActiveMachineState) -> Result<(), rusqlite::Error> {
    conn.execute(
        "INSERT INTO machines (machine_id, json) VALUES (?1, ?2)
         ON CONFLICT(machine_id) DO UPDATE SET json = excluded.json",
        params![state.machine_id.as_str(), to_json(state)?],
    )?;
    Ok(())
}

fn set_lifecycle_txn(
    conn: &mut Connection,
    machine_id: &MachineId,
    lifecycle: MachineLifecycle,
) -> Result<MachineLifecycleUpdate, rusqlite::Error> {
    let transaction = conn.transaction()?;
    let Some(mut state) = get_machine(&transaction, machine_id)? else {
        return Ok(MachineLifecycleUpdate::NoSuchMachine);
    };
    if state.lifecycle == lifecycle {
        return Ok(MachineLifecycleUpdate::Unchanged);
    }
    state.lifecycle = lifecycle;
    put_machine(&transaction, &state)?;
    transaction.commit()?;
    Ok(MachineLifecycleUpdate::Changed)
}

#[derive(Debug)]
pub struct MachineRosterStoreError {
    message: String,
}

fn store_error(error: CoreStoreError) -> MachineRosterStoreError {
    MachineRosterStoreError {
        message: error.to_string(),
    }
}

impl std::fmt::Display for MachineRosterStoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "machine roster store: {}", self.message)
    }
}

impl std::error::Error for MachineRosterStoreError {}
