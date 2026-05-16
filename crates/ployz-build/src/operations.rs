use std::path::PathBuf;

use ployz_model::{
    BuildInputSummary, BuildLocation, BuildMethod, BuildOperationKind, BuildOperationRecord,
    BuildOperationState, ImageArtifact, OperationStatus,
};
use ployz_operation_store::FileOperationStore;
use ployz_time::now_unix_secs;

const OPERATIONS_DIR_NAME: &str = "build-operations";
const DAEMON_RESTART_INTERRUPT_ERROR: &str =
    "daemon restarted before build completed; retry the build or inspect any recorded artifact";
pub use ployz_operation_store::MAX_OPERATION_ID_LEN;

#[derive(Debug, Clone)]
pub struct BuildOperationStore {
    files: FileOperationStore,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationStartupRecovery {
    pub interrupted: Vec<String>,
    pub failures: Vec<OperationStartupRecoveryFailure>,
}

impl OperationStartupRecovery {
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.failures.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OperationStartupRecoveryFailure {
    pub operation_id: String,
    pub error: String,
}

impl BuildOperationStore {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            files: FileOperationStore::new(root, OPERATIONS_DIR_NAME, "build operation"),
        }
    }

    #[allow(dead_code)]
    pub fn begin(
        &self,
        kind: BuildOperationKind,
        method: BuildMethod,
        location: BuildLocation,
        stage: impl Into<String>,
    ) -> Result<BuildOperationRecord, String> {
        self.begin_with_input_summary(kind, method, location, stage, BuildInputSummary::default())
    }

    #[allow(dead_code)]
    pub fn begin_with_input_summary(
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
            stage: stage.into(),
            inputs,
            state: BuildOperationState::Running { last_error: None },
            started_at: now,
            updated_at: now,
        };
        self.save(&record)?;
        Ok(record)
    }

    #[allow(dead_code)]
    pub fn begin_with_id(
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
            stage: stage.into(),
            inputs: BuildInputSummary::default(),
            state: BuildOperationState::Running { last_error: None },
            started_at: now,
            updated_at: now,
        };
        self.save(&record)?;
        Ok(record)
    }

    #[allow(dead_code)]
    pub fn update_stage(
        &self,
        record: &mut BuildOperationRecord,
        stage: impl Into<String>,
    ) -> Result<(), String> {
        record.stage = stage.into();
        record.updated_at = now_unix_secs();
        self.save(record)
    }

    #[allow(dead_code)]
    pub fn update_artifact(
        &self,
        record: &mut BuildOperationRecord,
        artifact: ImageArtifact,
    ) -> Result<(), String> {
        record.state = BuildOperationState::Succeeded { artifact };
        record.updated_at = now_unix_secs();
        self.save(record)
    }

    pub fn update_status(
        &self,
        record: &mut BuildOperationRecord,
        status: OperationStatus,
        last_error: Option<String>,
    ) -> Result<(), String> {
        apply_status_transition(record, status, last_error);
        self.save(record)
    }

    pub fn save(&self, record: &BuildOperationRecord) -> Result<(), String> {
        self.files.save(&record.id, record)
    }

    pub fn load(&self, id: &str) -> Result<Option<BuildOperationRecord>, String> {
        self.files.load(id)
    }

    pub fn list(&self) -> Result<Vec<BuildOperationRecord>, String> {
        self.files.list(
            |record: &BuildOperationRecord| record.started_at,
            |record| &record.id,
        )
    }

    pub fn interrupt_running_after_daemon_restart(
        &self,
    ) -> Result<OperationStartupRecovery, String> {
        let mut recovery = OperationStartupRecovery {
            interrupted: Vec::new(),
            failures: Vec::new(),
        };

        for mut record in self.list()? {
            if record.status() != OperationStatus::Running {
                continue;
            }
            match self.update_status(
                &mut record,
                OperationStatus::Interrupted,
                Some(DAEMON_RESTART_INTERRUPT_ERROR.into()),
            ) {
                Ok(()) => recovery.interrupted.push(record.id),
                Err(error) => recovery.failures.push(OperationStartupRecoveryFailure {
                    operation_id: record.id,
                    error,
                }),
            }
        }

        Ok(recovery)
    }
}

