use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mvp_bus::{
    BusActorHandle, BusSession, FactKey, FactKeyPattern, HandlerFailure, PrincipalId,
    RequestContext, Subject, SubjectPattern,
};
use mvp_identity::NodeId;
use mvp_p2panda_facts::{PandaFactAuthor, SharedPandaFactStore};
use mvp_p2panda_transport::{PandaNetFactImportOutcome, PandaNetFactNode, PandaNetTransportError};
use mvp_projection::CandidateStatus;
use serde::{Deserialize, Serialize};

use crate::error::{NodeError, NodeResult};
use crate::membership::spawn_fact_node;
use crate::state::LoadedNodeState;

const RPC_TIMEOUT: Duration = Duration::from_secs(15);
const RPC_IDLE: Duration = Duration::from_millis(50);

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NodeAgentRpcRequestFact {
    request_id: String,
    requester_node_id: NodeId,
    target_node_id: NodeId,
    subject: String,
    payload_hash: String,
    expires_at_ms: u64,
    payload: Vec<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct NodeAgentRpcReplyFact {
    request_id: String,
    requester_node_id: NodeId,
    target_node_id: NodeId,
    subject: String,
    payload_hash: String,
    outcome: NodeAgentRpcReplyOutcome,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
enum NodeAgentRpcReplyOutcome {
    Success { payload: Vec<u8> },
    Failure { message: String },
}

struct NodeAgentRpcBridge {
    fact_node: Arc<tokio::sync::Mutex<PandaNetFactNode>>,
    session: BusSession,
    author: PandaFactAuthor,
    requester_node_id: NodeId,
    next_request: u64,
}

pub(crate) struct RemoteNodeAgentBridgeSet {
    pub store: SharedPandaFactStore,
    pub fact_session: BusSession,
    bridge: Arc<Mutex<NodeAgentRpcBridge>>,
    registered_nodes: BTreeSet<NodeId>,
}

impl NodeAgentRpcBridge {
    fn new(
        fact_node: Arc<tokio::sync::Mutex<PandaNetFactNode>>,
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
        let payload = ctx.message.payload().as_bytes().to_vec();
        let payload_hash = payload_hash(&payload);
        let expires_at_ms = now_ms().saturating_add(millis_u64(RPC_TIMEOUT));
        let request_id = self.next_request_id(&target_node_id, &subject, &payload_hash);
        let request = NodeAgentRpcRequestFact {
            request_id: request_id.clone(),
            requester_node_id: self.requester_node_id.clone(),
            target_node_id: target_node_id.clone(),
            subject: subject.to_string(),
            payload_hash: payload_hash.clone(),
            expires_at_ms,
            payload,
        };
        let mut fact_node = self.fact_node.lock().await;
        publish_rpc_request(&mut fact_node, &self.session, &self.author, &request).await?;
        let reply = wait_for_rpc_reply(
            &mut fact_node,
            &self.session,
            &request,
            Instant::now() + RPC_TIMEOUT,
        )
        .await?;
        if reply.target_node_id != target_node_id
            || reply.requester_node_id != self.requester_node_id
            || reply.subject != subject.to_string()
            || reply.payload_hash != payload_hash
        {
            return Err(NodeError::NodeAgentRpc {
                message: format!("reply metadata did not match request '{request_id}'"),
            });
        }
        match reply.outcome {
            NodeAgentRpcReplyOutcome::Success { payload } => Ok(payload),
            NodeAgentRpcReplyOutcome::Failure { message } => Err(NodeError::NodeAgentRpc {
                message: format!("remote node-agent request failed: {message}"),
            }),
        }
    }

    fn next_request_id(
        &mut self,
        target_node_id: &NodeId,
        subject: &Subject,
        payload_hash: &str,
    ) -> String {
        let request = self.next_request;
        self.next_request = self.next_request.saturating_add(1);
        let hash = blake3::hash(
            format!(
                "{}:{target_node_id}:{subject}:{payload_hash}:{}:{request}",
                self.requester_node_id,
                now_ms()
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
    let fact_node = Arc::new(tokio::sync::Mutex::new(fact_node));
    register_remote_node_agent_bridges_with_fact_node(
        bus,
        session,
        state,
        fact_node,
        writer_session,
        author,
    )
    .await
}

pub(crate) async fn register_remote_node_agent_bridges_with_fact_node(
    bus: &BusActorHandle,
    session: &BusSession,
    state: &LoadedNodeState,
    fact_node: Arc<tokio::sync::Mutex<PandaNetFactNode>>,
    writer_session: BusSession,
    author: PandaFactAuthor,
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

    let store = fact_node.lock().await.store();
    let bridge = Arc::new(Mutex::new(NodeAgentRpcBridge::new(
        fact_node,
        writer_session,
        author,
        state.node_id(),
    )));
    let fact_session = bridge
        .lock()
        .map_err(|_| NodeError::NodeAgentRpc {
            message: "node-agent bridge mutex poisoned".to_string(),
        })?
        .session
        .clone();
    let mut bridge_set = RemoteNodeAgentBridgeSet {
        store,
        fact_session,
        bridge,
        registered_nodes: BTreeSet::new(),
    };
    bridge_set
        .register_missing_remote_nodes(bus, session, state, remote_nodes)
        .await?;
    Ok(Some(bridge_set))
}

impl RemoteNodeAgentBridgeSet {
    pub(crate) async fn register_missing(
        &mut self,
        bus: &BusActorHandle,
        session: &BusSession,
        state: &LoadedNodeState,
    ) -> NodeResult<()> {
        let remote_nodes = state
            .admitted_peers()
            .into_iter()
            .filter_map(|peer| peer.node_id)
            .map(NodeId::new)
            .filter(|node_id| node_id != &state.node_id())
            .collect::<Vec<_>>();
        self.register_missing_remote_nodes(bus, session, state, remote_nodes)
            .await
    }

    async fn register_missing_remote_nodes(
        &mut self,
        bus: &BusActorHandle,
        session: &BusSession,
        _state: &LoadedNodeState,
        remote_nodes: Vec<NodeId>,
    ) -> NodeResult<()> {
        for node_id in remote_nodes {
            if self.registered_nodes.contains(&node_id) {
                continue;
            }
            register_bridge_handler(
                bus,
                session,
                node_id.clone(),
                SubjectPattern::parse(format!("node.{}.capacity", node_id.as_str()))
                    .map_err(|source| NodeError::BusSubject { source })?,
                Arc::clone(&self.bridge),
            )
            .await?;
            register_bridge_handler(
                bus,
                session,
                node_id.clone(),
                SubjectPattern::parse(format!("node.{}.rpc.>", node_id.as_str()))
                    .map_err(|source| NodeError::BusSubject { source })?,
                Arc::clone(&self.bridge),
            )
            .await?;
            self.registered_nodes.insert(node_id);
        }
        Ok(())
    }
}

async fn register_bridge_handler(
    bus: &BusActorHandle,
    session: &BusSession,
    target_node_id: NodeId,
    pattern: SubjectPattern,
    bridge: Arc<Mutex<NodeAgentRpcBridge>>,
) -> NodeResult<()> {
    bus.subscribe(session, pattern, move |ctx| {
        let bridge = Arc::clone(&bridge);
        let target_node_id = target_node_id.clone();
        let ctx_for_thread = ctx.clone();
        let result = std::thread::spawn(move || {
            let mut bridge = bridge.lock().map_err(|_| NodeError::NodeAgentRpc {
                message: "node-agent bridge mutex poisoned".to_string(),
            })?;
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .map_err(|source| NodeError::Runtime { source })?;
            runtime.block_on(bridge.request(target_node_id, ctx_for_thread.clone()))
        })
        .join()
        .map_err(|_| handler_failed(&ctx))?;
        match result {
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
        if candidate.status() != CandidateStatus::Verified {
            continue;
        }
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            continue;
        };
        let Ok(request) = serde_json::from_slice::<NodeAgentRpcRequestFact>(payload.as_bytes())
        else {
            continue;
        };
        if validate_rpc_request_candidate(&candidate, &request, state).is_err() {
            continue;
        }
        if has_rpc_reply(&store, session, state, &request).await? {
            continue;
        }
        if now_ms() >= request.expires_at_ms {
            publish_rpc_reply(
                fact_node,
                session,
                author,
                &request,
                NodeAgentRpcReplyOutcome::Failure {
                    message: "expired node-agent rpc request".to_string(),
                },
            )
            .await?;
            handled += 1;
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
        requester_node_id: request.requester_node_id.clone(),
        target_node_id: request.target_node_id.clone(),
        subject: request.subject.clone(),
        payload_hash: request.payload_hash.clone(),
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
    request: &NodeAgentRpcRequestFact,
    deadline: Instant,
) -> NodeResult<NodeAgentRpcReplyFact> {
    let mut last_stream_refresh = Instant::now();
    loop {
        if let Some(reply) = read_rpc_reply(
            &fact_node.store(),
            session,
            PrincipalId::new(format!("node:{}", request.target_node_id.as_str())),
            request,
        )
        .await?
        {
            return Ok(reply);
        }
        if Instant::now() >= deadline {
            return Err(NodeError::NodeAgentRpc {
                message: format!("timed out waiting for reply to {}", request.request_id),
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
                if last_stream_refresh.elapsed() >= Duration::from_secs(1) {
                    fact_node
                        .refresh_stream()
                        .await
                        .map_err(|source| NodeError::Transport { source })?;
                    last_stream_refresh = Instant::now();
                }
                tokio::time::sleep(RPC_IDLE).await;
            }
            Err(source) if is_recoverable_stream_error(&source) => {
                fact_node
                    .refresh_stream()
                    .await
                    .map_err(|source| NodeError::Transport { source })?;
                last_stream_refresh = Instant::now();
                tokio::time::sleep(RPC_IDLE).await;
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
    state: &LoadedNodeState,
    request: &NodeAgentRpcRequestFact,
) -> NodeResult<bool> {
    read_rpc_reply(store, session, state.principal(), request)
        .await
        .map(|reply| reply.is_some())
}

async fn read_rpc_reply(
    store: &SharedPandaFactStore,
    session: &BusSession,
    expected_author: PrincipalId,
    request: &NodeAgentRpcRequestFact,
) -> NodeResult<Option<NodeAgentRpcReplyFact>> {
    let expected_key = rpc_reply_key(&request.requester_node_id, &request.request_id)?;
    let pattern =
        FactKeyPattern::parse(expected_key.as_str()).map_err(|source| NodeError::Bus { source })?;
    let candidates = store
        .list_fact_candidates(session.island(), &pattern, session)
        .await
        .map_err(|source| NodeError::FactSource { source })?;
    let payloads = store
        .read_fact_payloads(session.island(), &candidates, session)
        .await
        .map_err(|source| NodeError::FactSource { source })?;
    for candidate in candidates {
        if candidate.status() != CandidateStatus::Verified {
            continue;
        }
        if candidate.key() != &expected_key || candidate.author() != &expected_author {
            continue;
        }
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            continue;
        };
        if let Ok(reply) = serde_json::from_slice::<NodeAgentRpcReplyFact>(payload.as_bytes())
            && reply.request_id == request.request_id
            && reply.requester_node_id == request.requester_node_id
            && reply.target_node_id == request.target_node_id
            && reply.subject == request.subject
            && reply.payload_hash == request.payload_hash
        {
            return Ok(Some(reply));
        }
    }
    Ok(None)
}

fn validate_rpc_request_candidate(
    candidate: &mvp_projection::FactCandidate,
    request: &NodeAgentRpcRequestFact,
    state: &LoadedNodeState,
) -> NodeResult<()> {
    if request.target_node_id != state.node_id() {
        return Err(NodeError::NodeAgentRpc {
            message: format!(
                "request target '{}' did not match local node '{}'",
                request.target_node_id,
                state.node_id()
            ),
        });
    }
    let expected_key = rpc_request_key(&request.target_node_id, &request.request_id)?;
    if candidate.key() != &expected_key {
        return Err(NodeError::NodeAgentRpc {
            message: format!("request key did not match request '{}'", request.request_id),
        });
    }
    let expected_author = PrincipalId::new(format!("node:{}", request.requester_node_id.as_str()));
    if candidate.author() != &expected_author {
        return Err(NodeError::NodeAgentRpc {
            message: format!(
                "request author '{}' did not match requester '{}'",
                candidate.author(),
                expected_author
            ),
        });
    }
    if request.payload_hash != payload_hash(&request.payload) {
        return Err(NodeError::NodeAgentRpc {
            message: format!(
                "request payload hash did not match '{}'",
                request.request_id
            ),
        });
    }
    Ok(())
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

fn payload_hash(payload: &[u8]) -> String {
    blake3::hash(payload).to_hex().to_string()
}

fn now_ms() -> u64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    millis_u64(duration)
}

fn millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
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

#[cfg(test)]
mod tests {
    use mvp_bus::{FactContentHash, IslandId};
    use mvp_identity::NodeId;
    use mvp_projection::{CandidateStatus, FactCandidate, FactKind};

    use crate::{InitOptions, init_node};

    use super::{
        NodeAgentRpcRequestFact, payload_hash, rpc_request_key, validate_rpc_request_candidate,
    };

    #[test]
    fn rpc_request_candidate_must_be_authored_by_requester_principal() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = init_node(
            InitOptions::new(temp.path().join("node-b"))
                .with_island("prod")
                .with_node_id("node-b"),
        )
        .expect("init node");
        let payload = b"start".to_vec();
        let request = NodeAgentRpcRequestFact {
            request_id: "req-1".to_string(),
            requester_node_id: NodeId::new("node-a"),
            target_node_id: NodeId::new("node-b"),
            subject: "node.node-b.rpc.start".to_string(),
            payload_hash: payload_hash(&payload),
            expires_at_ms: u64::MAX,
            payload,
        };
        let candidate = FactCandidate::new(
            IslandId::new("prod"),
            rpc_request_key(&request.target_node_id, &request.request_id).expect("key"),
            mvp_bus::PrincipalId::new("node:node-c"),
            FactContentHash::new("b3:req"),
            FactKind::Unsupported,
            0,
            CandidateStatus::Verified,
        );

        let error = validate_rpc_request_candidate(&candidate, &request, &state)
            .expect_err("wrong author rejected");

        assert!(error.to_string().contains("request author"));
    }

    #[test]
    fn rpc_request_candidate_must_bind_key_and_payload_hash() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state = init_node(
            InitOptions::new(temp.path().join("node-b"))
                .with_island("prod")
                .with_node_id("node-b"),
        )
        .expect("init node");
        let request = NodeAgentRpcRequestFact {
            request_id: "req-1".to_string(),
            requester_node_id: NodeId::new("node-a"),
            target_node_id: NodeId::new("node-b"),
            subject: "node.node-b.rpc.start".to_string(),
            payload_hash: payload_hash(b"other"),
            expires_at_ms: u64::MAX,
            payload: b"start".to_vec(),
        };
        let candidate = FactCandidate::new(
            IslandId::new("prod"),
            rpc_request_key(&request.target_node_id, "different").expect("key"),
            mvp_bus::PrincipalId::new("node:node-a"),
            FactContentHash::new("b3:req"),
            FactKind::Unsupported,
            0,
            CandidateStatus::Verified,
        );

        let error = validate_rpc_request_candidate(&candidate, &request, &state)
            .expect_err("key mismatch rejected before payload execution");

        assert!(error.to_string().contains("request key"));
    }
}
