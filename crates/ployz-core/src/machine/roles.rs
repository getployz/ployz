//! Machine role policy and process-set planning.

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

/// Which optional roles an installed machine runs next to its required
/// processes.
///
/// DNS is required while every accepted machine is workload-eligible. Making
/// DNS optional requires workload-ineligible machine intent and matching DNS
/// scoping in network status, resolve, and repair.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct InstallRolePolicy {
    pub gateway: GatewayRole,
}

impl InstallRolePolicy {
    /// Installs the optional gateway role; DNS is always planned separately.
    #[must_use]
    pub const fn install_all() -> Self {
        Self {
            gateway: GatewayRole::Install,
        }
    }

    /// Explicit `--no-gateway` opt-out.
    #[must_use]
    pub const fn without_gateway(mut self) -> Self {
        self.gateway = GatewayRole::Skip;
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
    planned.extend(machine_roles(roles));
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
    planned.extend(machine_roles(roles));
    JoinedMachineProcessSet { roles: planned }
}

fn machine_roles(roles: InstallRolePolicy) -> Vec<DaemonProcessRole> {
    let InstallRolePolicy { gateway } = roles;
    match gateway {
        GatewayRole::Install => vec![DaemonProcessRole::Gateway, DaemonProcessRole::Dns],
        GatewayRole::Skip => vec![DaemonProcessRole::Dns],
    }
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

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }
}
