use std::fs;
use std::path::PathBuf;

use crate::execution::{
    ArtifactInstallDurability, ArtifactInstallError, ArtifactKind, ArtifactSource,
    ArtifactStoreError, ArtifactTarget, ArtifactVerificationError, ArtifactVersion,
    PloyzdArtifactStore, Sha256Digest, VerifiedArtifactFile,
    acquire_remote_artifact_content_addressed, install_verified_artifact,
    stage_verified_artifact_content_addressed, verify_artifact_file,
};

#[test]
fn artifact_verification_accepts_matching_sha256() {
    let artifact = temp_artifact("ployz-artifact-ok");
    fs::write(&artifact, "ployz\n").expect("artifact can be written");

    let verified = verify_artifact_file(&artifact, &digest(PLOYZ_NEWLINE_SHA256))
        .expect("matching artifact verifies");

    assert_eq!(verified.path, artifact);
    assert_eq!(verified.digest, digest(PLOYZ_NEWLINE_SHA256));
}

#[test]
fn artifact_verification_rejects_digest_mismatch() {
    let artifact = temp_artifact("ployz-artifact-mismatch");
    fs::write(&artifact, "ployz\n").expect("artifact can be written");

    assert_eq!(
        verify_artifact_file(&artifact, &digest(ALL_A_SHA256)),
        Err(ArtifactVerificationError::DigestMismatch {
            path: artifact,
            expected: digest(ALL_A_SHA256),
            actual: digest(PLOYZ_NEWLINE_SHA256),
        })
    );
}

#[test]
fn artifact_verification_reports_read_failure_with_path() {
    let missing = temp_artifact("ployz-artifact-missing");

    assert!(matches!(
        verify_artifact_file(&missing, &digest(PLOYZ_NEWLINE_SHA256)),
        Err(ArtifactVerificationError::ReadFailed { path, message })
            if path == missing && !message.is_empty()
    ));
}

#[test]
fn verified_artifact_installs_to_target_path() {
    let staged = temp_artifact("ployz-artifact-install-source");
    let install_path = temp_artifact("ployz-artifact-install-target").join("bin/ployzd");
    fs::write(&staged, "ployz\n").expect("artifact can be written");
    let target = ployzd_target(&install_path, PLOYZ_NEWLINE_SHA256);
    let verified = verify_artifact_file(&staged, &target.digest).expect("staged artifact verifies");

    let installed =
        install_verified_artifact(&verified, &target).expect("verified artifact installs");

    assert_eq!(installed.source_path, staged);
    assert_eq!(installed.install_path, install_path);
    assert_eq!(installed.digest, digest(PLOYZ_NEWLINE_SHA256));
    assert_eq!(installed.durability, ArtifactInstallDurability::Confirmed);
    assert_eq!(
        fs::read_to_string(&installed.install_path).expect("installed artifact is readable"),
        "ployz\n"
    );
    assert!(
        installed
            .install_path
            .parent()
            .expect("install path has parent")
            .is_dir()
    );
}

#[cfg(unix)]
#[test]
fn installed_artifact_is_executable_on_unix() {
    use std::os::unix::fs::PermissionsExt;

    let staged = temp_artifact("ployz-artifact-executable-source");
    let install_path = temp_artifact("ployz-artifact-executable-target").join("bin/ployzd");
    fs::write(&staged, "ployz\n").expect("artifact can be written");
    let target = ployzd_target(&install_path, PLOYZ_NEWLINE_SHA256);
    let verified = verify_artifact_file(&staged, &target.digest).expect("staged artifact verifies");

    let installed =
        install_verified_artifact(&verified, &target).expect("verified artifact installs");

    let mode = fs::metadata(installed.install_path)
        .expect("installed artifact metadata is readable")
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(mode, 0o755);
}

