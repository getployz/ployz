//! Renderable machine bootstrap commands.
//!
//! Bootstrap delivery can be copy/paste, SSH, cloud-init, or a dashboard
//! envelope. This module owns the command string so those delivery paths use
//! the same shell shape.

use ployz_core::ids::MachineId;
use ployz_core::install::{MachineBootstrapUrl, MachineJoinClusterName, MachineJoinRuntimeNatsUrl};
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::roles::{GatewayRole, InstallRolePolicy};
use ployz_sdk_types::{CloudBootstrapToken, MachineJoinToken};
use std::net::IpAddr;

use crate::shell::shell_quote;

/// Release channel the quick start resolves when `--version` is not given.
/// Must match the `default_channel` in `scripts/ployz.sh`.
pub const DEFAULT_RELEASE_CHANNEL: &str = "alpha";
/// The default installer the remote machine pipes through `sh`.
pub const DEFAULT_BOOTSTRAP_URL: &str = ployz_core::install::DEFAULT_MACHINE_BOOTSTRAP_URL;
/// Default cluster name recorded in the machine-join template.
pub const DEFAULT_CLUSTER_NAME: &str = "ployz";
/// The cluster control-plane NATS port on every machine.
pub const MACHINE_NATS_PORT: u16 = 4222;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapInstaller {
    BootstrapUrl(MachineBootstrapUrl),
    RemoteScript(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BootstrapRelease {
    Channel(String),
    Version(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CloudBootstrapMode {
    Interactive,
    Token(CloudBootstrapToken),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CloudBootstrapCommand {
    pub installer: BootstrapInstaller,
    pub cloud_host: Option<String>,
    pub mode: CloudBootstrapMode,
}

impl CloudBootstrapCommand {
    #[must_use]
    pub fn render(&self) -> String {
        let mut bootstrap = String::from("sudo ployz host bootstrap cloud");
        if let Some(host) = &self.cloud_host {
            bootstrap.push_str(&format!(" --cloud-host {}", shell_quote(host)));
        }
        if let CloudBootstrapMode::Token(token) = &self.mode {
            bootstrap.push_str(&format!(" --cloud-token {}", shell_quote(token.secret())));
        }

        match &self.installer {
            BootstrapInstaller::BootstrapUrl(url) => {
                format!(
                    "curl -fsSL -- {} | sh && {bootstrap}",
                    shell_quote(url.as_str())
                )
            }
            BootstrapInstaller::RemoteScript(path) => {
                format!("sh {} && {bootstrap}", shell_quote(path))
            }
        }
    }
}

impl BootstrapRelease {
    #[must_use]
    fn env_pair(&self) -> (&'static str, &str) {
        match self {
            Self::Channel(channel) => ("PLOYZ_CHANNEL", channel.as_str()),
            Self::Version(version) => ("PLOYZ_VERSION", version.as_str()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinBootstrapCommand {
    pub installer: BootstrapInstaller,
    pub version: String,
    pub runtime_nats_url: MachineJoinRuntimeNatsUrl,
    pub trusted_ca_b64: String,
    pub join_seed: NatsUserSeed,
    pub join_token: MachineJoinToken,
}

impl JoinBootstrapCommand {
    #[must_use]
    pub fn render(&self) -> String {
        let env = format!(
            "PLOYZ_VERSION={} PLOYZ_NATS_URL={} PLOYZ_NATS_CA_FILE=\"$ca_file\" PLOYZ_JOIN_NKEY_SEED={}",
            shell_quote(&self.version),
            shell_quote(self.runtime_nats_url.as_str()),
            shell_quote(self.join_seed.secret()),
        );
        let host_runner = format!(
            "ca_file=\"$(mktemp)\"; trap 'rm -f \"$ca_file\"' EXIT; printf '%s' {} | base64 -d > \"$ca_file\"; {env} ployz host bootstrap join {}",
            shell_quote(&self.trusted_ca_b64),
            shell_quote(self.join_token.as_str()),
        );

        match &self.installer {
            BootstrapInstaller::BootstrapUrl(url) => format!(
                "curl -fsSL -- {} | PLOYZ_VERSION={} sh && {host_runner}",
                shell_quote(url.as_str()),
                shell_quote(&self.version),
            ),
            BootstrapInstaller::RemoteScript(path) => format!(
                "PLOYZ_VERSION={} sh {} && {host_runner}",
                shell_quote(&self.version),
                shell_quote(path),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FounderBootstrapCommand {
    pub installer: BootstrapInstaller,
    pub release: BootstrapRelease,
    pub release_manifest_url: Option<String>,
    pub machine_id: MachineId,
    pub roles: InstallRolePolicy,
    pub bootstrap_url: MachineBootstrapUrl,
    pub cluster_name: MachineJoinClusterName,
    pub runtime_nats_url: MachineJoinRuntimeNatsUrl,
    /// The operator's `--public-ip` override. When absent, the first machine
    /// self-discovers its public address at install (the common cloud-VM case).
    pub machine_public_ip: Option<IpAddr>,
}

impl FounderBootstrapCommand {
    #[must_use]
    pub fn render(&self) -> String {
        let (release_key, release_value) = self.release.env_pair();
        let mut env = format!("{release_key}={}", shell_quote(release_value));
        if let Some(url) = &self.release_manifest_url {
            env.push_str(&format!(" PLOYZ_RELEASE_MANIFEST_URL={}", shell_quote(url)));
        }
        env.push_str(&format!(
            " PLOYZ_MACHINE_ID={} PLOYZ_GATEWAY={} PLOYZ_MACHINE_BOOTSTRAP_URL={} PLOYZ_MACHINE_JOIN_CLUSTER_NAME={} PLOYZ_MACHINE_JOIN_NATS_URL={}",
            shell_quote(self.machine_id.as_str()),
            shell_quote(gateway_role_value(self.roles.gateway)),
            shell_quote(self.bootstrap_url.as_str()),
            shell_quote(self.cluster_name.as_str()),
            shell_quote(self.runtime_nats_url.as_str()),
        ));
        if let Some(public_ip) = &self.machine_public_ip {
            env.push_str(&format!(
                " PLOYZ_MACHINE_PUBLIC_IP={}",
                shell_quote(&public_ip.to_string()),
            ));
        }

        match &self.installer {
            BootstrapInstaller::BootstrapUrl(url) => format!(
                "curl -fsSL -- {} | {env} sh && {env} ployz host bootstrap core",
                shell_quote(url.as_str()),
            ),
            BootstrapInstaller::RemoteScript(path) => {
                format!(
                    "{env} sh {} && {env} ployz host bootstrap core",
                    shell_quote(path)
                )
            }
        }
    }
}

const fn gateway_role_value(role: GatewayRole) -> &'static str {
    match role {
        GatewayRole::Install => "install",
        GatewayRole::Skip => "skip",
    }
}
