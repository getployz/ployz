use crate::coordination::fanout::{FanOutTarget, NodeStatusResult, fanout_node_status};
use crate::daemon::DaemonState;
use crate::daemon::store::StoreDriver;
use ployz_api::{DaemonPayload, DaemonResponse, MachineDrainPayload, MachineRemovePayload};
use ployz_store_api::MachineStore;
use ployz_types::model::{MachineId, MachineRecord};
use std::collections::HashMap;
use std::time::Duration;

use super::render::{format_status, format_timestamp, render_machine_list_report};
use super::types::{MachineListReport, MachineListReportRow};

impl DaemonState {
    pub(crate) async fn handle_machine_list(&self) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };
        let local_ready = active.mesh.ready_status().await;
        let local_draining = active
            .mesh
            .authoritative_self_record()
            .await
            .is_some_and(|record| record.drain);
        let local_status = LocalNodeStatus {
            ready: local_ready.ready,
            phase: local_ready.phase.to_string(),
            draining: local_draining,
        };

        let report = match machine_list_report(
            active.store.clone(),
            &self.identity.machine_id,
            self.coordination_rpc_port,
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

        if !force && !record.drain {
            return self.err(
                "MACHINE_NOT_DRAINED",
                format!(
                    "machine '{id}' must be drained before removal (current draining: {})",
                    record.drain
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
        let mut record = match find_machine_record(machine_store.as_ref(), &machine_id).await {
            Ok(Some(record)) => record,
            Ok(None) => {
                return self.err("MACHINE_NOT_FOUND", format!("machine '{id}' not found"));
            }
            Err(err) => {
                return self.err("LIST_FAILED", format!("failed to read machines: {err}"));
            }
        };

        if record.drain == drain {
            let status = if drain { "drained" } else { "undrained" };
            return self.ok_with_payload(
                format!("machine '{id}' already {status}"),
                Some(DaemonPayload::MachineDrain(MachineDrainPayload {
                    id: id.to_string(),
                    draining: drain,
                })),
            );
        }

        record.drain = drain;
        record.updated_at = ployz_types::time::now_unix_secs();
        match active.store.machine().upsert_self_machine(&record).await {
            Ok(()) => {
                if machine_id == self.identity.machine_id {
                    let updated = active
                        .mesh
                        .update_authoritative_self_record(|self_record| {
                            self_record.drain = drain;
                            self_record.updated_at = record.updated_at;
                        })
                        .await;
                    if updated.is_none() {
                        return self.err(
                            "UPDATE_FAILED",
                            "failed to update local machine drain state in authoritative mesh record",
                        );
                    }
                }
                let status = if drain { "drained" } else { "undrained" };
                self.ok_with_payload(
                    format!("machine '{id}' marked {status}"),
                    Some(DaemonPayload::MachineDrain(MachineDrainPayload {
                        id: id.to_string(),
                        draining: drain,
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
    local_status: &LocalNodeStatus,
) -> Result<MachineListReport, String> {
    let machines = store
        .machine()
        .list_machines()
        .await
        .map_err(|err| format!("failed to list machines: {err}"))?;
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
            .map(|machine| MachineListReportRow {
                id: machine.id.0.clone(),
                status: format_status(machine),
                overlay: machine.overlay_ip.0.to_string(),
                subnet: machine.subnet,
                subnet_display: machine
                    .subnet
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "—".into()),
                reachable: machine.id == *local_machine_id
                    || matches!(
                        status_by_machine.get(&machine.id),
                        Some(NodeStatusResult::Ok(_))
                    ),
                ready: if machine.id == *local_machine_id {
                    Some(local_status.ready)
                } else {
                    match status_by_machine.get(&machine.id) {
                        Some(NodeStatusResult::Ok(status)) => Some(status.ready),
                        Some(NodeStatusResult::Offline)
                        | Some(NodeStatusResult::InvalidIdentity { .. })
                        | None => None,
                    }
                },
                draining: if machine.id == *local_machine_id {
                    Some(local_status.draining)
                } else {
                    match status_by_machine.get(&machine.id) {
                        Some(NodeStatusResult::Ok(status)) => Some(status.draining),
                        Some(NodeStatusResult::Offline)
                        | Some(NodeStatusResult::InvalidIdentity { .. })
                        | None => Some(machine.drain),
                        | None => Some(machine.drain),
                    }
                },
                phase: if machine.id == *local_machine_id {
                    Some(local_status.phase.clone())
                } else {
                    match status_by_machine.get(&machine.id) {
                        Some(NodeStatusResult::Ok(status)) => Some(status.phase.clone()),
                        Some(NodeStatusResult::Offline)
                        | Some(NodeStatusResult::InvalidIdentity { .. })
                        | None => None,
                    }
                },
                created_at: machine.created_at,
                created_display: format_timestamp(machine.created_at),
            })
            .collect(),
    })
}

pub(super) struct LocalNodeStatus {
    pub ready: bool,
    pub phase: String,
    pub draining: bool,
}
