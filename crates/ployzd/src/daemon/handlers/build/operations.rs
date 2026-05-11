use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ployz_api::{BuildOperationListPayload, BuildOperationPayload, DaemonPayload};
use ployz_types::model::{
    BuildInputSummary, BuildLocation, BuildMethod, BuildOperationKind, BuildOperationRecord,
    ImageArtifact, OperationStatus,
};
use ployz_types::time::now_unix_secs;

use crate::daemon::DaemonState;

const OPERATIONS_DIR_NAME: &str = "build-operations";
const MAX_OPERATION_ID_LEN: usize = 128;

#[derive(Debug, Clone)]
pub(super) struct BuildOperationStore {
    root: PathBuf,
}

impl BuildOperationStore {
    #[must_use]
    pub(super) fn new(root: PathBuf) -> Self {
        Self { root }
    }

    #[allow(dead_code)]
    pub(super) fn begin(
        &self,
        kind: BuildOperationKind,
        method: BuildMethod,
        location: BuildLocation,
        stage: impl Into<String>,
    ) -> Result<BuildOperationRecord, String> {
        self.begin_with_input_summary(kind, method, location, stage, BuildInputSummary::default())
    }

    #[allow(dead_code)]
    pub(super) fn begin_with_input_summary(
        &self,
        kind: BuildOperationKind,
        method: BuildMethod,
        location: BuildLocation,
        stage: impl Into<String>,
        inputs: BuildInputSummary,
    ) -> Result<BuildOperationRecord, String> {
        let now = now_unix_secs();
        let record = BuildOperationRecord {
            id: unique_operation_id(kind, now),
            kind,
            method,
            location,
            status: OperationStatus::Running,
            stage: stage.into(),
            inputs,
            artifact: None,
            last_error: None,
            started_at: now,
            updated_at: now,
        };
        self.save(&record)?;
        Ok(record)
    }

    #[allow(dead_code)]
    pub(super) fn begin_with_id(
        &self,
        id: String,
        kind: BuildOperationKind,
        method: BuildMethod,
        location: BuildLocation,
        stage: impl Into<String>,
    ) -> Result<BuildOperationRecord, String> {
        validate_operation_id(&id)?;
        let now = now_unix_secs();
        let record = BuildOperationRecord {
            id,
            kind,
            method,
            location,
            status: OperationStatus::Running,
            stage: stage.into(),
            inputs: BuildInputSummary::default(),
            artifact: None,
            last_error: None,
            started_at: now,
            updated_at: now,
        };
        self.save(&record)?;
        Ok(record)
    }

    #[allow(dead_code)]
    pub(super) fn update_stage(
        &self,
        record: &mut BuildOperationRecord,
        stage: impl Into<String>,
    ) -> Result<(), String> {
        record.stage = stage.into();
        record.updated_at = now_unix_secs();
        self.save(record)
    }

    #[allow(dead_code)]
    pub(super) fn update_artifact(
        &self,
        record: &mut BuildOperationRecord,
        artifact: ImageArtifact,
    ) -> Result<(), String> {
        record.artifact = Some(artifact);
        record.updated_at = now_unix_secs();
        self.save(record)
    }

