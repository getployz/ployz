//! Release artifacts and their installation destinations.

use std::path::Path;

use serde::{Deserialize, Serialize};

use super::paths::AbsoluteInstallPath;
use super::validation::InstallContractError;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FirstMachineInstallArtifacts {
    pub ployzd: InstallArtifactSpec,
    pub ebpf_bytecode: InstallArtifactSpec,
    pub ebpf_ctl: InstallArtifactSpec,
    pub railpack: InstallArtifactSpec,
    /// Absent when the release manifest ships no `nats-server` (a dev
    /// substrate push). Installs that found or promote a core require it.
    pub nats_server: Option<NatsServerInstallSpec>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineJoinArtifactBundleSpec {
    pub ployzd: InstallArtifactSpec,
    pub ebpf_bytecode: InstallArtifactSpec,
    pub ebpf_ctl: InstallArtifactSpec,
    pub railpack: InstallArtifactSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct InstallArtifactSpec {
    pub version: InstallArtifactVersion,
    pub source: InstallArtifactSource,
    pub sha256: InstallSha256Digest,
    pub install_path: AbsoluteInstallPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct NatsServerInstallSpec {
    pub version: InstallArtifactVersion,
    pub source: InstallArtifactSource,
    pub sha256: InstallSha256Digest,
    pub binary: AbsoluteInstallPath,
    pub config: AbsoluteInstallPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct InstallArtifactVersion(String);

impl InstallArtifactVersion {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InstallContractError> {
        let value = value.into();
        if value.is_empty() {
            return Err(InstallContractError::EmptyArtifactVersion);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for InstallArtifactVersion {
    type Error = InstallContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<InstallArtifactVersion> for String {
    fn from(value: InstallArtifactVersion) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct InstallArtifactSource(String);

impl InstallArtifactSource {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InstallContractError> {
        let value = value.into();
        if value.is_empty() {
            return Err(InstallContractError::EmptyArtifactSource);
        }
        if value.starts_with("https://") || value.starts_with("http://") {
            return Ok(Self(value));
        }
        if Path::new(&value).is_absolute() {
            Ok(Self(value))
        } else {
            Err(InstallContractError::RelativeArtifactSource { value })
        }
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for InstallArtifactSource {
    type Error = InstallContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<InstallArtifactSource> for String {
    fn from(value: InstallArtifactSource) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct InstallSha256Digest(String);

impl InstallSha256Digest {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InstallContractError> {
        let value = value.into();
        if value.is_empty() {
            return Err(InstallContractError::EmptySha256Digest);
        }
        if value.len() != 64 || !value.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err(InstallContractError::InvalidSha256Digest { value });
        }
        Ok(Self(value.to_ascii_lowercase()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for InstallSha256Digest {
    type Error = InstallContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<InstallSha256Digest> for String {
    fn from(value: InstallSha256Digest) -> Self {
        value.0
    }
}
