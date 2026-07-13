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
use std::net::{IpAddr, SocketAddr};

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

    /// Record a machine's advertised reachable public endpoint (ADR 0030). The
    /// read-modify-write runs in one transaction. Returns whether the row
    /// changed, so the caller rebroadcasts intent only on a real change; a
    /// machine absent from the roster is a no-op.
    pub async fn set_endpoints(
        &self,
        machine_id: &MachineId,
        control_endpoints: Vec<IpAddr>,
        mesh_endpoints: Vec<SocketAddr>,
    ) -> Result<bool, MachineRosterStoreError> {
        let machine_id = machine_id.clone();
        self.store
            .call(move |conn| {
                set_endpoints_txn(conn, &machine_id, control_endpoints, mesh_endpoints)
            })
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

fn set_endpoints_txn(
    conn: &mut Connection,
    machine_id: &MachineId,
    control_endpoints: Vec<IpAddr>,
    mesh_endpoints: Vec<SocketAddr>,
) -> Result<bool, rusqlite::Error> {
    let transaction = conn.transaction()?;
    let Some(mut state) = get_machine(&transaction, machine_id)? else {
        return Ok(false);
    };
    // A partial discovery reports an empty set for a kind it couldn't determine (e.g.
    // the public-IP echo timed out while private interfaces still yield mesh
    // endpoints). Treat empty as "no news" and keep the durable value rather than
    // clearing a still-valid address — reachability is a durable address property that
    // is never cleared by silence (ADR 0030).
    let control_endpoints = if control_endpoints.is_empty() {
        state.control_endpoints.clone()
    } else {
        control_endpoints
    };
    let mesh_endpoints = if mesh_endpoints.is_empty() {
        state.mesh_endpoints.clone()
    } else {
        mesh_endpoints
    };
    if state.control_endpoints == control_endpoints && state.mesh_endpoints == mesh_endpoints {
        return Ok(false);
    }
    state.control_endpoints = control_endpoints;
    state.mesh_endpoints = mesh_endpoints;
    put_machine(&transaction, &state)?;
    transaction.commit()?;
    Ok(true)
}

#[derive(Debug, thiserror::Error)]
#[error("machine roster store: {message}")]
pub struct MachineRosterStoreError {
    message: String,
}

fn store_error(error: CoreStoreError) -> MachineRosterStoreError {
    MachineRosterStoreError {
        message: error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_core::machine::MachineName;
    use ployz_test_support::ids::{machine_id, operation_id};
    use std::net::Ipv4Addr;

    async fn seeded_roster(machine: &str) -> (MachineRosterStore, MachineId) {
        let roster = MachineRosterStore::new(CoreStore::open_in_memory().await.expect("store"));
        let id = machine_id(machine);
        roster
            .replace_active_machine(&ActiveMachineState {
                machine_id: id.clone(),
                name: MachineName::try_new(machine).expect("name"),
                activated_by: operation_id("op_activate"),
                roles: ployz_core::roles::InstallRolePolicy::install_all(),
                lifecycle: MachineLifecycle::Active,
                control_endpoints: Vec::new(),
                mesh_endpoints: Vec::new(),
                endpoint_subnet: ployz_core::dataplane::MachineEndpointSubnet::try_new(
                    "10.198.0.0/24",
                )
                .expect("valid endpoint subnet"),
                wireguard_public_key: ployz_core::dataplane::WireGuardPublicKey::try_new(format!(
                    "public-{machine}"
                ))
                .expect("public key"),
            })
            .await
            .expect("seed machine");
        (roster, id)
    }

    #[tokio::test]
    async fn set_endpoints_records_on_change_and_is_idempotent() {
        let (roster, id) = seeded_roster("machine_a").await;
        let control = vec![IpAddr::V4(Ipv4Addr::new(203, 0, 113, 5))];
        let mesh = vec!["10.0.0.5:51820".parse().expect("valid mesh endpoint")];

        assert!(
            roster
                .set_endpoints(&id, control.clone(), mesh.clone())
                .await
                .expect("set")
        );
        // The same endpoint is not a change, so nothing rebroadcasts.
        assert!(
            !roster
                .set_endpoints(&id, control.clone(), mesh.clone())
                .await
                .expect("set again")
        );
        let machine = roster
            .active_machine(&id)
            .await
            .expect("read")
            .expect("machine present");
        assert_eq!(machine.control_endpoints, control);
        assert_eq!(machine.mesh_endpoints, mesh);
    }

    #[tokio::test]
    async fn set_endpoints_on_unknown_machine_is_a_noop() {
        let roster = MachineRosterStore::new(CoreStore::open_in_memory().await.expect("store"));
        assert!(
            !roster
                .set_endpoints(
                    &machine_id("ghost"),
                    vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
                    Vec::new()
                )
                .await
                .expect("set")
        );
    }
}
