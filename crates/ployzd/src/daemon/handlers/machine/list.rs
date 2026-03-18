use crate::daemon::DaemonState;
use ployz_api::{DaemonPayload, DaemonResponse, MachineRemovePayload};
use ployz_state::StoreDriver;
use ployz_state::machine_liveness::{MachineLiveness, machine_liveness};
use ployz_store_api::MachineStore;
use ployz_state::time::now_unix_secs;
use ployz_types::model::{MachineId, MachineRecord, Participation};

use super::render::{
    degraded_mesh_warning, format_heartbeat, format_liveness, format_participation, format_status,
    format_timestamp, render_machine_list_report,
};
use super::types::{MachineListReport, MachineListReportRow};

impl DaemonState {
    pub(crate) async fn handle_machine_list(&self) -> DaemonResponse {
        let active = match self.active.as_ref() {
            Some(active) => active,
            None => return self.err("NO_RUNNING_NETWORK", "no mesh running"),
        };

        let report = match machine_list_report(active.mesh.store.clone()).await {
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
        let record = match find_machine_record(&active.mesh.store, &machine_id).await {
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

        match active.mesh.store.delete_machine(&machine_id).await {
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

    pub(super) async fn degraded_mesh_warnings(&self) -> Result<Vec<String>, String> {
        let active = self
            .active
            .as_ref()
            .ok_or_else(|| "no running network".to_string())?;
        let machines = active
            .mesh
            .store
            .list_machines()
            .await
            .map_err(|err| format!("failed to list machines: {err}"))?;
        let now = now_unix_secs();

        Ok(machines
            .into_iter()
            .filter(|machine| machine.id != self.identity.machine_id)
            .filter(|machine| match machine.participation {
                Participation::Disabled => false,
                Participation::Enabled | Participation::Draining => true,
            })
            .filter(|machine| machine_liveness(machine, now) == MachineLiveness::Stale)
            .map(|machine| degraded_mesh_warning(&machine))
            .collect())
    }
}

pub(super) async fn find_machine_record(
    store: &StoreDriver,
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

pub(super) async fn machine_list_report(store: StoreDriver) -> Result<MachineListReport, String> {
    let machines = store
        .list_machines()
        .await
        .map_err(|err| format!("failed to list machines: {err}"))?;
    let now = now_unix_secs();

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
                    .map(|subnet| subnet.to_string())
                    .unwrap_or_else(|| "—".into()),
                last_heartbeat: machine.last_heartbeat,
                heartbeat_display: format_heartbeat(machine.last_heartbeat, now),
                created_at: machine.created_at,
                created_display: format_timestamp(machine.created_at),
            })
            .collect(),
    })
}
