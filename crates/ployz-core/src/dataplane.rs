//! WireGuard/eBPF preparation models.

use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::net::SocketAddr;

use crate::deploy::DeployPlan;
use crate::ids::{NodeId, OperationId};
use crate::ops::FailureMessage;

pub const DEFAULT_WIREGUARD_LISTEN_PORT: u16 = 51820;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireGuardEbpfPrepareRequest {
    pub operation_id: OperationId,
    pub nodes: Vec<NodeId>,
    pub endpoint_routes: Vec<WireGuardEbpfEndpointRoute>,
    pub peer_endpoints: Vec<WireGuardPeerEndpoint>,
    pub peers: Vec<WireGuardPeer>,
}

impl WireGuardEbpfPrepareRequest {
    #[must_use]
    pub fn for_deploy_plan(
        operation_id: OperationId,
        plan: &DeployPlan,
        dataplane_nodes: &[NodeId],
        peer_endpoints: &[WireGuardPeerEndpoint],
    ) -> Self {
        let nodes = sorted_unique_nodes(
            plan.target_nodes()
                .into_iter()
                .chain(dataplane_nodes.iter().cloned()),
        );
        let requested = nodes.iter().collect::<BTreeSet<_>>();
        let peer_endpoints = peer_endpoints
            .iter()
            .filter(|peer| requested.contains(&peer.node_id))
            .cloned()
            .collect();
        Self::for_nodes(operation_id, nodes, peer_endpoints, Vec::new())
    }

    #[must_use]
    pub fn with_peers(mut self, peers: Vec<WireGuardPeer>) -> Self {
        self.peers = peers;
        self
    }

    #[must_use]
    fn for_nodes(
        operation_id: OperationId,
        nodes: Vec<NodeId>,
        peer_endpoints: Vec<WireGuardPeerEndpoint>,
        peers: Vec<WireGuardPeer>,
    ) -> Self {
        Self {
            operation_id,
            endpoint_routes: nodes
                .iter()
                .map(WireGuardEbpfEndpointRoute::default_for_node)
                .collect(),
            nodes,
            peer_endpoints,
            peers,
        }
    }
}

