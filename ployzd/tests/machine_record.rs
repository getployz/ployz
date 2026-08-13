mod test_dir;

use std::{
    collections::BTreeMap,
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};

use ployz_core::{LocalMachinePhase, MachineRpc, RpcErrorCode, RpcRequest, RpcResponseBody};
use ployzd::{
    machine::{LocalMachineRecord, LocalMachineStore, StoreError},
    rpc::MachineService,
};

use test_dir::TestDir;

#[test]
fn machine_record_is_created_once_and_reopened_with_private_permissions() {
    let dir = TestDir::new("ployzd-state");
    let created = LocalMachineStore::open(&dir.0).unwrap();
    let machine_id = created.record().id.clone();

    assert_eq!(created.record().phase, LocalMachinePhase::Uninitialized);
    assert!(created.record().min_store_version.is_empty());
    assert_eq!(
        fs::metadata(&dir.0).unwrap().permissions().mode() & 0o777,
        0o711
    );
    assert_eq!(
        fs::metadata(dir.0.join("machine.json"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    drop(created);

    let reopened = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(reopened.record().id, machine_id);
    assert_eq!(reopened.record().phase, LocalMachinePhase::Uninitialized);
}

#[test]
fn resetting_state_is_durable_and_completed_on_the_next_open() {
    let dir = TestDir::new("ployzd-state");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    let old_machine_id = store.record().id.clone();

    assert!(store.complete_reset().is_err());
    assert!(dir.0.exists());
    store.begin_reset().unwrap();
    assert_eq!(store.record().phase, LocalMachinePhase::Resetting);
    assert!(store.begin_reset().is_err());
    drop(store);

    let persisted: LocalMachineRecord =
        serde_json::from_slice(&fs::read(dir.0.join("machine.json")).unwrap()).unwrap();
    assert_eq!(persisted.id, old_machine_id);
    assert_eq!(persisted.phase, LocalMachinePhase::Resetting);

    let recreated = LocalMachineStore::open(&dir.0).unwrap();
    assert_ne!(recreated.record().id, old_machine_id);
    assert_eq!(recreated.record().phase, LocalMachinePhase::Uninitialized);
}

#[tokio::test]
async fn repeated_reset_returns_a_typed_conflict() {
    let dir = TestDir::new("ployzd-state");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    store.begin_reset().unwrap();
    let (reset, _) = tokio::sync::watch::channel(false);
    let service = MachineService::new(Arc::new(Mutex::new(store)), reset);

    let response = service
        .reset(tonic::Request::new(RpcRequest::reset().encode().unwrap()))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap();

    assert!(matches!(
        response.body,
        RpcResponseBody::Error(error) if error.code == RpcErrorCode::Conflict
    ));
}

#[test]
fn nonempty_directory_without_a_machine_record_is_not_claimed() {
    let dir = TestDir::new("ployzd-state");
    fs::create_dir_all(&dir.0).unwrap();
    let existing = dir.0.join("keep-me");
    fs::write(&existing, b"unrelated").unwrap();

    assert!(matches!(
        LocalMachineStore::open(&dir.0),
        Err(StoreError::UnownedDataDirectory(_))
    ));
    assert_eq!(fs::read(existing).unwrap(), b"unrelated");
}

#[test]
fn reset_stops_if_the_machine_record_changes() {
    let dir = TestDir::new("ployzd-state");
    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    store.begin_reset().unwrap();
    let replacement = LocalMachineRecord {
        id: ployz_core::MachineId::random(),
        phase: LocalMachinePhase::Resetting,
        machine: None,
        min_store_version: BTreeMap::new(),
    };
    fs::write(
        dir.0.join("machine.json"),
        serde_json::to_vec(&replacement).unwrap(),
    )
    .unwrap();

    assert!(store.complete_reset().is_err());
    assert!(dir.0.exists());
}

#[test]
fn machine_store_is_process_exclusive() {
    let dir = TestDir::new("ployzd-state");
    let store = LocalMachineStore::open(&dir.0).unwrap();

    assert!(matches!(
        LocalMachineStore::open(&dir.0),
        Err(StoreError::AlreadyRunning(_))
    ));
    drop(store);
    LocalMachineStore::open(&dir.0).unwrap();
}

#[test]
fn interrupted_initial_write_is_recovered() {
    let dir = TestDir::new("ployzd-state");
    fs::create_dir_all(&dir.0).unwrap();
    fs::write(dir.0.join(".machine.json.tmp"), b"partial").unwrap();

    let store = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(store.record().phase, LocalMachinePhase::Uninitialized);
    assert!(!dir.0.join(".machine.json.tmp").exists());
}

#[test]
fn completing_catch_up_persists_participation_and_clears_the_target() {
    let dir = TestDir::new("ployzd-state");
    fs::create_dir_all(&dir.0).unwrap();
    let record = LocalMachineRecord {
        id: ployz_core::MachineId::random(),
        phase: LocalMachinePhase::Joining,
        machine: None,
        min_store_version: BTreeMap::from([("actor".to_owned(), 4)]),
    };
    fs::write(
        dir.0.join("machine.json"),
        serde_json::to_vec(&record).unwrap(),
    )
    .unwrap();

    let mut store = LocalMachineStore::open(&dir.0).unwrap();
    store.complete_catch_up().unwrap();
    drop(store);

    let reopened = LocalMachineStore::open(&dir.0).unwrap();
    assert_eq!(reopened.record().phase, LocalMachinePhase::Participating);
    assert!(reopened.record().min_store_version.is_empty());
}
