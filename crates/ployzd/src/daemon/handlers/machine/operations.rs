use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::daemon::DaemonState;
use crate::daemon::ssh::SshOptions;
use crate::model::MachineId;
use ployz_sdk::transport::{
    DaemonPayload, MachineOperationInfo, MachineOperationListPayload, MachineOperationPayload,
};

use super::join::{best_effort_remote_cleanup, remove_transient_peer};
use super::types::MachineAddStage;

const OPERATIONS_DIR_NAME: &str = "machine-operations";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum MachineOperationKind {
    Init,
    Add,
    Heal,
}

impl MachineOperationKind {
    #[must_use]
    fn as_str(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Add => "add",
            Self::Heal => "heal",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum MachineOperationStatus {
    Running,
    Succeeded,
    Failed,
    Interrupted,
}

impl MachineOperationStatus {
    #[must_use]
    fn as_str(self) -> &'static str {
        match self {
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Interrupted => "interrupted",
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub(super) struct MachineOperationArtifacts {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub machine_id: Option<MachineId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub invite_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allocated_subnet: Option<String>,
    #[serde(default)]
    pub uses_operation_identity: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct MachineOperationRecord {
    pub id: String,
    pub kind: MachineOperationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    pub status: MachineOperationStatus,
    pub stage: String,
    pub started_at: u64,
    pub updated_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub artifacts: MachineOperationArtifacts,
}

impl MachineOperationRecord {
    #[must_use]
    pub(super) fn info(&self) -> MachineOperationInfo {
        MachineOperationInfo {
            id: self.id.clone(),
            kind: self.kind.as_str().into(),
            network_name: self.network_name.clone(),
            targets: self.targets.clone(),
            status: self.status.as_str().into(),
            stage: self.stage.clone(),
            started_at: self.started_at,
            updated_at: self.updated_at,
            last_error: self.last_error.clone(),
            machine_id: self.artifacts.machine_id.clone(),
            invite_id: self.artifacts.invite_id.clone(),
            allocated_subnet: self.artifacts.allocated_subnet.clone(),
        }
    }
}

#[derive(Debug, Clone)]
pub(super) struct MachineOperationStore {
    root: PathBuf,
}

impl MachineOperationStore {
    #[must_use]
    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    pub(super) fn begin(
        &self,
        kind: MachineOperationKind,
        network_name: Option<String>,
        targets: Vec<String>,
        stage: impl Into<String>,
        artifacts: MachineOperationArtifacts,
    ) -> Result<MachineOperationRecord, String> {
        let now = crate::time::now_unix_secs();
        let record = MachineOperationRecord {
            id: unique_operation_id(kind, now),
            kind,
            network_name,
            targets,
            status: MachineOperationStatus::Running,
            stage: stage.into(),
            started_at: now,
            updated_at: now,
            last_error: None,
            artifacts,
        };
        self.save(&record)?;
        Ok(record)
    }

    pub(super) fn update_stage(
        &self,
        record: &mut MachineOperationRecord,
        stage: impl Into<String>,
    ) -> Result<(), String> {
        record.stage = stage.into();
        record.updated_at = crate::time::now_unix_secs();
        self.save(record)
    }

    pub(super) fn update_status(
        &self,
        record: &mut MachineOperationRecord,
        status: MachineOperationStatus,
        last_error: Option<String>,
    ) -> Result<(), String> {
        record.status = status;
        if let Some(last_error) = last_error {
            record.last_error = Some(last_error);
        }
        record.updated_at = crate::time::now_unix_secs();
        self.save(record)
    }

    pub(super) fn save(&self, record: &MachineOperationRecord) -> Result<(), String> {
        let path = self.path_for(&record.id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)
                .map_err(|err| format!("create machine operations dir '{}': {err}", parent.display()))?;
        }
        let body = serde_json::to_vec_pretty(record)
            .map_err(|err| format!("encode machine operation '{}': {err}", record.id))?;
        std::fs::write(&path, body)
            .map_err(|err| format!("write machine operation '{}': {err}", path.display()))
    }

    pub(super) fn load(&self, id: &str) -> Result<Option<MachineOperationRecord>, String> {
        let path = self.path_for(id);
        if !path.exists() {
            return Ok(None);
        }
        read_machine_operation(&path).map(Some)
    }

    pub(super) fn list(&self) -> Result<Vec<MachineOperationRecord>, String> {
        let dir = self.dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(format!(
                    "read machine operations dir '{}': {err}",
                    dir.display()
                ));
            }
        };

        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| format!("read machine operation entry: {err}"))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            records.push(read_machine_operation(&path)?);
        }
        records.sort_by(|left, right| {
            right
                .started_at
                .cmp(&left.started_at)
                .then(left.id.cmp(&right.id))
        });
        Ok(records)
    }