    pub(super) fn update_status(
        &self,
        record: &mut BuildOperationRecord,
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

    pub(super) fn save(&self, record: &BuildOperationRecord) -> Result<(), String> {
        validate_operation_id(&record.id)?;
        let path = self.path_for(&record.id);
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|err| {
                format!("create build operations dir '{}': {err}", parent.display())
            })?;
        }
        let body = serde_json::to_vec_pretty(record)
            .map_err(|err| format!("encode build operation '{}': {err}", record.id))?;
        std::fs::write(&path, body)
            .map_err(|err| format!("write build operation '{}': {err}", path.display()))
    }

    pub(super) fn load(&self, id: &str) -> Result<Option<BuildOperationRecord>, String> {
        validate_operation_id(id)?;
        let path = self.path_for(id);
        match read_build_operation(&path) {
            Ok(record) => Ok(Some(record)),
            Err(ReadBuildOperationError::NotFound) => Ok(None),
            Err(ReadBuildOperationError::Other(message)) => Err(message),
        }
    }

    pub(super) fn list(&self) -> Result<Vec<BuildOperationRecord>, String> {
        let dir = self.dir();
        let entries = match std::fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => {
                return Err(format!(
                    "read build operations dir '{}': {err}",
                    dir.display()
                ));
            }
        };

        let mut records = Vec::new();
        for entry in entries {
            let entry = entry.map_err(|err| format!("read build operation entry: {err}"))?;
            let path = entry.path();
            if path.extension().and_then(|value| value.to_str()) != Some("json") {
                continue;
            }
            match read_build_operation(&path) {
                Ok(record) => records.push(record),
                Err(ReadBuildOperationError::NotFound) => continue,
                Err(ReadBuildOperationError::Other(message)) => return Err(message),
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
    pub(super) fn build_operation_store(&self) -> BuildOperationStore {
        BuildOperationStore::new(self.data_dir.clone())
    }

    pub(crate) async fn handle_build_operation_list(&self) -> ployz_api::DaemonResponse {
        let operations = match self.build_operation_store().list() {
            Ok(records) => records,
            Err(err) => return self.err("BUILD_OPERATION_LIST_FAILED", err),
        };
        if operations.is_empty() {
            return self.ok_with_payload(
                "no build operations",
                Some(DaemonPayload::BuildOperationList(
                    BuildOperationListPayload { operations },
                )),
            );
        }

        let lines = operations
            .iter()
            .map(|record| {
                format!(
                    "{}  {}  {}  {}  {}",
                    record.id, record.kind, record.method, record.status, record.stage
                )
            })
            .collect::<Vec<_>>()
            .join("\n");

        self.ok_with_payload(
            lines,
            Some(DaemonPayload::BuildOperationList(
                BuildOperationListPayload { operations },
            )),
        )
    }

    pub(crate) async fn handle_build_operation_get(&self, id: &str) -> ployz_api::DaemonResponse {
        let record = match self.build_operation_store().load(id) {
            Ok(Some(record)) => record,
            Ok(None) => {
                return self.err(
                    "BUILD_OPERATION_NOT_FOUND",
                    format!("build operation '{id}' not found"),
                );
            }
            Err(err) => return self.err("BUILD_OPERATION_GET_FAILED", err),
        };

        let payload = BuildOperationPayload {
            operation: record.clone(),
        };
        match serde_json::to_string_pretty(&record) {
            Ok(body) => self.ok_with_payload(body, Some(DaemonPayload::BuildOperation(payload))),
            Err(err) => self.err(
                "ENCODE_FAILED",
                format!("failed to encode build operation: {err}"),
            ),
        }
    }

    pub async fn recover_build_operations_on_startup(&self) {
        let store = self.build_operation_store();
        let records = match store.list() {
            Ok(records) => records,
            Err(err) => {
                tracing::warn!(error = %err, "build operation startup recovery: list failed");
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
                    "daemon restarted before build completed; retry the build or inspect any recorded artifact"
                        .into(),
                ),
            ) {
                tracing::warn!(error = %err, operation_id = %record.id, "build operation startup recovery: mark interrupted failed");
            }
        }
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
fn unique_operation_id(kind: BuildOperationKind, now: u64) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("time after epoch")
        .as_nanos();
    format!("build-{kind}-{now}-{nanos}")
}

fn validate_operation_id(id: &str) -> Result<(), String> {
    if id.is_empty() {
        return Err("build operation id cannot be empty".into());
    }
    if id.len() > MAX_OPERATION_ID_LEN {
        return Err(format!(
            "build operation id exceeds {MAX_OPERATION_ID_LEN} characters"
        ));
    }
    if !id
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        return Err(format!(
            "build operation id '{id}' contains characters outside [A-Za-z0-9_-]"
        ));
    }
    Ok(())
}

