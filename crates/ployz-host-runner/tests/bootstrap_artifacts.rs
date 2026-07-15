mod support;

use std::path::PathBuf;

use ployz_host_runner::execution::{
    ArtifactKind, ArtifactTarget, ArtifactTargetError, Sha256Digest,
};
use ployz_test_support::host_runner::{
    artifact_source as source, artifact_version as version, sha256_digest as digest,
};
use support::bootstrap::{HOST_RUNNER_DIGEST, PLOYZD_DIGEST};

#[test]
fn artifact_digest_must_be_sha256_hex() {
    assert_eq!(
        Sha256Digest::try_new("sha256:host-runner"),
        Err(ArtifactTargetError::InvalidSha256Digest {
            value: "sha256:host-runner".to_owned()
        })
    );
    assert!(Sha256Digest::try_new(HOST_RUNNER_DIGEST).is_ok());
}

#[test]
fn artifact_install_paths_must_be_absolute() {
    assert_eq!(
        ArtifactTarget::new(
            ArtifactKind::Ployzd,
            version("0.1.0"),
            source("https://example.invalid/ployzd"),
            digest(PLOYZD_DIGEST),
            PathBuf::new(),
        ),
        Err(ArtifactTargetError::EmptyInstallPath)
    );
    assert_eq!(
        ArtifactTarget::new(
            ArtifactKind::Ployzd,
            version("0.1.0"),
            source("https://example.invalid/ployzd"),
            digest(PLOYZD_DIGEST),
            PathBuf::from("bin/ployzd"),
        ),
        Err(ArtifactTargetError::RelativeInstallPath {
            value: "bin/ployzd".to_owned(),
        })
    );
    assert_eq!(
        ArtifactTarget::new(
            ArtifactKind::Ployzd,
            version("0.1.0"),
            source("https://example.invalid/ployzd"),
            digest(PLOYZD_DIGEST),
            PathBuf::from("/"),
        ),
        Err(ArtifactTargetError::MissingInstallParent {
            value: "/".to_owned(),
        })
    );
    assert_eq!(
        ArtifactTarget::new(
            ArtifactKind::Ployzd,
            version("0.1.0"),
            source("https://example.invalid/ployzd"),
            digest(PLOYZD_DIGEST),
            PathBuf::from("/usr/local/bin/"),
        ),
        Err(ArtifactTargetError::MissingInstallFileName {
            value: "/usr/local/bin/".to_owned(),
        })
    );
}
