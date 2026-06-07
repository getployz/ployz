use std::fs;
use std::path::PathBuf;

use ployz_keeper::artifacts::{ArtifactVerificationError, Sha256Digest, verify_artifact_file};

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

fn digest(value: &str) -> Sha256Digest {
    Sha256Digest::try_new(value).expect("valid artifact digest")
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

const PLOYZ_NEWLINE_SHA256: &str =
    "2dcc3bb1142455239d3b3391d9569a8ce0fbdfb906cd0434329e5dd736592138";
const ALL_A_SHA256: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
