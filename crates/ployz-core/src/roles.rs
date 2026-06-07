//! Process roles for the shared `ployzd` runtime artifact.

use serde::{Deserialize, Serialize};
use std::{error::Error, fmt};

use crate::ids::NodeId;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum DaemonProcessRole {
    Control,
    Node(NodeId),
    Gateway,
    Dns,
    Tunnel(TunnelSide),
}

impl DaemonProcessRole {
    #[must_use]
    pub const fn process_name(&self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Node(_) => "node",
            Self::Gateway => "gateway",
            Self::Dns => "dns",
            Self::Tunnel(TunnelSide::Edge) => "tunnel-edge",
            Self::Tunnel(TunnelSide::Core) => "tunnel-core",
        }
    }

    #[must_use]
    pub fn command_args(&self) -> Vec<String> {
        match self {
            Self::Control => vec!["control".to_owned()],
            Self::Node(node_id) => vec![
                "node".to_owned(),
                "--id".to_owned(),
                node_id.as_str().to_owned(),
            ],
            Self::Gateway => vec!["gateway".to_owned()],
            Self::Dns => vec!["dns".to_owned()],
            Self::Tunnel(side) => vec![
                "tunnel".to_owned(),
                "--side".to_owned(),
                side.as_str().to_owned(),
            ],
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TunnelSide {
    Edge,
    Core,
}

impl TunnelSide {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Edge => "edge",
            Self::Core => "core",
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
        DaemonProcessRole::Tunnel(TunnelSide::Core),
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
    let mut roles = vec![
        DaemonProcessRole::Tunnel(TunnelSide::Edge),
        DaemonProcessRole::Node(node_id.clone()),
    ];
    if gateway == FirstNodeGateway::Install {
        roles.push(DaemonProcessRole::Gateway);
    }
    JoinedNodeProcessSet { roles }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DaemonRoleParseError {
    MissingRole,
    UnknownRole(String),
    MissingNodeId,
    InvalidNodeId(String),
    MissingTunnelSide,
    UnknownTunnelSide(String),
    UnexpectedArguments(Vec<String>),
}

impl fmt::Display for DaemonRoleParseError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRole => write!(formatter, "missing ployzd role"),
            Self::UnknownRole(role) => write!(formatter, "unknown ployzd role: {role}"),
            Self::MissingNodeId => write!(formatter, "missing node id for ployzd node role"),
            Self::InvalidNodeId(node_id) => write!(formatter, "invalid node id: {node_id}"),
            Self::MissingTunnelSide => write!(formatter, "missing tunnel side"),
            Self::UnknownTunnelSide(side) => write!(formatter, "unknown tunnel side: {side}"),
            Self::UnexpectedArguments(arguments) => {
                write!(
                    formatter,
                    "unexpected ployzd arguments: {}",
                    arguments.join(" ")
                )
            }
        }
    }
}

impl Error for DaemonRoleParseError {}

pub fn parse_role_args(
    args: impl IntoIterator<Item = String>,
) -> Result<DaemonProcessRole, DaemonRoleParseError> {
    let args = args.into_iter().collect::<Vec<_>>();
    match args.as_slice() {
        [] => Err(DaemonRoleParseError::MissingRole),
        [role] if role == "control" => Ok(DaemonProcessRole::Control),
        [role] if role == "gateway" => Ok(DaemonProcessRole::Gateway),
        [role] if role == "dns" => Ok(DaemonProcessRole::Dns),
        [role, flag, value] if role == "node" && flag == "--id" => {
            let node_id = NodeId::try_new(value.clone())
                .map_err(|_error| DaemonRoleParseError::InvalidNodeId(value.clone()))?;
            Ok(DaemonProcessRole::Node(node_id))
        }
        [role, flag] if role == "node" && flag == "--id" => {
            Err(DaemonRoleParseError::MissingNodeId)
        }
        [role, flag, value] if role == "tunnel" && flag == "--side" => {
            parse_tunnel_side(value).map(DaemonProcessRole::Tunnel)
        }
        [role, flag] if role == "tunnel" && flag == "--side" => {
            Err(DaemonRoleParseError::MissingTunnelSide)
        }
        [role, ..] if is_known_role(role) => Err(DaemonRoleParseError::UnexpectedArguments(args)),
        [role, ..] => Err(DaemonRoleParseError::UnknownRole(role.clone())),
    }
}

