use ployz_store_api::{MachineMembershipStore, StoreDriver};

use super::super::render::{format_lifecycle, format_timestamp};
use super::super::types::{MachineListReport, MachineListReportRow};

pub(super) async fn machine_list_report(store: StoreDriver) -> Result<MachineListReport, String> {
    let machines = store
        .list_machines()
        .await
        .map_err(|err| format!("failed to list machines: {err}"))?;

    Ok(MachineListReport {
        rows: machines
            .iter()
            .map(|machine| MachineListReportRow {
                id: machine.id.as_str().to_string(),
                lifecycle: format_lifecycle(machine),
                authority: ployz_model::AuthorityNodePosture::from_machine_membership(machine),
                region: machine.topology.region.0.clone(),
                region_role: machine.region_role.to_string(),
                availability_zone: machine
                    .topology
                    .availability_zone
                    .as_ref()
                    .map(|zone| zone.0.clone()),
                availability_zone_display: machine
                    .topology
                    .availability_zone
                    .as_ref()
                    .map(|zone| zone.0.clone())
                    .unwrap_or_else(|| "—".into()),
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
