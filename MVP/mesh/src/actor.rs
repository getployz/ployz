use std::sync::Arc;
use std::time::{Duration, SystemTime};

use kameo::Actor;
use kameo::actor::{ActorRef, Spawn};
use kameo::error::SendError;
use kameo::mailbox;
use kameo::message::{Context, Message};

use crate::{MeshError, MeshResult, WireGuardAppliedSnapshot, WireGuardBackend};

const WG_ACTOR_MAILBOX_CAPACITY: usize = 16;
const WG_ACTOR_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WireGuardActorStatus {
    pub applied_revision: Option<u64>,
    pub applied_peer_count: usize,
    pub apply_attempts: u64,
    pub last_apply_success_at: Option<SystemTime>,
    pub last_failure: Option<String>,
}

#[derive(Actor)]
struct WireGuardActor {
    backend: Arc<dyn WireGuardBackend>,
    status: WireGuardActorStatus,
}

impl WireGuardActor {
    fn new(backend: Arc<dyn WireGuardBackend>) -> Self {
        Self {
            backend,
            status: WireGuardActorStatus {
                applied_revision: None,
                applied_peer_count: 0,
                apply_attempts: 0,
                last_apply_success_at: None,
                last_failure: None,
            },
        }
    }
}

#[derive(Clone)]
pub struct WireGuardActorHandle {
    actor: ActorRef<WireGuardActor>,
}

impl WireGuardActorHandle {
    #[must_use]
    pub fn spawn(backend: Arc<dyn WireGuardBackend>) -> Self {
        Self {
            actor: WireGuardActor::spawn_with_mailbox(
                WireGuardActor::new(backend),
                mailbox::bounded(WG_ACTOR_MAILBOX_CAPACITY),
            ),
        }
    }

    pub async fn apply(
        &self,
        snapshot: WireGuardAppliedSnapshot,
    ) -> MeshResult<WireGuardActorStatus> {
        self.actor
            .ask(ApplySnapshot { snapshot })
            .mailbox_timeout(WG_ACTOR_TIMEOUT)
            .reply_timeout(WG_ACTOR_TIMEOUT)
            .await
            .map_err(|error| map_send_error("apply", error))
    }

    pub async fn status(&self) -> MeshResult<WireGuardActorStatus> {
        self.actor
            .ask(ReadStatus)
            .mailbox_timeout(WG_ACTOR_TIMEOUT)
            .reply_timeout(WG_ACTOR_TIMEOUT)
            .await
            .map_err(|error| map_send_error("status", error))
    }
}

struct ApplySnapshot {
    snapshot: WireGuardAppliedSnapshot,
}

impl Message<ApplySnapshot> for WireGuardActor {
    type Reply = MeshResult<WireGuardActorStatus>;

    async fn handle(
        &mut self,
        message: ApplySnapshot,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        self.status.apply_attempts += 1;
        let revision = message.snapshot.revision;
        let peer_count = message.snapshot.peers.len();
        match self.backend.apply(message.snapshot).await {
            Ok(()) => {
                self.status.applied_revision = Some(revision);
                self.status.applied_peer_count = peer_count;
                self.status.last_apply_success_at = Some(SystemTime::now());
                self.status.last_failure = None;
                Ok(self.status.clone())
            }
            Err(error) => {
                self.status.last_failure = Some(error.to_string());
                Err(error)
            }
        }
    }
}

struct ReadStatus;

impl Message<ReadStatus> for WireGuardActor {
    type Reply = MeshResult<WireGuardActorStatus>;

    async fn handle(
        &mut self,
        _message: ReadStatus,
        _ctx: &mut Context<Self, Self::Reply>,
    ) -> Self::Reply {
        Ok(self.status.clone())
    }
}

fn map_send_error<T>(operation: &'static str, error: SendError<T, MeshError>) -> MeshError {
    match error {
        SendError::HandlerError(error) => error,
        SendError::ActorNotRunning(_)
        | SendError::ActorStopped
        | SendError::MailboxFull(_)
        | SendError::Timeout(_) => MeshError::ActorUnavailable {
            operation,
            reason: error.to_string(),
        },
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use mvp_bus::IslandId;
    use mvp_projection::{NodeId, NodeProjection, ProjectionState};

    use crate::{
        MemoryWireGuardBackend, WireGuardActorHandle, WireGuardAppliedSnapshot, WireGuardBackend,
        plan_full_mesh,
    };

    #[tokio::test]
    async fn actor_applies_snapshot_through_backend() {
        let backend = Arc::new(MemoryWireGuardBackend::new());
        let actor = WireGuardActorHandle::spawn(backend.clone());
        let plan = plan_full_mesh(&projection(), &NodeId::new("node-0"), 5).expect("plan");
        let snapshot = WireGuardAppliedSnapshot::from_plan(IslandId::new("prod"), plan);

        let status = actor.apply(snapshot.clone()).await.expect("apply");

        assert_eq!(status.applied_revision, Some(5));
        assert_eq!(status.applied_peer_count, 1);
        assert_eq!(
            backend.last_applied().await.expect("last applied"),
            Some(snapshot)
        );
    }

    fn projection() -> ProjectionState {
        let mut state = ProjectionState::for_island(IslandId::new("prod"));
        for index in 0..2 {
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
