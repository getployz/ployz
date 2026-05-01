//! ployzd-side `ParticipantProbe` implementation.
//!
//! Issues a `DaemonRequest::Ping` to each participant over NATS request-reply.
//! This is the live, decision-time
//! reachability check that AGENTS.md mandates: it confirms the peer's daemon
//! is processing requests at the moment a deploy is about to mutate cluster
//! state, rather than relying on freshness of cached membership gossip.

use async_trait::async_trait;
use ployz_api::DaemonRequest;
use ployz_nats::coord::rpc::{NatsNodeRpcClient, NodeCommandSubject, RpcFailureKind};
use ployz_orchestrator::deploy::{ParticipantProbe, ProbeError, ProbeErrorKind};
use ployz_types::model::MachineMembership;

pub struct NatsRpcProbe {
    client: NatsNodeRpcClient,
}

impl NatsRpcProbe {
    #[must_use]
    pub fn new(client: NatsNodeRpcClient) -> Self {
        Self { client }
    }
}

#[async_trait]
impl ParticipantProbe for NatsRpcProbe {
    async fn ping(&self, machine: &MachineMembership) -> Result<(), ProbeError> {
        match self
            .client
            .request(NodeCommandSubject::ping(&machine.id), &DaemonRequest::Ping)
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
                let kind = match error.kind {
                    RpcFailureKind::Timeout => ProbeErrorKind::Timeout,
                    RpcFailureKind::NoResponders => ProbeErrorKind::Refused,
                    RpcFailureKind::Transport | RpcFailureKind::Encode | RpcFailureKind::Decode => {
                        ProbeErrorKind::Unexpected
                    }
                };
                Err(ProbeError {
                    kind,
                    detail: error.to_string(),
                })
            }
        }
    }
}
