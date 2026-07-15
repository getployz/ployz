//! Trusted NATS material and its well-known machine-local paths.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::machine::roles::DaemonProcessRole;
use crate::nats_config::NatsCaCertificatePem;

use super::validation::{
    InstallContractError, has_invisible_characters, nats_url_has_host_and_port,
};

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