enum ReadBuildOperationError {
    NotFound,
    Other(String),
}

fn read_build_operation(path: &Path) -> Result<BuildOperationRecord, ReadBuildOperationError> {
    let body = match std::fs::read(path) {
        Ok(body) => body,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Err(ReadBuildOperationError::NotFound);
        }
        Err(error) => {
            return Err(ReadBuildOperationError::Other(format!(
                "read build operation '{}': {error}",
                path.display()
            )));
        }
    };
    serde_json::from_slice(&body).map_err(|err| {
        ReadBuildOperationError::Other(format!(
            "decode build operation '{}': {err}",
            path.display()
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::{
        BuildOperationStore, MAX_OPERATION_ID_LEN, unique_operation_id, validate_operation_id,
    };
    use crate::daemon::DaemonState;
    use ployz_runtime_api::Identity;
    use ployz_types::model::{
        BuildLocation, BuildMethod, BuildOperationKind, MachineId, OperationStatus,
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

    fn make_state() -> DaemonState {
        let data_dir = unique_temp_dir("ployz-build-op-state");
        let identity = Identity::generate(MachineId("founder".into()), [32; 32]);
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
        assert!(
            validate_operation_id(&unique_operation_id(BuildOperationKind::Machine, 42)).is_ok()
        );
        assert!(validate_operation_id("build_local-123").is_ok());
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
        let store = BuildOperationStore::new(unique_temp_dir("ployz-build-ops-test"));
        let result = store.begin_with_id(
            "../../etc/passwd".into(),
            BuildOperationKind::Local,
            BuildMethod::Dockerfile,
            BuildLocation::Local,
            "building",
        );
        assert!(result.is_err());
        assert!(store.load("../../etc/passwd").is_err());
    }

    #[test]
    fn running_failure_remains_visible_until_success() {
        let store = BuildOperationStore::new(unique_temp_dir("ployz-build-ops-test"));
        let mut record = store
            .begin_with_id(
                "build-visible-failure".into(),
                BuildOperationKind::Local,
                BuildMethod::Dockerfile,
                BuildLocation::Local,
                "building",
            )
            .expect("begin operation");

        store
            .update_status(
                &mut record,
                OperationStatus::Failed,
                Some("build failed".into()),
            )
            .expect("mark failed");
        store
            .update_status(&mut record, OperationStatus::Running, None)
            .expect("mark running");
        store
            .update_stage(&mut record, "retrying")
            .expect("update stage");

        let running = store
            .load("build-visible-failure")
            .expect("load running operation")
            .expect("operation exists");
        assert_eq!(running.status, OperationStatus::Running);
        assert_eq!(running.last_error.as_deref(), Some("build failed"));

        store
            .update_status(&mut record, OperationStatus::Succeeded, None)
            .expect("mark succeeded");
        let succeeded = store
            .load("build-visible-failure")
            .expect("load succeeded operation")
            .expect("operation exists");
        assert_eq!(succeeded.status, OperationStatus::Succeeded);
        assert_eq!(succeeded.last_error, None);
    }

    #[test]
    fn list_orders_newest_first() {
        let store = BuildOperationStore::new(unique_temp_dir("ployz-build-ops-test"));
        let mut older = store
            .begin_with_id(
                "older".into(),
                BuildOperationKind::Local,
                BuildMethod::Dockerfile,
                BuildLocation::Local,
                "building",
            )
            .expect("begin older");
        older.started_at = 1;
        older.updated_at = 1;
        store.save(&older).expect("save older");

        let mut newer = store
            .begin_with_id(
                "newer".into(),
                BuildOperationKind::Machine,
                BuildMethod::Railpack,
                BuildLocation::Local,
                "building",
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
        let store = state.build_operation_store();
        let operation = store
            .begin(
                BuildOperationKind::Local,
                BuildMethod::Dockerfile,
                BuildLocation::Local,
                "building",
            )
            .expect("begin operation");

        state.recover_build_operations_on_startup().await;

        let recovered = state
            .build_operation_store()
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