fn sorted_unique_nodes(nodes: impl IntoIterator<Item = NodeId>) -> Vec<NodeId> {
    nodes
        .into_iter()
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardEbpfEndpointRoute {
    pub node_id: NodeId,
    pub endpoint_subnet: String,
}

impl WireGuardEbpfEndpointRoute {
    #[must_use]
    pub fn default_for_node(node_id: &NodeId) -> Self {
        Self {
            node_id: node_id.clone(),
            endpoint_subnet: default_endpoint_subnet(node_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardPeerEndpoint {
    pub node_id: NodeId,
    pub endpoint_subnet: String,
    pub public_endpoint: SocketAddr,
}

impl WireGuardPeerEndpoint {
    #[must_use]
    pub fn new(node_id: NodeId, public_endpoint: SocketAddr) -> Self {
        Self {
            endpoint_subnet: default_endpoint_subnet(&node_id),
            node_id,
            public_endpoint,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardPeer {
    pub node_id: NodeId,
    pub endpoint_subnet: String,
    pub public_endpoint: SocketAddr,
    pub public_key: WireGuardPublicKey,
}

impl WireGuardPeer {
    #[must_use]
    pub fn from_endpoint(endpoint: WireGuardPeerEndpoint, public_key: WireGuardPublicKey) -> Self {
        Self {
            node_id: endpoint.node_id,
            endpoint_subnet: endpoint.endpoint_subnet,
            public_endpoint: endpoint.public_endpoint,
            public_key,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(feature = "typescript", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct WireGuardPublicKey(String);

impl WireGuardPublicKey {
    pub fn try_new(value: impl Into<String>) -> Result<Self, WireGuardPublicKeyError> {
        let value = value.into();
        if value.is_empty() {
            return Err(WireGuardPublicKeyError::Empty);
        }
        if value
            .chars()
            .any(|character| character.is_whitespace() || character.is_control())
        {
            return Err(WireGuardPublicKeyError::Invalid { value });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for WireGuardPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("WireGuardPublicKey")
            .field(&self.0)
            .finish()
    }
}

impl TryFrom<String> for WireGuardPublicKey {
    type Error = WireGuardPublicKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<WireGuardPublicKey> for String {
    fn from(value: WireGuardPublicKey) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireGuardPublicKeyError {
    Empty,
    Invalid { value: String },
}

impl fmt::Display for WireGuardPublicKeyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("wireguard public key is empty"),
            Self::Invalid { value } => {
                write!(
                    formatter,
                    "wireguard public key {value:?} must not contain whitespace"
                )
            }
        }
    }
}

#[must_use]
pub fn default_endpoint_subnet(node_id: &NodeId) -> String {
    let octet = trailing_node_number(node_id.as_str())
        .map(|number| ((number.saturating_sub(1)) % 254 + 1) as u8)
        .unwrap_or_else(|| stable_node_octet(node_id.as_str()));
    format!("10.42.{octet}.0/24")
}

fn trailing_node_number(value: &str) -> Option<u16> {
    let digits = value
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    if digits.is_empty() {
        return None;
    }
    digits.chars().rev().collect::<String>().parse().ok()
}

fn stable_node_octet(value: &str) -> u8 {
    let hash = value.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    (hash % 254 + 1) as u8
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum WireGuardEbpfComponent {
    #[serde(rename = "wireguard")]
    WireGuard,
    EbpfForwarding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardEbpfPrepareReport {
    pub nodes: Vec<WireGuardEbpfNodeReady>,
}

impl WireGuardEbpfPrepareReport {
    pub fn for_request(
        request: &WireGuardEbpfPrepareRequest,
        nodes: impl IntoIterator<Item = WireGuardEbpfNodeReady>,
    ) -> Result<Self, WireGuardEbpfPrepareReportError> {
        let nodes = nodes.into_iter().collect::<Vec<_>>();
        if request.nodes.is_empty() || nodes.is_empty() {
            return Err(WireGuardEbpfPrepareReportError::Empty);
        }
        let requested = request.nodes.iter().collect::<BTreeSet<_>>();
        if requested.len() != request.nodes.len() {
            return Err(WireGuardEbpfPrepareReportError::DuplicateNode);
        }
        let actual = nodes
            .iter()
            .map(|node| &node.node_id)
            .collect::<BTreeSet<_>>();
        if actual.len() != nodes.len() {
            return Err(WireGuardEbpfPrepareReportError::DuplicateNode);
        }
        if requested != actual {
            return Err(WireGuardEbpfPrepareReportError::NodeSetMismatch);
        }

        Ok(Self { nodes })
    }

    pub fn from_nodes(
        nodes: impl IntoIterator<Item = WireGuardEbpfNodeReady>,
    ) -> Result<Self, WireGuardEbpfPrepareReportError> {
        let nodes = nodes.into_iter().collect::<Vec<_>>();
        if nodes.is_empty() {
            return Err(WireGuardEbpfPrepareReportError::Empty);
        }
        let mut seen = BTreeSet::new();
        if nodes.iter().any(|node| !seen.insert(node.node_id.clone())) {
            return Err(WireGuardEbpfPrepareReportError::DuplicateNode);
        }

        Ok(Self { nodes })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(
    from = "WireGuardEbpfNodeReadyWire",
    into = "WireGuardEbpfNodeReadyWire"
)]
pub struct WireGuardEbpfNodeReady {
    pub node_id: NodeId,
    #[cfg_attr(feature = "typescript", ts(flatten))]
    pub ready: WireGuardEbpfReady,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireGuardEbpfNodeReadyWire {
    node_id: NodeId,
    wireguard: WireGuardReady,
    ebpf_forwarding: EbpfForwardingReady,
}

impl From<WireGuardEbpfNodeReadyWire> for WireGuardEbpfNodeReady {
    fn from(value: WireGuardEbpfNodeReadyWire) -> Self {
        let WireGuardEbpfNodeReadyWire {
            node_id,
            wireguard,
            ebpf_forwarding,
        } = value;
        Self {
            node_id,
            ready: WireGuardEbpfReady {
                wireguard,
                ebpf_forwarding,
            },
        }
    }
}

impl From<WireGuardEbpfNodeReady> for WireGuardEbpfNodeReadyWire {
    fn from(value: WireGuardEbpfNodeReady) -> Self {
        let WireGuardEbpfNodeReady { node_id, ready } = value;
        let WireGuardEbpfReady {
            wireguard,
            ebpf_forwarding,
        } = ready;
        Self {
            node_id,
            wireguard,
            ebpf_forwarding,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardEbpfReady {
    pub wireguard: WireGuardReady,
    pub ebpf_forwarding: EbpfForwardingReady,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardReady {
    pub public_key: WireGuardPublicKey,
    pub evidence: Vec<WireGuardReadyEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct EbpfForwardingReady {
    pub evidence: Vec<EbpfForwardingReadyEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireGuardReadyEvidence {
    HostPath { path: String },
    Command { program: String, args: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EbpfForwardingReadyEvidence {
    HostPath { path: String },
    Command { program: String, args: Vec<String> },
    PloyzTcBytecode { path: String, symbols: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireGuardEbpfPrepareReportError {
    Empty,
    DuplicateNode,
    NodeSetMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireGuardEbpfPrepareError {
    Unavailable {
        node_id: NodeId,
        component: WireGuardEbpfComponent,
        message: FailureMessage,
    },
    InvalidReport {
        message: FailureMessage,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_endpoint_subnet_uses_trailing_node_number() {
        assert_eq!(default_endpoint_subnet(&node_id("edge_2")), "10.42.2.0/24");
        assert_eq!(
            default_endpoint_subnet(&node_id("node_255")),
            "10.42.1.0/24"
        );
    }

    #[test]
    fn deploy_prepare_request_carries_endpoint_routes_for_target_nodes() {
        let plan = DeployPlan {
            service_id: crate::ids::ServiceId::try_new("svc_api").expect("valid service id"),
            target_revision: crate::ids::RevisionId::try_new("rev_1").expect("valid revision id"),
            steps: vec![
                crate::deploy::DeployPlanStep::RunContainer {
                    node_id: node_id("edge_2"),
                    slot: crate::deploy::ReplicaSlot::try_new(1).expect("valid slot"),
                },
                crate::deploy::DeployPlanStep::RunContainer {
                    node_id: node_id("core_1"),
                    slot: crate::deploy::ReplicaSlot::try_new(2).expect("valid slot"),
                },
            ],
            cleanup_containers: Vec::new(),
        };

        let request =
            WireGuardEbpfPrepareRequest::for_deploy_plan(operation_id("op_1"), &plan, &[], &[]);

        assert_eq!(
            request.endpoint_routes,
            vec![
                WireGuardEbpfEndpointRoute {
                    node_id: node_id("core_1"),
                    endpoint_subnet: "10.42.1.0/24".to_owned(),
                },
                WireGuardEbpfEndpointRoute {
                    node_id: node_id("edge_2"),
                    endpoint_subnet: "10.42.2.0/24".to_owned(),
                }
            ]
        );
    }

    fn node_id(value: &str) -> NodeId {
        NodeId::try_new(value).expect("valid node id")
    }

    fn operation_id(value: &str) -> OperationId {
        OperationId::try_new(value).expect("valid operation id")
    }
}
