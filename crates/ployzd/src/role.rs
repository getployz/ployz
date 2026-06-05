//! Process roles for the shared `ployzd` runtime artifact.

use std::{error::Error, fmt};

use ployz_core::ids::NodeId;

#[derive(Debug, Clone, PartialEq, Eq)]
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    use super::{DaemonProcessRole, DaemonRoleParseError, TunnelSide, parse_role_args};
    use ployz_core::ids::NodeId;

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
    fn missing_role_is_an_error() {
        assert_eq!(parse_role_args([]), Err(DaemonRoleParseError::MissingRole));
    }

    fn node_id(value: &str) -> NodeId {
        NodeId::try_new(value).expect("valid node id")
    }
}
