mod records;
mod remove;
mod report;
mod rtt;

use crate::daemon::DaemonState;
use ployz_api::{DaemonPayload, DaemonResponse};

use super::render::render_machine_list_report;
pub(super) use records::find_machine_record;
use report::machine_list_report;

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
}
