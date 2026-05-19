use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mvp_bus::{BusAuthority, FactKey, FactKeyPattern, Grant, PrincipalId, local::LocalBus};
use mvp_mesh::{IrohEndpointId, MeshError, WireGuardPublicKey, joined_fact_key};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactAuthorKey, PandaFactStore, PandaSqliteOpenConfig,
    PandaTrustedAuthorKey, SharedPandaFactStore,
};
use mvp_p2panda_transport::{
    PandaNetBindConfig, PandaNetFactImportOutcome, PandaNetFactNode, PandaNetFactNodeConfig,
    PandaNetNodeConfig, PandaNetNodeTicket, PandaNetTransportError,
};
use mvp_projection::{
    NodeJoinedFact, PeerAdmittedFact, ProjectionFactPayload, payload_matches_key,
};
use serde::{Deserialize, Serialize};

use crate::config::{BootstrapPeerConfig, JoinedInitOptions};
use crate::error::{NodeError, NodeResult};
use crate::node_agent::{node_agent_grant, node_agent_request_grant, register_node_agent_services};
use crate::node_agent_rpc::{
    handle_node_agent_rpc_requests, register_remote_node_agent_bridges_with_fact_node,
};
use crate::state::{
    IssuedInviteRecord, LoadedNodeState, init_joined_node, load_issued_invite, load_node,
    record_bootstrap_peer, record_issued_invite as persist_issued_invite, write_node_ticket,
};
use daemon_control::{DaemonControlTaskOptions, start_daemon_control_task};

mod daemon_control;
mod daemon_control_protocol;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteToken {
    pub island_id: String,
    #[serde(default)]
    pub bootstrap_node_id: Option<String>,
    pub p2panda_network_id_hex: String,
    pub p2panda_topic_hex: String,
    pub bootstrap_ticket: String,
    pub bootstrap_principal_id: String,
    pub bootstrap_author_key_hex: String,
    pub invite_id: String,
    pub invite_secret: String,
    pub expires_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdmissionRequest {
    pub island_id: String,
    pub node_id: String,
    pub principal_id: String,
    pub p2panda_ticket: String,
    pub author_key_hex: String,
    pub wg_public_key: String,
    pub wg_overlay_ip: String,
    pub invite_id: String,
    pub invite_secret: String,
    pub invite_expires_at_ms: u64,
}

impl AdmissionRequest {
    pub fn encode(&self) -> NodeResult<String> {
        serde_json::to_string(self).map_err(|source| NodeError::EncodeAdmissionRequest { source })
    }