#[test]
fn content_addressed_staging_uses_the_verified_sha256_as_its_path() {
    let root = tempfile::tempdir().expect("artifact root");
    let source = root.path().join("ployzd.download");
    let store = root.path().join("artifacts");
    fs::write(&source, "ployz\n").expect("artifact can be written");
    let verified =
        verify_artifact_file(&source, &digest(PLOYZ_NEWLINE_SHA256)).expect("artifact verifies");

    let staged = stage_verified_artifact_content_addressed(&verified, &store)
        .expect("artifact stages by content");

    assert_eq!(staged.staged_path, store.join(PLOYZ_NEWLINE_SHA256));
    assert_eq!(staged.digest, digest(PLOYZ_NEWLINE_SHA256));
    assert_eq!(staged.durability, ArtifactInstallDurability::Confirmed);
    assert_eq!(
        fs::read(&staged.staged_path).expect("staged bytes"),
        b"ployz\n"
    );
}

#[test]
fn content_addressed_staging_reuses_valid_existing_content() {
    let root = tempfile::tempdir().expect("artifact root");
    let source = root.path().join("ployzd.download");
    let store = root.path().join("artifacts");
    fs::write(&source, "ployz\n").expect("artifact can be written");
    let verified =
        verify_artifact_file(&source, &digest(PLOYZ_NEWLINE_SHA256)).expect("artifact verifies");
    let first =
        stage_verified_artifact_content_addressed(&verified, &store).expect("first stage succeeds");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(&first.staged_path, fs::Permissions::from_mode(0o644))
            .expect("stored mode can change");
    }
    fs::remove_file(&source).expect("download can disappear after staging");

    let repeated = stage_verified_artifact_content_addressed(&verified, &store)
        .expect("valid content is reusable without its source");

    assert_eq!(repeated.staged_path, first.staged_path);
    assert_eq!(repeated.digest, first.digest);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        let mode = fs::metadata(repeated.staged_path)
            .expect("stored metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o755);
    }
}

#[test]
fn content_addressed_staging_replaces_corrupted_content() {
    let root = tempfile::tempdir().expect("artifact root");
    let source = root.path().join("ployzd.download");
    let store = root.path().join("artifacts");
    fs::write(&source, "ployz\n").expect("artifact can be written");
    let verified =
        verify_artifact_file(&source, &digest(PLOYZ_NEWLINE_SHA256)).expect("artifact verifies");
    let first =
        stage_verified_artifact_content_addressed(&verified, &store).expect("first stage succeeds");
    fs::write(&first.staged_path, "corrupt\n").expect("stored artifact can be corrupted");

    let repaired = stage_verified_artifact_content_addressed(&verified, &store)
        .expect("verified source replaces corrupted content");

    assert_eq!(repaired.staged_path, first.staged_path);
    assert_eq!(
        fs::read(repaired.staged_path).expect("repaired bytes"),
        b"ployz\n"
    );
}

#[test]
fn content_addressed_staging_rejects_a_relative_store() {
    let root = tempfile::tempdir().expect("artifact root");
    let source = root.path().join("ployzd.download");
    fs::write(&source, "ployz\n").expect("artifact can be written");
    let verified =
        verify_artifact_file(&source, &digest(PLOYZ_NEWLINE_SHA256)).expect("artifact verifies");

    assert_eq!(
        stage_verified_artifact_content_addressed(&verified, std::path::Path::new("artifacts")),
        Err(ArtifactInstallError::RelativeContentStore {
            path: PathBuf::from("artifacts"),
        })
    );
}

#[test]
fn remote_acquisition_publishes_only_verified_content_at_the_digest_path() {
    let root = tempfile::tempdir().expect("artifact root");
    let store = root.path().join("downloads");
    let expected = digest(PLOYZ_NEWLINE_SHA256);
    let final_path = store.join(PLOYZ_NEWLINE_SHA256);
    let mut download_path = None;

    let acquired = acquire_remote_artifact_content_addressed(
        "https://example.invalid/ployzd",
        &expected,
        &store,
        |staged| {
            assert_ne!(staged, final_path);
            assert!(!final_path.exists());
            download_path = Some(staged.to_path_buf());
            fs::write(staged, b"ployz\n").map_err(|error| error.to_string())
        },
    )
    .expect("verified remote artifact publishes");

    assert_eq!(acquired.path, final_path);
    assert_eq!(
        fs::read(&acquired.path).expect("published bytes"),
        b"ployz\n"
    );
    assert!(!download_path.expect("download path recorded").exists());
}

