mod common;

use std::{
    fs,
    os::unix::fs::PermissionsExt,
    sync::{Arc, Mutex},
};

use ployz_core::{LocalMachinePhase, MachineRpc, RpcErrorCode, RpcRequest, RpcResponseBody};
use ployzd::{
    machine::{LocalMachineState, StateStore},
    rpc::MachineService,
};

use common::TestDir;

#[test]
fn machine_state_is_created_once_and_reopened_with_private_permissions() {
    let dir = TestDir::new("ployzd-state");
    let created = StateStore::open(&dir.0).unwrap();
    let machine_id = created.local_state().id.clone();

    assert_eq!(
        created.local_state().phase,
        LocalMachinePhase::Uninitialized
    );
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

    let reopened = StateStore::open(&dir.0).unwrap();
    assert_eq!(reopened.local_state().id, machine_id);
    assert_eq!(
        reopened.local_state().phase,
        LocalMachinePhase::Uninitialized
    );
}

#[test]
fn resetting_state_is_durable_and_completed_on_the_next_open() {
    let dir = TestDir::new("ployzd-state");
    let mut store = StateStore::open(&dir.0).unwrap();
    let old_machine_id = store.local_state().id.clone();

    assert!(store.complete_reset().is_err());
    assert!(dir.0.exists());
    store.begin_reset().unwrap();
    assert_eq!(store.local_state().phase, LocalMachinePhase::Resetting);
    assert!(store.begin_reset().is_err());
    drop(store);

    let persisted: LocalMachineState =
        serde_json::from_slice(&fs::read(dir.0.join("machine.json")).unwrap()).unwrap();
    assert_eq!(persisted.id, old_machine_id);
    assert_eq!(persisted.phase, LocalMachinePhase::Resetting);

    let recreated = StateStore::open(&dir.0).unwrap();
    assert_ne!(recreated.local_state().id, old_machine_id);
    assert_eq!(
        recreated.local_state().phase,
        LocalMachinePhase::Uninitialized
    );
}

#[tokio::test]
async fn repeated_reset_returns_a_typed_conflict() {
    let dir = TestDir::new("ployzd-state");
    let mut store = StateStore::open(&dir.0).unwrap();
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
