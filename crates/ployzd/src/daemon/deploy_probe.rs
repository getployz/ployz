//! ployzd-side `ParticipantProbe` implementation.
//!
//! Issues a `DaemonRequest::Ping` to each participant over the overlay-RPC
//! transport (`peer_rpc::overlay_rpc`). This is the live, decision-time
//! reachability check that AGENTS.md mandates: it confirms the peer's daemon
//! is processing requests at the moment a deploy is about to mutate cluster
//! state, rather than relying on freshness of cached membership gossip.

use async_trait::async_trait;
use ployz_api::DaemonRequest;
use ployz_orchestrator::deploy::{ParticipantProbe, ProbeError, ProbeErrorKind};
use ployz_types::model::MachineMembership;

use crate::daemon::handlers::peer_rpc;

pub struct OverlayRpcProbe {
    peer_rpc_port: u16,
}

impl OverlayRpcProbe {
    #[must_use]
    pub fn new(peer_rpc_port: u16) -> Self {
        Self { peer_rpc_port }
    }
}

#[async_trait]
impl ParticipantProbe for OverlayRpcProbe {
    async fn ping(&self, machine: &MachineMembership) -> Result<(), ProbeError> {
        match peer_rpc::overlay_rpc(machine.overlay_ip, self.peer_rpc_port, DaemonRequest::Ping)
            .await
        {
            Ok(response) if response.ok => Ok(()),
            Ok(response) => Err(ProbeError {
                kind: ProbeErrorKind::Unexpected,
                detail: format!(
                    "remote daemon error [{}]: {}",
                    response.code, response.message
                ),
            }),
            Err(error) => {
                let kind = if error.contains("timed out") {
                    ProbeErrorKind::Timeout
                } else {
                    ProbeErrorKind::Refused
                };
                Err(ProbeError {
                    kind,
                    detail: error,
                })
            }
        }
    }
}
