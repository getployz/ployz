use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use mvp_bus::{
    BusActorHandle, BusSession, FactKey, FactKeyPattern, HandlerFailure, RequestContext, Subject,
    SubjectPattern,
};
use mvp_identity::NodeId;
use mvp_p2panda_facts::{PandaFactAuthor, SharedPandaFactStore};
use mvp_p2panda_transport::{PandaNetFactImportOutcome, PandaNetFactNode, PandaNetTransportError};
use mvp_projection::CandidateStatus;
use serde::{Deserialize, Serialize};

use crate::error::{NodeError, NodeResult};
use crate::membership::spawn_fact_node;
use crate::state::LoadedNodeState;

const RPC_TIMEOUT: Duration = Duration::from_secs(5);
const RPC_IDLE: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NodeAgentRpcRequestFact {
    request_id: String,
    requester_node_id: NodeId,
    target_node_id: NodeId,
    subject: String,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NodeAgentRpcReplyFact {
    request_id: String,
    target_node_id: NodeId,
    outcome: NodeAgentRpcReplyOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum NodeAgentRpcReplyOutcome {
    Success { payload: Vec<u8> },
    Failure { message: String },
}

struct NodeAgentRpcBridge {
    fact_node: PandaNetFactNode,
    session: BusSession,
    author: PandaFactAuthor,
    requester_node_id: NodeId,
    next_request: u64,
}

pub(crate) struct RemoteNodeAgentBridgeSet {
    pub store: SharedPandaFactStore,
    pub fact_session: BusSession,
}

impl NodeAgentRpcBridge {
    fn new(
        fact_node: PandaNetFactNode,
        session: BusSession,
        author: PandaFactAuthor,
        requester_node_id: NodeId,
    ) -> Self {
        Self {
            fact_node,
            session,
            author,
            requester_node_id,
            next_request: 1,
        }
    }

    async fn request(
        &mut self,
        target_node_id: NodeId,
        ctx: RequestContext,
    ) -> NodeResult<Vec<u8>> {
        let subject = ctx.message.subject().clone();
        validate_target_subject(&target_node_id, &subject)?;
        let request_id = self.next_request_id(&target_node_id, &subject);
        let request = NodeAgentRpcRequestFact {
            request_id: request_id.clone(),
            requester_node_id: self.requester_node_id.clone(),
            target_node_id: target_node_id.clone(),
            subject: subject.to_string(),
            payload: ctx.message.payload().as_bytes().to_vec(),
        };
        publish_rpc_request(&mut self.fact_node, &self.session, &self.author, &request).await?;
        let reply = wait_for_rpc_reply(
            &mut self.fact_node,
            &self.session,
            self.requester_node_id.clone(),
            &request_id,
            Instant::now() + RPC_TIMEOUT,
        )
        .await?;
        if reply.target_node_id != target_node_id {
            return Err(NodeError::NodeAgentRpc {
                message: format!(
                    "reply target '{}' did not match requested target '{}'",
                    reply.target_node_id, target_node_id
                ),
            });
        }
        match reply.outcome {
            NodeAgentRpcReplyOutcome::Success { payload } => Ok(payload),
            NodeAgentRpcReplyOutcome::Failure { message } => Err(NodeError::NodeAgentRpc {
                message: format!("remote node-agent request failed: {message}"),
            }),
        }
    }

    fn next_request_id(&mut self, target_node_id: &NodeId, subject: &Subject) -> String {
        let request = self.next_request;
        self.next_request = self.next_request.saturating_add(1);
        let hash = blake3::hash(
            format!(
                "{}:{target_node_id}:{subject}:{request}",
                self.requester_node_id
            )
            .as_bytes(),
        );
        hash.to_hex()[..24].to_string()
    }
}

pub(crate) async fn register_remote_node_agent_bridges(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &LoadedNodeState,
) -> NodeResult<Option<RemoteNodeAgentBridgeSet>> {
    let remote_nodes = state
        .admitted_peers()
        .into_iter()
        .filter_map(|peer| peer.node_id)
        .map(NodeId::new)
        .filter(|node_id| node_id != &state.node_id())
        .collect::<Vec<_>>();
    if remote_nodes.is_empty() {
        return Ok(None);
    }

    let (fact_node, writer_session, author, _authority) = spawn_fact_node(state).await?;
    let store = fact_node.store();
    let bridge = Arc::new(Mutex::new(NodeAgentRpcBridge::new(
        fact_node,
        writer_session,
        author,
        state.node_id(),
    )));

    for node_id in remote_nodes {
        register_bridge_handler(
            bus,
            session,
            node_id.clone(),
            SubjectPattern::parse(format!("node.{}.capacity", node_id.as_str()))
                .map_err(|source| NodeError::BusSubject { source })?,
            Arc::clone(&bridge),
        )
        .await?;
        register_bridge_handler(
            bus,
            session,
            node_id.clone(),
            SubjectPattern::parse(format!("node.{}.rpc.>", node_id.as_str()))
                .map_err(|source| NodeError::BusSubject { source })?,
            Arc::clone(&bridge),
        )
        .await?;
    }

    Ok(Some(RemoteNodeAgentBridgeSet {
        store,
        fact_session: bridge
            .lock()
            .map_err(|_| NodeError::NodeAgentRpc {
                message: "node-agent bridge mutex poisoned".to_string(),
            })?
            .session
            .clone(),
    }))
}

async fn register_bridge_handler(
    bus: &BusActorHandle,
    session: &BusSession,
    target_node_id: NodeId,
    pattern: SubjectPattern,
    bridge: Arc<Mutex<NodeAgentRpcBridge>>,
) -> NodeResult<()> {
    bus.subscribe(session, pattern, move |ctx| {
        let mut bridge = bridge.lock().map_err(|_| handler_failed(&ctx))?;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()
            .map_err(|_| handler_failed(&ctx))?;
        match runtime.block_on(bridge.request(target_node_id.clone(), ctx.clone())) {
            Ok(payload) => ctx.reply(payload),
            Err(error) => {
                eprintln!(
                    "node-agent rpc bridge failed for {}: {error}",
                    ctx.message.subject()
                );
                Err(handler_failed(&ctx))
            }
        }
    })
    .await
    .map(|_| ())
    .map_err(|source| NodeError::Bus { source })
}

pub(crate) async fn handle_node_agent_rpc_requests(
    fact_node: &mut PandaNetFactNode,
    session: &BusSession,
    author: &PandaFactAuthor,
    product_bus: &BusActorHandle,
    product_session: &BusSession,
    state: &LoadedNodeState,
) -> NodeResult<u64> {
    let store = fact_node.store();
    let pattern =
        FactKeyPattern::parse(format!("/facts/node-rpc/request/{}/>", state.node_id_str()))
            .map_err(|source| NodeError::Bus { source })?;
    let candidates = store
        .list_fact_candidates(&state.island(), &pattern, session)
        .await
        .map_err(|source| NodeError::FactSource { source })?;
    let payloads = store
        .read_fact_payloads(&state.island(), &candidates, session)
        .await
        .map_err(|source| NodeError::FactSource { source })?;

    let mut handled = 0;
    for candidate in candidates {
        if !matches!(
            candidate.status(),
            CandidateStatus::Verified | CandidateStatus::Conflict
        ) {
            continue;
        }
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            continue;
        };
        let Ok(request) = serde_json::from_slice::<NodeAgentRpcRequestFact>(payload.as_bytes())
        else {
            continue;
        };
        if request.target_node_id != state.node_id() {
            continue;
        }
        if has_rpc_reply(&store, session, &request).await? {
            continue;
        }
        let reply = execute_rpc_request(product_bus, product_session, &request).await;
        publish_rpc_reply(fact_node, session, author, &request, reply).await?;
        handled += 1;
    }
    Ok(handled)
}

async fn execute_rpc_request(
    product_bus: &BusActorHandle,
    product_session: &BusSession,
    request: &NodeAgentRpcRequestFact,
) -> NodeAgentRpcReplyOutcome {
    let subject = match Subject::parse(request.subject.clone()) {
        Ok(subject) => subject,
        Err(error) => {
            return NodeAgentRpcReplyOutcome::Failure {
                message: format!("invalid subject: {error}"),
            };
        }
    };
    if let Err(error) = validate_target_subject(&request.target_node_id, &subject) {
        return NodeAgentRpcReplyOutcome::Failure {
            message: error.to_string(),
        };
    }
    match product_bus
        .request(
            product_session,
            subject,
            request.payload.clone(),
            RPC_TIMEOUT,
        )
        .await
    {
        Ok(response) => NodeAgentRpcReplyOutcome::Success {
            payload: response.payload().as_bytes().to_vec(),
        },
        Err(error) => NodeAgentRpcReplyOutcome::Failure {
            message: error.to_string(),
        },
    }
}

async fn publish_rpc_request(
    fact_node: &mut PandaNetFactNode,
    session: &BusSession,
    author: &PandaFactAuthor,
    request: &NodeAgentRpcRequestFact,
) -> NodeResult<()> {
    let key = rpc_request_key(&request.target_node_id, &request.request_id)?;
    let payload = serde_json::to_vec(request)
        .map_err(|source| NodeError::EncodeNodeAgentRpc { source })?
        .into();
    fact_node
        .publish_fact_payload(session, author, key, payload)
        .await
        .map_err(|source| NodeError::Transport { source })?;
    Ok(())
}

async fn publish_rpc_reply(
    fact_node: &mut PandaNetFactNode,
    session: &BusSession,
    author: &PandaFactAuthor,
    request: &NodeAgentRpcRequestFact,
    outcome: NodeAgentRpcReplyOutcome,
) -> NodeResult<()> {
    let reply = NodeAgentRpcReplyFact {
        request_id: request.request_id.clone(),
        target_node_id: request.target_node_id.clone(),
        outcome,
    };
    let key = rpc_reply_key(&request.requester_node_id, &request.request_id)?;
    let payload = serde_json::to_vec(&reply)
        .map_err(|source| NodeError::EncodeNodeAgentRpc { source })?
        .into();
    fact_node
        .publish_fact_payload(session, author, key, payload)
        .await
        .map_err(|source| NodeError::Transport { source })?;
    Ok(())
}

async fn wait_for_rpc_reply(
    fact_node: &mut PandaNetFactNode,
    session: &BusSession,
    requester_node_id: NodeId,
    request_id: &str,
    deadline: Instant,
) -> NodeResult<NodeAgentRpcReplyFact> {
    loop {
        if let Some(reply) = read_rpc_reply(
            &fact_node.store(),
            session,
            requester_node_id.clone(),
            request_id,
        )
        .await?
        {
            return Ok(reply);
        }
        if Instant::now() >= deadline {
            return Err(NodeError::NodeAgentRpc {
                message: format!("timed out waiting for reply to {request_id}"),
            });
        }
        match fact_node
            .import_next_fact_batch_with_idle_timeout(RPC_IDLE)
            .await
        {
            Ok(Some(outcomes))
                if outcomes
                    .iter()
                    .any(|outcome| matches!(outcome, PandaNetFactImportOutcome::Imported)) => {}
            Ok(Some(_)) | Ok(None) => {
                fact_node
                    .refresh_stream()
                    .await
                    .map_err(|source| NodeError::Transport { source })?;
            }
            Err(source) if is_recoverable_stream_error(&source) => {
                fact_node
                    .refresh_stream()
                    .await
                    .map_err(|source| NodeError::Transport { source })?;
            }
            Err(source) => return Err(NodeError::Transport { source }),
        }
    }
}

fn is_recoverable_stream_error(error: &PandaNetTransportError) -> bool {
    matches!(
        error,
        PandaNetTransportError::StreamEnded { .. }
            | PandaNetTransportError::StreamLagged { .. }
            | PandaNetTransportError::StreamFailed { .. }
    )
}

async fn has_rpc_reply(
    store: &SharedPandaFactStore,
    session: &BusSession,
    request: &NodeAgentRpcRequestFact,
) -> NodeResult<bool> {
    read_rpc_reply(
        store,
        session,
        request.requester_node_id.clone(),
        &request.request_id,
    )
    .await
    .map(|reply| reply.is_some())
}

async fn read_rpc_reply(
    store: &SharedPandaFactStore,
    session: &BusSession,
    requester_node_id: NodeId,
    request_id: &str,
) -> NodeResult<Option<NodeAgentRpcReplyFact>> {
    let pattern = FactKeyPattern::parse(rpc_reply_key(&requester_node_id, request_id)?.as_str())
        .map_err(|source| NodeError::Bus { source })?;
    let candidates = store
        .list_fact_candidates(session.island(), &pattern, session)
        .await
        .map_err(|source| NodeError::FactSource { source })?;
    let payloads = store
        .read_fact_payloads(session.island(), &candidates, session)
        .await
        .map_err(|source| NodeError::FactSource { source })?;
    for candidate in candidates {
        if !matches!(
            candidate.status(),
            CandidateStatus::Verified | CandidateStatus::Conflict
        ) {
            continue;
        }
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            continue;
        };
        if let Ok(reply) = serde_json::from_slice::<NodeAgentRpcReplyFact>(payload.as_bytes())
            && reply.request_id == request_id
        {
            return Ok(Some(reply));
        }
    }
    Ok(None)
}

