//! Renderable machine bootstrap commands.
//!
//! Bootstrap delivery can be copy/paste, SSH, cloud-init, or a dashboard
//! envelope. This module owns the command string so those delivery paths use
//! the same shell shape.

use std::net::IpAddr;

use ployz_core::ids::NodeId;
use ployz_core::install::{MachineBootstrapUrl, MachineJoinClusterName, MachineJoinRuntimeNatsUrl};
use ployz_core::nats_config::NatsUserSeed;
use ployz_core::roles::{DnsRole, GatewayRole, InstallRolePolicy};
use ployz_sdk_types::MachineJoinToken;

use crate::shell::shell_quote;

/// Release channel the quick start resolves when `--version` is not given.
/// Must match the `default_channel` in `scripts/ployz.sh`.
pub const DEFAULT_RELEASE_CHANNEL: &str = "alpha";
/// The default installer the remote machine pipes through `sh`.
pub const DEFAULT_BOOTSTRAP_URL: &str = "https://ployz.sh";
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
    pub node_public_ip: Option<IpAddr>,
}

impl JoinBootstrapCommand {
    #[must_use]
    pub fn render(&self) -> String {
        let mut env = String::new();
        if let Some(public_ip) = self.node_public_ip {
            env.push_str(&format!(
                "PLOYZ_NODE_PUBLIC_IP={} ",
                shell_quote(&public_ip.to_string())
            ));
        }
        env.push_str(&format!(
            "PLOYZ_VERSION={} PLOYZ_NATS_URL={} PLOYZ_NATS_CA_B64={} PLOYZ_JOIN_NKEY_SEED={}",
            shell_quote(&self.version),
            shell_quote(self.runtime_nats_url.as_str()),
            shell_quote(&self.trusted_ca_b64),
            shell_quote(self.join_seed.secret()),
        ));

        match &self.installer {
            BootstrapInstaller::BootstrapUrl(url) => format!(
                "curl -fsSL -- {} | {env} sh -s -- --join-token {}",
                shell_quote(url.as_str()),
                shell_quote(self.join_token.as_str()),
            ),
            BootstrapInstaller::RemoteScript(path) => format!(
                "{env} sh {} --join-token {}",
                shell_quote(path),
                shell_quote(self.join_token.as_str()),
            ),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FounderBootstrapCommand {
    pub installer: BootstrapInstaller,
    pub release: BootstrapRelease,
    pub release_manifest_url: Option<String>,
    pub node_id: NodeId,
    pub roles: InstallRolePolicy,
    pub bootstrap_url: MachineBootstrapUrl,
    pub cluster_name: MachineJoinClusterName,
    pub runtime_nats_url: MachineJoinRuntimeNatsUrl,
    pub node_public_ip: Option<IpAddr>,
}

impl FounderBootstrapCommand {
    #[must_use]
    pub fn render(&self) -> String {
        let (release_key, release_value) = self.release.env_pair();
        let mut env = format!("{release_key}={}", shell_quote(release_value));
        if let Some(url) = &self.release_manifest_url {
            env.push_str(&format!(" PLOYZ_RELEASE_MANIFEST_URL={}", shell_quote(url)));
        }
        if let Some(public_ip) = self.node_public_ip {
            env.push_str(&format!(
                " PLOYZ_NODE_PUBLIC_IP={}",
                shell_quote(&public_ip.to_string())
            ));
        }
        env.push_str(&format!(
            " PLOYZ_NODE_ID={} PLOYZ_GATEWAY={} PLOYZ_DNS={} PLOYZ_MACHINE_BOOTSTRAP_URL={} PLOYZ_MACHINE_JOIN_CLUSTER_NAME={} PLOYZ_MACHINE_JOIN_NATS_URL={}",
            shell_quote(self.node_id.as_str()),
            shell_quote(gateway_role_value(self.roles.gateway)),
            shell_quote(dns_role_value(self.roles.dns)),
            shell_quote(self.bootstrap_url.as_str()),
            shell_quote(self.cluster_name.as_str()),
            shell_quote(self.runtime_nats_url.as_str()),
        ));

        match &self.installer {
            BootstrapInstaller::BootstrapUrl(url) => format!(
                "curl -fsSL -- {} | {env} sh -s -- --first-node",
                shell_quote(url.as_str()),
            ),
            BootstrapInstaller::RemoteScript(path) => {
                format!("{env} sh {} --first-node", shell_quote(path))
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

const fn dns_role_value(role: DnsRole) -> &'static str {
    match role {
        DnsRole::Install => "install",
        DnsRole::Skip => "skip",
    }
}
