//! Supervisor unit targets written by keeper.

use ployz_core::roles::{DaemonProcessRole, TunnelSide};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupervisorUnitTarget {
    Keeper,
    PloyzdRole(DaemonProcessRole),
}

impl SupervisorUnitTarget {
    #[must_use]
    pub fn unit_name(&self) -> String {
        match self {
            Self::Keeper => "ployz-keeper.service".to_owned(),
            Self::PloyzdRole(role) => role_unit_name(role),
        }
    }
}

#[must_use]
pub fn role_unit_name(role: &DaemonProcessRole) -> String {
    match role {
        DaemonProcessRole::Control => "ployzd-control.service".to_owned(),
        DaemonProcessRole::Node(node_id) => format!("ployzd-node-{}.service", node_id.as_str()),
        DaemonProcessRole::Gateway => "ployzd-gateway.service".to_owned(),
        DaemonProcessRole::Dns => "ployzd-dns.service".to_owned(),
        DaemonProcessRole::Tunnel(TunnelSide::Edge) => "ployzd-tunnel-edge.service".to_owned(),
        DaemonProcessRole::Tunnel(TunnelSide::Core) => "ployzd-tunnel-core.service".to_owned(),
    }
}
