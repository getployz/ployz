use std::sync::{Arc, PoisonError, RwLock};

use async_trait::async_trait;
use mvp_projection::{NodeId, ProjectionState};
use serde::{Deserialize, Serialize};

use crate::{
    MeshError, MeshNode, MeshResult, WireGuardAppliedSnapshot, WireGuardOverlayIp,
    WireGuardPublicKey,
};

pub const FULL_MESH_NODE_LIMIT: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireGuardPeer {
    pub node_id: NodeId,
    pub public_key: WireGuardPublicKey,
    pub allowed_ip: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WireGuardPeerPlan {
    pub local_node_id: NodeId,
    pub local_overlay_ip: WireGuardOverlayIp,
    pub revision: u64,
    pub peers: Vec<WireGuardPeer>,
}

pub fn plan_full_mesh(
    state: &ProjectionState,
    local_node_id: &NodeId,
    revision: u64,
) -> MeshResult<WireGuardPeerPlan> {
    if state.nodes.len() > FULL_MESH_NODE_LIMIT {
        return Err(MeshError::TooManyFullMeshNodes {
            count: state.nodes.len(),
            limit: FULL_MESH_NODE_LIMIT,
        });
    }
    let local = state
        .nodes
        .get(local_node_id)
        .ok_or_else(|| MeshError::LocalNodeNotLive {
            node_id: local_node_id.clone(),
        })?;
    let local = MeshNode::from_projection(local)?;
    let mut peers = Vec::new();
    for node in state.nodes.values() {
        if node.node_id == *local_node_id {
            continue;
        }
        let Ok(mesh_node) = MeshNode::from_projection(node) else {
            continue;
        };
        peers.push(WireGuardPeer {
            node_id: mesh_node.node_id,
            public_key: mesh_node.wg_public_key,
            allowed_ip: mesh_node.overlay_ip.allowed_ip_cidr(),
            endpoint: mesh_node.iroh_endpoint_id.to_string(),
        });
    }
    peers.sort_by(|left, right| left.node_id.cmp(&right.node_id));

    Ok(WireGuardPeerPlan {
        local_node_id: local.node_id,
        local_overlay_ip: local.overlay_ip,
        revision,
        peers,
    })
}

#[async_trait]
pub trait WireGuardBackend: Send + Sync {
    async fn apply(&self, snapshot: WireGuardAppliedSnapshot) -> MeshResult<()>;
    async fn last_applied(&self) -> MeshResult<Option<WireGuardAppliedSnapshot>>;
}

#[derive(Debug, Clone, Default)]
pub struct MemoryWireGuardBackend {
    applied: Arc<RwLock<Option<WireGuardAppliedSnapshot>>>,
}

impl MemoryWireGuardBackend {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait]
impl WireGuardBackend for MemoryWireGuardBackend {
    async fn apply(&self, snapshot: WireGuardAppliedSnapshot) -> MeshResult<()> {
        *self.applied.write().unwrap_or_else(PoisonError::into_inner) = Some(snapshot);
        Ok(())
    }

    async fn last_applied(&self) -> MeshResult<Option<WireGuardAppliedSnapshot>> {
        Ok(self
            .applied
            .read()
            .unwrap_or_else(PoisonError::into_inner)
            .clone())
    }
}

#[cfg(test)]
mod tests {
    use mvp_bus::IslandId;
    use mvp_projection::{NodeId, NodeProjection, ProjectionState};

    use crate::{WireGuardAppliedSnapshot, WireGuardBackend};

    use super::{MemoryWireGuardBackend, plan_full_mesh};

    #[test]
    fn full_mesh_excludes_local_node_and_sorts_peers() {
        let state = projection_with_nodes(3);

        let plan = plan_full_mesh(&state, &NodeId::new("node-1"), 7).expect("mesh plan");

        assert_eq!(plan.local_node_id, NodeId::new("node-1"));
        assert_eq!(plan.peers.len(), 2);
        assert_eq!(plan.peers[0].node_id, NodeId::new("node-0"));
        assert_eq!(plan.peers[1].node_id, NodeId::new("node-2"));
        assert_eq!(plan.peers[0].allowed_ip, "fd00::/128");
    }

    #[test]
    fn full_mesh_rejects_missing_local_node() {
        let state = projection_with_nodes(2);

        let error = plan_full_mesh(&state, &NodeId::new("missing"), 1).expect_err("missing local");

        assert!(error.to_string().contains("not live"));
    }

    #[test]
    fn full_mesh_skips_malformed_remote_peer() {
        let mut state = projection_with_nodes(2);
        state.nodes.insert(
            NodeId::new("bad-peer"),
            NodeProjection {
                node_id: NodeId::new("bad-peer"),
                epoch: 1,
                overlay_ip: "not-an-ip".to_string(),
                iroh_endpoint_id: "iroh-bad-peer".to_string(),
                wg_public_key: "wg-bad-peer".to_string(),
            },
        );

        let plan = plan_full_mesh(&state, &NodeId::new("node-0"), 1).expect("mesh plan");

        assert_eq!(plan.peers.len(), 1);
        assert_eq!(plan.peers[0].node_id, NodeId::new("node-1"));
    }

    #[tokio::test]
    async fn memory_backend_records_last_applied_snapshot() {
        let state = projection_with_nodes(2);
        let plan = plan_full_mesh(&state, &NodeId::new("node-0"), 2).expect("mesh plan");
        let snapshot = WireGuardAppliedSnapshot::from_plan(IslandId::new("prod"), plan);
        let backend = MemoryWireGuardBackend::new();

        backend
            .apply(snapshot.clone())
            .await
            .expect("apply snapshot");

        assert_eq!(
            backend.last_applied().await.expect("read last applied"),
            Some(snapshot)
        );
    }

    fn projection_with_nodes(count: usize) -> ProjectionState {
        let mut state = ProjectionState::for_island(IslandId::new("prod"));
        for index in 0..count {
            let node_id = NodeId::new(format!("node-{index}"));
            state.nodes.insert(
                node_id.clone(),
                NodeProjection {
                    node_id,
                    epoch: 1,
                    overlay_ip: format!("fd00::{index:x}"),
                    iroh_endpoint_id: format!("iroh-node-{index}"),
                    wg_public_key: format!("wg-node-{index}"),
                },
            );
        }
        state
    }
}
