//! Inputs for installing the first machine and its selected roles.

use std::net::IpAddr;

use serde::{Deserialize, Serialize};

use crate::dataplane::MachineEndpointSupernet;
use crate::ids::MachineId;
use crate::machine::roles::{GatewayRole, InstallRolePolicy};

use super::artifacts::FirstMachineInstallArtifacts;
use super::join::{MachineBootstrapUrl, MachineJoinClusterName};
use super::nats::MachineJoinRuntimeNatsUrl;
use super::paths::AbsoluteInstallPath;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum HostPortAssurance {
    Keeper,
    External,
}

impl HostPortAssurance {
    #[must_use]
    pub const fn keeper() -> Self {
        Self::Keeper
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct FirstMachineInstallSpec {
    pub machine_id: MachineId,
    #[serde(default = "MachineEndpointSupernet::default_v1")]
    pub dataplane_endpoint_supernet: MachineEndpointSupernet,
    pub gateway: GatewayRole,
    #[serde(default = "HostPortAssurance::keeper")]
    pub host_port_assurance: HostPortAssurance,
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