#[test]
fn remote_acquisition_reuses_a_valid_cache_without_downloading() {
    let root = tempfile::tempdir().expect("artifact root");
    let store = root.path().join("downloads");
    let cached = store.join(PLOYZ_NEWLINE_SHA256);
    fs::create_dir_all(&store).expect("download store");
    fs::write(&cached, b"ployz\n").expect("valid cache");

    let acquired = acquire_remote_artifact_content_addressed(
        "https://example.invalid/ployzd",
        &digest(PLOYZ_NEWLINE_SHA256),
        &store,
        |_| -> Result<(), String> { panic!("valid cache must skip download") },
    )
    .expect("valid cache is reusable");

    assert_eq!(acquired.path, cached);
}

#[test]
fn remote_acquisition_atomically_repairs_a_corrupt_cache() {
    let root = tempfile::tempdir().expect("artifact root");
    let store = root.path().join("downloads");
    let cached = store.join(PLOYZ_NEWLINE_SHA256);
    fs::create_dir_all(&store).expect("download store");
    fs::write(&cached, b"corrupt\n").expect("corrupt cache");

    acquire_remote_artifact_content_addressed(
        "https://example.invalid/ployzd",
        &digest(PLOYZ_NEWLINE_SHA256),
        &store,
        |staged| {
            assert_ne!(staged, cached);
            assert_eq!(fs::read(&cached).expect("old cache remains"), b"corrupt\n");
            fs::write(staged, b"ployz\n").map_err(|error| error.to_string())
        },
    )
    .expect("corrupt cache repairs");

    assert_eq!(fs::read(cached).expect("repaired cache"), b"ployz\n");
}

#[test]
fn remote_acquisition_preserves_non_digest_cache_read_failures() {
    let root = tempfile::tempdir().expect("artifact root");
    let store = root.path().join("downloads");
    let cached = store.join(PLOYZ_NEWLINE_SHA256);
    fs::create_dir_all(&cached).expect("cache path is unreadable as a file");

    let error = acquire_remote_artifact_content_addressed(
        "https://example.invalid/ployzd",
        &digest(PLOYZ_NEWLINE_SHA256),
        &store,
        |_| -> Result<(), String> { panic!("read failure must skip download") },
    )
    .expect_err("non-digest read failure is preserved");

    assert!(matches!(
        error,
        ArtifactInstallError::VerificationFailed(ArtifactVerificationError::ReadFailed {
            path,
            ..
        }) if path == cached
    ));
    assert!(cached.is_dir());
}

#[test]
fn failed_remote_download_retains_error_evidence_and_cleans_temporary_bytes() {
    let root = tempfile::tempdir().expect("artifact root");
    let store = root.path().join("downloads");
    let cached = store.join(PLOYZ_NEWLINE_SHA256);
    let mut download_path = None;

    let error = acquire_remote_artifact_content_addressed(
        "https://example.invalid/ployzd",
        &digest(PLOYZ_NEWLINE_SHA256),
        &store,
        |staged| {
            download_path = Some(staged.to_path_buf());
            fs::write(staged, b"partial").expect("partial download evidence");
            Err("connection reset")
        },
    )
    .expect_err("failed download remains failed");

    assert!(matches!(
        error,
        ArtifactInstallError::DownloadFailed {
            source_url,
            message,
            ..
        } if source_url == "https://example.invalid/ployzd" && message == "connection reset"
    ));
    assert!(!cached.exists());
    assert!(!download_path.expect("download path recorded").exists());
}

#[test]
fn install_rejects_verified_digest_for_another_target() {
    let staged = temp_artifact("ployz-artifact-wrong-target");
    let install_path = temp_artifact("ployz-artifact-wrong-target-install").join("bin/ployzd");
    fs::write(&staged, "ployz\n").expect("artifact can be written");
    let verified = verify_artifact_file(&staged, &digest(PLOYZ_NEWLINE_SHA256))
        .expect("staged artifact verifies");
    let target = ployzd_target(&install_path, ALL_A_SHA256);

    assert_eq!(
        install_verified_artifact(&verified, &target),
        Err(ArtifactInstallError::VerifiedDigestMismatch {
            install_path,
            expected: digest(ALL_A_SHA256),
            verified: digest(PLOYZ_NEWLINE_SHA256),
        })
    );
}