fn parse_tunnel_side(value: &str) -> Result<TunnelSide, DaemonRoleParseError> {
    match value {
        "edge" => Ok(TunnelSide::Edge),
        "core" => Ok(TunnelSide::Core),
        side => Err(DaemonRoleParseError::UnknownTunnelSide(side.to_owned())),
    }
}

fn is_known_role(value: &str) -> bool {
    matches!(value, "control" | "node" | "gateway" | "dns" | "tunnel")
}

#[cfg(test)]
mod tests {
    use super::{
        DaemonProcessRole, DaemonRoleParseError, FirstNodeGateway, FirstNodeNatsServer, TunnelSide,
        first_node_process_set, joined_node_process_set, parse_role_args,
    };
    use crate::ids::NodeId;

    #[test]
    fn parses_control_role() {
        assert_eq!(
            parse_role_args(["control"].map(str::to_owned)),
            Ok(DaemonProcessRole::Control)
        );
    }

    #[test]
    fn parses_node_role_with_node_id() {
        assert_eq!(
            parse_role_args(["node", "--id", "node_7"].map(str::to_owned)),
            Ok(DaemonProcessRole::Node(node_id("node_7")))
        );
    }

    #[test]
    fn parses_tunnel_side_without_subtle_defaults() {
        assert_eq!(
            parse_role_args(["tunnel", "--side", "edge"].map(str::to_owned)),
            Ok(DaemonProcessRole::Tunnel(TunnelSide::Edge))
        );
        assert_eq!(
            parse_role_args(["tunnel", "--side", "core"].map(str::to_owned)),
            Ok(DaemonProcessRole::Tunnel(TunnelSide::Core))
        );
    }

    #[test]
    fn command_args_round_trip_through_parser() {
        let roles = [
            DaemonProcessRole::Control,
            DaemonProcessRole::Node(node_id("node_7")),
            DaemonProcessRole::Gateway,
            DaemonProcessRole::Dns,
            DaemonProcessRole::Tunnel(TunnelSide::Edge),
            DaemonProcessRole::Tunnel(TunnelSide::Core),
        ];

        for role in roles {
            assert_eq!(parse_role_args(role.command_args()), Ok(role));
        }
    }

    #[test]
    fn missing_role_is_an_error() {
        assert_eq!(parse_role_args([]), Err(DaemonRoleParseError::MissingRole));
    }

    #[test]
    fn first_node_roles_are_the_product_install_shape() {
        let without_gateway = first_node_process_set(&node_id("node_1"), FirstNodeGateway::Skip);
        assert_eq!(without_gateway.nats_server, FirstNodeNatsServer::Supervised);
        assert_eq!(
            without_gateway.roles(),
            &[
                DaemonProcessRole::Tunnel(TunnelSide::Core),
                DaemonProcessRole::Control,
                DaemonProcessRole::Node(node_id("node_1")),
            ]
        );
        let with_gateway = first_node_process_set(&node_id("node_1"), FirstNodeGateway::Install);
        assert_eq!(
            with_gateway.roles(),
            &[
                DaemonProcessRole::Tunnel(TunnelSide::Core),
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
            &[
                DaemonProcessRole::Tunnel(TunnelSide::Edge),
                DaemonProcessRole::Node(node_id("node_2")),
            ]
        );
        assert_eq!(
            joined_node_process_set(&node_id("node_2"), FirstNodeGateway::Install).roles(),
            &[
                DaemonProcessRole::Tunnel(TunnelSide::Edge),
                DaemonProcessRole::Node(node_id("node_2")),
                DaemonProcessRole::Gateway,
            ]
        );
    }

    fn node_id(value: &str) -> NodeId {
        NodeId::try_new(value).expect("valid node id")
    }
}
