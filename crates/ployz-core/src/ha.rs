//! Core control-plane topology.

use crate::ids::NodeId;
use serde::{Deserialize, Serialize};
use std::fmt;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "CoreTopologyWire", into = "CoreTopologyWire")]
pub struct CoreTopology {
    node: NodeId,
}

impl CoreTopology {
    #[must_use]
    pub fn single(node: NodeId) -> Self {
        Self { node }
    }

    pub fn from_nodes(nodes: Vec<NodeId>) -> Result<Self, CoreTopologyError> {
        match nodes.as_slice() {
            [] => Err(CoreTopologyError::Empty),
            [node] => Ok(Self::single(node.clone())),
            [first, rest @ ..] if rest.iter().any(|node| node == first) => {
                Err(CoreTopologyError::DuplicateNode {
                    node_id: first.clone(),
                })
            }
            [_, rest @ ..] => {
                reject_duplicate_nodes(rest).map_err(|error| match error {
                    DuplicateNodeError::DuplicateNode { node_id } => {
                        CoreTopologyError::DuplicateNode { node_id }
                    }
                })?;
                Err(CoreTopologyError::UnsupportedCoreCount { count: nodes.len() })
            }
        }
    }

    #[must_use]
    pub fn core_count(&self) -> usize {
        1
    }

    #[must_use]
    pub fn node(&self) -> &NodeId {
        &self.node
    }

    #[must_use]
    pub fn nodes(&self) -> &[NodeId] {
        std::slice::from_ref(&self.node)
    }

    pub fn availability(
        &self,
        available_nodes: Vec<NodeId>,
    ) -> Result<CoreAvailability, CoreAvailabilityError> {
        reject_duplicate_nodes(&available_nodes).map_err(|error| match error {
            DuplicateNodeError::DuplicateNode { node_id } => {
                CoreAvailabilityError::DuplicateAvailableNode { node_id }
            }
        })?;

        for node in &available_nodes {
            if node != &self.node {
                return Err(CoreAvailabilityError::UnknownCore {
                    node_id: node.clone(),
                });
            }
        }

        if available_nodes.iter().any(|node| node == &self.node) {
            Ok(CoreAvailability::Available)
        } else {
            Ok(CoreAvailability::Unavailable)
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoreAvailability {
    Available,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreTopologyError {
    Empty,
    DuplicateNode { node_id: NodeId },
    UnsupportedCoreCount { count: usize },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreAvailabilityError {
    DuplicateAvailableNode { node_id: NodeId },
    UnknownCore { node_id: NodeId },
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum DuplicateNodeError {
    DuplicateNode { node_id: NodeId },
}

fn reject_duplicate_nodes(nodes: &[NodeId]) -> Result<(), DuplicateNodeError> {
    let mut remaining = nodes;
    while let [node, tail @ ..] = remaining {
        if tail.iter().any(|candidate| candidate == node) {
            return Err(DuplicateNodeError::DuplicateNode {
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
            CoreTopologyWire::Single { node } => Ok(Self::single(node)),
            CoreTopologyWire::HighAvailability { nodes } => {
                Err(CoreTopologyWireError::UnsupportedHighAvailability { count: nodes.len() })
            }
        }
    }
}

impl From<CoreTopology> for CoreTopologyWire {
    fn from(value: CoreTopology) -> Self {
        Self::Single { node: value.node }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoreTopologyWireError {
    UnsupportedHighAvailability { count: usize },
}

impl fmt::Display for CoreTopologyWireError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedHighAvailability { count } => write!(
                formatter,
                "v1 does not support high availability core topology with {count} node(s)"
            ),
        }
    }
}

impl fmt::Display for CoreTopologyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => formatter.write_str("core topology requires one node"),
            Self::DuplicateNode { node_id } => {
                write!(formatter, "duplicate core node id {}", node_id.as_str())
            }
            Self::UnsupportedCoreCount { count } => {
                write!(
                    formatter,
                    "v1 core topology supports exactly one core node, got {count}"
                )
            }
        }
    }
}