#[test]
fn install_rechecks_staged_artifact_before_copying() {
    let staged = temp_artifact("ployz-artifact-mutated-source");
    let install_path = temp_artifact("ployz-artifact-mutated-target").join("bin/ployzd");
    fs::write(&staged, "ployz\n").expect("artifact can be written");
    let target = ployzd_target(&install_path, PLOYZ_NEWLINE_SHA256);
    let verified = verify_artifact_file(&staged, &target.digest).expect("staged artifact verifies");
    fs::write(&staged, "changed\n").expect("artifact can be mutated");

    assert!(matches!(
        install_verified_artifact(&verified, &target),
        Err(ArtifactInstallError::VerificationFailed(
            ArtifactVerificationError::DigestMismatch { .. }
        ))
    ));
    assert!(!install_path.exists());
}

#[test]
fn install_commit_failure_preserves_existing_target() {
    let staged = temp_artifact("ployz-artifact-commit-failure-source");
    let install_path = temp_artifact("ployz-artifact-commit-failure-target").join("bin/ployzd");
    fs::write(&staged, "ployz\n").expect("artifact can be written");
    fs::create_dir_all(&install_path).expect("existing target directory can be created");
    let target = ployzd_target(&install_path, PLOYZ_NEWLINE_SHA256);
    let verified = verify_artifact_file(&staged, &target.digest).expect("staged artifact verifies");

    assert!(matches!(
        install_verified_artifact(&verified, &target),
        Err(ArtifactInstallError::CommitFailed {
            install_path: failed_path,
            ..
        }) if failed_path == install_path
    ));
    assert!(install_path.is_dir());
    assert!(staged_artifacts(&install_path).is_empty());
}

#[cfg(unix)]
#[test]
fn ployzd_artifact_store_seeds_arms_reverts_and_can_arm_again() {
    let root = tempfile::tempdir().expect("artifact-store root");
    let state = root.path().join("state");
    let old_source = root.path().join("old-ployzd");
    let candidate_source = root.path().join("candidate-ployzd");
    fs::write(&old_source, "ployz\n").expect("old artifact writes");
    fs::write(&candidate_source, "new\n").expect("candidate artifact writes");
    let old = verify_artifact_file(&old_source, &digest(PLOYZ_NEWLINE_SHA256))
        .expect("old artifact verifies");
    let candidate = verify_artifact_file(&candidate_source, &digest(NEWLINE_NEW_SHA256))
        .expect("candidate artifact verifies");
    let store = PloyzdArtifactStore::new(state.clone()).expect("absolute state directory");

    store.seed_current(&old).expect("initial seed");
    assert_eq!(
        fs::read_link(store.current_path()).expect("current link"),
        PathBuf::from("artifacts").join(PLOYZ_NEWLINE_SHA256)
    );
    let staged = store.stage(&candidate).expect("candidate stages");
    assert_eq!(
        staged.staged_path,
        state.join("artifacts").join(NEWLINE_NEW_SHA256)
    );
    assert_eq!(
        fs::read_link(store.current_path()).expect("current stays old before arm"),
        PathBuf::from("artifacts").join(PLOYZ_NEWLINE_SHA256)
    );

    let armed = store
        .arm_staged(&digest(NEWLINE_NEW_SHA256))
        .expect("verified candidate arms");
    assert_eq!(armed.previous, digest(PLOYZ_NEWLINE_SHA256));
    assert_eq!(armed.current, digest(NEWLINE_NEW_SHA256));
    assert_eq!(
        fs::read_link(store.previous_path()).expect("previous link"),
        PathBuf::from("artifacts").join(PLOYZ_NEWLINE_SHA256)
    );
    assert_eq!(
        fs::read_link(store.current_path()).expect("current link"),
        PathBuf::from("artifacts").join(NEWLINE_NEW_SHA256)
    );
    assert_eq!(
        store.pending_upgrade().expect("pending marker"),
        Some(digest(NEWLINE_NEW_SHA256))
    );

    let retry = store
        .arm_staged(&digest(NEWLINE_NEW_SHA256))
        .expect("retrying the armed candidate succeeds");
    assert_eq!(retry, armed);

    assert_eq!(
        store.revert_armed().expect("local revert"),
        digest(PLOYZ_NEWLINE_SHA256)
    );
    assert_eq!(
        fs::read_link(store.current_path()).expect("current now points at previous"),
        state.join("previous")
    );
    assert_eq!(
        fs::read(store.current_path()).expect("reverted executable bytes"),
        b"ployz\n"
    );
    assert_eq!(store.pending_upgrade().expect("marker clears"), None);

    store
        .arm_staged(&digest(NEWLINE_NEW_SHA256))
        .expect("approved current-to-previous indirection can arm again");
    store.confirm_armed().expect("first converge confirms arm");
    assert_eq!(store.pending_upgrade().expect("marker clears"), None);
}

