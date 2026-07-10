use std::net::IpAddr;
use std::path::{Path, PathBuf};

use crate::dataplane::MachineEndpointSupernet;
use crate::ids::MachineId;
use crate::nats_config::{NatsCaCertificatePem, NatsUserSeed, is_valid_host_syntax};
use crate::roles::{DaemonProcessRole, GatewayRole, InstallRolePolicy};
use serde::{Deserialize, Serialize};
use std::fmt;
use url::{Host, Url};

pub const DEFAULT_MACHINE_BOOTSTRAP_URL: &str = "https://ployz.sh";

/// The machine-local intent mirror's file name, persisted beside the machine's
/// NKey seed file (ADR 0031). Shared by `ployzd` (which writes and reads the
/// mirror) and `ployz host` (which reads it for core promotion) so both
/// always derive the same path.
pub const INTENT_MIRROR_FILE_NAME: &str = "intent-mirror.json";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FirstMachineInstallSpec {
    pub machine_id: MachineId,
    #[serde(default = "MachineEndpointSupernet::default_v1")]
    pub dataplane_endpoint_supernet: MachineEndpointSupernet,
    pub gateway: GatewayRole,
    pub machine_public_ip: Option<IpAddr>,
    pub machine_bootstrap_url: Option<MachineBootstrapUrl>,
    pub machine_join_template_file: Option<AbsoluteInstallPath>,
    pub machine_join_cluster_name: MachineJoinClusterName,
    pub machine_join_runtime_nats_url: MachineJoinRuntimeNatsUrl,
    pub artifacts: FirstMachineInstallArtifacts,
}