fn apply_status_transition(
    record: &mut BuildOperationRecord,
    status: OperationStatus,
    last_error: Option<String>,
) {
    record.state = match status {
        OperationStatus::Running => BuildOperationState::Running {
            last_error: last_error.or_else(|| record.last_error().map(str::to_string)),
        },
        OperationStatus::Succeeded => {
            let Some(artifact) = record.artifact().cloned() else {
                record.updated_at = now_unix_secs();
                return;
            };
            BuildOperationState::Succeeded { artifact }
        }
        OperationStatus::Failed => BuildOperationState::Failed {
            last_error: last_error.unwrap_or_else(|| "build operation failed".into()),
        },
        OperationStatus::Interrupted => BuildOperationState::Interrupted { last_error },
    };
    record.updated_at = now_unix_secs();
}

#[allow(dead_code)]
pub fn unique_operation_id(kind: BuildOperationKind, now: u64) -> String {
    ployz_operation_store::unique_operation_id("build", kind, now)
}

pub fn validate_operation_id(id: &str) -> Result<(), String> {
    ployz_operation_store::validate_operation_id("build operation", id)
}

#[cfg(test)]
mod tests {
    use super::{
        BuildOperationStore, MAX_OPERATION_ID_LEN, unique_operation_id, validate_operation_id,
    };
    use ployz_model::{
        BuildLocation, BuildMethod, BuildOperationKind, ImageArtifact, ImageArtifactProvenance,
        ImageDigest, ImageRef, OperationStatus,
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

    fn artifact() -> ImageArtifact {
        ImageArtifact {
            image: ImageRef::digest_only(
                ImageDigest::try_new(format!("sha256:{}", "a".repeat(64))).expect("digest"),
            ),
            platform: None,
            provenance: ImageArtifactProvenance::External { source: None },
            created_at: 1,
        }
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
        assert_eq!(running.status(), OperationStatus::Running);
        assert_eq!(running.last_error(), Some("build failed"));

        store
            .update_artifact(&mut record, artifact())
            .expect("record artifact");
        store
            .update_status(&mut record, OperationStatus::Succeeded, None)
            .expect("mark succeeded");
        let succeeded = store
            .load("build-visible-failure")
            .expect("load succeeded operation")
            .expect("operation exists");
        assert_eq!(succeeded.status(), OperationStatus::Succeeded);
        assert_eq!(succeeded.last_error(), None);
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

    #[test]
    fn startup_recovery_marks_running_operation_interrupted() {
        let store = BuildOperationStore::new(unique_temp_dir("ployz-build-ops-test"));
        let operation = store
            .begin(
                BuildOperationKind::Local,
                BuildMethod::Dockerfile,
                BuildLocation::Local,
                "building",
            )
            .expect("begin operation");

        let recovery = store
            .interrupt_running_after_daemon_restart()
            .expect("recover startup");

        assert_eq!(recovery.interrupted, vec![operation.id.clone()]);
        assert!(recovery.is_clean());
        let recovered = store
            .load(&operation.id)
            .expect("load operation")
            .expect("operation exists");
        assert_eq!(recovered.status(), OperationStatus::Interrupted);
        assert!(
            recovered
                .last_error()
                .expect("last error")
                .contains("daemon restarted")
        );
    }

    #[test]
    fn startup_recovery_leaves_terminal_operations_untouched() {
        let store = BuildOperationStore::new(unique_temp_dir("ployz-build-ops-test"));
        let mut operation = store
            .begin_with_id(
                "finished-build-operation".into(),
                BuildOperationKind::Local,
                BuildMethod::Dockerfile,
                BuildLocation::Local,
                "building",
            )
            .expect("begin operation");
        store
            .update_artifact(&mut operation, artifact())
            .expect("record artifact");

        let recovery = store
            .interrupt_running_after_daemon_restart()
            .expect("recover startup");

        assert!(recovery.interrupted.is_empty());
        assert!(recovery.is_clean());
        let recovered = store
            .load("finished-build-operation")
            .expect("load operation")
            .expect("operation exists");
        assert_eq!(recovered.status(), OperationStatus::Succeeded);
        assert_eq!(recovered.last_error(), None);
    }
}