#[cfg(unix)]
#[test]
fn ployzd_artifact_store_retrying_the_partially_armed_candidate_completes_the_swap() {
    let root = tempfile::tempdir().expect("artifact-store root");
    let state = root.path().join("state");
    let old_source = root.path().join("old-ployzd");
    let candidate_source = root.path().join("candidate-ployzd");
    fs::write(&old_source, "ployz\n").expect("old artifact writes");
    fs::write(&candidate_source, "new\n").expect("candidate artifact writes");
    let old = verify_artifact_file(&old_source, &digest(PLOYZ_NEWLINE_SHA256))
        .expect("old artifact verifies");
    let candidate = verify_artifact_file(&candidate_source, &digest(NEWLINE_NEW_SHA256))
        .expect("candidate artifact verifies");
    let store = PloyzdArtifactStore::new(state).expect("absolute state directory");

    store.seed_current(&old).expect("initial seed");
    store.stage(&candidate).expect("candidate stages");
    std::os::unix::fs::symlink(
        PathBuf::from("artifacts").join(PLOYZ_NEWLINE_SHA256),
        store.previous_path(),
    )
    .expect("previous link writes");
    fs::write(store.pending_path(), format!("{}\n", NEWLINE_NEW_SHA256))
        .expect("pending marker writes");

    let retry = store
        .arm_staged(&digest(NEWLINE_NEW_SHA256))
        .expect("retry completes the partial arm");

    assert_eq!(retry.previous, digest(PLOYZ_NEWLINE_SHA256));
    assert_eq!(retry.current, digest(NEWLINE_NEW_SHA256));
    assert_eq!(
        fs::read_link(store.current_path()).expect("current link"),
        PathBuf::from("artifacts").join(NEWLINE_NEW_SHA256)
    );
    assert_eq!(
        store.pending_upgrade().expect("pending marker"),
        Some(digest(NEWLINE_NEW_SHA256))
    );
}

#[test]
fn ployzd_artifact_store_refuses_to_stage_unverified_bytes() {
    let root = tempfile::tempdir().expect("artifact-store root");
    let source = root.path().join("candidate-ployzd");
    fs::write(&source, "untrusted\n").expect("candidate writes");
    let store =
        PloyzdArtifactStore::new(root.path().join("state")).expect("absolute state directory");
    let forged = VerifiedArtifactFile {
        path: source,
        digest: digest(PLOYZ_NEWLINE_SHA256),
    };

    assert!(matches!(
        store.stage(&forged),
        Err(ArtifactStoreError::Stage(
            ArtifactInstallError::VerificationFailed(
                ArtifactVerificationError::DigestMismatch { .. }
            )
        ))
    ));
    assert!(!store.artifacts_path().join(PLOYZ_NEWLINE_SHA256).exists());
    assert!(!store.current_path().exists());
}

