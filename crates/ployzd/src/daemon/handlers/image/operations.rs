use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ployz_api::{DaemonPayload, ImageOperationListPayload, ImageOperationPayload};
use ployz_types::model::{
    ImageDigest, ImageOperationKind, ImageOperationRecord, ImageOperationTargetOutcome, MachineId,
    OperationStatus,
};
use ployz_types::time::now_unix_secs;

use crate::daemon::DaemonState;

const OPERATIONS_DIR_NAME: &str = "image-operations";
const MAX_OPERATION_ID_LEN: usize = 128;

#[derive(Debug, Clone)]
pub(super) struct ImageOperationStore {
    root: PathBuf,
}

impl ImageOperationStore {
    #[must_use]
    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    #[allow(dead_code)]
    pub(super) fn begin(
        &self,
        kind: ImageOperationKind,
        stage: impl Into<String>,
        digest: Option<ImageDigest>,
        source_machine: Option<MachineId>,
        target_machines: Vec<MachineId>,
    ) -> Result<ImageOperationRecord, String> {
        let now = now_unix_secs();
        let record = ImageOperationRecord {
            id: unique_operation_id(kind, now),
            kind,
            status: OperationStatus::Running,
            stage: stage.into(),
            digest,
            source_machine,
            targets: target_machines
                .into_iter()
                .map(running_target_outcome)
                .collect(),
            started_at: now,
            updated_at: now,
            last_error: None,
        };
        self.save(&record)?;
        Ok(record)
    }

    #[allow(dead_code)]
    pub(super) fn begin_with_id(
        &self,
        id: String,
        kind: ImageOperationKind,
        stage: impl Into<String>,
        digest: Option<ImageDigest>,
        source_machine: Option<MachineId>,
        target_machines: Vec<MachineId>,
    ) -> Result<ImageOperationRecord, String> {
        validate_operation_id(&id)?;
        let now = now_unix_secs();
        let record = ImageOperationRecord {
            id,
            kind,
            status: OperationStatus::Running,
            stage: stage.into(),
            digest,
            source_machine,
            targets: target_machines
                .into_iter()
                .map(running_target_outcome)
                .collect(),
            started_at: now,
            updated_at: now,
            last_error: None,
        };
        self.save(&record)?;
        Ok(record)
    }

    #[allow(dead_code)]
    pub(super) fn update_stage(
        &self,
        record: &mut ImageOperationRecord,
        stage: impl Into<String>,
    ) -> Result<(), String> {
        record.stage = stage.into();
        record.updated_at = now_unix_secs();
        self.save(record)
    }

    pub(super) fn update_status(
        &self,
        record: &mut ImageOperationRecord,
        status: OperationStatus,
        last_error: Option<String>,
    ) -> Result<(), String> {
        apply_status_transition(
            &mut record.status,
            &mut record.last_error,
            &mut record.updated_at,
            status,
            last_error,
        );
        self.save(record)
    }

    #[allow(dead_code)]
    pub(super) fn update_target(
        &self,
        record: &mut ImageOperationRecord,
        outcome: ImageOperationTargetOutcome,
    ) -> Result<(), String> {
        let machine_id = outcome.machine_id.clone();
        match record
            .targets
            .iter_mut()
            .find(|target| target.machine_id == machine_id)
        {
            Some(existing) => *existing = outcome,
            None => record.targets.push(outcome),
        }
        record.updated_at = now_unix_secs();
        self.save(record)
    }

