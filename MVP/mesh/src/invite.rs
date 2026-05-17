use std::collections::BTreeSet;
use std::fmt::{self, Display, Formatter};

use mvp_bus::{FactKey, FactPayload, IslandId};
use mvp_projection::{NodeId, NodeJoinedFact, NodeTombstonedFact, ProjectionFactPayload};
use serde::{Deserialize, Serialize};

use crate::{
    IrohEndpointId, MachineInvite, MeshError, MeshResult, VisibleNodes, WireGuardPublicKey,
    derive_overlay_ip,
};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct InviteId(String);

impl InviteId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for InviteId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteSecret(String);

impl InviteSecret {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinRequest {
    pub invite: MachineInvite,
    pub presented_secret: InviteSecret,
    pub node_id: NodeId,
    pub epoch: u64,
    pub iroh_endpoint_id: IrohEndpointId,
    pub wg_public_key: WireGuardPublicKey,
    pub now_ms: u64,
    pub visible_nodes: VisibleNodes,
    pub tombstoned_nodes: BTreeSet<NodeId>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct JoinCommandResult {
    pub island: IslandId,
    pub fact_key: FactKey,
    pub fact_payload: FactPayload,
    pub visible_nodes: VisibleNodes,
}

pub struct JoinCommand;

impl JoinCommand {
    pub fn admit(request: JoinRequest) -> MeshResult<JoinCommandResult> {
        if request.invite.is_expired(request.now_ms) {
            return Err(MeshError::InviteExpired {
                invite_id: request.invite.invite_id,
                expires_at_ms: request.invite.expires_at_ms,
                now_ms: request.now_ms,
            });
        }
        if request.invite.secret != request.presented_secret {
            return Err(MeshError::InviteSecretMismatch {
                invite_id: request.invite.invite_id,
            });
        }
        if request.tombstoned_nodes.contains(&request.node_id) {
            return Err(MeshError::NodeTombstoned {
                node_id: request.node_id,
            });
        }

        let overlay_ip = derive_overlay_ip(&request.invite.island, &request.node_id);
        let payload = ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: request.node_id.clone(),
            epoch: request.epoch,
            overlay_ip: overlay_ip.to_string(),
            iroh_endpoint_id: request.iroh_endpoint_id.to_string(),
            wg_public_key: request.wg_public_key.to_string(),
        });
        Ok(JoinCommandResult {
            island: request.invite.island,
            fact_key: joined_fact_key(&request.node_id, request.epoch)?,
            fact_payload: serialize_fact_payload(payload, "serialize joined fact")?,
            visible_nodes: request.visible_nodes,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstoneCommand {
    pub island: IslandId,
    pub node_id: NodeId,
    pub epoch: u64,
    pub reason: String,
    pub visible_nodes: VisibleNodes,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstoneCommandResult {
    pub island: IslandId,
    pub fact_key: FactKey,
    pub fact_payload: FactPayload,
    pub visible_nodes: VisibleNodes,
}

impl TombstoneCommand {
    pub fn force_remove(self) -> MeshResult<TombstoneCommandResult> {
        let payload = ProjectionFactPayload::NodeTombstoned(NodeTombstonedFact {
            node_id: self.node_id.clone(),
            epoch: self.epoch,
            reason: self.reason,
        });
        Ok(TombstoneCommandResult {
            island: self.island,
            fact_key: tombstone_fact_key(&self.node_id, self.epoch)?,
            fact_payload: serialize_fact_payload(payload, "serialize tombstone fact")?,
            visible_nodes: self.visible_nodes,
        })
    }
}

pub fn joined_fact_key(node_id: &NodeId, epoch: u64) -> MeshResult<FactKey> {
    FactKey::parse(format!("/facts/node/{}/joined/{epoch}", node_id.as_str())).map_err(|error| {
        MeshError::Backend {
            operation: "build joined fact key",
            message: error.to_string(),
        }
    })
}

pub fn tombstone_fact_key(node_id: &NodeId, epoch: u64) -> MeshResult<FactKey> {
    FactKey::parse(format!(
        "/facts/node/{}/tombstoned/{epoch}",
        node_id.as_str()
    ))
    .map_err(|error| MeshError::Backend {
        operation: "build tombstone fact key",
        message: error.to_string(),
    })
}

fn serialize_fact_payload(
    payload: ProjectionFactPayload,
    operation: &'static str,
) -> MeshResult<FactPayload> {
    payload
        .to_fact_bytes()
        .map(Into::into)
        .map_err(|error| MeshError::Backend {
            operation,
            message: error.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use mvp_bus::IslandId;
    use mvp_projection::{NodeId, ProjectionFactPayload};

    use crate::{
        InviteId, InviteSecret, IrohEndpointId, JoinCommand, JoinRequest, MachineInvite,
        TombstoneCommand, VisibleNodes, WireGuardPublicKey,
    };

    #[test]
    fn join_writes_joined_fact_after_invite_validation() {
        let result = JoinCommand::admit(JoinRequest {
            invite: invite(100),
            presented_secret: InviteSecret::new("secret"),
            node_id: NodeId::new("node-1"),
            epoch: 1,
            iroh_endpoint_id: IrohEndpointId::new("iroh-node-1"),
            wg_public_key: WireGuardPublicKey::new("wg-node-1"),
            now_ms: 10,
            visible_nodes: VisibleNodes::new(vec![NodeId::new("node-0")]),
            tombstoned_nodes: BTreeSet::new(),
        })
        .expect("join admitted");

        assert_eq!(result.fact_key.as_str(), "/facts/node/node-1/joined/1");
        assert_eq!(result.visible_nodes.len(), 1);
        let payload = ProjectionFactPayload::from_fact_bytes(result.fact_payload.as_bytes())
            .expect("payload decodes");
        let ProjectionFactPayload::NodeJoined(joined) = payload else {
            panic!("expected joined payload");
        };
        assert_eq!(joined.iroh_endpoint_id, "iroh-node-1");
        assert_eq!(joined.wg_public_key, "wg-node-1");
        assert!(joined.overlay_ip.starts_with("fd"));
    }

    #[test]
    fn expired_invite_fails_before_fact_creation() {
        let error = JoinCommand::admit(JoinRequest {
            invite: invite(100),
            presented_secret: InviteSecret::new("secret"),
            node_id: NodeId::new("node-1"),
            epoch: 1,
            iroh_endpoint_id: IrohEndpointId::new("iroh-node-1"),
            wg_public_key: WireGuardPublicKey::new("wg-node-1"),
            now_ms: 100,
            visible_nodes: VisibleNodes::new(Vec::new()),
            tombstoned_nodes: BTreeSet::new(),
        })
        .expect_err("invite expired");

        assert!(error.to_string().contains("expired"));
    }

    #[test]
    fn tombstoned_node_cannot_join_without_reinvite() {
        let error = JoinCommand::admit(JoinRequest {
            invite: invite(100),
            presented_secret: InviteSecret::new("secret"),
            node_id: NodeId::new("node-1"),
            epoch: 2,
            iroh_endpoint_id: IrohEndpointId::new("iroh-node-1"),
            wg_public_key: WireGuardPublicKey::new("wg-node-1"),
            now_ms: 10,
            visible_nodes: VisibleNodes::new(Vec::new()),
            tombstoned_nodes: BTreeSet::from([NodeId::new("node-1")]),
        })
        .expect_err("node tombstoned");

        assert!(error.to_string().contains("tombstoned"));
    }

    #[test]
    fn force_remove_writes_tombstone_fact() {
        let result = TombstoneCommand {
            island: IslandId::new("prod"),
            node_id: NodeId::new("node-1"),
            epoch: 3,
            reason: "force-remove".to_string(),
            visible_nodes: VisibleNodes::new(vec![NodeId::new("node-0")]),
        }
        .force_remove()
        .expect("tombstone command");

        assert_eq!(result.fact_key.as_str(), "/facts/node/node-1/tombstoned/3");
        let payload = ProjectionFactPayload::from_fact_bytes(result.fact_payload.as_bytes())
            .expect("payload decodes");
        let ProjectionFactPayload::NodeTombstoned(tombstone) = payload else {
            panic!("expected tombstone payload");
        };
        assert_eq!(tombstone.reason, "force-remove");
    }

    fn invite(expires_at_ms: u64) -> MachineInvite {
        MachineInvite {
            island: IslandId::new("prod"),
            bootstrap_endpoint: IrohEndpointId::new("iroh-bootstrap"),
            invite_id: InviteId::new("invite-1"),
            secret: InviteSecret::new("secret"),
            expires_at_ms,
        }
    }
}