fn validate_target_subject(target_node_id: &NodeId, subject: &Subject) -> NodeResult<()> {
    let tokens = subject.tokens();
    match tokens {
        [node, target, kind]
            if node == "node" && target == target_node_id.as_str() && kind == "capacity" =>
        {
            Ok(())
        }
        [node, target, rpc, _operation]
            if node == "node" && target == target_node_id.as_str() && rpc == "rpc" =>
        {
            Ok(())
        }
        [node, target, ..] if node == "node" && target != target_node_id.as_str() => {
            Err(NodeError::NodeAgentRpc {
                message: format!("subject '{subject}' does not target node '{target_node_id}'"),
            })
        }
        _ => Err(NodeError::NodeAgentRpc {
            message: format!("invalid node-agent subject '{subject}'"),
        }),
    }
}

fn rpc_request_key(target_node_id: &NodeId, request_id: &str) -> NodeResult<FactKey> {
    FactKey::parse(format!(
        "/facts/node-rpc/request/{target_node_id}/{request_id}"
    ))
    .map_err(|source| NodeError::Bus { source })
}

fn rpc_reply_key(requester_node_id: &NodeId, request_id: &str) -> NodeResult<FactKey> {
    FactKey::parse(format!(
        "/facts/node-rpc/reply/{requester_node_id}/{request_id}"
    ))
    .map_err(|source| NodeError::Bus { source })
}

fn handler_failed(ctx: &RequestContext) -> mvp_bus::BusError {
    mvp_bus::BusError::HandlerFailed {
        subject: ctx.message.subject().to_string(),
        failure: HandlerFailure::Application,
    }
}
