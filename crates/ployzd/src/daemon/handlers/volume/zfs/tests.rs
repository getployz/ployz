use std::path::PathBuf;
use std::time::Duration;

use ployz_api::VolumeZfsTransferState;
use ployz_model::MachineId;
use ployz_runtime_api::Identity;
use ployz_spec::Namespace;
use ployz_time::now_unix_secs;
use ployz_volume_zfs::{
    ClaimedTransfer, MoveClaimOutcome, TransferRecord, TransferStatus, TransferStore,
    move_claim_key, unique_transfer_id, wait_for_claimed_transfer_record,
};

use crate::daemon::DaemonState;

use super::responses::transfer_info;
fn tmp_root(label: &str) -> PathBuf {
    let id = unique_transfer_id(0).expect("unique id");
    std::env::temp_dir().join(format!("ployz-zfs-transfer-test-{label}-{id}"))
}

fn begin(store: &TransferStore) -> TransferRecord {
    store
        .begin(
            &Namespace::new("default"),
            "data",
            MachineId::new("source"),
            MachineId::new("target"),
            "snap".into(),
            None,
        )
        .expect("begin transfer")
}

fn create_claim(store: &TransferStore, transfer_id: &str) -> MoveClaimOutcome {
    store
        .create_move_claim(
            &Namespace::new("default"),
            "data",
            &MachineId::new("source"),
            &MachineId::new("target"),
            "snap",
            None,
            transfer_id,
        )
        .expect("create claim")
}

fn claim_path(store: &TransferStore) -> PathBuf {
    let key = move_claim_key(
        &Namespace::new("default"),
        "data",
        &MachineId::new("source"),
        &MachineId::new("target"),
        "snap",
        None,
    );
    store.claim_path(&key)
}

fn make_state(label: &str) -> DaemonState {
    let data_dir = tmp_root(label);
    let identity = Identity::generate(MachineId::new("founder"), [42; 32]);
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

#[tokio::test]
async fn peer_snapshot_rejects_invalid_namespace() {
    let state = make_state("peer-snapshot-invalid-namespace");

    let response = state
        .handle_volume_zfs_peer_snapshot("Prod", "data", "snap")
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "VOLUME_ZFS_PEER_SNAPSHOT_FAILED");
    assert!(
        response.message().contains("namespace"),
        "got: {}",
        response.message()
    );
}

#[tokio::test]
async fn peer_snapshot_guid_rejects_invalid_namespace() {
    let state = make_state("peer-snapshot-guid-invalid-namespace");

    let response = state
        .handle_volume_zfs_peer_snapshot_guid("bad ns", "data", "snap")
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "VOLUME_ZFS_PEER_SNAPSHOT_GUID_FAILED");
    assert!(
        response.message().contains("namespace"),
        "got: {}",
        response.message()
    );
}

#[tokio::test]
async fn peer_start_send_rejects_invalid_namespace() {
    let state = make_state("peer-start-send-invalid-namespace");

    let response = state
        .handle_volume_zfs_peer_start_send("bad ns", "data", "snap", "target", 1, None, None)
        .await;

    assert!(!response.is_ok());
    assert_eq!(response.code(), "VOLUME_ZFS_PEER_START_SEND_FAILED");
    assert!(
        response.message().contains("namespace"),
        "got: {}",
        response.message()
    );
}

