use std::fmt;
use std::net::IpAddr;
use std::path::{Path, PathBuf};

use crate::ids::NodeId;
use crate::nats_config::{NatsCaCertificatePem, NatsUserSeed, is_valid_host_syntax};
use crate::roles::{DaemonProcessRole, DnsRole, GatewayRole, InstallRolePolicy};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FirstNodeInstallSpec {
    pub node_id: NodeId,
    pub gateway: GatewayRole,
    /// DNS defaults on for the alpha machine shape; specs written before
    /// the DNS role existed parse as `install`.
    #[serde(default = "default_first_node_dns")]
    pub dns: DnsRole,
    pub node_public_ip: Option<IpAddr>,
    pub machine_bootstrap_url: Option<MachineBootstrapUrl>,
    pub machine_join_template_file: Option<AbsoluteInstallPath>,
    pub artifacts: FirstNodeInstallArtifacts,
}

impl FirstNodeInstallSpec {
    /// The optional-role policy carried by this spec.
    #[must_use]
    pub const fn role_policy(&self) -> InstallRolePolicy {
        InstallRolePolicy {
            gateway: self.gateway,
            dns: self.dns,
        }
    }
}

const fn default_first_node_dns() -> DnsRole {
    DnsRole::Install
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FirstNodeInstallArtifacts {
    pub ployzd: InstallArtifactSpec,
    pub ebpf_bytecode: InstallArtifactSpec,
    pub ebpf_ctl: InstallArtifactSpec,
    pub nats_server: NatsServerInstallSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineJoinArtifactBundleSpec {
    pub ployzd: InstallArtifactSpec,
    pub ebpf_bytecode: InstallArtifactSpec,
    pub ebpf_ctl: InstallArtifactSpec,
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
    pub material: MachineJoinMaterial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineJoinMaterial {
    pub cluster_name: MachineJoinClusterName,
    pub runtime_nats_url: MachineJoinRuntimeNatsUrl,
    pub trusted_nats: MachineJoinTrustedNats,
    pub ployzd: InstallArtifactSpec,
    pub ebpf_bytecode: InstallArtifactSpec,
    pub ebpf_ctl: InstallArtifactSpec,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineJoinSecretDelivery {
    pub nats_credentials: NatsUserSeed,
}

/// Non-secret machine-add bootstrap material loaded by the control role.
/// Per-machine secrets are minted at machine-add as bounded operation
/// work; the template never carries credentials.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineJoinTemplate {
    pub join_bundle: MachineJoinBundle,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineJoinTrustedNats {
    pub ca_pem: NatsCaCertificatePem,
}

/// Well-known on-machine NATS material paths.
///
/// This is the single owner of the Phase B file-ownership table: keeper
/// writes the TLS material and the controller/operator/join seeds at
/// install; `ployzd` control writes `node.seed` at activate-first-node.
/// `node.seed` deliberately does not exist at install time — node and
/// gateway roles await it with bounded retries instead of falling back to
/// controller authority.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NatsMachineMaterialPaths {
    state_dir: PathBuf,
}

impl NatsMachineMaterialPaths {
    #[must_use]
    pub const fn new(state_dir: PathBuf) -> Self {
        Self { state_dir }
    }

    /// The product path: `/var/lib/ployz/nats`.
    #[must_use]
    pub fn in_default_state_dir() -> Self {
        Self::new(PathBuf::from("/var/lib/ployz/nats"))
    }

    #[must_use]
    pub fn state_dir(&self) -> &Path {
        &self.state_dir
    }

    #[must_use]
    pub fn ca_file(&self) -> PathBuf {
        self.state_dir.join("ca.pem")
    }

    #[must_use]
    pub fn server_cert_file(&self) -> PathBuf {
        self.state_dir.join("server.crt")
    }

    #[must_use]
    pub fn server_key_file(&self) -> PathBuf {
        self.state_dir.join("server.key")
    }

    #[must_use]
    pub fn controller_seed_file(&self) -> PathBuf {
        self.state_dir.join("controller.seed")
    }

    #[must_use]
    pub fn operator_seed_file(&self) -> PathBuf {
        self.state_dir.join("operator.seed")
    }

    #[must_use]
    pub fn join_seed_file(&self) -> PathBuf {
        self.state_dir.join("join.seed")
    }

    /// Written by `ployzd` control at activate-first-node, never by keeper.
    #[must_use]
    pub fn node_seed_file(&self) -> PathBuf {
        self.state_dir.join("node.seed")
    }

    /// The seed file each daemon role authenticates with. Control holds
    /// Controller authority; node, gateway, and DNS share the machine's
    /// Node credential (there is no Gateway principal in v1).
    #[must_use]
    pub fn role_seed_file(&self, role: &DaemonProcessRole) -> PathBuf {
        match role {
            DaemonProcessRole::Control => self.controller_seed_file(),
            DaemonProcessRole::Node(_) | DaemonProcessRole::Gateway | DaemonProcessRole::Dns => {
                self.node_seed_file()
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct MachineJoinRuntimeNatsUrl(String);

impl MachineJoinRuntimeNatsUrl {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InstallContractError> {
        let value = value.into();
        if value.is_empty() {
            return Err(InstallContractError::EmptyRuntimeNatsUrl);
        }
        if value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
            || !nats_url_has_host_and_port(&value)
        {
            return Err(InstallContractError::InvalidRuntimeNatsUrl { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

fn nats_url_has_host_and_port(value: &str) -> bool {
    let Some(authority) = value
        .strip_prefix("nats://")
        .or_else(|| value.strip_prefix("tls://"))
    else {
        return false;
    };
    let Some((host, port)) = authority.rsplit_once(':') else {
        return false;
    };
    is_valid_host_syntax(host) && port.parse::<u16>().is_ok_and(|port| port > 0)
}

impl TryFrom<String> for MachineJoinRuntimeNatsUrl {
    type Error = InstallContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<MachineJoinRuntimeNatsUrl> for String {
    fn from(value: MachineJoinRuntimeNatsUrl) -> Self {
        value.0
    }
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
    EmptyRuntimeNatsUrl,
    InvalidRuntimeNatsUrl { value: String },
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
            Self::EmptyRuntimeNatsUrl => formatter.write_str("runtime NATS URL is empty"),
            Self::InvalidRuntimeNatsUrl { value } => write!(
                formatter,
                "runtime NATS URL {value:?} must be a nats:// or tls:// URL with host and port"
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
