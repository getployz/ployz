use std::collections::HashMap;
use std::time::Duration;

use crate::coordination::fanout::FanOutTarget;
use crate::daemon::DaemonState;
use crate::daemon::store::StoreDriver;
use crate::peers::fanout::{LiveStatus, fanout_node_status, live_status_map};
use ployz_api::{DaemonPayload, DaemonResponse, MachineDrainPayload, MachineRemovePayload};
use ployz_store_api::MachineStore;
use ployz_types::model::{MachineId, MachineRecord};

use super::render::{
    format_drain, format_live_status, format_status, format_timestamp, render_machine_list_report,
};
use super::types::{MachineListReport, MachineListReportRow};

const NODE_STATUS_FANOUT_DEADLINE: Duration = Duration::from_secs(5);

impl DaemonState {
    pub(crate) async fn handle_machine_list(&self) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let self_id = self.identity.machine_id.clone();
        let rpc_port = self.coordination_rpc_port;
        let machines = match active.store.machine().list_machines().await {
            Ok(m) => m,
            Err(err) => {
                return self.err("LIST_FAILED", format!("failed to list machines: {err}"));
            }
        };
        let targets: Vec<FanOutTarget> = machines
            .iter()
            .filter(|m| m.id != self_id)
            .map(|m| FanOutTarget {
                machine_id: m.id.clone(),
                overlay_ip: m.overlay_ip,
            })
            .collect();
        let items = fanout_node_status(&targets, rpc_port, NODE_STATUS_FANOUT_DEADLINE).await;
        let mut live_map = live_status_map(&items);
        live_map.insert(self_id.clone(), LiveStatus::Live);

        let report = match machine_list_report(active.store.clone(), &live_map).await {
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
                format!("machine '{id}' must be drained before removal (pass --force to override)"),
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

    pub(crate) async fn handle_machine_drain(&self, id: &str, drain: bool) -> DaemonResponse {
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
            return self.ok_with_payload(
                format!("machine '{id}' drain already {drain}"),
                Some(DaemonPayload::MachineDrain(MachineDrainPayload {
                    id: id.to_string(),
                    drain,
                })),
            );
        }

        let is_self = machine_id == self.identity.machine_id;
        if is_self {
            if active.mesh.apply_self_drain(drain).await.is_none() {
                return self.err(
                    "DRAIN_FAILED",
                    "self record writer unavailable or mutation failed".to_string(),
                );
            }
        } else {
            record.drain = drain;
            if let Err(err) = machine_store.upsert_self_machine(&record).await {
                return self.err("DRAIN_FAILED", format!("failed to persist drain: {err}"));
            }
        }

        let verb = if drain { "drained" } else { "undrained" };
        self.ok_with_payload(
            format!("machine '{id}' {verb}"),
            Some(DaemonPayload::MachineDrain(MachineDrainPayload {
                id: id.to_string(),
                drain,
            })),
        )
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
    live_map: &HashMap<MachineId, LiveStatus>,
) -> Result<MachineListReport, String> {
    let machines = store
        .machine()
        .list_machines()
        .await
        .map_err(|err| format!("failed to list machines: {err}"))?;

    Ok(MachineListReport {
        rows: machines
            .iter()
            .map(|machine| {
                let live = live_map
                    .get(&machine.id)
                    .copied()
                    .unwrap_or(LiveStatus::Offline);
                MachineListReportRow {
                    id: machine.id.0.clone(),
                    status: format_status(machine),
                    drain: machine.drain,
                    drain_display: format_drain(machine.drain),
                    liveness: format_live_status(live),
                    overlay: machine.overlay_ip.0.to_string(),
                    subnet: machine.subnet,
                    subnet_display: machine
                        .subnet
                        .map(|s| s.to_string())
                        .unwrap_or_else(|| "—".into()),
                    created_at: machine.created_at,
                    created_display: format_timestamp(machine.created_at),
                }
            })
            .collect(),
    })
}
