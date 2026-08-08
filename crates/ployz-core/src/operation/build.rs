use serde::{Deserialize, Serialize};

use crate::ids::MachineId;
use crate::image::{OciDigest, OciPlatform};
use crate::install::{InstallArtifactVersion, InstallSha256Digest};

use super::FailureMessage;

pub const MAX_BUILD_LOG_CHUNK_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BuildCachePruneEvidence {
    pub before_available_bytes: u64,
    pub reclaimed_bytes: u64,
    pub after_available_bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildPlatformFailure {
    MachineUnavailable {
        message: FailureMessage,
    },
    ExecutorUnavailable {
        message: FailureMessage,
    },
    ImageSeedUnavailable {
        image_seed: MachineId,
    },
    BuildkitDigestMismatch {
        expected: OciDigest,
        actual: OciDigest,
    },
    HelperDigestMismatch {
        expected: InstallSha256Digest,
        actual: InstallSha256Digest,
    },
    FrontendDigestMismatch {
        expected: OciDigest,
        actual: OciDigest,
    },
    PlatformMismatch {
        expected: OciPlatform,
        actual: OciPlatform,
    },
    InsufficientHostDisk {
        available_bytes: u64,
        required_free_bytes: u64,
    },
    SourceFetchFailed {
        message: FailureMessage,
    },
    AdapterFailed {
        message: FailureMessage,
    },
    ImagePushFailed {
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct BuildToolchainEvidence {
    pub buildkit_image: OciDigest,
    pub adapter: BuildAdapterToolchainEvidence,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "adapter", rename_all = "snake_case", deny_unknown_fields)]
pub enum BuildAdapterToolchainEvidence {
    Dockerfile,
    Railpack {
        helper_version: InstallArtifactVersion,
        helper_sha256: InstallSha256Digest,
        frontend_image: OciDigest,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(try_from = "String", into = "String")]
pub struct BuildLogChunk(String);

impl BuildLogChunk {
    pub fn try_new(value: impl Into<String>) -> Result<Self, BuildLogChunkError> {
        let value = value.into();
        if value.is_empty() {
            return Err(BuildLogChunkError::Empty);
        }
        if value.len() > MAX_BUILD_LOG_CHUNK_BYTES {
            return Err(BuildLogChunkError::TooLarge {
                actual: value.len(),
                maximum: MAX_BUILD_LOG_CHUNK_BYTES,
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for BuildLogChunk {
    type Error = BuildLogChunkError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<BuildLogChunk> for String {
    fn from(value: BuildLogChunk) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuildLogChunkError {
    #[error("build log chunk must not be empty")]
    Empty,
    #[error("build log chunk is {actual} bytes; maximum is {maximum}")]
    TooLarge { actual: usize, maximum: usize },
}