#[test]
fn root_store_adopts_only_exact_api_staged_candidate_metadata_and_bytes() {
    let root = tempfile::tempdir().expect("artifact-store root");
    let source = root.path().join("candidate-ployzd");
    fs::write(&source, "new\n").expect("candidate writes");
    let verified =
        verify_artifact_file(&source, &digest(NEWLINE_NEW_SHA256)).expect("candidate verifies");
    let version = ArtifactVersion::try_new("1.2.3").expect("version");
    let staging = PloyzdArtifactStore::new(root.path().join("api/upgrade-staging"))
        .expect("absolute staging directory");
    let live = PloyzdArtifactStore::new(root.path().join("live")).expect("absolute live directory");

    staging
        .stage_upgrade_candidate(&verified, &version)
        .expect("API candidate stages");
    assert!(!live.artifacts_path().join(NEWLINE_NEW_SHA256).exists());

    let adopted = live
        .adopt_upgrade_candidate(&staging, &version, &digest(NEWLINE_NEW_SHA256))
        .expect("Keeper adopts exact candidate");
    assert_eq!(
        adopted.staged_path,
        live.artifacts_path().join(NEWLINE_NEW_SHA256)
    );
    assert_eq!(
        fs::read(adopted.staged_path).expect("adopted bytes"),
        b"new\n"
    );

    let other_version = ArtifactVersion::try_new("1.2.4").expect("other version");
    assert!(matches!(
        live.adopt_upgrade_candidate(&staging, &other_version, &digest(NEWLINE_NEW_SHA256),),
        Err(ArtifactStoreError::CandidateMetadataMismatch { .. })
    ));
}

#[test]
fn root_store_reverifies_api_staged_candidate_before_adoption() {
    let root = tempfile::tempdir().expect("artifact-store root");
    let source = root.path().join("candidate-ployzd");
    fs::write(&source, "new\n").expect("candidate writes");
    let verified =
        verify_artifact_file(&source, &digest(NEWLINE_NEW_SHA256)).expect("candidate verifies");
    let version = ArtifactVersion::try_new("1.2.3").expect("version");
    let staging = PloyzdArtifactStore::new(root.path().join("api/upgrade-staging"))
        .expect("absolute staging directory");
    let live = PloyzdArtifactStore::new(root.path().join("live")).expect("absolute live directory");
    staging
        .stage_upgrade_candidate(&verified, &version)
        .expect("API candidate stages");
    fs::write(
        staging.artifacts_path().join(NEWLINE_NEW_SHA256),
        b"tampered\n",
    )
    .expect("staged bytes can be corrupted for test");

    assert!(matches!(
        live.adopt_upgrade_candidate(&staging, &version, &digest(NEWLINE_NEW_SHA256)),
        Err(ArtifactStoreError::Verification(
            ArtifactVerificationError::DigestMismatch { .. }
        ))
    ));
    assert!(!live.artifacts_path().join(NEWLINE_NEW_SHA256).exists());
}

fn digest(value: &str) -> Sha256Digest {
    Sha256Digest::try_new(value).expect("valid artifact digest")
}

fn ployzd_target(install_path: &std::path::Path, digest: &str) -> ArtifactTarget {
    ArtifactTarget::new(
        ArtifactKind::Ployzd,
        ArtifactVersion::try_new("0.1.0").expect("valid version"),
        ArtifactSource::try_new("https://example.invalid/ployzd").expect("valid source"),
        Sha256Digest::try_new(digest).expect("valid digest"),
        install_path.to_path_buf(),
    )
    .expect("valid ployzd target")
}

fn temp_artifact(prefix: &str) -> PathBuf {
    let unique = format!(
        "{}-{}",
        prefix,
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .expect("system clock is after unix epoch")
            .as_nanos()
    );
    std::env::temp_dir().join(unique)
}

fn staged_artifacts(install_path: &std::path::Path) -> Vec<PathBuf> {
    install_path
        .parent()
        .expect("install path has parent")
        .read_dir()
        .expect("install parent can be read")
        .filter_map(|entry| {
            let entry = entry.expect("directory entry is readable");
            let file_name = entry.file_name();
            let name = file_name.to_string_lossy();
            name.starts_with(".ployzd.ployz-install-")
                .then(|| entry.path())
        })
        .collect()
}

const PLOYZ_NEWLINE_SHA256: &str =
    "2dcc3bb1142455239d3b3391d9569a8ce0fbdfb906cd0434329e5dd736592138";
const NEWLINE_NEW_SHA256: &str = "7aa7a5359173d05b63cfd682e3c38487f3cb4f7f1d60659fe59fab1505977d4c";
const ALL_A_SHA256: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