    fn dir(&self) -> PathBuf {
        self.root.join(OPERATIONS_DIR_NAME)
    }

    fn path_for(&self, id: &str) -> PathBuf {
        self.dir().join(format!("{id}.json"))
    }
}

impl DaemonState {
    pub(super) fn machine_operation_store(&self) -> MachineOperationStore {
        MachineOperationStore::new(self.data_dir.clone())
    }

    pub(crate) async fn handle_machine_operation_list(&self) -> ployz_sdk::transport::DaemonResponse {
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
                    record.status.as_str(),
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

    pub(crate) async fn handle_machine_operation_get(
        &self,
        id: &str,
    ) -> ployz_sdk::transport::DaemonResponse {
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

    pub async fn reconcile_machine_operations_on_startup(&self) {
        let store = self.machine_operation_store();
        let records = match store.list() {
            Ok(records) => records,
            Err(err) => {
                tracing::warn!(error = %err, "machine operation reconciliation: list failed");
                return;
            }
        };

        for mut record in records {
            if record.status != MachineOperationStatus::Running {
                continue;
            }
            if let Err(err) = store.update_status(
                &mut record,
                MachineOperationStatus::Interrupted,
                Some("daemon restarted before operation completed".into()),
            ) {
                tracing::warn!(error = %err, operation_id = %record.id, "machine operation reconciliation: mark interrupted failed");
                continue;
            }

            let note = match self.reconcile_machine_operation(&record).await {
                Ok(note) => note,
                Err(err) => Some(err),
            };
            if let Some(note) = note {
                let combined = merge_operation_notes(record.last_error.as_deref(), &note);
                if let Err(err) =
                    store.update_status(&mut record, MachineOperationStatus::Interrupted, Some(combined))
                {
                    tracing::warn!(error = %err, operation_id = %record.id, "machine operation reconciliation: update note failed");
                }
            }
        }
    }

    async fn reconcile_machine_operation(
        &self,
        record: &MachineOperationRecord,
    ) -> Result<Option<String>, String> {
        match record.kind {
            MachineOperationKind::Init | MachineOperationKind::Heal => Ok(None),
            MachineOperationKind::Add => self.reconcile_machine_add_operation(record).await,
        }
    }

    async fn reconcile_machine_add_operation(
        &self,
        record: &MachineOperationRecord,
    ) -> Result<Option<String>, String> {
        let mut notes = Vec::new();
        if let Some(machine_id) = &record.artifacts.machine_id {
            let Some(active) = self.active.as_ref() else {
                notes.push("transient peer cleanup skipped: no running network".into());
                return Ok(Some(notes.join("; ")));
            };
            let Some(peer_sync_tx) = active.mesh.peer_sync_sender() else {
                notes.push("transient peer cleanup skipped: peer sync unavailable".into());
                return Ok(Some(notes.join("; ")));
            };
            if let Err(err) = remove_transient_peer(&peer_sync_tx, machine_id).await {
                notes.push(format!("transient peer cleanup failed: {err}"));
            } else {
                notes.push(format!("transient peer '{}' removed", machine_id.0));
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
                | MachineAddStage::SelfRecorded
                | MachineAddStage::TransientPeerInstalled
                | MachineAddStage::Ready
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

fn unique_operation_id(kind: MachineOperationKind, now: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time after epoch")
        .as_nanos();
    format!("machine-{}-{now}-{nanos}", kind.as_str())
}

fn read_machine_operation(path: &Path) -> Result<MachineOperationRecord, String> {
    let body = std::fs::read(path)
        .map_err(|err| format!("read machine operation '{}': {err}", path.display()))?;
    serde_json::from_slice(&body)
        .map_err(|err| format!("decode machine operation '{}': {err}", path.display()))
}

fn merge_operation_notes(existing: Option<&str>, next: &str) -> String {
    let mut notes = BTreeMap::new();
    if let Some(existing) = existing {
        notes.insert(existing.to_string(), ());
    }
    notes.insert(next.to_string(), ());
    notes.into_keys().collect::<Vec<_>>().join("; ")
}
