use super::*;

fn persist_owned(directory: &Path) {
    persist(
        directory,
        PreparedStorageOrigin::OwnedImage {
            backing_file: PathBuf::from(PLOYZ_OWNED_ZFS_BACKING_FILE),
        },
    );
}

fn verified_outputs() -> [HostRunnerCommandOutput; 5] {
    [
        stdout("ONLINE\n"),
        success(),
        stdout(VOLUME_MOUNTPOINT),
        pool_status(PLOYZ_OWNED_ZFS_POOL, PLOYZ_OWNED_ZFS_BACKING_FILE),
        stdout("yes\n"),
    ]
}

#[test]
fn recovery_requires_a_valid_owned_descriptor_before_any_effect() {
    let missing = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([]);
    assert!(matches!(
        recover_owned_storage(&mut runner, missing.path()),
        Err(ZfsEffectError::PreparedStateUnavailable { .. })
    ));
    assert!(runner.invocations.is_empty());

    let invalid = tempfile::tempdir().unwrap();
    std::fs::write(invalid.path().join("prepared-storage.json"), b"{}").unwrap();
    let mut runner = RecordingRunner::new([]);
    assert!(matches!(
        recover_owned_storage(&mut runner, invalid.path()),
        Err(ZfsEffectError::PreparedStateUnavailable { .. })
    ));
    assert!(runner.invocations.is_empty());

    let adopted = tempfile::tempdir().unwrap();
    persist(adopted.path(), PreparedStorageOrigin::Adopted);
    let mut runner = RecordingRunner::new([]);
    assert!(matches!(
        recover_owned_storage(&mut runner, adopted.path()),
        Err(ZfsEffectError::PreparedStateMismatch { .. })
    ));
    assert!(runner.invocations.is_empty());
}

#[test]
fn recovery_reuses_an_imported_online_mounted_pool_after_full_verification() {
    let state = tempfile::tempdir().unwrap();
    persist_owned(state.path());
    let mut outputs = vec![stdout("ployz\n")];
    outputs.extend(verified_outputs());
    let mut runner = RecordingRunner::new(outputs);

    recover_owned_storage(&mut runner, state.path()).unwrap();

    assert_eq!(runner.invocations.len(), 6);
    assert!(
        runner
            .invocations
            .iter()
            .all(|call| { !matches!(call.args.as_slice(), [command, ..] if command == "import") })
    );
}

#[test]
fn recovery_imports_only_the_descriptor_pool_then_fully_verifies_it() {
    let state = tempfile::tempdir().unwrap();
    persist_owned(state.path());
    let mut outputs = vec![stdout(""), success()];
    outputs.extend(verified_outputs());
    let mut runner = RecordingRunner::new(outputs);

    recover_owned_storage(&mut runner, state.path()).unwrap();

    assert_eq!(invocation(&runner, 0).args, ["list", "-H", "-o", "name"]);
    assert_eq!(
        invocation(&runner, 1).args,
        [
            "import",
            "-d",
            PLOYZ_OWNED_ZFS_BACKING_FILE,
            PLOYZ_OWNED_ZFS_POOL
        ]
    );
    assert_eq!(invocation(&runner, 1).timeout, COMMAND_TIMEOUT);
    assert_eq!(
        invocation(&runner, 2).args,
        ["list", "-H", "-o", "health", PLOYZ_OWNED_ZFS_POOL]
    );
    assert_eq!(
        invocation(&runner, 6).args,
        ["get", "-H", "-o", "value", "mounted", "ployz/ployz/volumes"]
    );
}

#[test]
fn recovery_reports_import_and_post_import_identity_failures() {
    let state = tempfile::tempdir().unwrap();
    persist_owned(state.path());
    let mut import_failure = RecordingRunner::new([stdout(""), failed("import failed")]);
    assert!(matches!(
        recover_owned_storage(&mut import_failure, state.path()),
        Err(ZfsEffectError::OwnedPool { .. })
    ));

    let mut mismatch = RecordingRunner::new([
        stdout(""),
        success(),
        stdout("ONLINE\n"),
        success(),
        stdout("/wrong/mountpoint"),
    ]);
    assert!(matches!(
        recover_owned_storage(&mut mismatch, state.path()),
        Err(ZfsEffectError::PreparedStateMismatch { .. })
    ));
}

#[test]
fn recovery_requires_online_health_and_a_mounted_dataset_root() {
    let state = tempfile::tempdir().unwrap();
    persist_owned(state.path());

    let mut unhealthy = RecordingRunner::new([stdout("ployz\n"), stdout("DEGRADED\n")]);
    assert!(matches!(
        recover_owned_storage(&mut unhealthy, state.path()),
        Err(ZfsEffectError::PreparedStateMismatch { .. })
    ));

    let mut outputs = vec![stdout("ployz\n")];
    let mut verification = verified_outputs();
    verification[4] = stdout("no\n");
    outputs.extend(verification);
    let mut unmounted = RecordingRunner::new(outputs);
    assert!(matches!(
        recover_owned_storage(&mut unmounted, state.path()),
        Err(ZfsEffectError::PreparedStateMismatch { .. })
    ));
}

#[test]
fn owned_recovery_service_refuses_openrc_before_writing_or_enabling() {
    let root = tempfile::tempdir().unwrap();
    let mut runner = RecordingRunner::new([]);
    let origin = PreparedStorageOrigin::OwnedImage {
        backing_file: PathBuf::from(PLOYZ_OWNED_ZFS_BACKING_FILE),
    };

    assert!(matches!(
        super::super::preparation::install_docker_zfs_ordering(
            &mut runner,
            crate::execution::SupervisorBackend::OpenRc,
            &root.path().join("docker.service.d"),
            root.path(),
            &origin,
        ),
        Err(ZfsEffectError::Dataset { .. })
    ));
    assert!(runner.invocations.is_empty());
    assert!(!root.path().join("docker.service.d").exists());
    assert!(!root.path().join("ployz-owned-zfs-import.service").exists());
}