#[test]
fn startup_recovery_marks_running_transfers_interrupted() {
    let root = tmp_root("startup-recovery");
    let store = TransferStore::new(root.clone());
    let transfer = begin(&store);
    assert!(matches!(
        create_claim(&store, &transfer.id),
        MoveClaimOutcome::Created
    ));
    assert_eq!(transfer.status(), TransferStatus::Running);

    let count = store.recover_startup().expect("recover startup");
    assert_eq!(count, 1);
    let loaded = store
        .load(&transfer.id)
        .expect("load")
        .expect("record exists");
    assert_eq!(loaded.status(), TransferStatus::Interrupted);
    assert!(
        !claim_path(&store).exists(),
        "startup recovery should delete the interrupted transfer claim"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn move_claim_created_then_existing_returns_same_transfer_id() {
    let root = tmp_root("claim-idempotent");
    let store = TransferStore::new(root.clone());

    assert!(matches!(
        create_claim(&store, "transfer-a"),
        MoveClaimOutcome::Created
    ));
    match create_claim(&store, "transfer-b") {
        MoveClaimOutcome::Exists(existing_id) => assert_eq!(existing_id, "transfer-a"),
        MoveClaimOutcome::Created => panic!("claim should already exist"),
    }

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deleting_move_claim_allows_reclaim_after_stale_claim() {
    let root = tmp_root("claim-delete");
    let store = TransferStore::new(root.clone());

    assert!(matches!(
        create_claim(&store, "missing-transfer"),
        MoveClaimOutcome::Created
    ));
    store.delete_move_claim(
        &Namespace::new("default"),
        "data",
        &MachineId::new("source"),
        &MachineId::new("target"),
        "snap",
        None,
    );
    assert!(matches!(
        create_claim(&store, "transfer-b"),
        MoveClaimOutcome::Created
    ));

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn deleting_move_claim_allows_reclaim_after_invalid_claim() {
    let root = tmp_root("claim-invalid");
    let store = TransferStore::new(root.clone());
    std::fs::create_dir_all(store.claim_dir()).expect("claim dir");
    std::fs::write(claim_path(&store), "\n").expect("invalid claim");

    match create_claim(&store, "transfer-a") {
        MoveClaimOutcome::Exists(existing_id) => assert!(existing_id.is_empty()),
        MoveClaimOutcome::Created => panic!("claim should already exist"),
    }
    store.delete_move_claim(
        &Namespace::new("default"),
        "data",
        &MachineId::new("source"),
        &MachineId::new("target"),
        "snap",
        None,
    );
    assert!(matches!(
        create_claim(&store, "transfer-b"),
        MoveClaimOutcome::Created
    ));

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn claimed_transfer_waits_for_record_creation() {
    let root = tmp_root("claim-waits-for-record");
    let store = TransferStore::new(root.clone());

    assert!(matches!(
        create_claim(&store, "transfer-a"),
        MoveClaimOutcome::Created
    ));

    let delayed_store = store.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(5)).await;
        delayed_store
            .begin_with_id(
                "transfer-a".into(),
                &Namespace::new("default"),
                "data",
                MachineId::new("source"),
                MachineId::new("target"),
                "snap".into(),
                None,
                now_unix_secs(),
            )
            .expect("begin delayed transfer");
    });

    match wait_for_claimed_transfer_record(&store, "transfer-a")
        .await
        .expect("wait for claimed transfer")
    {
        ClaimedTransfer::Running(record) => assert_eq!(record.id, "transfer-a"),
        ClaimedTransfer::Terminal(record) => {
            panic!("expected running transfer, got {:?}", record.status())
        }
        ClaimedTransfer::MissingAfterWait => {
            panic!("claim should wait for newly created transfer record")
        }
    }

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn load_rejects_path_segment_id() {
    let store = TransferStore::new(tmp_root("reject-id"));
    for bad in ["../etc/passwd", "foo/bar", "with space", "", "."] {
        let err = store
            .load(bad)
            .expect_err(&format!("id '{bad}' should be rejected"));
        assert!(err.contains("transfer id"), "got: {err}");
    }
}

#[test]
fn list_orders_by_started_at_desc_then_id_asc() {
    let root = tmp_root("list-order");
    let store = TransferStore::new(root.clone());
    let mut a = begin(&store);
    let mut b = begin(&store);
    let mut c = begin(&store);
    // begin uses nanos in the id, so ids grow with call order
    assert!(a.id < b.id && b.id < c.id);

    a.started_at = 100;
    b.started_at = 200;
    c.started_at = 200;
    store.save(&a).expect("save a");
    store.save(&b).expect("save b");
    store.save(&c).expect("save c");

    let listed = store.list().expect("list");
    let ids: Vec<&str> = listed.iter().map(|r| r.id.as_str()).collect();
    assert_eq!(ids, vec![b.id.as_str(), c.id.as_str(), a.id.as_str()]);
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn reusable_transfer_returns_running_matching_move() {
    let root = tmp_root("reusable");
    let store = TransferStore::new(root.clone());
    let transfer = begin(&store);

    let found = store
        .find_reusable(
            &Namespace::new("default"),
            "data",
            &MachineId::new("source"),
            &MachineId::new("target"),
            "snap",
            None,
        )
        .expect("find reusable")
        .expect("running transfer reusable");

    assert_eq!(found.id, transfer.id);

    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn reusable_transfer_ignores_terminal_matching_move() {
    let root = tmp_root("terminal-not-reusable");
    let store = TransferStore::new(root.clone());
    let mut transfer = begin(&store);
    for status in [TransferStatus::Succeeded, TransferStatus::Failed] {
        store
            .update_status(&mut transfer, status, Some("boom".into()))
            .expect("terminal transfer");

        let found = store
            .find_reusable(
                &Namespace::new("default"),
                "data",
                &MachineId::new("source"),
                &MachineId::new("target"),
                "snap",
                None,
            )
            .expect("find reusable");

        assert!(found.is_none());
    }
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn finalize_records_complete_stage_and_succeeded_status() {
    let root = tmp_root("finalize-ok");
    let store = TransferStore::new(root.clone());
    let mut transfer = begin(&store);
    store
        .update_stage(&mut transfer, "snapshot")
        .expect("stage snapshot");
    store
        .update_stage(&mut transfer, "send")
        .expect("stage send");

    store.finalize_result(&mut transfer, Ok(()));

    let loaded = store
        .load(&transfer.id)
        .expect("load")
        .expect("record exists");
    assert_eq!(loaded.status(), TransferStatus::Succeeded);
    assert_eq!(loaded.stage(), "complete");
    assert!(loaded.last_error().is_none());
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn finalize_captures_last_error_on_failure() {
    let root = tmp_root("finalize-err");
    let store = TransferStore::new(root.clone());
    let mut transfer = begin(&store);
    assert!(matches!(
        create_claim(&store, &transfer.id),
        MoveClaimOutcome::Created
    ));

    store.finalize_result(&mut transfer, Err("boom".into()));

    let loaded = store
        .load(&transfer.id)
        .expect("load")
        .expect("record exists");
    assert_eq!(loaded.status(), TransferStatus::Failed);
    assert_eq!(loaded.last_error(), Some("boom"));
    assert!(
        !claim_path(&store).exists(),
        "failed transfer should delete its move claim"
    );
    let _ = std::fs::remove_dir_all(root);
}

#[test]
fn interrupted_transfer_preserves_prior_failure_error() {
    let root = tmp_root("interrupt-after-failure");
    let store = TransferStore::new(root.clone());
    let mut transfer = begin(&store);
    store
        .update_status(
            &mut transfer,
            TransferStatus::Failed,
            Some("send failed".into()),
        )
        .expect("record transfer failure");

    store
        .update_status(&mut transfer, TransferStatus::Interrupted, None)
        .expect("record interruption");

    let loaded = store
        .load(&transfer.id)
        .expect("load")
        .expect("record exists");
    assert_eq!(loaded.status(), TransferStatus::Interrupted);
    assert_eq!(loaded.last_error(), Some("send failed"));
    assert!(
        matches!(
            transfer_info(&loaded).state,
            VolumeZfsTransferState::Interrupted {
                last_error: Some(ref error),
                ..
            } if error == "send failed"
        ),
        "operator-facing payload should preserve the failure audience"
    );
    let _ = std::fs::remove_dir_all(root);
}