    pub(super) fn save(&self, record: &ImageOperationRecord) -> Result<(), String> {
        validate_operation_id(&record.id)?;
        let path = self.path_for(&record.id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                format!("create image operations dir '{}': {err}", parent.display())
            })?;
        }
        let body = serde_json::to_vec_pretty(record)
            .map_err(|err| format!("encode image operation '{}': {err}", record.id))?;
        std::fs::write(&path, body)
            .map_err(|err| format!("write image operation '{}': {err}", path.display()))
    }

    pub(super) fn load(&self, id: &str) -> Result<Option<ImageOperationRecord>, String> {
        validate_operation_id(id)?;
        let path = self.path_for(id);
        match read_image_operation(&path) {
            Ok(record) => Ok(Some(record)),
            Err(ReadImageOperationError::NotFound) => Ok(None),
            Err(ReadImageOperationError::Other(message)) => Err(message),
        }
    }

    pub(super) fn list(&self) -> Result<Vec<ImageOperationRecord>, String> {
        let dir = self.dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(format!(
                    "read image operations dir '{}': {err}",
                    dir.display()
                ));
            }
        };

        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| format!("read image operation entry: {err}"))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            match read_image_operation(&path) {
                Ok(record) => records.push(record),
                Err(ReadImageOperationError::NotFound) => continue,
                Err(ReadImageOperationError::Other(message)) => return Err(message),
            }
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
    pub(super) fn image_operation_store(&self) -> ImageOperationStore {
        ImageOperationStore::new(self.data_dir.clone())
    }

    pub(crate) async fn handle_image_operation_list(&self) -> ployz_api::DaemonResponse {
        let operations = match self.image_operation_store().list() {
            Ok(records) => records,
            Err(err) => return self.err("IMAGE_OPERATION_LIST_FAILED", err),
        };
        if operations.is_empty() {
            return self.ok_with_payload(
                "no image operations",
                Some(DaemonPayload::ImageOperationList(
                    ImageOperationListPayload { operations },
                )),
            );
        }

        let lines = operations
            .iter()
            .map(|record| {
                let digest = record
                    .digest
                    .as_ref()
                    .map(ImageDigest::as_str)
                    .unwrap_or("-");
                format!(
                    "{}  {}  {}  {}  {}",
                    record.id, record.kind, record.status, digest, record.stage
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        self.ok_with_payload(
            lines,
            Some(DaemonPayload::ImageOperationList(
                ImageOperationListPayload { operations },
            )),
        )
    }

    pub(crate) async fn handle_image_operation_get(&self, id: &str) -> ployz_api::DaemonResponse {
        let record = match self.image_operation_store().load(id) {
            Ok(Some(record)) => record,
            Ok(None) => {
                return self.err(
                    "IMAGE_OPERATION_NOT_FOUND",
                    format!("image operation '{id}' not found"),
                );
            }
            Err(err) => return self.err("IMAGE_OPERATION_GET_FAILED", err),
        };

        let payload = ImageOperationPayload {
            operation: record.clone(),
        };
        match serde_json::to_string_pretty(&record) {
            Ok(body) => self.ok_with_payload(body, Some(DaemonPayload::ImageOperation(payload))),
            Err(err) => self.err(
                "ENCODE_FAILED",
                format!("failed to encode image operation: {err}"),
            ),
        }
    }

    pub async fn recover_image_operations_on_startup(&self) {
        let store = self.image_operation_store();
        let records = match store.list() {
            Ok(records) => records,
            Err(err) => {
                tracing::warn!(error = %err, "image operation startup recovery: list failed");
                return;
            }
        };

        for mut record in records {
            if record.status != OperationStatus::Running {
                continue;
            }
            if let Err(err) = store.update_status(
                &mut record,
                OperationStatus::Interrupted,
                Some(
                    "daemon restarted before image operation completed; inspect image status before retrying"
                        .into(),
                ),
            ) {
                tracing::warn!(error = %err, operation_id = %record.id, "image operation startup recovery: mark interrupted failed");
            }
        }
    }
}

#[allow(dead_code)]
fn running_target_outcome(machine_id: MachineId) -> ImageOperationTargetOutcome {
    ImageOperationTargetOutcome {
        machine_id,
        status: OperationStatus::Running,
        bytes_transferred: None,
        last_error: None,
    }
}

fn apply_status_transition(
    current_status: &mut OperationStatus,
    current_error: &mut Option<String>,
    updated_at: &mut u64,
    status: OperationStatus,
    last_error: Option<String>,
) {
    *current_status = status;
    match status {
        OperationStatus::Succeeded => *current_error = None,
        OperationStatus::Failed | OperationStatus::Interrupted | OperationStatus::Running => {
            if let Some(last_error) = last_error {
                *current_error = Some(last_error);
            }
        }
    }
    *updated_at = now_unix_secs();
}

#[allow(dead_code)]
fn unique_operation_id(kind: ImageOperationKind, now: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time after epoch")
        .as_nanos();
    format!("image-{kind}-{now}-{nanos}")
}

pub(super) fn validate_operation_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("image operation id cannot be empty".into());
    }
    if id.len() > MAX_OPERATION_ID_LEN {
        return Err(format!(
            "image operation id exceeds {MAX_OPERATION_ID_LEN} characters"
        ));
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(format!(
            "image operation id '{id}' contains characters outside [A-Za-z0-9_-]"
        ));
    }
    Ok(())
}