impl FirstMachineInstallSpec {
    /// The optional-role policy carried by this spec.
    #[must_use]
    pub const fn role_policy(&self) -> InstallRolePolicy {
        InstallRolePolicy {
            gateway: self.gateway,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FirstMachineInstallArtifacts {
    pub ployzd: InstallArtifactSpec,
    pub ebpf_bytecode: InstallArtifactSpec,
    pub ebpf_ctl: InstallArtifactSpec,
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
        let Ok(url) = Url::parse(&value) else {
            return Err(InstallContractError::InvalidBootstrapUrl { value });
        };
        if url.scheme() != "https" || url.host().is_none() || has_invisible_characters(&value) {
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

/// The cluster CA signing key encrypted with the operator recovery secret
/// (ADR 0031). Ciphertext, not a secret — safe to deliver broadly; only the
/// recovery-secret holder can decrypt it at promote time.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(transparent)]
pub struct WrappedCaKey(Vec<u8>);

impl WrappedCaKey {
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for WrappedCaKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "WrappedCaKey({} bytes)", self.0.len())
    }
}

/// The core's controller/operator/join principal seeds encrypted with the operator
/// recovery secret (ADR 0031). Ciphertext, not a secret — pre-positioned on every
/// candidate so promotion reuses the old core's principals verbatim instead of
/// rotating them (which would lock out the operator and Cloud). Only the
/// recovery-secret holder can decrypt it at promote time.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(transparent)]
pub struct WrappedCoreSeeds(Vec<u8>);

impl WrappedCoreSeeds {
    #[must_use]
    pub fn new(bytes: Vec<u8>) -> Self {
        Self(bytes)
    }

    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.0
    }
}

impl fmt::Debug for WrappedCoreSeeds {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "WrappedCoreSeeds({} bytes)", self.0.len())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineJoinMaterial {
    pub cluster_name: MachineJoinClusterName,
    #[serde(default = "MachineEndpointSupernet::default_v1")]
    pub dataplane_endpoint_supernet: MachineEndpointSupernet,
    pub runtime_nats_url: MachineJoinRuntimeNatsUrl,
    pub trusted_nats: MachineJoinTrustedNats,
    /// The cluster CA signing key wrapped with the recovery secret (ADR 0031),
    /// delivered so a joined promotion candidate can decrypt it and self-issue the
    /// trusted NATS server cert.
    pub recovery_key_wrapped: WrappedCaKey,
    /// The core's controller/operator/join principal seeds wrapped with the recovery
    /// secret (ADR 0031), delivered so a promoted core reuses them verbatim rather
    /// than rotating (which would lock out the operator and Cloud).
    pub core_seeds_wrapped: WrappedCoreSeeds,
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
/// This is the single owner of the Phase B file-ownership table: Host Runner
/// writes the TLS material and the controller/operator/join seeds at
/// install; `ployzd` control writes `machine.seed` at activate-first-machine.
/// `machine.seed` deliberately does not exist at install time — machine and
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

    /// The CA signing key, wrapped with the operator recovery secret (ADR 0031).
    /// Pre-positioned so a promotion can decrypt it and self-issue a server cert.
    #[must_use]
    pub fn recovery_key_file(&self) -> PathBuf {
        self.state_dir.join("ca-recovery.key")
    }

    /// The core principal seeds, wrapped with the operator recovery secret (ADR 0031).
    /// Persisted beside the recovery key so a promotion can reuse them and so a later
    /// `init join-template` can read them.
    #[must_use]
    pub fn core_seeds_file(&self) -> PathBuf {
        self.state_dir.join("core-seeds.key")
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

    /// Written by `ployzd` control at activate-first-machine, never by Host Runner.
    #[must_use]
    pub fn machine_seed_file(&self) -> PathBuf {
        self.state_dir.join("machine.seed")
    }

    /// The seed file each daemon role authenticates with. Control holds
    /// Controller authority; machine, gateway, and DNS share the machine's
    /// Machine credential (there is no Gateway principal in v1).
    #[must_use]
    pub fn role_seed_file(&self, role: &DaemonProcessRole) -> PathBuf {
        match role {
            DaemonProcessRole::Control => self.controller_seed_file(),
            DaemonProcessRole::Machine(_) | DaemonProcessRole::Gateway | DaemonProcessRole::Dns => {
                self.machine_seed_file()
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
        if has_invisible_characters(&value) || !nats_url_has_host_and_port(&value) {
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
    let Ok(url) = Url::parse(value) else {
        return false;
    };
    if !matches!(url.scheme(), "nats" | "tls")
        || !url.username().is_empty()
        || url.password().is_some()
        || !url.path().is_empty()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return false;
    }
    let Some(host) = url.host() else {
        return false;
    };
    let Some(port) = url.port() else {
        return false;
    };
    port > 0
        && match host {
            Host::Domain(host) => is_valid_host_syntax(host),
            Host::Ipv4(_) | Host::Ipv6(_) => true,
        }
}

fn has_invisible_characters(value: &str) -> bool {
    value
        .chars()
        .any(|character| character.is_whitespace() || character.is_control())
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

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum InstallContractError {
    #[error("cluster name is empty")]
    EmptyClusterName,
    #[error("cluster name {value:?} contains unsupported characters")]
    InvalidClusterName { value: String },
    #[error("machine bootstrap URL is empty")]
    EmptyBootstrapUrl,
    #[error("machine bootstrap URL {value:?} must be an HTTPS URL without whitespace")]
    InvalidBootstrapUrl { value: String },
    #[error("runtime NATS URL is empty")]
    EmptyRuntimeNatsUrl,
    #[error("runtime NATS URL {value:?} must be a nats:// or tls:// URL with host and port")]
    InvalidRuntimeNatsUrl { value: String },
    #[error("artifact version is empty")]
    EmptyArtifactVersion,
    #[error("artifact source is empty")]
    EmptyArtifactSource,
    #[error("artifact source path {value} must be absolute")]
    RelativeArtifactSource { value: String },
    #[error("sha256 digest is empty")]
    EmptySha256Digest,
    #[error("sha256 digest {value:?} must be 64 hex characters")]
    InvalidSha256Digest { value: String },
    #[error("install path is empty")]
    EmptyInstallPath,
    #[error("install path {value} must be absolute")]
    RelativeInstallPath { value: String },
    #[error("install path {value} must include a parent")]
    MissingInstallParent { value: String },
    #[error("install path {value} must include a file name")]
    MissingInstallFileName { value: String },
}
