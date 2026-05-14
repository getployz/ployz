use std::collections::BTreeMap;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::daemon::DaemonState;
use crate::daemon::ssh::SshOptions;
use ployz_api::{
    DaemonPayload, MachineOperationInfo, MachineOperationListPayload, MachineOperationPayload,
};
use ployz_model::{MachineId, MachineLifecycle};
use ployz_operation_store::FileOperationStore;
use ployz_store_api::MachineMembershipStore;
use ployz_time::now_unix_secs;

use super::join::rollback::best_effort_remote_cleanup;
use super::types::MachineAddStage;

const OPERATIONS_DIR_NAME: &str = "machine-operations";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(super) enum MachineOperationKind {
    Init,
    Add,
    Update,
    StoragePromote,
}

impl MachineOperationKind {
    #[must_use]
    fn as_str(self) -> &'static str {
        match self {
            Self::Init => "init",
            Self::Add => "add",
            Self::Update => "update",
            Self::StoragePromote => "storage-promote",
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct MachineOperationTransition {
    status: MachineOperationStatus,
    last_error: Option<String>,
    at_unix_secs: u64,
}

impl MachineOperationTransition {
    fn succeed(at_unix_secs: u64) -> Self {
        Self {
            status: MachineOperationStatus::Succeeded,
            last_error: None,
            at_unix_secs,
        }
    }

    fn fail(last_error: String, at_unix_secs: u64) -> Self {
        Self {
            status: MachineOperationStatus::Failed,
            last_error: Some(last_error),
            at_unix_secs,
        }
    }

    fn interrupt(last_error: Option<String>, at_unix_secs: u64) -> Self {
        Self {
            status: MachineOperationStatus::Interrupted,
            last_error,
            at_unix_secs,
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_version: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "kebab-case", deny_unknown_fields)]
pub(super) enum MachineOperationState {
    Running {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
    },
    Succeeded {},
    Failed {
        last_error: String,
    },
    Interrupted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        last_error: Option<String>,
    },
}

impl MachineOperationState {
    #[must_use]
    fn status(&self) -> MachineOperationStatus {
        match self {
            Self::Running { .. } => MachineOperationStatus::Running,
            Self::Succeeded { .. } => MachineOperationStatus::Succeeded,
            Self::Failed { .. } => MachineOperationStatus::Failed,
            Self::Interrupted { .. } => MachineOperationStatus::Interrupted,
        }
    }

    #[must_use]
    fn last_error(&self) -> Option<&str> {
        match self {
            Self::Running { last_error } | Self::Interrupted { last_error } => {
                last_error.as_deref()
            }
            Self::Failed { last_error } => Some(last_error.as_str()),
            Self::Succeeded { .. } => None,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub(super) struct MachineOperationRecord {
    pub id: String,
    pub kind: MachineOperationKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub network_name: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub targets: Vec<String>,
    pub stage: String,
    pub started_at: u64,
    pub updated_at: u64,
    pub state: MachineOperationState,
    #[serde(default)]
    pub artifacts: MachineOperationArtifacts,
}

impl MachineOperationRecord {
    #[must_use]
    pub(super) fn status(&self) -> MachineOperationStatus {
        self.state.status()
    }

    #[must_use]
    pub(super) fn last_error(&self) -> Option<&str> {
        self.state.last_error()
    }

    #[must_use]
    pub(super) fn info(&self) -> MachineOperationInfo {
        MachineOperationInfo {
            id: self.id.clone(),
            kind: self.kind.as_str().into(),
            network_name: self.network_name.clone(),
            targets: self.targets.clone(),
            status: self.status().as_str().into(),
            stage: self.stage.clone(),
            started_at: self.started_at,
            updated_at: self.updated_at,
            last_error: self.last_error().map(str::to_string),
            machine_id: self.artifacts.machine_id.clone(),
            invite_id: self.artifacts.invite_id.clone(),
            allocated_subnet: self.artifacts.allocated_subnet.clone(),
        }
    }

    fn apply_transition(&mut self, transition: MachineOperationTransition) {
        let MachineOperationTransition {
            status,
            last_error,
            at_unix_secs,
        } = transition;
        self.state = match status {
            MachineOperationStatus::Running => MachineOperationState::Running {
                last_error: last_error.or_else(|| self.last_error().map(str::to_string)),
            },
            MachineOperationStatus::Succeeded => MachineOperationState::Succeeded {},
            MachineOperationStatus::Failed => MachineOperationState::Failed {
                last_error: last_error.unwrap_or_else(|| "machine operation failed".into()),
            },
            MachineOperationStatus::Interrupted => {
                MachineOperationState::Interrupted { last_error }
            }
        };
        self.updated_at = at_unix_secs;
    }
}

#[derive(Debug, Clone)]
pub(super) struct MachineOperationStore {
    files: FileOperationStore,
}

impl MachineOperationStore {
    #[must_use]
    pub(super) fn new(root: PathBuf) -> Self {
        Self {
            files: FileOperationStore::new(root, OPERATIONS_DIR_NAME, "machine operation"),
        }
    }

    pub(super) fn begin(
        &self,
        kind: MachineOperationKind,
        network_name: Option<String>,
        targets: Vec<String>,
        stage: impl Into<String>,
        artifacts: MachineOperationArtifacts,
    ) -> Result<MachineOperationRecord, String> {
        let now = now_unix_secs();
        let record = MachineOperationRecord {
            id: unique_operation_id(kind, now),
            kind,
            network_name,
            targets,
            stage: stage.into(),
            started_at: now,
            updated_at: now,
            state: MachineOperationState::Running { last_error: None },
            artifacts,
        };
        self.save(&record)?;
        Ok(record)
    }

    pub(super) fn begin_with_id(
        &self,
        id: String,
        kind: MachineOperationKind,
        network_name: Option<String>,
        targets: Vec<String>,
        stage: impl Into<String>,
        artifacts: MachineOperationArtifacts,
    ) -> Result<MachineOperationRecord, String> {
        validate_operation_id(&id)?;
        let now = now_unix_secs();
        let record = MachineOperationRecord {
            id,
            kind,
            network_name,
            targets,
            stage: stage.into(),
            started_at: now,
            updated_at: now,
            state: MachineOperationState::Running { last_error: None },
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
        record.updated_at = now_unix_secs();
        self.save(record)
    }

    pub(super) fn update_status(
        &self,
        record: &mut MachineOperationRecord,
        status: MachineOperationStatus,
        last_error: Option<String>,
    ) -> Result<(), String> {
        let at_unix_secs = now_unix_secs();
        let transition = match status {
            MachineOperationStatus::Running => MachineOperationTransition {
                status,
                last_error,
                at_unix_secs,
            },
            MachineOperationStatus::Succeeded => MachineOperationTransition::succeed(at_unix_secs),
            MachineOperationStatus::Failed => {
                MachineOperationTransition::fail(last_error.unwrap_or_default(), at_unix_secs)
            }
            MachineOperationStatus::Interrupted => {
                MachineOperationTransition::interrupt(last_error, at_unix_secs)
            }
        };
        record.apply_transition(transition);
        self.save(record)
    }

    pub(super) fn save(&self, record: &MachineOperationRecord) -> Result<(), String> {
        self.files.save(&record.id, record)
    }

    pub(super) fn load(&self, id: &str) -> Result<Option<MachineOperationRecord>, String> {
        self.files.load(id)
    }

    pub(super) fn list(&self) -> Result<Vec<MachineOperationRecord>, String> {
        self.files.list(
            |record: &MachineOperationRecord| record.started_at,
            |record| &record.id,
        )
    }
}

impl DaemonState {
    pub(super) fn machine_operation_store(&self) -> MachineOperationStore {
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
                match super::list::find_machine_record(&active.mesh.store, machine_id).await {
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

fn unique_operation_id(kind: MachineOperationKind, now: u64) -> String {
    ployz_operation_store::unique_operation_id("machine", kind.as_str(), now)
}

#[cfg(test)]
const MAX_OPERATION_ID_LEN: usize = ployz_operation_store::MAX_OPERATION_ID_LEN;

fn validate_operation_id(id: &str) -> Result<(), String> {
    ployz_operation_store::validate_operation_id("machine operation", id)
}

fn merge_operation_notes(existing: Option<&str>, next: &str) -> String {
    let mut notes = BTreeMap::new();
    if let Some(existing) = existing {
        notes.insert(existing.to_string(), ());
    }
    notes.insert(next.to_string(), ());
    notes.into_keys().collect::<Vec<_>>().join("; ")
}

#[cfg(test)]
mod tests {
    use super::{
        MAX_OPERATION_ID_LEN, MachineOperationArtifacts, MachineOperationKind,
        MachineOperationRecord, MachineOperationStatus, MachineOperationStore, unique_operation_id,
        validate_operation_id,
    };
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir(label: &str) -> PathBuf {
        static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);
        let sequence = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}-{sequence}", std::process::id()))
    }

    #[test]
    fn validate_operation_id_accepts_generated_ids() {
        assert!(
            validate_operation_id(&unique_operation_id(MachineOperationKind::Update, 42)).is_ok()
        );
        assert!(validate_operation_id("custom_id-123").is_ok());
    }

    #[test]
    fn validate_operation_id_rejects_path_traversal() {
        assert!(validate_operation_id("../etc/passwd").is_err());
        assert!(validate_operation_id("/etc/passwd").is_err());
        assert!(validate_operation_id("a/b").is_err());
        assert!(validate_operation_id("..").is_err());
        assert!(validate_operation_id(".hidden").is_err());
        assert!(validate_operation_id("with space").is_err());
    }

    #[test]
    fn validate_operation_id_rejects_empty_and_oversized() {
        assert!(validate_operation_id("").is_err());
        let oversized = "a".repeat(MAX_OPERATION_ID_LEN + 1);
        assert!(validate_operation_id(&oversized).is_err());
    }

    #[test]
    fn store_begin_with_id_rejects_traversal_id() {
        let store = MachineOperationStore::new(unique_temp_dir("ployz-machine-ops-test"));
        let result = store.begin_with_id(
            "../../etc/passwd".into(),
            MachineOperationKind::Update,
            None,
            vec!["self".into()],
            "execute",
            MachineOperationArtifacts::default(),
        );
        assert!(
            result.is_err(),
            "begin_with_id should reject path traversal"
        );
    }

    #[test]
    fn store_load_rejects_traversal_id() {
        let store = MachineOperationStore::new(unique_temp_dir("ployz-machine-ops-test"));
        assert!(store.load("../../etc/passwd").is_err());
    }

    #[test]
    fn operation_failure_remains_visible_until_success() {
        let store = MachineOperationStore::new(unique_temp_dir("ployz-machine-ops-test"));
        let mut record = store
            .begin_with_id(
                "op-visible-failure".into(),
                MachineOperationKind::Add,
                Some("alpha".into()),
                vec!["machine-a".into()],
                "bootstrap",
                MachineOperationArtifacts::default(),
            )
            .expect("begin operation");

        store
            .update_status(
                &mut record,
                MachineOperationStatus::Failed,
                Some("bootstrap failed".into()),
            )
            .expect("mark failed");
        store
            .update_status(&mut record, MachineOperationStatus::Running, None)
            .expect("mark running");
        store
            .update_stage(&mut record, "cleanup")
            .expect("update stage");

        let loaded = store
            .load("op-visible-failure")
            .expect("load operation")
            .expect("operation present");
        assert_eq!(loaded.status(), MachineOperationStatus::Running);
        assert_eq!(loaded.stage, "cleanup");
        assert_eq!(loaded.last_error(), Some("bootstrap failed"));
        assert_eq!(
            loaded.info().last_error.as_deref(),
            Some("bootstrap failed")
        );

        store
            .update_status(&mut record, MachineOperationStatus::Succeeded, None)
            .expect("mark succeeded");
        let succeeded = store
            .load("op-visible-failure")
            .expect("load succeeded operation")
            .expect("operation present");
        assert_eq!(succeeded.status(), MachineOperationStatus::Succeeded);
        assert_eq!(succeeded.last_error(), None);
    }

    #[test]
    fn operation_state_rejects_success_with_error() {
        let json = serde_json::json!({
            "id": "op-invalid",
            "kind": "add",
            "network_name": "alpha",
            "targets": ["machine-a"],
            "stage": "bootstrap",
            "started_at": 1,
            "updated_at": 1,
            "state": {
                "status": "succeeded",
                "last_error": "bootstrap failed"
            },
            "artifacts": {}
        });

        serde_json::from_value::<MachineOperationRecord>(json)
            .expect_err("succeeded machine operation cannot carry last_error");
    }
}
