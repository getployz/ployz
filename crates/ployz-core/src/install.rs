use std::fmt;
use std::net::IpAddr;
use std::net::SocketAddr;
use std::path::Path;

use crate::ids::NodeId;
use crate::roles::FirstNodeGateway;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeeperFirstNodeInstall {
    pub node_id: NodeId,
    pub gateway: FirstNodeGateway,
    pub node_public_ip: Option<IpAddr>,
    pub machine_bootstrap_url: Option<MachineBootstrapUrl>,
    pub machine_join_template_file: Option<AbsoluteInstallPath>,
    pub ployzd_version: InstallArtifactVersion,
    pub ployzd_source: InstallArtifactSource,
    pub ployzd_sha256: InstallSha256Digest,
    pub ployzd_install_path: AbsoluteInstallPath,
    pub ebpf_bytecode_version: InstallArtifactVersion,
    pub ebpf_bytecode_source: InstallArtifactSource,
    pub ebpf_bytecode_sha256: InstallSha256Digest,
    pub ebpf_bytecode_install_path: AbsoluteInstallPath,
    pub ebpf_ctl_version: InstallArtifactVersion,
    pub ebpf_ctl_source: InstallArtifactSource,
    pub ebpf_ctl_sha256: InstallSha256Digest,
    pub ebpf_ctl_install_path: AbsoluteInstallPath,
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
            "--ebpf-bytecode-version".to_owned(),
            self.ebpf_bytecode_version.as_str().to_owned(),
            "--ebpf-bytecode-source".to_owned(),
            self.ebpf_bytecode_source.as_str().to_owned(),
            "--ebpf-bytecode-sha256".to_owned(),
            self.ebpf_bytecode_sha256.as_str().to_owned(),
            "--ebpf-bytecode-install-path".to_owned(),
            self.ebpf_bytecode_install_path.as_str().to_owned(),
            "--ebpf-ctl-version".to_owned(),
            self.ebpf_ctl_version.as_str().to_owned(),
            "--ebpf-ctl-source".to_owned(),
            self.ebpf_ctl_source.as_str().to_owned(),
            "--ebpf-ctl-sha256".to_owned(),
            self.ebpf_ctl_sha256.as_str().to_owned(),
            "--ebpf-ctl-install-path".to_owned(),
            self.ebpf_ctl_install_path.as_str().to_owned(),
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
        if let Some(node_public_ip) = self.node_public_ip {
            args.extend(["--node-public-ip".to_owned(), node_public_ip.to_string()]);
        }
        if let Some(machine_bootstrap_url) = &self.machine_bootstrap_url {
            args.extend([
                "--machine-bootstrap-url".to_owned(),
                machine_bootstrap_url.as_str().to_owned(),
            ]);
        }
        if let Some(machine_join_template_file) = &self.machine_join_template_file {
            args.extend([
                "--machine-join-template-file".to_owned(),
                machine_join_template_file.as_str().to_owned(),
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
    pub material: MachineJoinMaterial,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineJoinMaterial {
    pub cluster_name: MachineJoinClusterName,
    pub runtime_nats_url: MachineJoinRuntimeNatsUrl,
    pub trusted_nats: MachineJoinTrustedNats,
    pub core_iroh: MachineJoinCoreIrohEndpoint,
    pub ployzd: MachineJoinPloyzdArtifact,
    pub ebpf_bytecode: MachineJoinArtifact,
    pub ebpf_ctl: MachineJoinArtifact,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineJoinSecretDelivery {
    pub nats_credentials: MachineJoinNatsCredentials,
    pub core_iroh_ticket: MachineJoinIrohTicket,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineJoinTemplate {
    pub join_bundle: MachineJoinBundle,
    pub secret_delivery: MachineJoinSecretDelivery,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineJoinTrustedNats {
    pub server_id: MachineJoinTrustedNatsServerId,
    pub config_sha256: InstallSha256Digest,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineJoinCoreIrohEndpoint {
    pub node_id: NodeId,
    pub public_key: MachineJoinIrohPublicKey,
    pub direct_addresses: Vec<MachineJoinIrohDirectAddress>,
    pub relay_url: Option<MachineJoinIrohRelayUrl>,
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
#[serde(deny_unknown_fields)]
pub struct MachineJoinArtifact {
    pub version: InstallArtifactVersion,
    pub source: InstallArtifactSource,
    pub sha256: InstallSha256Digest,
    pub install_path: AbsoluteInstallPath,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct MachineJoinNatsCredentials(String);

impl MachineJoinNatsCredentials {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InstallContractError> {
        let value = value.into();
        if value.is_empty() {
            return Err(InstallContractError::EmptyNatsCredentials);
        }
        if value.contains('\0') {
            return Err(InstallContractError::InvalidNatsCredentials);
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn secret(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub const fn redacted(&self) -> &'static str {
        "[redacted]"
    }
}

impl TryFrom<String> for MachineJoinNatsCredentials {
    type Error = InstallContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<MachineJoinNatsCredentials> for String {
    fn from(value: MachineJoinNatsCredentials) -> Self {
        value.0
    }
}

impl fmt::Debug for MachineJoinNatsCredentials {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.redacted())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct MachineJoinTrustedNatsServerId(String);

impl MachineJoinTrustedNatsServerId {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InstallContractError> {
        let value = value.into();
        validate_plain_join_token("trusted NATS server id", value, |value| {
            InstallContractError::InvalidTrustedNatsServerId { value }
        })
        .map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MachineJoinTrustedNatsServerId {
    type Error = InstallContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<MachineJoinTrustedNatsServerId> for String {
    fn from(value: MachineJoinTrustedNatsServerId) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct MachineJoinIrohPublicKey(String);

impl MachineJoinIrohPublicKey {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InstallContractError> {
        let value = value.into();
        validate_plain_join_token("core iroh public key", value, |value| {
            InstallContractError::InvalidIrohPublicKey { value }
        })
        .map(Self)
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MachineJoinIrohPublicKey {
    type Error = InstallContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<MachineJoinIrohPublicKey> for String {
    fn from(value: MachineJoinIrohPublicKey) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct MachineJoinIrohDirectAddress(String);

impl MachineJoinIrohDirectAddress {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InstallContractError> {
        let value = value.into();
        if value.is_empty() {
            return Err(InstallContractError::EmptyJoinToken {
                label: "core iroh direct address",
            });
        }
        value.parse::<SocketAddr>().map_err(|_| {
            InstallContractError::InvalidIrohDirectAddress {
                value: value.clone(),
            }
        })?;
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MachineJoinIrohDirectAddress {
    type Error = InstallContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<MachineJoinIrohDirectAddress> for String {
    fn from(value: MachineJoinIrohDirectAddress) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct MachineJoinIrohRelayUrl(String);

impl MachineJoinIrohRelayUrl {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InstallContractError> {
        let value = value.into();
        if value.is_empty() {
            return Err(InstallContractError::EmptyJoinToken {
                label: "core iroh relay URL",
            });
        }
        if !value.starts_with("https://")
            || value
                .chars()
                .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(InstallContractError::InvalidIrohRelayUrl { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl TryFrom<String> for MachineJoinIrohRelayUrl {
    type Error = InstallContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<MachineJoinIrohRelayUrl> for String {
    fn from(value: MachineJoinIrohRelayUrl) -> Self {
        value.0
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct MachineJoinIrohTicket(String);

impl MachineJoinIrohTicket {
    pub fn try_new(value: impl Into<String>) -> Result<Self, InstallContractError> {
        let value = value.into();
        validate_plain_join_token("core iroh ticket", value, |value| {
            InstallContractError::InvalidIrohTicket { value }
        })
        .map(Self)
    }

    #[must_use]
    pub fn secret(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub const fn redacted(&self) -> &'static str {
        "[redacted]"
    }
}

impl TryFrom<String> for MachineJoinIrohTicket {
    type Error = InstallContractError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<MachineJoinIrohTicket> for String {
    fn from(value: MachineJoinIrohTicket) -> Self {
        value.0
    }
}

impl fmt::Debug for MachineJoinIrohTicket {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.redacted())
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
            || nats_url_socket_addr(&value).is_none()
        {
            return Err(InstallContractError::InvalidRuntimeNatsUrl { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn socket_addr(&self) -> SocketAddr {
        nats_url_socket_addr(&self.0).expect("runtime NATS URL is validated as a socket URL")
    }
}

fn nats_url_socket_addr(value: &str) -> Option<SocketAddr> {
    value.strip_prefix("nats://")?.parse().ok()
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
    EmptyNatsCredentials,
    InvalidNatsCredentials,
    EmptyJoinToken { label: &'static str },
    InvalidTrustedNatsServerId { value: String },
    InvalidIrohPublicKey { value: String },
    InvalidIrohDirectAddress { value: String },
    InvalidIrohRelayUrl { value: String },
    InvalidIrohTicket { value: String },
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
                "runtime NATS URL {value:?} must be a nats:// URL without whitespace"
            ),
            Self::EmptyNatsCredentials => formatter.write_str("NATS credentials are empty"),
            Self::InvalidNatsCredentials => {
                formatter.write_str("NATS credentials contain an unsupported NUL byte")
            }
            Self::EmptyJoinToken { label } => write!(formatter, "{label} is empty"),
            Self::InvalidTrustedNatsServerId { value } => write!(
                formatter,
                "trusted NATS server id {value:?} must not contain whitespace"
            ),
            Self::InvalidIrohPublicKey { value } => write!(
                formatter,
                "core iroh public key {value:?} must not contain whitespace"
            ),
            Self::InvalidIrohDirectAddress { value } => write!(
                formatter,
                "core iroh direct address {value:?} must be a socket address"
            ),
            Self::InvalidIrohRelayUrl { value } => write!(
                formatter,
                "core iroh relay URL {value:?} must be an HTTPS URL without whitespace"
            ),
            Self::InvalidIrohTicket { value } => write!(
                formatter,
                "core iroh ticket {value:?} must not contain whitespace"
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

fn validate_plain_join_token(
    label: &'static str,
    value: String,
    invalid: impl FnOnce(String) -> InstallContractError,
) -> Result<String, InstallContractError> {
    if value.is_empty() {
        return Err(InstallContractError::EmptyJoinToken { label });
    }
    if value
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
    {
        return Err(invalid(value));
    }
    Ok(value)
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}
