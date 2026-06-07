use std::fmt;
use std::path::Path;

use crate::ids::NodeId;
use crate::roles::FirstNodeGateway;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperFirstNodeInstall {
    pub node_id: NodeId,
    pub gateway: FirstNodeGateway,
    pub ployzd_version: InstallArtifactVersion,
    pub ployzd_source: InstallArtifactSource,
    pub ployzd_sha256: InstallSha256Digest,
    pub ployzd_install_path: AbsoluteInstallPath,
    pub nats_binary: AbsoluteInstallPath,
    pub nats_config: AbsoluteInstallPath,
}

impl KeeperFirstNodeInstall {
    #[must_use]
    pub fn render_command(&self) -> String {
        let mut command = format!(
            "ployz-keeper first-node-install --node {} --ployzd-version {} --ployzd-source {} --ployzd-sha256 {} --ployzd-install-path {} --nats-binary {} --nats-config {}",
            shell_quote(self.node_id.as_str()),
            shell_quote(self.ployzd_version.as_str()),
            shell_quote(self.ployzd_source.as_str()),
            shell_quote(self.ployzd_sha256.as_str()),
            shell_quote(self.ployzd_install_path.as_str()),
            shell_quote(self.nats_binary.as_str()),
            shell_quote(self.nats_config.as_str())
        );
        if self.gateway == FirstNodeGateway::Install {
            command.push_str(" --gateway");
        }
        command
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallContractError {
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
