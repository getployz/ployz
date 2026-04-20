use crate::coordination::fanout::{FanOutTarget, NodeStatusResult, fanout_node_status};
use crate::daemon::DaemonState;
use crate::daemon::store::StoreDriver;
use ployz_api::{DaemonPayload, DaemonResponse, MachineDrainPayload, MachineRemovePayload};
use ployz_sdk::{DaemonClient, TcpTransport};
use ployz_store_api::MachineStore;
use ployz_types::model::{DrainState, MachineId, MachineRecord, Phase};
use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use super::super::mesh::{LocalPeerStatus, NodeView, resolve_peer, with_local_machine_record};
use super::render::{format_status, format_timestamp, render_machine_list_report};
use super::types::{MachineListReport, MachineListReportRow};

impl DaemonState {
    pub(crate) async fn handle_machine_list(&self) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };
        let authoritative_self = active.mesh.authoritative_self_record().await;
        let local_record = match local_machine_record(
            active.store.machine().as_ref(),
            &self.identity.machine_id,
            authoritative_self.as_ref(),
        )
        .await
        {
            Ok(Some(record)) => record,
            Ok(None) => {
                return self.err(
                    "SELF_RECORD_MISSING",
                    format!(
                        "local machine '{}' is not available in the mesh or store",
                        self.identity.machine_id
                    ),
                );
            }
            Err(err) => return self.err("LIST_FAILED", format!("failed to read machines: {err}")),
        };
        let local_ready = active.mesh.ready_status().await;
        let local_draining = authoritative_self
            .as_ref()
            .map(|record| record.drain_state)
            .unwrap_or(local_record.drain_state);
        let local_status = LocalNodeStatus {
            ready: local_ready.ready,
            phase: local_ready.phase,
            drain_state: local_draining,
        };

        let report = match machine_list_report(
            active.store.clone(),
            &self.identity.machine_id,
            self.coordination_rpc_port,
            &local_record,
            &local_status,
        )
        .await
        {
            Ok(report) => report,
            Err(err) => return self.err("LIST_FAILED", err),
        };
        if report.rows.is_empty() {
            return self.ok_with_payload(
                "no machines",
                Some(DaemonPayload::MachineList(report.payload())),
            );
        }

        self.ok_with_payload(
            render_machine_list_report(&report),
            Some(DaemonPayload::MachineList(report.payload())),
        )
    }

    pub(crate) async fn handle_machine_remove(&self, id: &str, force: bool) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let machine_id = MachineId(id.to_string());
        let machine_store = active.store.machine();
        let record = match find_machine_record(machine_store.as_ref(), &machine_id).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                return self.err("MACHINE_NOT_FOUND", format!("machine '{id}' not found"));
            }
            Err(err) => {
                return self.err("LIST_FAILED", format!("failed to read machines: {err}"));
            }
        };

        if !force && !record.drain_state.is_drained() {
            return self.err(
                "MACHINE_NOT_DRAINED",
                format!(
                    "machine '{id}' must be drained before removal (current drain state: {})",
                    record.drain_state
                ),
            );
        }

        match active.store.machine().delete_machine(&machine_id).await {
            Ok(()) => self.ok_with_payload(
                format!("machine '{id}' removed"),
                Some(DaemonPayload::MachineRemove(MachineRemovePayload {
                    id: id.to_string(),
                    force,
                })),
            ),
            Err(err) => self.err("DELETE_FAILED", format!("failed to remove machine: {err}")),
        }
    }

    pub(crate) async fn handle_machine_set_drain(&self, id: &str, drain: bool) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let machine_id = MachineId(id.to_string());
        let machine_store = active.store.machine();
        let record = match find_machine_record(machine_store.as_ref(), &machine_id).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                return self.err("MACHINE_NOT_FOUND", format!("machine '{id}' not found"));
            }
            Err(err) => {
                return self.err("LIST_FAILED", format!("failed to read machines: {err}"));
            }
        };

        let next_drain_state = if drain {
            DrainState::Drained
        } else {
            DrainState::Active
        };

        if machine_id != self.identity.machine_id {
            return match forward_machine_drain(&record, self.coordination_rpc_port, drain).await {
                Ok(payload) => {
                    let status = if payload.drain_state.is_drained() {
                        "drained"
                    } else {
                        "active"
                    };
                    self.ok_with_payload(
                        format!("machine '{id}' marked {status}"),
                        Some(DaemonPayload::MachineDrain(payload)),
                    )
                }
                Err(err) => self.err("UPDATE_FAILED", err),
            };
        }

        let mut record = record;
        let Some(_) = active.mesh.authoritative_self_record().await else {
            return self.err(
                "UPDATE_FAILED",
                "local authoritative mesh record missing; refusing to mutate local drain state",
            );
        };

        if record.drain_state == next_drain_state {
            let status = if next_drain_state.is_drained() {
                "drained"
            } else {
                "active"
            };
            return self.ok_with_payload(
                format!("machine '{id}' already {status}"),
                Some(DaemonPayload::MachineDrain(MachineDrainPayload {
                    id: id.to_string(),
                    drain_state: next_drain_state,
                })),
            );
        }

        record.drain_state = next_drain_state;
        record.updated_at = ployz_types::time::now_unix_secs();
        match active.store.machine().upsert_self_machine(&record).await {
            Ok(()) => {
                let updated = active
                    .mesh
                    .update_authoritative_self_record(|self_record| {
                        self_record.drain_state = next_drain_state;
                        self_record.updated_at = record.updated_at;
                    })
                    .await;
                if updated.is_none() {
                    return self.err(
                        "UPDATE_FAILED",
                        "failed to update local machine drain state in authoritative mesh record",
                    );
                }
                let status = if next_drain_state.is_drained() {
                    "drained"
                } else {
                    "active"
                };
                self.ok_with_payload(
                    format!("machine '{id}' marked {status}"),
                    Some(DaemonPayload::MachineDrain(MachineDrainPayload {
                        id: id.to_string(),
                        drain_state: next_drain_state,
                    })),
                )
            }
            Err(err) => self.err("UPDATE_FAILED", format!("failed to update machine: {err}")),
        }
    }
}

