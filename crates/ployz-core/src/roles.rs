//! Process roles for the shared `ployzd` runtime artifact.

use serde::{Deserialize, Serialize};

use crate::ids::NodeId;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DaemonProcessRole {
    Control,
    Node(NodeId),
    Gateway,
    Dns,
}

impl DaemonProcessRole {
    #[must_use]
    pub const fn process_name(&self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Node(_) => "node",
            Self::Gateway => "gateway",
            Self::Dns => "dns",
        }
    }

    /// The `ployzd` process arguments that select this role.
    ///
    /// This is the single owner of the role argv contract: supervisor unit
    /// rendering emits exactly this shape and `ployzd`'s role parser must
    /// accept it.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        match self {
            Self::Control => vec!["control".to_owned()],
            Self::Node(node_id) => vec![
                "node".to_owned(),
                "--id".to_owned(),
                node_id.as_str().to_owned(),
            ],
            Self::Gateway => vec!["gateway".to_owned()],
            Self::Dns => vec!["dns".to_owned()],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum GatewayRole {
    Install,
    Skip,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DnsRole {
    Install,
    Skip,
}

/// Which optional roles an installed machine runs next to its required
/// processes. The alpha default installs everything (`install_all`);
/// skipping a role is an explicit opt-out (`--no-gateway` / `--no-dns`
/// shaped).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct InstallRolePolicy {
    pub gateway: GatewayRole,
    pub dns: DnsRole,
}

impl InstallRolePolicy {
    /// The default alpha machine shape: gateway and DNS roles installed.
    #[must_use]
    pub const fn install_all() -> Self {
        Self {
            gateway: GatewayRole::Install,
            dns: DnsRole::Install,
        }
    }

    /// Explicit `--no-gateway` opt-out.
    #[must_use]
    pub const fn without_gateway(mut self) -> Self {
        self.gateway = GatewayRole::Skip;
        self
    }

    /// Explicit `--no-dns` opt-out.
    #[must_use]
    pub const fn without_dns(mut self) -> Self {
        self.dns = DnsRole::Skip;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirstNodeProcessSet {
    pub nats_server: FirstNodeNatsServer,
    roles: Vec<DaemonProcessRole>,
}

impl FirstNodeProcessSet {
    #[must_use]
    pub fn roles(&self) -> &[DaemonProcessRole] {
        &self.roles
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinedNodeProcessSet {
    roles: Vec<DaemonProcessRole>,
}

impl JoinedNodeProcessSet {
    #[must_use]
    pub fn roles(&self) -> &[DaemonProcessRole] {
        &self.roles
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstNodeNatsServer {
    Supervised,
}

impl FirstNodeNatsServer {
    #[must_use]
    pub const fn process_name(self) -> &'static str {
        match self {
            Self::Supervised => "nats-server",
        }
    }
}

#[must_use]
pub fn plan_first_node_process_set(
    node_id: &NodeId,
    roles: InstallRolePolicy,
) -> FirstNodeProcessSet {
    let mut planned = vec![
        DaemonProcessRole::Control,
        DaemonProcessRole::Node(node_id.clone()),
    ];
    planned.extend(optional_roles(roles));
    FirstNodeProcessSet {
        nats_server: FirstNodeNatsServer::Supervised,
        roles: planned,
    }
}

#[must_use]
pub fn plan_joined_node_process_set(
    node_id: &NodeId,
    roles: InstallRolePolicy,
) -> JoinedNodeProcessSet {
    let mut planned = vec![DaemonProcessRole::Node(node_id.clone())];
    planned.extend(optional_roles(roles));
    JoinedNodeProcessSet { roles: planned }
}

fn optional_roles(roles: InstallRolePolicy) -> Vec<DaemonProcessRole> {
    let InstallRolePolicy { gateway, dns } = roles;
    let mut planned = Vec::new();
    match gateway {
        GatewayRole::Install => planned.push(DaemonProcessRole::Gateway),
        GatewayRole::Skip => {}
    }
    match dns {
        DnsRole::Install => planned.push(DaemonProcessRole::Dns),
        DnsRole::Skip => {}
    }
    planned
}

#[cfg(test)]
mod tests {
    use super::{
        DaemonProcessRole, FirstNodeNatsServer, InstallRolePolicy, plan_first_node_process_set,
        plan_joined_node_process_set,
    };
    use crate::ids::NodeId;

    #[test]
    fn first_node_default_roles_include_gateway_and_dns() {
        let process_set =
            plan_first_node_process_set(&node_id("node_1"), InstallRolePolicy::install_all());
        assert_eq!(process_set.nats_server, FirstNodeNatsServer::Supervised);
        assert_eq!(
            process_set.roles(),
            &[
                DaemonProcessRole::Control,
                DaemonProcessRole::Node(node_id("node_1")),
                DaemonProcessRole::Gateway,
                DaemonProcessRole::Dns,
            ]
        );
    }

    #[test]
    fn joined_node_default_roles_include_gateway_and_dns() {
        assert_eq!(
            plan_joined_node_process_set(&node_id("node_2"), InstallRolePolicy::install_all())
                .roles(),
            &[
                DaemonProcessRole::Node(node_id("node_2")),
                DaemonProcessRole::Gateway,
                DaemonProcessRole::Dns,
            ]
        );
    }

    #[test]
    fn no_gateway_opt_out_skips_only_the_gateway_role() {
        assert_eq!(
            plan_first_node_process_set(
                &node_id("node_1"),
                InstallRolePolicy::install_all().without_gateway()
            )
            .roles(),
            &[
                DaemonProcessRole::Control,
                DaemonProcessRole::Node(node_id("node_1")),
                DaemonProcessRole::Dns,
            ]
        );
        assert_eq!(
            plan_joined_node_process_set(
                &node_id("node_2"),
                InstallRolePolicy::install_all().without_gateway()
            )
            .roles(),
            &[
                DaemonProcessRole::Node(node_id("node_2")),
                DaemonProcessRole::Dns,
            ]
        );
    }

    #[test]
    fn no_dns_opt_out_skips_only_the_dns_role() {
        assert_eq!(
            plan_first_node_process_set(
                &node_id("node_1"),
                InstallRolePolicy::install_all().without_dns()
            )
            .roles(),
            &[
                DaemonProcessRole::Control,
                DaemonProcessRole::Node(node_id("node_1")),
                DaemonProcessRole::Gateway,
            ]
        );
        assert_eq!(
            plan_joined_node_process_set(
                &node_id("node_2"),
                InstallRolePolicy::install_all().without_dns()
            )
            .roles(),
            &[
                DaemonProcessRole::Node(node_id("node_2")),
                DaemonProcessRole::Gateway,
            ]
        );
    }

    #[test]
    fn both_opt_outs_leave_the_required_roles() {
        assert_eq!(
            plan_first_node_process_set(
                &node_id("node_1"),
                InstallRolePolicy::install_all()
                    .without_gateway()
                    .without_dns()
            )
            .roles(),
            &[
                DaemonProcessRole::Control,
                DaemonProcessRole::Node(node_id("node_1")),
            ]
        );
        assert_eq!(
            plan_joined_node_process_set(
                &node_id("node_2"),
                InstallRolePolicy::install_all()
                    .without_gateway()
                    .without_dns()
            )
            .roles(),
            &[DaemonProcessRole::Node(node_id("node_2"))]
        );
    }

    fn node_id(value: &str) -> NodeId {
        NodeId::try_new(value).expect("valid node id")
    }
}
