use crate::coordination::fanout::{FanOutTarget, NodeStatusResult, fanout_node_status};
use crate::daemon::DaemonState;
use crate::daemon::store::StoreDriver;
use ployz_api::{DaemonPayload, DaemonResponse, MachineRemovePayload};
use ployz_store_api::MachineStore;
use ployz_types::model::{MachineId, MachineRecord, Participation};
use ployz_types::time::now_unix_secs;
use std::collections::HashMap;
use std::time::Duration;

use super::render::{
    format_heartbeat, format_liveness, format_participation, format_status, format_timestamp,
    render_machine_list_report,
};
use super::types::{MachineListReport, MachineListReportRow};

impl DaemonState {
    pub(crate) async fn handle_machine_list(&self) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let report = match machine_list_report(
            active.store.clone(),
            &self.identity.machine_id,
            self.coordination_rpc_port,
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

        if !force && record.participation != Participation::Disabled {
            return self.err(
                "MACHINE_NOT_DISABLED",
                format!(
                    "machine '{id}' must be disabled before removal (current participation: {})",
                    record.participation
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
) -> Result<MachineListReport, String> {
    let machines = store
        .machine()
        .list_machines()
        .await
        .map_err(|err| format!("failed to list machines: {err}"))?;
    let now = now_unix_secs();
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
                participation: format_participation(machine),
                liveness: format_liveness(machine, now),
                overlay: machine.overlay_ip.0.to_string(),
                subnet: machine.subnet,
                subnet_display: machine
                    .subnet
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "—".into()),
                last_heartbeat: machine.last_heartbeat,
                heartbeat_display: format_heartbeat(machine.last_heartbeat, now),
                reachable: machine.id == *local_machine_id
                    || matches!(
                        status_by_machine.get(&machine.id),
                        Some(NodeStatusResult::Ok(_))
                    ),
                ready: if machine.id == *local_machine_id {
                    Some(true)
                } else {
                    match status_by_machine.get(&machine.id) {
                        Some(NodeStatusResult::Ok(status)) => Some(status.ready),
                        Some(NodeStatusResult::Offline)
                        | Some(NodeStatusResult::InvalidIdentity { .. })
                        | None => None,
                    }
                },
                draining: if machine.id == *local_machine_id {
                    Some(machine.participation == Participation::Draining)
                } else {
                    match status_by_machine.get(&machine.id) {
                        Some(NodeStatusResult::Ok(status)) => Some(status.draining),
                        Some(NodeStatusResult::Offline)
                        | Some(NodeStatusResult::InvalidIdentity { .. })
                        | None => None,
                    }
                },
                phase: if machine.id == *local_machine_id {
                    Some("running".to_string())
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
