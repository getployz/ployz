use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use super::store::{MAX_OPERATION_ID_LEN, unique_operation_id, validate_operation_id};
use super::{
    MachineOperationArtifacts, MachineOperationKind, MachineOperationRecord,
    MachineOperationStatus, MachineOperationStore,
};

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
    assert!(validate_operation_id(&unique_operation_id(MachineOperationKind::Update, 42)).is_ok());
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
