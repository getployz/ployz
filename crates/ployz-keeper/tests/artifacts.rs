use std::fs;
use std::path::PathBuf;
use std::process::Command;

use ployz_keeper::artifacts::{
    ArtifactInstallDurability, ArtifactInstallError, ArtifactKind, ArtifactSource, ArtifactTarget,
    ArtifactVerificationError, ArtifactVersion, Sha256Digest, install_verified_artifact,
    install_verified_nats_server_archive, verify_artifact_file,
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
fn nats_server_archive_install_extracts_binary() {
    let root = temp_artifact("ployz-nats-archive");
    let package = root.join("nats-server-v2.14.2-linux-amd64");
    fs::create_dir_all(&package).expect("package dir can be created");
    fs::write(package.join("nats-server"), "nats-server-binary\n")
        .expect("nats-server binary can be written");
    let archive = root.join("nats-server.tar.gz");
    let status = Command::new("tar")
        .arg("-czf")
        .arg(&archive)
        .arg("-C")
        .arg(&root)
        .arg("nats-server-v2.14.2-linux-amd64")
        .status()
        .expect("tar can run");
    assert!(status.success());
    let install_path = root.join("bin/nats-server");
    let archive_digest = sha256(&archive);
    let target = nats_server_target(&install_path, archive_digest.as_str());
    let verified = verify_artifact_file(&archive, &target.digest).expect("archive verifies");

    let installed =
        install_verified_nats_server_archive(&verified, &target).expect("nats archive installs");

    assert_eq!(installed.source_path, archive);
    assert_eq!(installed.install_path, install_path);
    assert_eq!(
        fs::read_to_string(&installed.install_path).expect("installed binary is readable"),
        "nats-server-binary\n"
    );
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

fn nats_server_target(install_path: &std::path::Path, digest: &str) -> ArtifactTarget {
    ArtifactTarget::new(
        ArtifactKind::NatsServer,
        ArtifactVersion::try_new("2.14.2").expect("valid version"),
        ArtifactSource::try_new("https://example.invalid/nats-server.tar.gz")
            .expect("valid source"),
        Sha256Digest::try_new(digest).expect("valid digest"),
        install_path.to_path_buf(),
    )
    .expect("valid nats-server target")
}

fn sha256(path: &std::path::Path) -> Sha256Digest {
    use sha2::{Digest, Sha256};

    let bytes = fs::read(path).expect("file can be read");
    let digest = Sha256::digest(bytes);
    Sha256Digest::try_new(format!("{digest:x}")).expect("valid digest")
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
const ALL_A_SHA256: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
