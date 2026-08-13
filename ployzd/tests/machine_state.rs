use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};

use ployz_core::LocalMachinePhase;
use ployzd::machine::{MachineState, StateStore};

struct TestDir(PathBuf);

impl TestDir {
    fn new() -> Self {
        Self(std::env::temp_dir().join(format!("ployzd-state-{}", ployz_core::MachineId::random())))
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

#[test]
fn machine_state_is_created_once_and_reopened_with_private_permissions() {
    let dir = TestDir::new();
    let created = StateStore::open(&dir.0).unwrap();
    let machine_id = created.state().id.clone();

    assert_eq!(created.state().phase, LocalMachinePhase::Uninitialized);
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
    assert_eq!(reopened.state().id, machine_id);
    assert_eq!(reopened.state().phase, LocalMachinePhase::Uninitialized);
}

#[test]
fn resetting_state_is_durable_and_completed_on_the_next_open() {
    let dir = TestDir::new();
    let mut store = StateStore::open(&dir.0).unwrap();
    let old_machine_id = store.state().id.clone();

    store.begin_reset().unwrap();
    assert_eq!(store.state().phase, LocalMachinePhase::Resetting);
    assert!(store.begin_reset().is_err());
    drop(store);

    let persisted: MachineState =
        serde_json::from_slice(&fs::read(dir.0.join("machine.json")).unwrap()).unwrap();
    assert_eq!(persisted.id, old_machine_id);
    assert_eq!(persisted.phase, LocalMachinePhase::Resetting);

    let recreated = StateStore::open(&dir.0).unwrap();
    assert_ne!(recreated.state().id, old_machine_id);
    assert_eq!(recreated.state().phase, LocalMachinePhase::Uninitialized);
}
