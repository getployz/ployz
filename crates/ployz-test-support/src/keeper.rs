//! Typed constructors and the canonical artifact-target fixtures for
//! keeper install-plan tests.

use std::path::PathBuf;

use ployz_keeper::artifacts::{
    ArtifactKind, ArtifactSource, ArtifactTarget, ArtifactVersion, Sha256Digest,
};

/// A syntactically valid sha256 hex digest for ployzd artifact fixtures.
pub const TEST_PLOYZD_DIGEST: &str =
    "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";

#[must_use]
pub fn artifact_version(value: &str) -> ArtifactVersion {
    ArtifactVersion::try_new(value).expect("valid artifact version")
}

#[must_use]
pub fn artifact_source(value: &str) -> ArtifactSource {
    ArtifactSource::try_new(value).expect("valid artifact source")
}

#[must_use]
pub fn sha256_digest(value: &str) -> Sha256Digest {
    Sha256Digest::try_new(value).expect("valid artifact digest")
}

/// The canonical remote ployzd artifact target installed by plan fixtures.
#[must_use]
pub fn ployzd_artifact() -> ArtifactTarget {
    ArtifactTarget::new(
        ArtifactKind::Ployzd,
        artifact_version("0.1.0"),
        artifact_source("https://example.invalid/ployzd"),
        sha256_digest(TEST_PLOYZD_DIGEST),
        PathBuf::from("/usr/local/bin/ployzd"),
    )
    .expect("valid ployzd artifact")
}

/// The canonical remote nats-server artifact target installed by plan
/// fixtures.
#[must_use]
pub fn nats_server_artifact() -> ArtifactTarget {
    ArtifactTarget::new(
        ArtifactKind::NatsServer,
        artifact_version("2.12.0"),
        artifact_source("https://example.invalid/nats-server"),
        sha256_digest(TEST_PLOYZD_DIGEST),
        PathBuf::from("/usr/local/bin/nats-server"),
    )
    .expect("valid nats-server artifact")
}