pub(super) async fn find_machine_record(
    store: &dyn MachineStore,
    machine_id: &MachineId,
) -> Result<Option<MachineRecord>, String> {
    let machines = store
        .list_machines()
        .await
        .map_err(|err| format!("{err}"))?;
    Ok(machines
        .into_iter()
        .find(|machine| machine.id == *machine_id))
}

pub(super) async fn machine_list_report(
    store: StoreDriver,
    local_machine_id: &MachineId,
    rpc_port: u16,
    local_record: &MachineRecord,
    local_status: &LocalNodeStatus,
) -> Result<MachineListReport, String> {
    let machines = with_local_machine_record(
        store
            .machine()
            .list_machines()
            .await
            .map_err(|err| format!("failed to list machines: {err}"))?,
        local_record,
    );
    let targets: Vec<FanOutTarget> = machines
        .iter()
        .filter(|machine| machine.id != *local_machine_id)
        .map(|machine| FanOutTarget {
            machine_id: machine.id.clone(),
            overlay_ip: machine.overlay_ip,
        })
        .collect();
    let status_by_machine: HashMap<MachineId, NodeStatusResult> =
        fanout_node_status(&targets, rpc_port, Duration::from_millis(750))
            .await
            .into_iter()
            .collect();

    Ok(MachineListReport {
        rows: machines
            .iter()
            .map(|machine| {
                let view = NodeView::new(
                    machine,
                    resolve_peer(
                        &machine.id,
                        local_machine_id,
                        LocalPeerStatus {
                            ready: local_status.ready,
                            phase: local_status.phase,
                            drain_state: local_status.drain_state,
                        },
                        &status_by_machine,
                    ),
                );
                MachineListReportRow {
                    id: machine.id.0.clone(),
                    status: format_status(machine),
                    admitted: machine.admitted,
                    overlay: machine.overlay_ip.0.to_string(),
                    subnet: machine.subnet,
                    subnet_display: machine
                        .subnet
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "—".into()),
                    peer_state: view.peer_state(),
                    reachable: view.reachable(),
                    ready: view.ready(),
                    drain_state: Some(view.drain_state()),
                    phase: view.phase(),
                    reported_machine_id: view.reported_machine_id().map(ToString::to_string),
                    reported_boot_id: view.reported_boot_id().map(ToOwned::to_owned),
                    created_at: machine.created_at,
                    created_display: format_timestamp(machine.created_at),
                }
            })
            .collect(),
    })
}

pub(super) struct LocalNodeStatus {
    pub ready: bool,
    pub phase: Phase,
    pub drain_state: DrainState,
}

async fn local_machine_record(
    store: &dyn MachineStore,
    machine_id: &MachineId,
    authoritative_self: Option<&MachineRecord>,
) -> Result<Option<MachineRecord>, String> {
    let store_record = find_machine_record(store, machine_id).await?;
    Ok(store_record.or_else(|| authoritative_self.cloned()))
}

fn client(machine: &MachineRecord, rpc_port: u16) -> DaemonClient<TcpTransport> {
    let addr = SocketAddr::new(IpAddr::V6(machine.overlay_ip.0), rpc_port);
    DaemonClient::new(TcpTransport::new(addr))
}

async fn forward_machine_drain(
    machine: &MachineRecord,
    rpc_port: u16,
    drain: bool,
) -> Result<MachineDrainPayload, String> {
    let client = client(machine, rpc_port);
    let result = if drain {
        client.machine_drain(&machine.id.0).await
    } else {
        client.machine_undrain(&machine.id.0).await
    };
    result.map_err(|err| format!("failed to update machine '{}': {err}", machine.id))
}
