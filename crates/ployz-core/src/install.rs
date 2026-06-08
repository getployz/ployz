use std::fmt;
use std::path::Path;

use crate::ids::NodeId;
use crate::roles::FirstNodeGateway;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperFirstNodeInstall {
    pub node_id: NodeId,
    pub gateway: FirstNodeGateway,
    pub machine_bootstrap_url: Option<MachineBootstrapUrl>,
    pub ployzd_version: InstallArtifactVersion,
    pub ployzd_source: InstallArtifactSource,
    pub ployzd_sha256: InstallSha256Digest,
    pub ployzd_install_path: AbsoluteInstallPath,
    pub nats_version: InstallArtifactVersion,
    pub nats_source: InstallArtifactSource,
    pub nats_sha256: InstallSha256Digest,
    pub nats_binary: AbsoluteInstallPath,
    pub nats_config: AbsoluteInstallPath,
}

impl KeeperFirstNodeInstall {
    #[must_use]
    pub fn command_args(&self) -> Vec<String> {
        let mut args = vec![
            "first-node-install".to_owned(),
            "--node".to_owned(),
            self.node_id.as_str().to_owned(),
            "--ployzd-version".to_owned(),
            self.ployzd_version.as_str().to_owned(),
            "--ployzd-source".to_owned(),
            self.ployzd_source.as_str().to_owned(),
            "--ployzd-sha256".to_owned(),
            self.ployzd_sha256.as_str().to_owned(),
            "--ployzd-install-path".to_owned(),
            self.ployzd_install_path.as_str().to_owned(),
            "--nats-version".to_owned(),
            self.nats_version.as_str().to_owned(),
            "--nats-source".to_owned(),
            self.nats_source.as_str().to_owned(),
            "--nats-sha256".to_owned(),
            self.nats_sha256.as_str().to_owned(),
            "--nats-binary".to_owned(),
            self.nats_binary.as_str().to_owned(),
            "--nats-config".to_owned(),
            self.nats_config.as_str().to_owned(),
        ];
        if self.gateway == FirstNodeGateway::Install {
            args.push("--gateway".to_owned());
        }
        if let Some(machine_bootstrap_url) = &self.machine_bootstrap_url {
            args.extend([
                "--machine-bootstrap-url".to_owned(),
                machine_bootstrap_url.as_str().to_owned(),
            ]);
        }
        args
    }

    #[must_use]
    pub fn render_command(&self) -> String {
        std::iter::once("ployz-keeper".to_owned())
            .chain(self.command_args())
            .enumerate()
            .map(|(index, token)| {
                if index == 0 || token == "first-node-install" || token.starts_with("--") {
                    token
                } else {
                    shell_quote(&token)
                }
            })
            .collect::<Vec<_>>()
            .join(" ")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"MachineBootstrapUrl\">")
)]
#[serde(try_from = "String", into = "String")]
pub struct MachineBootstrapUrl(String);

impl MachineBootstrapUrl {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InstallContractError> {
        let value = value.into();
        if value.is_empty() {
            return Err(InstallContractError::EmptyBootstrapUrl);
        }
        if !value.starts_with("https://")
            || value
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(InstallContractError::InvalidBootstrapUrl { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MachineBootstrapUrl {
    type Error = InstallContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<MachineBootstrapUrl> for String {
    fn from(value: MachineBootstrapUrl) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineJoinBundle {
    pub cluster_name: MachineJoinClusterName,
    pub ployzd: MachineJoinPloyzdArtifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineJoinPloyzdArtifact {
    pub version: InstallArtifactVersion,
    pub source: InstallArtifactSource,
    pub sha256: InstallSha256Digest,
    pub install_path: AbsoluteInstallPath,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct MachineJoinClusterName(String);

impl MachineJoinClusterName {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InstallContractError> {
        let value = value.into();
        if value.is_empty() {
            return Err(InstallContractError::EmptyClusterName);
        }
        if value.contains(['\n', '\r', '=']) {
            return Err(InstallContractError::InvalidClusterName { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MachineJoinClusterName {
    type Error = InstallContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<MachineJoinClusterName> for String {
    fn from(value: MachineJoinClusterName) -> Self {
        value.0
    }
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct AbsoluteInstallPath(String);

impl AbsoluteInstallPath {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InstallContractError> {
        let value = value.into();
        let path = Path::new(&value);
        if value.is_empty() {
            return Err(InstallContractError::EmptyInstallPath);
        }
        if !path.is_absolute() {
            return Err(InstallContractError::RelativeInstallPath { value });
        }
        if path.parent().is_none() {
            return Err(InstallContractError::MissingInstallParent { value });
        }
        if path.file_name().is_none() || value.ends_with(std::path::MAIN_SEPARATOR) {
            return Err(InstallContractError::MissingInstallFileName { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for AbsoluteInstallPath {
    type Error = InstallContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<AbsoluteInstallPath> for String {
    fn from(value: AbsoluteInstallPath) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallContractError {
    EmptyClusterName,
    InvalidClusterName { value: String },
    EmptyBootstrapUrl,
    InvalidBootstrapUrl { value: String },
    EmptyArtifactVersion,
    EmptyArtifactSource,
    RelativeArtifactSource { value: String },
    EmptySha256Digest,
    InvalidSha256Digest { value: String },
    EmptyInstallPath,
    RelativeInstallPath { value: String },
    MissingInstallParent { value: String },
    MissingInstallFileName { value: String },
}

impl fmt::Display for InstallContractError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyClusterName => formatter.write_str("cluster name is empty"),
            Self::InvalidClusterName { value } => {
                write!(
                    formatter,
                    "cluster name {value:?} contains unsupported characters"
                )
            }
            Self::EmptyBootstrapUrl => formatter.write_str("machine bootstrap URL is empty"),
            Self::InvalidBootstrapUrl { value } => write!(
                formatter,
                "machine bootstrap URL {value:?} must be an HTTPS URL without whitespace"
            ),
            Self::EmptyArtifactVersion => formatter.write_str("artifact version is empty"),
            Self::EmptyArtifactSource => formatter.write_str("artifact source is empty"),
            Self::RelativeArtifactSource { value } => {
                write!(formatter, "artifact source path {value} must be absolute")
            }
            Self::EmptySha256Digest => formatter.write_str("sha256 digest is empty"),
            Self::InvalidSha256Digest { value } => write!(
                formatter,
                "sha256 digest {value:?} must be 64 hex characters"
            ),
            Self::EmptyInstallPath => formatter.write_str("install path is empty"),
            Self::RelativeInstallPath { value } => {
                write!(formatter, "install path {value} must be absolute")
            }
            Self::MissingInstallParent { value } => {
                write!(formatter, "install path {value} must include a parent")
            }
            Self::MissingInstallFileName { value } => {
                write!(formatter, "install path {value} must include a file name")
            }
        }
    }
}

impl std::error::Error for InstallContractError {}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
