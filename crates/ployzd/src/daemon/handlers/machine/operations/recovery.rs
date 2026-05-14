use std::collections::BTreeMap;

use ployz_model::MachineLifecycle;
use ployz_store_api::MachineMembershipStore;
use ployz_time::now_unix_secs;

use crate::daemon::DaemonState;
use crate::daemon::ssh::SshOptions;

use super::super::join::rollback::best_effort_remote_cleanup;
use super::super::types::MachineAddStage;
use super::types::{
    MachineOperationKind, MachineOperationRecord, MachineOperationStatus,
    MachineOperationTransition,
};

impl DaemonState {
    pub async fn recover_machine_operations_on_startup(&self) {
        let store = self.machine_operation_store();
        let records = match store.list() {
            Ok(records) => records,
            Err(err) => {
                tracing::warn!(error = %err, "machine operation startup recovery: list failed");
                return;
            }
        };

        for mut record in records {
            if record.status() != MachineOperationStatus::Running {
                continue;
            }
            if let Err(err) = store.update_status(
                &mut record,
                MachineOperationStatus::Interrupted,
                Some("daemon restarted before operation completed".into()),
            ) {
                tracing::warn!(error = %err, operation_id = %record.id, "machine operation startup recovery: mark interrupted failed");
                continue;
            }

            let note = match self.recover_machine_operation(&record).await {
                Ok(note) => note,
                Err(err) => Some(err),
            };
            if let Some(note) = note {
                let combined = merge_operation_notes(record.last_error(), &note);
                if let Err(err) = store.update_status(
                    &mut record,
                    MachineOperationStatus::Interrupted,
                    Some(combined),
                ) {
                    tracing::warn!(error = %err, operation_id = %record.id, "machine operation startup recovery: update note failed");
                }
            }
        }
    }

    async fn recover_machine_operation(
        &self,
        record: &MachineOperationRecord,
    ) -> Result<Option<String>, String> {
        match record.kind {
            MachineOperationKind::Init => Ok(None),
            MachineOperationKind::Add => self.recover_machine_add_operation(record).await,
            MachineOperationKind::Update => self.recover_machine_update_operation(record).await,
            MachineOperationKind::StoragePromote => Ok(Some(
                "daemon restarted before storage promotion completed; inspect machine list and status before retrying".into(),
            )),
        }
    }

    async fn recover_machine_update_operation(
        &self,
        record: &MachineOperationRecord,
    ) -> Result<Option<String>, String> {
        let store = self.machine_operation_store();
        let Some(mut current) = store.load(&record.id)? else {
            return Ok(Some(
                "update operation disappeared during startup recovery".into(),
            ));
        };
        let requested_version = record
            .artifacts
            .requested_version
            .as_deref()
            .unwrap_or("latest");
        let version_matches = if requested_version == "latest" {
            record
                .artifacts
                .previous_version
                .as_deref()
                .is_some_and(|previous_version| previous_version != env!("CARGO_PKG_VERSION"))
        } else {
            requested_version == env!("CARGO_PKG_VERSION")
        };
        if version_matches {
            current.apply_transition(MachineOperationTransition::succeed(now_unix_secs()));
            store.save(&current)?;
            return Ok(None);
        }
        Ok(Some(format!(
            "daemon restarted but reports version {}; expected {}",
            env!("CARGO_PKG_VERSION"),
            requested_version
        )))
    }

    async fn recover_machine_add_operation(
        &self,
        record: &MachineOperationRecord,
    ) -> Result<Option<String>, String> {
        let mut notes = Vec::new();
        if let Some(machine_id) = &record.artifacts.machine_id {
            if let Some(active) = self.active.as_ref() {
                match super::super::list::find_machine_record(&active.mesh.store, machine_id).await
                {
                    Ok(Some(machine)) if machine.lifecycle == MachineLifecycle::Active => {
                        notes.push(format!(
                            "bootstrap membership cleanup skipped: machine '{}' is active",
                            machine_id.as_str()
                        ));
                    }
                    Ok(Some(_machine)) => {
                        if let Err(err) = active.mesh.store.delete_machine(machine_id).await {
                            notes.push(format!("bootstrap membership cleanup failed: {err}"));
                        } else {
                            notes.push(format!(
                                "bootstrap membership seed '{}' removed",
                                machine_id.as_str()
                            ));
                        }
                    }
                    Ok(None) => {}
                    Err(err) => notes.push(format!("bootstrap membership lookup failed: {err}")),
                }
            } else {
                notes.push("bootstrap membership cleanup skipped: no running network".into());
            }
        }

        let add_stage = match record.stage.parse::<MachineAddStage>() {
            Ok(stage) => stage,
            Err(err) => {
                notes.push(format!("remote cleanup skipped: {err}"));
                return Ok(Some(notes.join("; ")));
            }
        };
        if !matches!(
            add_stage,
            MachineAddStage::Joined
                | MachineAddStage::BootstrapPublished
                | MachineAddStage::SelfRecorded
                | MachineAddStage::Ready
                | MachineAddStage::Enabled
                | MachineAddStage::Finalized
        ) {
            return Ok((!notes.is_empty()).then_some(notes.join("; ")));
        }

        if record.artifacts.uses_operation_identity {
            notes.push("remote cleanup skipped: operation-scoped ssh identity is unavailable after restart".into());
            return Ok(Some(notes.join("; ")));
        }
        let Some(network_name) = record.network_name.as_deref() else {
            notes.push("remote cleanup skipped: network name missing".into());
            return Ok(Some(notes.join("; ")));
        };
        let [target] = record.targets.as_slice() else {
            notes.push("remote cleanup skipped: operation targets were not single-target".into());
            return Ok(Some(notes.join("; ")));
        };
        match best_effort_remote_cleanup(target, network_name, &SshOptions::default()).await {
            Ok(()) => notes.push(format!("remote cleanup attempted for '{target}'")),
            Err(err) => notes.push(format!("remote cleanup failed for '{target}': {err}")),
        }
        Ok(Some(notes.join("; ")))
    }
}

fn merge_operation_notes(existing: Option<&str>, next: &str) -> String {
    let mut notes = BTreeMap::new();
    if let Some(existing) = existing {
        notes.insert(existing.to_string(), ());
    }
    notes.insert(next.to_string(), ());
    notes.into_keys().collect::<Vec<_>>().join("; ")
}
