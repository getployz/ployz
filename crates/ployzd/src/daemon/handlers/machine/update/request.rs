use std::collections::HashSet;

use ployz_api::{DaemonPayload, DaemonResponse, MachineUpdatePayload};
use ployz_model::MachineId;
use tokio::sync::oneshot;

use crate::daemon::DaemonState;
use crate::daemon::handlers::machine::operations::{
    MachineOperationArtifacts, MachineOperationKind, MachineOperationStatus,
};

use super::installer::prepare_machine_update;
use super::local::{ensure_update_operation, spawn_update_after_response};
use super::version::{normalize_requested_version, requested_version_matches_current};

impl DaemonState {
    pub(crate) async fn handle_machine_update(
        &self,
        ids: &[String],
        version: &str,
        mut response_flushed: Option<oneshot::Receiver<()>>,
    ) -> DaemonResponse {
        let version = normalize_requested_version(version);
        if version.is_empty() {
            return self.err("INVALID_VERSION", "machine update version cannot be empty");
        }

        let targets = if ids.is_empty() {
            vec![self.identity.machine_id.as_str().to_string()]
        } else {
            if let Some(duplicate) = first_duplicate(ids) {
                return self.err(
                    "DUPLICATE_MACHINE",
                    format!("machine '{duplicate}' was targeted more than once"),
                );
            }
            for id in ids {
                if let Err(error) = MachineId::try_new(id.as_str()) {
                    return self.err("MACHINE_UPDATE_INVALID_TARGET", error);
                }
            }
            update_targets_with_self_last(ids, &self.identity.machine_id)
        };

        let operation_store = self.machine_operation_store();
        let mut operation = match operation_store.begin(
            MachineOperationKind::Update,
            self.active
                .as_ref()
                .map(|active| active.config.name.0.clone()),
            targets.clone(),
            "resolved",
            MachineOperationArtifacts {
                requested_version: Some(version.clone()),
                previous_version: Some(env!("CARGO_PKG_VERSION").to_string()),
                ..MachineOperationArtifacts::default()
            },
        ) {
            Ok(operation) => operation,
            Err(error) => return self.err("MACHINE_UPDATE_OPERATION_FAILED", error),
        };
        let operation_id = operation.id.clone();
        let mut updated = Vec::new();
        for target in targets {
            if let Err(error) =
                operation_store.update_stage(&mut operation, format!("updating:{target}"))
            {
                return self.err("MACHINE_UPDATE_OPERATION_FAILED", error);
            }
            let result = if target == self.identity.machine_id.as_str() {
                self.update_local_machine(
                    &operation_id,
                    &version,
                    response_flushed.take(),
                    operation_store.clone(),
                    operation.clone(),
                )
                .await
            } else {
                self.update_remote_machine(&operation_id, &target, &version)
                    .await
            };

            match result {
                Ok(row) => {
                    let deferred_self_update = target == self.identity.machine_id.as_str()
                        && row.message == "scheduled local update";
                    updated.push(row);
                    if deferred_self_update {
                        let payload = MachineUpdatePayload {
                            operation_id,
                            updated,
                            failed: Vec::new(),
                        };
                        return self.ok_with_payload(
                            "machine update scheduled",
                            Some(DaemonPayload::MachineUpdate(payload)),
                        );
                    }
                }
                Err(row) => {
                    let _ = operation_store.update_status(
                        &mut operation,
                        MachineOperationStatus::Failed,
                        Some(row.message.clone()),
                    );
                    let message = format!("machine '{}' update failed: {}", row.id, row.message);
                    let payload = MachineUpdatePayload {
                        operation_id,
                        updated,
                        failed: vec![row],
                    };
                    return self.err_with_payload(
                        "MACHINE_UPDATE_FAILED",
                        message,
                        Some(DaemonPayload::MachineUpdate(payload)),
                    );
                }
            }
        }

        if let Err(error) =
            operation_store.update_status(&mut operation, MachineOperationStatus::Succeeded, None)
        {
            return self.err("MACHINE_UPDATE_OPERATION_FAILED", error);
        }
        let payload = MachineUpdatePayload {
            operation_id,
            updated,
            failed: Vec::new(),
        };
        self.ok_with_payload(
            "machine update scheduled",
            Some(DaemonPayload::MachineUpdate(payload)),
        )
    }

    pub(crate) async fn handle_mesh_peer_prepare_update(
        &self,
        operation_id: &str,
        version: &str,
    ) -> DaemonResponse {
        match prepare_machine_update(version).await {
            Ok(()) => self.ok(format!("machine update '{operation_id}' prepared")),
            Err(error) => self.err("MACHINE_UPDATE_PREPARE_FAILED", error),
        }
    }

    pub(crate) async fn handle_mesh_peer_execute_update(
        &self,
        operation_id: &str,
        version: &str,
        response_flushed: Option<oneshot::Receiver<()>>,
    ) -> DaemonResponse {
        let version = normalize_requested_version(version);
        if let Err(error) = prepare_machine_update(&version).await {
            return self.err("MACHINE_UPDATE_PREPARE_FAILED", error);
        }

        if requested_version_matches_current(&version) {
            let operation_store = self.machine_operation_store();
            if let Err(error) = ensure_update_operation(
                &operation_store,
                operation_id,
                &[self.identity.machine_id.as_str().to_string()],
                &version,
                "already-current",
            )
            .and_then(|mut operation| {
                operation_store.update_status(
                    &mut operation,
                    MachineOperationStatus::Succeeded,
                    None,
                )
            }) {
                return self.err("MACHINE_UPDATE_OPERATION_FAILED", error);
            }
            return self.ok(format!(
                "machine update '{operation_id}' skipped; daemon already reports version {}",
                env!("CARGO_PKG_VERSION")
            ));
        }

        let operation_store = self.machine_operation_store();
        let operation = match ensure_update_operation(
            &operation_store,
            operation_id,
            &[self.identity.machine_id.as_str().to_string()],
            &version,
            "execute",
        ) {
            Ok(operation) => operation,
            Err(error) => return self.err("MACHINE_UPDATE_OPERATION_FAILED", error),
        };
        spawn_update_after_response(
            operation_id.to_string(),
            version,
            response_flushed,
            operation_store,
            operation,
        );
        self.ok(format!("machine update '{operation_id}' scheduled"))
    }
}

pub(super) fn first_duplicate(values: &[String]) -> Option<String> {
    let mut seen = HashSet::new();
    for value in values {
        if !seen.insert(value) {
            return Some(value.clone());
        }
    }
    None
}

pub(super) fn update_targets_with_self_last(
    ids: &[String],
    local_machine_id: &MachineId,
) -> Vec<String> {
    let mut targets: Vec<String> = ids
        .iter()
        .filter(|id| id.as_str() != local_machine_id.as_str())
        .cloned()
        .collect();
    if ids
        .iter()
        .any(|id| id.as_str() == local_machine_id.as_str())
    {
        targets.push(local_machine_id.as_str().to_string());
    }
    targets
}