enum ReadImageOperationError {
    NotFound,
    Other(String),
}

fn read_image_operation(path: &Path) -> Result<ImageOperationRecord, ReadImageOperationError> {
    let body = match std::fs::read(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ReadImageOperationError::NotFound);
        }
        Err(error) => {
            return Err(ReadImageOperationError::Other(format!(
                "read image operation '{}': {error}",
                path.display()
            )));
        }
    };
    serde_json::from_slice(&body).map_err(|err| {
        ReadImageOperationError::Other(format!(
            "decode image operation '{}': {err}",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::{
        ImageOperationStore, MAX_OPERATION_ID_LEN, unique_operation_id, validate_operation_id,
    };
    use crate::daemon::DaemonState;
    use ployz_runtime_api::Identity;
    use ployz_types::model::{
        ImageDigest, ImageOperationKind, ImageOperationTargetOutcome, MachineId, OperationStatus,
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

    fn digest() -> ImageDigest {
        ImageDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("valid digest")
    }

    fn make_state() -> DaemonState {
        let data_dir = unique_temp_dir("ployz-image-op-state");
        let identity = Identity::generate(MachineId::new("founder"), [31; 32]);
        DaemonState::new_for_tests(
            &data_dir,
            identity,
            "10.210.0.0/16".into(),
            24,
            4319,
            "127.0.0.1:0".into(),
            None,
            1,
        )
    }

    #[test]
    fn validate_operation_id_accepts_generated_ids() {
        assert!(validate_operation_id(&unique_operation_id(ImageOperationKind::Push, 42)).is_ok());
        assert!(validate_operation_id("image_push-123").is_ok());
    }

    #[test]
    fn validate_operation_id_rejects_unsafe_ids() {
        assert!(validate_operation_id("").is_err());
        assert!(validate_operation_id("../etc/passwd").is_err());
        assert!(validate_operation_id("/etc/passwd").is_err());
        assert!(validate_operation_id("a/b").is_err());
        assert!(validate_operation_id("..").is_err());
        assert!(validate_operation_id("with space").is_err());
        assert!(validate_operation_id(&"a".repeat(MAX_OPERATION_ID_LEN + 1)).is_err());
    }

    #[test]
    fn store_rejects_path_traversal_id() {
        let store = ImageOperationStore::new(unique_temp_dir("ployz-image-ops-test"));
        let result = store.begin_with_id(
            "../../etc/passwd".into(),
            ImageOperationKind::Push,
            "streaming",
            Some(digest()),
            None,
            vec![MachineId::new("machine-a")],
        );
        assert!(result.is_err());
        assert!(store.load("../../etc/passwd").is_err());
    }

    #[test]
    fn target_outcome_updates_are_persisted() {
        let store = ImageOperationStore::new(unique_temp_dir("ployz-image-ops-test"));
        let mut record = store
            .begin_with_id(
                "image-visible-target".into(),
                ImageOperationKind::Distribute,
                "streaming",
                Some(digest()),
                Some(MachineId::new("source")),
                vec![MachineId::new("target-a")],
            )
            .expect("begin operation");

        store
            .update_target(
                &mut record,
                ImageOperationTargetOutcome {
                    machine_id: MachineId::new("target-a"),
                    status: OperationStatus::Failed,
                    bytes_transferred: Some(128),
                    last_error: Some("disk full".into()),
                },
            )
            .expect("update target");

        let loaded = store
            .load("image-visible-target")
            .expect("load operation")
            .expect("operation exists");
        assert_eq!(loaded.targets.len(), 1);
        assert_eq!(loaded.targets[0].status, OperationStatus::Failed);
        assert_eq!(loaded.targets[0].bytes_transferred, Some(128));
        assert_eq!(loaded.targets[0].last_error.as_deref(), Some("disk full"));
    }

    #[test]
    fn running_failure_remains_visible_until_success() {
        let store = ImageOperationStore::new(unique_temp_dir("ployz-image-ops-test"));
        let mut record = store
            .begin_with_id(
                "image-visible-failure".into(),
                ImageOperationKind::Push,
                "streaming",
                Some(digest()),
                None,
                vec![MachineId::new("target-a")],
            )
            .expect("begin operation");

        store
            .update_status(
                &mut record,
                OperationStatus::Failed,
                Some("copy failed".into()),
            )
            .expect("mark failed");
        store
            .update_status(&mut record, OperationStatus::Running, None)
            .expect("mark running");
        store
            .update_stage(&mut record, "retrying")
            .expect("update stage");

        let running = store
            .load("image-visible-failure")
            .expect("load running operation")
            .expect("operation exists");
        assert_eq!(running.status, OperationStatus::Running);
        assert_eq!(running.last_error.as_deref(), Some("copy failed"));

        store
            .update_status(&mut record, OperationStatus::Succeeded, None)
            .expect("mark succeeded");
        let succeeded = store
            .load("image-visible-failure")
            .expect("load succeeded operation")
            .expect("operation exists");
        assert_eq!(succeeded.status, OperationStatus::Succeeded);
        assert_eq!(succeeded.last_error, None);
    }

    #[test]
    fn list_orders_newest_first() {
        let store = ImageOperationStore::new(unique_temp_dir("ployz-image-ops-test"));
        let mut older = store
            .begin_with_id(
                "older".into(),
                ImageOperationKind::Inspect,
                "inspect",
                Some(digest()),
                None,
                Vec::new(),
            )
            .expect("begin older");
        older.started_at = 1;
        older.updated_at = 1;
        store.save(&older).expect("save older");

        let mut newer = store
            .begin_with_id(
                "newer".into(),
                ImageOperationKind::Inspect,
                "inspect",
                Some(digest()),
                None,
                Vec::new(),
            )
            .expect("begin newer");
        newer.started_at = 2;
        newer.updated_at = 2;
        store.save(&newer).expect("save newer");

        let ids = store
            .list()
            .expect("list operations")
            .into_iter()
            .map(|record| record.id)
            .collect::<Vec<_>>();
        assert_eq!(ids, vec!["newer", "older"]);
    }

    #[tokio::test]
    async fn startup_recovery_marks_running_operation_interrupted() {
        let state = make_state();
        let store = state.image_operation_store();
        let operation = store
            .begin(
                ImageOperationKind::Push,
                "streaming",
                Some(digest()),
                None,
                vec![MachineId::new("target-a")],
            )
            .expect("begin operation");

        state.recover_image_operations_on_startup().await;

        let recovered = state
            .image_operation_store()
            .load(&operation.id)
            .expect("load operation")
            .expect("operation exists");
        assert_eq!(recovered.status, OperationStatus::Interrupted);
        assert!(
            recovered
                .last_error
                .as_deref()
                .expect("last error")
                .contains("daemon restarted")
        );
    }
}
