use crate::daemon::DaemonState;
use ployz_api::{DaemonPayload, DaemonRequest, DaemonResponse, MachineRemovePayload};
use ployz_store_api::MachineStore;
use ployz_store_api::StoreDriver;
use ployz_types::model::{MachineId, MachineMembership};

use super::render::{format_lifecycle, format_timestamp, render_machine_list_report};
use super::types::{MachineListReport, MachineListReportRow};
use crate::daemon::handlers::peer_rpc::{
    OverlayRpcExpectOkError, PEER_RPC_DESTRUCTIVE_READ_TIMEOUT,
    overlay_rpc_expect_ok_classified_with_read_timeout,
};

impl DaemonState {
    pub(crate) async fn handle_machine_list(&self) -> DaemonResponse {
        let active = match self.require_active("NO_RUNNING_NETWORK", "no mesh running") {
            Ok(active) => active,
            Err(response) => return *response,
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
        let active = match self.require_active("NO_RUNNING_NETWORK", "no mesh running") {
            Ok(active) => active,
            Err(response) => return *response,
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

        if record.id == self.identity.machine_id {
            return self.err(
                "CANNOT_REMOVE_SELF",
                "cannot remove the local machine with `machine rm`; use `mesh destroy` for whole-mesh teardown",
            );
        }

        if !force {
            let peer_rpc_port = match self.peer_control_port() {
                Ok(port) => port,
                Err(error) => return self.err("PEER_RPC_UNAVAILABLE", error.to_string()),
            };
            let operation_id = format!("machine-rm-{}", ployz_types::model::NetworkId::random());
            if let Err(error) = overlay_rpc_expect_ok_classified_with_read_timeout(
                record.overlay_ip,
                peer_rpc_port,
                DaemonRequest::MeshPeerRemoveMachine {
                    operation_id,
                    network_id: active.config.id.clone(),
                    machine_id: record.id.clone(),
                },
                PEER_RPC_DESTRUCTIVE_READ_TIMEOUT,
            )
            .await
            {
                return match error {
                    OverlayRpcExpectOkError::Transport(error) => self.err(
                        "MACHINE_REMOVE_PEER_UNREACHABLE",
                        format!(
                            "machine '{id}' did not confirm online removal; rerun with --force for registry-only removal: {error}"
                        ),
                    ),
                    OverlayRpcExpectOkError::Remote { code, message } => self.err(
                        "MACHINE_REMOVE_PEER_REJECTED",
                        format!(
                            "machine '{id}' rejected coordinated removal [{code}]: {message}; resolve the remote failure or rerun with --force only if you intend registry-only removal"
                        ),
                    ),
                };
            }
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
}

pub(super) async fn find_machine_record(
    store: &StoreDriver,
    machine_id: &MachineId,
) -> Result<Option<MachineMembership>, String> {
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

    Ok(MachineListReport {
        rows: machines
            .iter()
            .map(|machine| MachineListReportRow {
                id: machine.id.0.clone(),
                lifecycle: format_lifecycle(machine),
                overlay: machine.overlay_ip.0.to_string(),
                subnet: machine.subnet,
                subnet_display: machine
                    .subnet
                    .map(|subnet| subnet.to_string())
                    .unwrap_or_else(|| "—".into()),
                created_at: machine.created_at,
                created_display: format_timestamp(machine.created_at),
            })
            .collect(),
    })
}
