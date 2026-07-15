//! Bootstrap and join material delivered to machines.

use std::fmt;

use serde::{Deserialize, Serialize};
use url::Url;

use crate::nats_config::NatsUserSeed;
use crate::network::MachineEndpointSupernet;

use super::artifacts::InstallArtifactSpec;
use super::nats::{MachineJoinRuntimeNatsUrl, MachineJoinTrustedNats};
use super::validation::{InstallContractError, has_invisible_characters};

pub const DEFAULT_MACHINE_BOOTSTRAP_URL: &str = "https://ployz.sh";

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
