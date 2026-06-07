//! Core node topology and quorum policy.

use crate::ids::NodeId;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "CoreTopologyWire", into = "CoreTopologyWire")]
pub struct CoreTopology {
    nodes: CanonicalNodeSet,
}

impl CoreTopology {
    pub fn from_nodes(nodes: Vec<NodeId>) -> Result<Self, CoreTopologyError> {
        let nodes = CanonicalNodeSet::from_nodes(nodes)?;

        match nodes.len() {
            0 => Err(CoreTopologyError::Empty),
            1 | 3 => Ok(Self { nodes }),
            2 => Err(CoreTopologyError::TransitionalCoreCount { count: 2 }),
            count => Err(CoreTopologyError::TooMany { count }),
        }
    }

    #[must_use]
    pub fn mode(&self) -> CoreTopologyMode {
        match self.nodes.len() {
            1 => CoreTopologyMode::Single,
            3 => CoreTopologyMode::HighAvailability,
            _ => unreachable!("CoreTopology validates final node count"),
        }
    }

    #[must_use]
    pub fn core_count(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub fn is_high_availability(&self) -> bool {
        self.mode() == CoreTopologyMode::HighAvailability
    }

    #[must_use]
    pub fn nodes(&self) -> &[NodeId] {
        self.nodes.nodes()
    }

    pub fn quorum_status(
        &self,
        available_nodes: Vec<NodeId>,
    ) -> Result<CoreQuorumStatus, CoreAvailabilityError> {
        quorum_status(self.mode().into(), self.nodes(), &available_nodes)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<NodeId>", into = "Vec<NodeId>")]
pub struct CorePromotionTopology {
    nodes: CanonicalNodeSet,
}

impl CorePromotionTopology {
    pub fn from_nodes(nodes: Vec<NodeId>) -> Result<Self, CorePromotionTopologyError> {
        let nodes = CanonicalNodeSet::from_nodes(nodes)?;

        match nodes.len() {
            2 => Ok(Self { nodes }),
            count => Err(CorePromotionTopologyError::WrongNodeCount { count }),
        }
    }

    #[must_use]
    pub fn nodes(&self) -> &[NodeId] {
        self.nodes.nodes()
    }

    pub fn quorum_status(
        &self,
        available_nodes: Vec<NodeId>,
    ) -> Result<CoreQuorumStatus, CoreAvailabilityError> {
        quorum_status(CoreQuorumMode::PromotingTwo, self.nodes(), &available_nodes)
    }
}

impl TryFrom<Vec<NodeId>> for CorePromotionTopology {
    type Error = CorePromotionTopologyError;

    fn try_from(nodes: Vec<NodeId>) -> Result<Self, Self::Error> {
        Self::from_nodes(nodes)
    }
}

impl From<CorePromotionTopology> for Vec<NodeId> {
    fn from(value: CorePromotionTopology) -> Self {
        value.nodes.into()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreTopologyMode {
    Single,
    HighAvailability,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreQuorumMode {
    Single,
    PromotingTwo,
    HighAvailability,
}

impl From<CoreTopologyMode> for CoreQuorumMode {
    fn from(value: CoreTopologyMode) -> Self {
        match value {
            CoreTopologyMode::Single => Self::Single,
            CoreTopologyMode::HighAvailability => Self::HighAvailability,
        }
    }
}

impl CoreQuorumMode {
    const fn required_available_cores(self) -> usize {
        match self {
            Self::Single | Self::PromotingTwo => 1,
            Self::HighAvailability => 2,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreQuorumStatus {
    Available {
        mode: CoreQuorumMode,
        available: usize,
        required: usize,
    },
    Unavailable {
        mode: CoreQuorumMode,
        available: usize,
        required: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreTopologyError {
    Empty,
    DuplicateNode { node_id: NodeId },
    TransitionalCoreCount { count: usize },
    TooMany { count: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorePromotionTopologyError {
    DuplicateNode { node_id: NodeId },
    WrongNodeCount { count: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreAvailabilityError {
    DuplicateAvailableNode { node_id: NodeId },
    UnknownCore { node_id: NodeId },
}

fn quorum_status(
    mode: CoreQuorumMode,
    core_nodes: &[NodeId],
    available_nodes: &[NodeId],
) -> Result<CoreQuorumStatus, CoreAvailabilityError> {
    let available = count_available_core_nodes(core_nodes, available_nodes)?;
    let required = mode.required_available_cores();

    if available >= required {
        Ok(CoreQuorumStatus::Available {
            mode,
            available,
            required,
        })
    } else {
        Ok(CoreQuorumStatus::Unavailable {
            mode,
            available,
            required,
        })
    }
}

fn count_available_core_nodes(
    candidate_nodes: &[NodeId],
    available_nodes: &[NodeId],
) -> Result<usize, CoreAvailabilityError> {
    reject_duplicate_available_nodes(available_nodes)?;

    for node in available_nodes {
        if !candidate_nodes.iter().any(|candidate| candidate == node) {
            return Err(CoreAvailabilityError::UnknownCore {
                node_id: node.clone(),
            });
        }
    }

    Ok(available_nodes.len())
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "Vec<NodeId>", into = "Vec<NodeId>")]
pub struct CanonicalNodeSet {
    nodes: Vec<NodeId>,
}

impl CanonicalNodeSet {
    pub fn from_nodes(nodes: Vec<NodeId>) -> Result<Self, CanonicalNodeSetError> {
        reject_duplicate_nodes(&nodes)?;
        let mut nodes = nodes;
        nodes.sort();

        Ok(Self { nodes })
    }

    #[must_use]
    pub fn nodes(&self) -> &[NodeId] {
        self.nodes.as_slice()
    }

    #[must_use]
    pub fn len(&self) -> usize {
        self.nodes.len()
    }

    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    #[must_use]
    pub fn contains_all(&self, expected: &[NodeId]) -> bool {
        expected
            .iter()
            .all(|node| self.nodes.iter().any(|candidate| candidate == node))
    }
}

impl TryFrom<Vec<NodeId>> for CanonicalNodeSet {
    type Error = CanonicalNodeSetError;

    fn try_from(nodes: Vec<NodeId>) -> Result<Self, Self::Error> {
        Self::from_nodes(nodes)
    }
}

impl From<CanonicalNodeSet> for Vec<NodeId> {
    fn from(value: CanonicalNodeSet) -> Self {
        value.nodes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalNodeSetError {
    DuplicateNode { node_id: NodeId },
}

impl fmt::Display for CanonicalNodeSetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateNode { node_id } => {
                write!(formatter, "duplicate node id {}", node_id.as_str())
            }
        }
    }
}

impl From<CanonicalNodeSetError> for CoreTopologyError {
    fn from(value: CanonicalNodeSetError) -> Self {
        match value {
            CanonicalNodeSetError::DuplicateNode { node_id } => Self::DuplicateNode { node_id },
        }
    }
}

impl From<CanonicalNodeSetError> for CorePromotionTopologyError {
    fn from(value: CanonicalNodeSetError) -> Self {
        match value {
            CanonicalNodeSetError::DuplicateNode { node_id } => Self::DuplicateNode { node_id },
        }
    }
}

fn reject_duplicate_nodes(nodes: &[NodeId]) -> Result<(), CanonicalNodeSetError> {
    let mut remaining = nodes;
    while let [node, tail @ ..] = remaining {
        if tail.iter().any(|candidate| candidate == node) {
            return Err(CanonicalNodeSetError::DuplicateNode {
                node_id: node.clone(),
            });
        }
        remaining = tail;
    }

    Ok(())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", deny_unknown_fields)]
enum CoreTopologyWire {
    Single { node: NodeId },
    HighAvailability { nodes: Vec<NodeId> },
}

impl TryFrom<CoreTopologyWire> for CoreTopology {
    type Error = CoreTopologyWireError;

    fn try_from(value: CoreTopologyWire) -> Result<Self, Self::Error> {
        match value {
            CoreTopologyWire::Single { node } => Ok(Self::from_nodes(vec![node])?),
            CoreTopologyWire::HighAvailability { nodes } => topology_from_wire_nodes(
                "high_availability",
                CoreTopologyMode::HighAvailability,
                nodes,
            ),
        }
    }
}

fn topology_from_wire_nodes(
    mode_name: &'static str,
    mode: CoreTopologyMode,
    nodes: Vec<NodeId>,
) -> Result<CoreTopology, CoreTopologyWireError> {
    let topology = CoreTopology::from_nodes(nodes)?;
    if topology.mode() == mode {
        Ok(topology)
    } else {
        Err(CoreTopologyWireError::WrongNodeCount { mode: mode_name })
    }
}

impl From<CoreTopology> for CoreTopologyWire {
    fn from(value: CoreTopology) -> Self {
        match value.mode() {
            CoreTopologyMode::Single => {
                let [node] = value.nodes.nodes.as_slice() else {
                    unreachable!("single topology has one node");
                };
                Self::Single { node: node.clone() }
            }
            CoreTopologyMode::HighAvailability => Self::HighAvailability {
                nodes: value.nodes.into(),
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreTopologyWireError {
    Topology(CoreTopologyError),
    WrongNodeCount { mode: &'static str },
}

impl fmt::Display for CoreTopologyWireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Topology(error) => write!(formatter, "{error}"),
            Self::WrongNodeCount { mode } => {
                write!(formatter, "{mode} topology has the wrong node count")
            }
        }
    }
}

impl From<CoreTopologyError> for CoreTopologyWireError {
    fn from(value: CoreTopologyError) -> Self {
        Self::Topology(value)
    }
}

impl fmt::Display for CoreTopologyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("core topology requires at least one node"),
            Self::DuplicateNode { node_id } => {
                write!(formatter, "duplicate core node id {}", node_id.as_str())
            }
            Self::TransitionalCoreCount { count } => {
                write!(
                    formatter,
                    "{count} core nodes is promotion progress, not final topology"
                )
            }
            Self::TooMany { count } => {
                write!(formatter, "core topology supports 1-3 nodes, got {count}")
            }
        }
    }
}

impl fmt::Display for CorePromotionTopologyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateNode { node_id } => {
                write!(
                    formatter,
                    "duplicate promotion node id {}",
                    node_id.as_str()
                )
            }
            Self::WrongNodeCount { count } => {
                write!(
                    formatter,
                    "core promotion topology requires 2 nodes, got {count}"
                )
            }
        }
    }
}

fn reject_duplicate_available_nodes(nodes: &[NodeId]) -> Result<(), CoreAvailabilityError> {
    let mut remaining = nodes;
    while let [node, tail @ ..] = remaining {
        if tail.iter().any(|candidate| candidate == node) {
            return Err(CoreAvailabilityError::DuplicateAvailableNode {
                node_id: node.clone(),
            });
        }
        remaining = tail;
    }

    Ok(())
}
