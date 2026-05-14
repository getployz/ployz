use ployz_api::{DaemonPayload, MachineOperationListPayload, MachineOperationPayload};

use crate::daemon::DaemonState;

use super::store::MachineOperationStore;
use super::types::MachineOperationRecord;

impl DaemonState {
    pub(in crate::daemon::handlers::machine) fn machine_operation_store(
        &self,
    ) -> MachineOperationStore {
        MachineOperationStore::new(self.data_dir.clone())
    }

    pub(crate) async fn handle_machine_operation_list(&self) -> ployz_api::DaemonResponse {
        let records = match self.machine_operation_store().list() {
            Ok(records) => records,
            Err(err) => return self.err("MACHINE_OPERATION_LIST_FAILED", err),
        };
        let payload = MachineOperationListPayload {
            operations: records.iter().map(MachineOperationRecord::info).collect(),
        };

        if records.is_empty() {
            return self.ok_with_payload(
                "no machine operations",
                Some(DaemonPayload::MachineOperationList(payload)),
            );
        }

        let lines: Vec<String> = records
            .iter()
            .map(|record| {
                let network = record.network_name.as_deref().unwrap_or("—");
                format!(
                    "{}  {}  {}  {}  {}",
                    record.id,
                    record.kind.as_str(),
                    record.status().as_str(),
                    network,
                    record.stage
                )
            })
            .collect();

        self.ok_with_payload(
            lines.join("\n"),
            Some(DaemonPayload::MachineOperationList(payload)),
        )
    }

    pub(crate) async fn handle_machine_operation_get(&self, id: &str) -> ployz_api::DaemonResponse {
        let record = match self.machine_operation_store().load(id) {
            Ok(Some(record)) => record,
            Ok(None) => {
                return self.err(
                    "MACHINE_OPERATION_NOT_FOUND",
                    format!("machine operation '{id}' not found"),
                );
            }
            Err(err) => return self.err("MACHINE_OPERATION_GET_FAILED", err),
        };

        let payload = MachineOperationPayload {
            operation: record.info(),
        };
        let body = serde_json::to_string_pretty(&record)
            .map_err(|err| format!("failed to encode machine operation: {err}"));
        match body {
            Ok(body) => self.ok_with_payload(body, Some(DaemonPayload::MachineOperation(payload))),
            Err(err) => self.err("ENCODE_FAILED", err),
        }
    }
}