    pub fn decode(value: &str) -> NodeResult<Self> {
        serde_json::from_str(value).map_err(|source| NodeError::DecodeAdmissionRequest { source })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AdmissionReport {
    pub node_id: String,
    pub principal_id: String,
}

impl InviteToken {
    pub fn encode(&self) -> NodeResult<String> {
        serde_json::to_string(self).map_err(|source| NodeError::EncodeInviteToken { source })
    }

    pub fn decode(value: &str) -> NodeResult<Self> {
        serde_json::from_str(value).map_err(|source| NodeError::DecodeInviteToken { source })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonOptions {
    pub run_for: Duration,
    pub import_idle: Duration,
    pub control_socket: Option<PathBuf>,
}

impl DaemonOptions {
    #[must_use]
    pub fn new(run_for: Duration) -> Self {
        Self {
            run_for,
            import_idle: Duration::from_millis(50),
            control_socket: None,
        }
    }

    #[must_use]
    pub fn with_control_socket(mut self, control_socket: impl Into<PathBuf>) -> Self {
        self.control_socket = Some(control_socket.into());
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonReport {
    pub node_id: String,
    pub ticket: String,
    pub imported_batches: u64,
    pub imported_operations: u64,
    pub node_agent_handlers: usize,
}

pub fn create_invite(state_dir: impl AsRef<std::path::Path>, ttl: Duration) -> NodeResult<String> {
    let state = load_node(state_dir)?;
    let author = state.author()?;
    let bootstrap_ticket = ensure_node_ticket(&state)?;
    let now = now_ms();
    let token = InviteToken {
        island_id: state.island_id().to_string(),
        bootstrap_node_id: Some(state.node_id_str().to_string()),
        p2panda_network_id_hex: state.p2panda_network_id_hex().to_string(),
        p2panda_topic_hex: state.p2panda_topic_hex().to_string(),
        bootstrap_ticket,
        bootstrap_principal_id: state.principal_id().to_string(),
        bootstrap_author_key_hex: author.author_key().as_hex(),
        invite_id: format!("invite-{}", stable_suffix(now, state.node_id_str())),
        invite_secret: stable_suffix(now, state.principal_id()),
        expires_at_ms: now + millis_u64(ttl),
    };
    record_invite_token(&state, &token)?;
    token.encode()
}

pub fn join_from_token(
    state_dir: impl Into<std::path::PathBuf>,
    token: &str,
    node_id: Option<String>,
    now_ms: u64,
) -> NodeResult<LoadedNodeState> {
    let token = InviteToken::decode(token)?;
    if now_ms >= token.expires_at_ms {
        return Err(NodeError::InviteExpired {
            expires_at_ms: token.expires_at_ms,
            now_ms,
        });
    }
    let state = init_joined_node(JoinedInitOptions {
        state_dir: state_dir.into(),
        island: token.island_id,
        node_id: node_id.unwrap_or_else(|| format!("node-{}", stable_suffix(now_ms, "join"))),
        invite_id: token.invite_id.clone(),
        invite_secret: token.invite_secret,
        invite_expires_at_ms: token.expires_at_ms,
        p2panda_network_id_hex: token.p2panda_network_id_hex,
        p2panda_topic_hex: token.p2panda_topic_hex,
        bootstrap_peer: BootstrapPeerConfig::new(
            token.bootstrap_node_id,
            token.bootstrap_principal_id,
            token.bootstrap_author_key_hex,
            token.bootstrap_ticket,
            Some(token.invite_id.clone()),
        ),
    })?;
    Ok(state)
}

pub fn create_admission_request(state_dir: impl AsRef<std::path::Path>) -> NodeResult<String> {
    let state = load_node(state_dir)?;
    let join_invite = state.join_invite().ok_or(NodeError::MissingJoinInvite)?;
    let author = state.author()?;
    let p2panda_ticket = ensure_node_ticket(&state)?;
    AdmissionRequest {
        island_id: state.island_id().to_string(),
        node_id: state.node_id_str().to_string(),
        principal_id: state.principal_id().to_string(),
        p2panda_ticket,
        author_key_hex: author.author_key().as_hex(),
        wg_public_key: state.wireguard_public_key().to_string(),
        wg_overlay_ip: state.wireguard_overlay_ip().to_string(),
        invite_id: join_invite.invite_id.clone(),
        invite_secret: join_invite.invite_secret.clone(),
        invite_expires_at_ms: join_invite.expires_at_ms,
    }
    .encode()
}

pub fn admit_joiner(
    state_dir: impl AsRef<std::path::Path>,
    request_json: &str,
    now_ms: u64,
) -> NodeResult<AdmissionReport> {
    let state = load_node(state_dir.as_ref())?;
    let request = AdmissionRequest::decode(request_json)?;
    if request.island_id != state.island_id() {
        return Err(NodeError::AdmissionIslandMismatch {
            local_island: state.island_id().to_string(),
            request_island: request.island_id,
        });
    }
    let expected_principal = format!("node:{}", request.node_id);
    if request.principal_id != expected_principal {
        return Err(NodeError::InvalidAdmissionPrincipal {
            node_id: request.node_id,
            principal_id: request.principal_id,
            expected: expected_principal,
        });
    }
    if now_ms >= request.invite_expires_at_ms {
        return Err(NodeError::InviteExpired {
            expires_at_ms: request.invite_expires_at_ms,
            now_ms,
        });
    }
    let issued = load_issued_invite(&state, &request.invite_id)?;
    if now_ms >= issued.expires_at_ms {
        return Err(NodeError::InviteExpired {
            expires_at_ms: issued.expires_at_ms,
            now_ms,
        });
    }
    if issued.invite_secret != request.invite_secret {
        return Err(NodeError::InviteSecretMismatch {
            invite_id: request.invite_id,
        });
    }
    let node_id = request.node_id;
    let principal_id = request.principal_id;
    let admitted = record_bootstrap_peer(
        state.paths().state_dir.clone(),
        BootstrapPeerConfig::new(
            Some(node_id.clone()),
            principal_id.clone(),
            request.author_key_hex,
            request.p2panda_ticket,
            Some(request.invite_id),
        ),
    )?;
    Ok(AdmissionReport {
        node_id,
        principal_id: admitted
            .trusted_fact_authors()?
            .into_iter()
            .find(|trusted| trusted.principal().as_str() == principal_id)
            .map(|trusted| trusted.principal().to_string())
            .unwrap_or(principal_id),
    })
}

fn record_invite_token(state: &LoadedNodeState, token: &InviteToken) -> NodeResult<()> {
    let issued = IssuedInviteRecord {
        invite_id: token.invite_id.clone(),
        invite_secret: token.invite_secret.clone(),
        expires_at_ms: token.expires_at_ms,
    };
    persist_issued_invite(state, &issued)
}

pub async fn run_daemon_once(
    state_dir: impl AsRef<std::path::Path>,
    options: DaemonOptions,
) -> NodeResult<DaemonReport> {
    let state = load_node(state_dir)?;
    let (product_bus, product_authority, _raw_product_bus) = mvp_bus::local::actor_with_authority();
    let operator_session = product_authority.grant_in(
        state.island(),
        PrincipalId::new(format!("daemon-operator:{}", state.node_id_str())),
        Grant::allow_all(),
    );
    let node_agent_session = product_authority.grant_in(
        state.island(),
        state.principal(),
        node_agent_grant(state.node_id_str())?,
    );
    let node_agent_request_session = product_authority.grant_in(
        state.island(),
        PrincipalId::new(format!("node-agent-rpc:{}", state.node_id_str())),
        node_agent_request_grant(),
    );
    let (_node_agent, node_agent_report) =
        register_node_agent_services(&product_bus, &node_agent_session, &state).await?;
    let (fact_node_inner, writer_session, author, authority) = spawn_fact_node(&state).await?;
    let fact_node = Arc::new(tokio::sync::Mutex::new(fact_node_inner));
    let ticket = ensure_node_ticket(&state)?;
    write_node_ticket(&state, &ticket)?;
    {
        let mut fact_node = fact_node.lock().await;
        publish_self_join(&mut fact_node, &state, &writer_session, &author).await?;
        publish_admitted_peers(&mut fact_node, &state, &writer_session, &author).await?;
    }
    let mut remote_bridges = register_remote_node_agent_bridges_with_fact_node(
        &product_bus,
        &operator_session,
        &state,
        Arc::clone(&fact_node),
        writer_session.clone(),
        state.author()?,
    )
    .await?;
    let local_fact_store = {
        let fact_node = fact_node.lock().await;
        fact_node.store()
    };
    let deploy_facts = remote_bridges
        .as_ref()
        .map(|bridges| bridges.store.clone())
        .unwrap_or(local_fact_store);
    let deploy_fact_session = remote_bridges
        .as_ref()
        .map(|bridges| bridges.fact_session.clone())
        .unwrap_or_else(|| writer_session.clone());
    let control = match &options.control_socket {
        Some(path) => Some(start_daemon_control_task(DaemonControlTaskOptions {
            socket_path: path.clone(),
            state: state.clone(),
            product_bus: product_bus.clone(),
            operator_session: operator_session.clone(),
            facts: deploy_facts.clone(),
            fact_session: deploy_fact_session.clone(),
            node_agent_handlers: node_agent_report.registered_handlers,
        })?),
        None => None,
    };
    let started = std::time::Instant::now();
    let mut last_advertised = started;
    let mut last_stream_refresh = started;
    let mut imported_batches = 0;
    let mut imported_operations = 0;
    while started.elapsed() < options.run_for {
        if last_advertised.elapsed() >= Duration::from_millis(100) {
            {
                let mut fact_node = fact_node.lock().await;
                publish_self_join(&mut fact_node, &state, &writer_session, &author).await?;
            }
            let current_state = load_node(state.paths().state_dir.clone())?;
            {
                let mut fact_node = fact_node.lock().await;
                publish_admitted_peers(&mut fact_node, &current_state, &writer_session, &author)
                    .await?;
            }
            last_advertised = std::time::Instant::now();
        }
        let import_result = {
            let mut fact_node = fact_node.lock().await;
            fact_node
                .import_next_fact_batch_with_idle_timeout(options.import_idle)
                .await
        };
        match import_result {
            Ok(Some(outcomes)) => {
                imported_batches += 1;
                imported_operations += outcomes
                    .iter()
                    .filter(|outcome| matches!(outcome, PandaNetFactImportOutcome::Imported))
                    .count() as u64;
                if let Some(control) = &control {
                    control.record_import_progress(imported_batches, imported_operations);
                }
                let current_state = load_node(state.paths().state_dir.clone())?;
                {
                    let mut fact_node = fact_node.lock().await;
                    apply_peer_admissions(
                        &mut fact_node,
                        &current_state,
                        &writer_session,
                        &authority,
                    )
                    .await?;
                }
                let current_state = load_node(state.paths().state_dir.clone())?;
                ensure_remote_bridges_registered(
                    &product_bus,
                    &operator_session,
                    &current_state,
                    Arc::clone(&fact_node),
                    &writer_session,
                    &mut remote_bridges,
                )
                .await?;
                {
                    let mut fact_node = fact_node.lock().await;
                    let _handled_rpc = handle_node_agent_rpc_requests(
                        &mut fact_node,
                        &writer_session,
                        &author,
                        &product_bus,
                        &node_agent_request_session,
                        &current_state,
                    )
                    .await?;
                }
            }
            Ok(None) => {
                if last_stream_refresh.elapsed() >= Duration::from_secs(1) {
                    let mut fact_node = fact_node.lock().await;
                    fact_node
                        .refresh_stream()
                        .await
                        .map_err(|source| NodeError::Transport { source })?;
                    last_stream_refresh = std::time::Instant::now();
                }
                let current_state = load_node(state.paths().state_dir.clone())?;
                ensure_remote_bridges_registered(
                    &product_bus,
                    &operator_session,
                    &current_state,
                    Arc::clone(&fact_node),
                    &writer_session,
                    &mut remote_bridges,
                )
                .await?;
                {
                    let mut fact_node = fact_node.lock().await;
                    let _handled_rpc = handle_node_agent_rpc_requests(
                        &mut fact_node,
                        &writer_session,
                        &author,
                        &product_bus,
                        &node_agent_request_session,
                        &current_state,
                    )
                    .await?;
                }
            }
            Err(error) if is_recoverable_stream_error(&error) => {
                let mut fact_node = fact_node.lock().await;
                fact_node
                    .refresh_stream()
                    .await
                    .map_err(|source| NodeError::Transport { source })?;
                last_stream_refresh = std::time::Instant::now();
            }
            Err(error) => return Err(NodeError::Transport { source: error }),
        }
    }
    if let Some(control) = control {
        control.shutdown().await?;
    }
    Ok(DaemonReport {
        node_id: state.node_id_str().to_string(),
        ticket,
        imported_batches,
        imported_operations,
        node_agent_handlers: node_agent_report.registered_handlers,
    })
}

async fn ensure_remote_bridges_registered(
    product_bus: &mvp_bus::BusActorHandle,
    operator_session: &mvp_bus::BusSession,
    current_state: &LoadedNodeState,
    fact_node: Arc<tokio::sync::Mutex<PandaNetFactNode>>,
    writer_session: &mvp_bus::BusSession,
    remote_bridges: &mut Option<crate::node_agent_rpc::RemoteNodeAgentBridgeSet>,
) -> NodeResult<()> {
    if let Some(bridges) = remote_bridges {
        return bridges
            .register_missing(product_bus, operator_session, current_state)
            .await;
    }
    *remote_bridges = register_remote_node_agent_bridges_with_fact_node(
        product_bus,
        operator_session,
        current_state,
        fact_node,
        writer_session.clone(),
        current_state.author()?,
    )
    .await?;
    Ok(())
}

fn is_recoverable_stream_error(error: &PandaNetTransportError) -> bool {
    matches!(
        error,
        PandaNetTransportError::StreamEnded { .. }
            | PandaNetTransportError::StreamLagged { .. }
            | PandaNetTransportError::StreamFailed { .. }
    )
}

async fn publish_admitted_peers(
    fact_node: &mut PandaNetFactNode,
    state: &LoadedNodeState,
    session: &mvp_bus::BusSession,
    author: &PandaFactAuthor,
) -> NodeResult<()> {
    for peer in state.admitted_peers() {
        let Some(node_id) = peer.node_id.clone() else {
            continue;
        };
        let Some(invite_id) = peer.invite_id.clone() else {
            continue;
        };
        let fact_key = admitted_peer_fact_key(&node_id, 1)?;
        let fact_payload = ProjectionFactPayload::PeerAdmitted(PeerAdmittedFact {
            node_id: mvp_identity::NodeId::new(node_id),
            principal_id: peer.principal_id,
            author_key_hex: peer.author_key_hex,
            p2panda_ticket: peer.p2panda_ticket,
            invite_id,
            epoch: 1,
        })
        .to_fact_bytes()
        .map(Into::into)
        .map_err(|error| NodeError::Mesh {
            source: MeshError::Backend {
                operation: "serialize admitted peer fact",
                message: error.to_string(),
            },
        })?;
        fact_node
            .publish_fact_payload(session, author, fact_key, fact_payload)
            .await
            .map_err(|source| NodeError::Transport { source })?;
    }
    Ok(())
}

async fn apply_peer_admissions(
    fact_node: &mut PandaNetFactNode,
    state: &LoadedNodeState,
    session: &mvp_bus::BusSession,
    authority: &BusAuthority,
) -> NodeResult<()> {
    let store = fact_node.store();
    let pattern = FactKeyPattern::parse("/facts/peer/>").expect("valid peer fact pattern");
    let candidates = store
        .list_fact_candidates(&state.island(), &pattern, session)
        .await
        .map_err(|source| NodeError::FactSource { source })?;
    let payloads = store
        .read_fact_payloads(&state.island(), &candidates, session)
        .await
        .map_err(|source| NodeError::FactSource { source })?;
    for candidate in candidates {
        if candidate.status() != mvp_projection::CandidateStatus::Verified {
            continue;
        }
        if !admission_authorized(state, candidate.author()) {
            continue;
        }
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            continue;
        };
        let Ok(ProjectionFactPayload::PeerAdmitted(fact)) =
            ProjectionFactPayload::from_fact_bytes(payload.as_bytes())
        else {
            continue;
        };
        let fact_payload = ProjectionFactPayload::PeerAdmitted(fact.clone());
        if !payload_matches_key(&candidate, &fact_payload) {
            continue;
        }
        if fact.node_id == state.node_id() {
            continue;
        }
        let peer = BootstrapPeerConfig::new(
            Some(fact.node_id.as_str().to_string()),
            fact.principal_id.clone(),
            fact.author_key_hex.clone(),
            fact.p2panda_ticket.clone(),
            Some(fact.invite_id.clone()),
        );
        let updated = record_bootstrap_peer(state.paths().state_dir.clone(), peer.clone())?;
        install_peer_runtime(fact_node, &updated, authority, &peer).await?;
    }
    Ok(())
}

fn admission_authorized(state: &LoadedNodeState, author: &PrincipalId) -> bool {
    if author == &state.principal() {
        return true;
    }
    state
        .bootstrap_peers()
        .into_iter()
        .filter(|peer| peer.invite_id.is_some())
        .any(|peer| PrincipalId::new(peer.principal_id) == *author)
}

async fn install_peer_runtime(
    fact_node: &PandaNetFactNode,
    state: &LoadedNodeState,
    authority: &BusAuthority,
    peer: &BootstrapPeerConfig,
) -> NodeResult<()> {
    authority.grant_in(
        state.island(),
        PrincipalId::new(peer.principal_id.clone()),
        product_fact_grant(),
    );
    let author_key = PandaFactAuthorKey::parse_hex(&peer.author_key_hex)
        .map_err(|source| NodeError::InvalidAuthorKey { source })?;
    fact_node
        .store()
        .trust_author_key(
            &state.island(),
            PrincipalId::new(peer.principal_id.clone()),
            author_key,
        )
        .await
        .map_err(|source| NodeError::FactStore { source })?;
    let node_info = PandaNetNodeTicket::parse(&peer.p2panda_ticket)
        .map_err(|source| NodeError::InvalidBootstrapTicket { source })?
        .into_node_info()
        .map_err(|source| NodeError::InvalidBootstrapTicket { source })?;
    fact_node
        .add_node_info(node_info)
        .await
        .map_err(|source| NodeError::Transport { source })
}

fn admitted_peer_fact_key(node_id: &str, epoch: u64) -> NodeResult<FactKey> {
    FactKey::parse(format!("/facts/peer/{node_id}/admitted/{epoch}")).map_err(|error| {
        NodeError::Mesh {
            source: MeshError::Backend {
                operation: "build admitted peer fact key",
                message: error.to_string(),
            },
        }
    })
}

fn product_fact_grant() -> Grant {
    Grant::empty()
        .with_fact_write(FactKeyPattern::parse("/facts/>").expect("valid product fact grant"))
        .with_fact_read(FactKeyPattern::parse("/facts/>").expect("valid product fact grant"))
}

fn ensure_node_ticket(state: &LoadedNodeState) -> NodeResult<String> {
    let ticket = PandaNetNodeTicket::from_socket_addr(
        state.p2panda_node_seed()?,
        SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, state.p2panda_port())),
    )
    .map_err(|source| NodeError::InvalidBootstrapTicket { source })?
    .as_str()
    .to_string();
    write_node_ticket(state, &ticket)?;
    Ok(ticket)
}

pub(crate) async fn spawn_fact_node(
    state: &LoadedNodeState,
) -> NodeResult<(
    PandaNetFactNode,
    mvp_bus::BusSession,
    PandaFactAuthor,
    BusAuthority,
)> {
    let (raw_bus, authority) = LocalBus::new_with_authority();
    let trusted_authors = state.trusted_fact_authors()?;
    let writer_session =
        authority.grant_in(state.island(), state.principal(), product_fact_grant());
    let replica_session = authority.grant_in(
        writer_session.island().clone(),
        PrincipalId::new(format!("{}:replica", writer_session.principal().as_str())),
        Grant::empty()
            .with_fact_read(FactKeyPattern::parse("/facts/>").expect("valid product fact grant")),
    );
    for trusted in &trusted_authors {
        authority.grant_in(
            state.island(),
            trusted.principal().clone(),
            product_fact_grant(),
        );
    }
    let author = state.author()?;
    let mut store_config =
        PandaSqliteOpenConfig::new(state.paths().fact_store.clone(), vec![state.island()])
            .with_trusted_author_key(PandaTrustedAuthorKey::new(
                state.island(),
                state.principal(),
                author.author_key(),
            ));
    for trusted in &trusted_authors {
        store_config = store_config.with_trusted_author_key(PandaTrustedAuthorKey::new(
            state.island(),
            trusted.principal().clone(),
            trusted.author_key(),
        ));
    }
    let store = PandaFactStore::open_sqlite(Arc::new(raw_bus), store_config)
        .await
        .map_err(|source| NodeError::FactStore { source })?;
    let shared = SharedPandaFactStore::new(store);
    shared
        .trust_author_key(
            writer_session.island(),
            writer_session.principal().clone(),
            author.author_key(),
        )
        .await
        .map_err(|source| NodeError::FactStore { source })?;
    for trusted in &trusted_authors {
        shared
            .trust_author_key(
                writer_session.island(),
                trusted.principal().clone(),
                trusted.author_key(),
            )
            .await
            .map_err(|source| NodeError::FactStore { source })?;
    }
    shared
        .trust_replica_peer(
            replica_session.island(),
            replica_session.principal().clone(),
        )
        .await;
    let bootstrap_nodes = state
        .bootstrap_tickets()?
        .into_iter()
        .map(PandaNetNodeTicket::into_node_info)
        .collect::<Result<Vec<_>, _>>()
        .map_err(|source| NodeError::InvalidBootstrapTicket { source })?;
    let node_config = PandaNetNodeConfig::localhost(
        state.p2panda_network_id()?,
        state.p2panda_node_seed()?,
        PandaNetBindConfig::localhost(state.p2panda_port(), state.p2panda_port()),
        bootstrap_nodes,
    );
    let fact_node = PandaNetFactNode::spawn(PandaNetFactNodeConfig::new(
        node_config,
        state.p2panda_topic()?,
        shared,
        replica_session,
    ))
    .await
    .map_err(|source| NodeError::Transport { source })?;
    Ok((fact_node, writer_session, author, authority))
}

async fn publish_self_join(
    fact_node: &mut PandaNetFactNode,
    state: &LoadedNodeState,
    session: &mvp_bus::BusSession,
    author: &PandaFactAuthor,
) -> NodeResult<()> {
    let fact_key =
        joined_fact_key(&state.node_id(), 1).map_err(|source| NodeError::Mesh { source })?;
    let fact_payload = ProjectionFactPayload::NodeJoined(NodeJoinedFact {
        node_id: state.node_id(),
        epoch: 1,
        overlay_ip: state.wireguard_overlay_ip().to_string(),
        iroh_endpoint_id: IrohEndpointId::new(state.node_id_str()).to_string(),
        wg_public_key: WireGuardPublicKey::new(state.wireguard_public_key()).to_string(),
    })
    .to_fact_bytes()
    .map(Into::into)
    .map_err(|error| NodeError::Mesh {
        source: MeshError::Backend {
            operation: "serialize self joined fact",
            message: error.to_string(),
        },
    })?;
    fact_node
        .publish_fact_payload(session, author, fact_key, fact_payload)
        .await
        .map_err(|source| NodeError::Transport { source })?;
    Ok(())
}

pub fn now_ms() -> u64 {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    millis_u64(duration)
}

fn millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn stable_suffix(now_ms: u64, value: &str) -> String {
    let hash = blake3::hash(format!("{now_ms}:{value}").as_bytes());
    hash.to_hex()[..16].to_string()
}

#[cfg(test)]
mod tests;
