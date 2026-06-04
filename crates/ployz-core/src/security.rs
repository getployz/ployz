//! Role and authority models for NATS subject permissions.

use crate::ids::NodeId;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NatsPrincipal {
    Node { node_id: NodeId },
    Controller,
    User,
    System,
}
