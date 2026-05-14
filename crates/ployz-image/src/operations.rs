use std::path::PathBuf;

use ployz_model::{
    ImageDigest, ImageOperationKind, ImageOperationRecord, ImageOperationState,
    ImageOperationTargetOutcome, MachineId, OperationStatus,
};
use ployz_operation_store::FileOperationStore;
use ployz_time::now_unix_secs;

const OPERATIONS_DIR_NAME: &str = "image-operations";
pub use ployz_operation_store::MAX_OPERATION_ID_LEN;

#[derive(Debug, Clone)]
pub struct ImageOperationStore {
    files: FileOperationStore,
}

impl ImageOperationStore {
    #[must_use]
    pub fn new(root: PathBuf) -> Self {
        Self {
            files: FileOperationStore::new(root, OPERATIONS_DIR_NAME, "image operation"),
        }
    }

    #[allow(dead_code)]
    pub fn begin(
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
            stage: stage.into(),
            digest,
            source_machine,
            targets: target_machines
                .into_iter()
                .map(running_target_outcome)
                .collect(),
            started_at: now,
            updated_at: now,
            state: ImageOperationState::Running { last_error: None },
        };
        self.save(&record)?;
        Ok(record)
    }

    #[allow(dead_code)]
    pub fn begin_with_id(
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
            stage: stage.into(),
            digest,
            source_machine,
            targets: target_machines
                .into_iter()
                .map(running_target_outcome)
                .collect(),
            started_at: now,
            updated_at: now,
            state: ImageOperationState::Running { last_error: None },
        };
        self.save(&record)?;
        Ok(record)
    }

    #[allow(dead_code)]
    pub fn update_stage(
        &self,
        record: &mut ImageOperationRecord,
        stage: impl Into<String>,
    ) -> Result<(), String> {
        record.stage = stage.into();
        record.updated_at = now_unix_secs();
        self.save(record)
    }

    pub fn update_status(
        &self,
        record: &mut ImageOperationRecord,
        status: OperationStatus,
        last_error: Option<String>,
    ) -> Result<(), String> {
        apply_status_transition(record, status, last_error);
        self.save(record)
    }

    #[allow(dead_code)]
    pub fn update_target(
        &self,
        record: &mut ImageOperationRecord,
        outcome: ImageOperationTargetOutcome,
    ) -> Result<(), String> {
        let machine_id = outcome.machine_id().clone();
        match record
            .targets
            .iter_mut()
            .find(|target| target.machine_id() == &machine_id)
        {
            Some(existing) => *existing = outcome,
            None => record.targets.push(outcome),
        }
        record.updated_at = now_unix_secs();
        self.save(record)
    }

    pub fn save(&self, record: &ImageOperationRecord) -> Result<(), String> {
        self.files.save(&record.id, record)
    }

    pub fn load(&self, id: &str) -> Result<Option<ImageOperationRecord>, String> {
        self.files.load(id)
    }

    pub fn list(&self) -> Result<Vec<ImageOperationRecord>, String> {
        self.files.list(
            |record: &ImageOperationRecord| record.started_at,
            |record| &record.id,
        )
    }
}

#[allow(dead_code)]
fn running_target_outcome(machine_id: MachineId) -> ImageOperationTargetOutcome {
    ImageOperationTargetOutcome::running(machine_id)
}

fn apply_status_transition(
    record: &mut ImageOperationRecord,
    status: OperationStatus,
    last_error: Option<String>,
) {
    record.state = match status {
        OperationStatus::Running => ImageOperationState::Running {
            last_error: last_error.or_else(|| record.last_error().map(str::to_string)),
        },
        OperationStatus::Succeeded => ImageOperationState::Succeeded {},
        OperationStatus::Failed => ImageOperationState::Failed {
            last_error: last_error.unwrap_or_else(|| "image operation failed".into()),
        },
        OperationStatus::Interrupted => ImageOperationState::Interrupted { last_error },
    };
    record.updated_at = now_unix_secs();
}

#[allow(dead_code)]
fn unique_operation_id(kind: ImageOperationKind, now: u64) -> String {
    ployz_operation_store::unique_operation_id("image", kind, now)
}

pub fn validate_operation_id(id: &str) -> Result<(), String> {
    ployz_operation_store::validate_operation_id("image operation", id)
}

#[cfg(test)]
mod tests {
    use super::{
        ImageOperationStore, MAX_OPERATION_ID_LEN, unique_operation_id, validate_operation_id,
    };
    use ployz_model::{
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
                ImageOperationTargetOutcome::failed(MachineId::new("target-a"), "disk full"),
            )
            .expect("update target");

        let loaded = store
            .load("image-visible-target")
            .expect("load operation")
            .expect("operation exists");
        assert_eq!(loaded.targets.len(), 1);
        assert_eq!(loaded.targets[0].status(), OperationStatus::Failed);
        assert_eq!(loaded.targets[0].bytes_transferred(), None);
        assert_eq!(loaded.targets[0].last_error(), Some("disk full"));
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
        assert_eq!(running.status(), OperationStatus::Running);
        assert_eq!(running.last_error(), Some("copy failed"));

        store
            .update_status(&mut record, OperationStatus::Succeeded, None)
            .expect("mark succeeded");
        let succeeded = store
            .load("image-visible-failure")
            .expect("load succeeded operation")
            .expect("operation exists");
        assert_eq!(succeeded.status(), OperationStatus::Succeeded);
        assert_eq!(succeeded.last_error(), None);
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
}
