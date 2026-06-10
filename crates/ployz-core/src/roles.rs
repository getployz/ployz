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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum FirstNodeGateway {
    Install,
    Skip,
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
pub fn first_node_process_set(node_id: &NodeId, gateway: FirstNodeGateway) -> FirstNodeProcessSet {
    let mut roles = vec![
        DaemonProcessRole::Control,
        DaemonProcessRole::Node(node_id.clone()),
    ];
    if gateway == FirstNodeGateway::Install {
        roles.push(DaemonProcessRole::Gateway);
    }
    FirstNodeProcessSet {
        nats_server: FirstNodeNatsServer::Supervised,
        roles,
    }
}

#[must_use]
pub fn joined_node_process_set(
    node_id: &NodeId,
    gateway: FirstNodeGateway,
) -> JoinedNodeProcessSet {
    let mut roles = vec![DaemonProcessRole::Node(node_id.clone())];
    if gateway == FirstNodeGateway::Install {
        roles.push(DaemonProcessRole::Gateway);
    }
    JoinedNodeProcessSet { roles }
}

#[cfg(test)]
mod tests {
    use super::{
        DaemonProcessRole, FirstNodeGateway, FirstNodeNatsServer, first_node_process_set,
        joined_node_process_set,
    };
    use crate::ids::NodeId;

    #[test]
    fn first_node_roles_are_the_product_install_shape() {
        let without_gateway = first_node_process_set(&node_id("node_1"), FirstNodeGateway::Skip);
        assert_eq!(without_gateway.nats_server, FirstNodeNatsServer::Supervised);
        assert_eq!(
            without_gateway.roles(),
            &[
                DaemonProcessRole::Control,
                DaemonProcessRole::Node(node_id("node_1")),
            ]
        );
        let with_gateway = first_node_process_set(&node_id("node_1"), FirstNodeGateway::Install);
        assert_eq!(
            with_gateway.roles(),
            &[
                DaemonProcessRole::Control,
                DaemonProcessRole::Node(node_id("node_1")),
                DaemonProcessRole::Gateway,
            ]
        );
    }

    #[test]
    fn joined_node_roles_are_the_machine_add_shape() {
        assert_eq!(
            joined_node_process_set(&node_id("node_2"), FirstNodeGateway::Skip).roles(),
            &[DaemonProcessRole::Node(node_id("node_2"))]
        );
        assert_eq!(
            joined_node_process_set(&node_id("node_2"), FirstNodeGateway::Install).roles(),
            &[
                DaemonProcessRole::Node(node_id("node_2")),
                DaemonProcessRole::Gateway,
            ]
        );
    }

    fn node_id(value: &str) -> NodeId {
        NodeId::try_new(value).expect("valid node id")
    }
}
