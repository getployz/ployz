//! Process roles for the shared `ployzd` runtime artifact.

use serde::{Deserialize, Serialize};

use crate::ids::MachineId;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DaemonProcessRole {
    Control,
    Machine(MachineId),
    Gateway,
    Dns,
}

impl DaemonProcessRole {
    #[must_use]
    pub const fn process_name(&self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Machine(_) => "machine",
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
            Self::Machine(machine_id) => vec![
                "machine".to_owned(),
                "--id".to_owned(),
                machine_id.as_str().to_owned(),
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
pub struct FirstMachineProcessSet {
    pub nats_server: FirstMachineNatsServer,
    roles: Vec<DaemonProcessRole>,
}

impl FirstMachineProcessSet {
    #[must_use]
    pub fn roles(&self) -> &[DaemonProcessRole] {
        &self.roles
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinedMachineProcessSet {
    roles: Vec<DaemonProcessRole>,
}

impl JoinedMachineProcessSet {
    #[must_use]
    pub fn roles(&self) -> &[DaemonProcessRole] {
        &self.roles
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FirstMachineNatsServer {
    Supervised,
}

impl FirstMachineNatsServer {
    #[must_use]
    pub const fn process_name(self) -> &'static str {
        match self {
            Self::Supervised => "nats-server",
        }
    }
}

#[must_use]
pub fn plan_first_machine_process_set(
    machine_id: &MachineId,
    roles: InstallRolePolicy,
) -> FirstMachineProcessSet {
    let mut planned = vec![
        DaemonProcessRole::Control,
        DaemonProcessRole::Machine(machine_id.clone()),
    ];
    planned.extend(optional_roles(roles));
    FirstMachineProcessSet {
        nats_server: FirstMachineNatsServer::Supervised,
        roles: planned,
    }
}

#[must_use]
pub fn plan_joined_machine_process_set(
    machine_id: &MachineId,
    roles: InstallRolePolicy,
) -> JoinedMachineProcessSet {
    let mut planned = vec![DaemonProcessRole::Machine(machine_id.clone())];
    planned.extend(optional_roles(roles));
    JoinedMachineProcessSet { roles: planned }
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
        DaemonProcessRole, FirstMachineNatsServer, InstallRolePolicy,
        plan_first_machine_process_set, plan_joined_machine_process_set,
    };
    use crate::ids::MachineId;

    #[test]
    fn first_machine_default_roles_include_gateway_and_dns() {
        let process_set = plan_first_machine_process_set(
            &machine_id("machine_1"),
            InstallRolePolicy::install_all(),
        );
        assert_eq!(process_set.nats_server, FirstMachineNatsServer::Supervised);
        assert_eq!(
            process_set.roles(),
            &[
                DaemonProcessRole::Control,
                DaemonProcessRole::Machine(machine_id("machine_1")),
                DaemonProcessRole::Gateway,
                DaemonProcessRole::Dns,
            ]
        );
    }

    #[test]
    fn joined_machine_default_roles_include_gateway_and_dns() {
        assert_eq!(
            plan_joined_machine_process_set(
                &machine_id("machine_2"),
                InstallRolePolicy::install_all()
            )
            .roles(),
            &[
                DaemonProcessRole::Machine(machine_id("machine_2")),
                DaemonProcessRole::Gateway,
                DaemonProcessRole::Dns,
            ]
        );
    }

    #[test]
    fn no_gateway_opt_out_skips_only_the_gateway_role() {
        assert_eq!(
            plan_first_machine_process_set(
                &machine_id("machine_1"),
                InstallRolePolicy::install_all().without_gateway()
            )
            .roles(),
            &[
                DaemonProcessRole::Control,
                DaemonProcessRole::Machine(machine_id("machine_1")),
                DaemonProcessRole::Dns,
            ]
        );
        assert_eq!(
            plan_joined_machine_process_set(
                &machine_id("machine_2"),
                InstallRolePolicy::install_all().without_gateway()
            )
            .roles(),
            &[
                DaemonProcessRole::Machine(machine_id("machine_2")),
                DaemonProcessRole::Dns,
            ]
        );
    }

    #[test]
    fn no_dns_opt_out_skips_only_the_dns_role() {
        assert_eq!(
            plan_first_machine_process_set(
                &machine_id("machine_1"),
                InstallRolePolicy::install_all().without_dns()
            )
            .roles(),
            &[
                DaemonProcessRole::Control,
                DaemonProcessRole::Machine(machine_id("machine_1")),
                DaemonProcessRole::Gateway,
            ]
        );
        assert_eq!(
            plan_joined_machine_process_set(
                &machine_id("machine_2"),
                InstallRolePolicy::install_all().without_dns()
            )
            .roles(),
            &[
                DaemonProcessRole::Machine(machine_id("machine_2")),
                DaemonProcessRole::Gateway,
            ]
        );
    }

    #[test]
    fn both_opt_outs_leave_the_required_roles() {
        assert_eq!(
            plan_first_machine_process_set(
                &machine_id("machine_1"),
                InstallRolePolicy::install_all()
                    .without_gateway()
                    .without_dns()
            )
            .roles(),
            &[
                DaemonProcessRole::Control,
                DaemonProcessRole::Machine(machine_id("machine_1")),
            ]
        );
        assert_eq!(
            plan_joined_machine_process_set(
                &machine_id("machine_2"),
                InstallRolePolicy::install_all()
                    .without_gateway()
                    .without_dns()
            )
            .roles(),
            &[DaemonProcessRole::Machine(machine_id("machine_2"))]
        );
    }

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }
}
