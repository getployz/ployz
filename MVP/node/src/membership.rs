use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mvp_bus::{BusAuthority, FactKey, FactKeyPattern, Grant, PrincipalId, harness::InMemoryBus};
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
use crate::state::{
    IssuedInviteRecord, LoadedNodeState, init_joined_node, load_issued_invite, load_node,
    record_bootstrap_peer, record_issued_invite as persist_issued_invite, write_node_ticket,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteToken {
    pub island_id: String,
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DaemonOptions {
    pub run_for: Duration,
    pub import_idle: Duration,
}

impl DaemonOptions {
    #[must_use]
    pub fn new(run_for: Duration) -> Self {
        Self {
            run_for,
            import_idle: Duration::from_millis(50),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonReport {
    pub node_id: String,
    pub ticket: String,
    pub imported_batches: u64,
    pub imported_operations: u64,
}

pub fn create_invite(state_dir: impl AsRef<std::path::Path>, ttl: Duration) -> NodeResult<String> {
    let state = load_node(state_dir)?;
    let author = state.author()?;
    let bootstrap_ticket = ensure_node_ticket(&state)?;
    let now = now_ms();
    let token = InviteToken {
        island_id: state.island_id().to_string(),
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
            None,
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
    let (mut fact_node, writer_session, author, authority) = spawn_fact_node(&state).await?;
    let ticket = ensure_node_ticket(&state)?;
    write_node_ticket(&state, &ticket)?;
    publish_self_join(&mut fact_node, &state, &writer_session, &author).await?;
    publish_admitted_peers(&mut fact_node, &state, &writer_session, &author).await?;
    let started = std::time::Instant::now();
    let mut last_advertised = started;
    let mut imported_batches = 0;
    let mut imported_operations = 0;
    while started.elapsed() < options.run_for {
        if last_advertised.elapsed() >= Duration::from_millis(100) {
            publish_self_join(&mut fact_node, &state, &writer_session, &author).await?;
            let current_state = load_node(state.paths().state_dir.clone())?;
            publish_admitted_peers(&mut fact_node, &current_state, &writer_session, &author)
                .await?;
            last_advertised = std::time::Instant::now();
        }
        match fact_node
            .import_next_fact_batch_with_idle_timeout(options.import_idle)
            .await
        {
            Ok(Some(outcomes)) => {
                imported_batches += 1;
                imported_operations += outcomes
                    .iter()
                    .filter(|outcome| matches!(outcome, PandaNetFactImportOutcome::Imported))
                    .count() as u64;
                apply_peer_admissions(&mut fact_node, &state, &writer_session, &authority).await?;
            }
            Ok(None) => {
                fact_node
                    .refresh_stream()
                    .await
                    .map_err(|source| NodeError::Transport { source })?;
            }
            Err(error) if is_recoverable_stream_error(&error) => {
                fact_node
                    .refresh_stream()
                    .await
                    .map_err(|source| NodeError::Transport { source })?;
            }
            Err(error) => return Err(NodeError::Transport { source: error }),
        }
    }
    Ok(DaemonReport {
        node_id: state.node_id_str().to_string(),
        ticket,
        imported_batches,
        imported_operations,
    })
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

async fn spawn_fact_node(
    state: &LoadedNodeState,
) -> NodeResult<(
    PandaNetFactNode,
    mvp_bus::BusSession,
    PandaFactAuthor,
    BusAuthority,
)> {
    let (raw_bus, authority) = InMemoryBus::new_with_authority();
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
mod tests {
    use std::time::Duration;

    use mvp_bus::{FactKeyPattern, Grant, IslandId, PrincipalId, harness::InMemoryBus};
    use mvp_p2panda_facts::{PandaFactStore, PandaSqliteOpenConfig, PandaTrustedAuthorKey};
    use mvp_projection::{FactSource, reduce_facts};

    use crate::{
        DaemonOptions, InitOptions, admit_joiner, create_admission_request, create_invite,
        init_node, join_from_token, load_node, load_node_ticket, run_daemon_once,
    };

    #[test]
    fn invite_creates_stable_ticket_without_daemon() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join("node-a");
        init_node(InitOptions::new(&state_dir)).expect("init node");

        let token = create_invite(&state_dir, Duration::from_secs(60)).expect("invite");
        let state = load_node(&state_dir).expect("load node");
        let ticket = load_node_ticket(&state).expect("load ticket");

        assert!(!token.is_empty());
        assert!(!ticket.is_empty());
    }

    #[tokio::test]
    async fn join_from_token_initializes_state_with_bootstrap_network_and_topic() {
        let temp = tempfile::tempdir().expect("tempdir");
        let node_a = temp.path().join("node-a");
        init_node(
            InitOptions::new(&node_a)
                .with_island("prod")
                .with_node_id("node-a"),
        )
        .expect("init node a");
        let token = create_invite(&node_a, Duration::from_secs(60)).expect("invite");
        let invite = super::InviteToken::decode(&token).expect("decode invite");

        let state = join_from_token(
            temp.path().join("node-b"),
            &token,
            Some("node-b".to_string()),
            super::now_ms(),
        )
        .expect("join state");

        assert_eq!(state.island_id(), "prod");
        assert_eq!(state.node_id_str(), "node-b");
        assert_eq!(
            state.p2panda_network_id_hex(),
            invite.p2panda_network_id_hex
        );
        assert_eq!(state.p2panda_topic_hex(), invite.p2panda_topic_hex);
    }

    #[tokio::test]
    async fn malformed_non_expired_join_token_does_not_poison_state_dir() {
        let temp = tempfile::tempdir().expect("tempdir");
        let node_a = temp.path().join("node-a");
        let node_b = temp.path().join("node-b");
        init_node(
            InitOptions::new(&node_a)
                .with_island("prod")
                .with_node_id("node-a"),
        )
        .expect("init node a");
        let token = create_invite(&node_a, Duration::from_secs(60)).expect("invite");
        let mut malformed = super::InviteToken::decode(&token).expect("decode invite");
        malformed.bootstrap_ticket = "not-a-ticket".to_string();
        let malformed = malformed.encode().expect("encode malformed invite");

        let error = join_from_token(
            &node_b,
            &malformed,
            Some("node-b".to_string()),
            super::now_ms(),
        )
        .expect_err("malformed invite fails");
        let joined = join_from_token(&node_b, &token, Some("node-b".to_string()), super::now_ms())
            .expect("valid invite still works");

        assert!(matches!(
            error,
            crate::NodeError::InvalidBootstrapTicket { .. }
        ));
        assert_eq!(joined.node_id_str(), "node-b");
    }

    #[test]
    fn expired_join_token_fails_before_state_init() {
        let temp = tempfile::tempdir().expect("tempdir");
        let token = super::InviteToken {
            island_id: "prod".to_string(),
            p2panda_network_id_hex: "01".repeat(32),
            p2panda_topic_hex: "02".repeat(32),
            bootstrap_ticket: "".to_string(),
            bootstrap_principal_id: "node:bootstrap".to_string(),
            bootstrap_author_key_hex: mvp_p2panda_facts::PandaFactAuthor::new(PrincipalId::new(
                "node:bootstrap",
            ))
            .author_key()
            .as_hex(),
            invite_id: "invite".to_string(),
            invite_secret: "secret".to_string(),
            expires_at_ms: 10,
        }
        .encode()
        .expect("encode token");

        let error = join_from_token(temp.path().join("node-b"), &token, None, 10)
            .expect_err("expired token fails");

        assert!(matches!(error, crate::NodeError::InviteExpired { .. }));
    }

    #[tokio::test]
    async fn three_admitted_product_daemons_converge_join_facts_over_p2panda_net() {
        let temp = tempfile::tempdir().expect("tempdir");
        let node_a = temp.path().join("node-a");
        let node_b = temp.path().join("node-b");
        let node_c = temp.path().join("node-c");
        init_node(
            InitOptions::new(&node_a)
                .with_island("prod")
                .with_node_id("node-a"),
        )
        .expect("init node a");
        let token_b = create_invite(&node_a, Duration::from_secs(60)).expect("invite b");
        join_from_token(
            &node_b,
            &token_b,
            Some("node-b".to_string()),
            super::now_ms(),
        )
        .expect("join node b");
        let admission_b = create_admission_request(&node_b).expect("admission request b");
        admit_joiner(&node_a, &admission_b, super::now_ms()).expect("admit node b");

        let token_c = create_invite(&node_a, Duration::from_secs(60)).expect("invite c");
        join_from_token(
            &node_c,
            &token_c,
            Some("node-c".to_string()),
            super::now_ms(),
        )
        .expect("join node c");
        let admission_c = create_admission_request(&node_c).expect("admission request c");
        admit_joiner(&node_a, &admission_c, super::now_ms()).expect("admit node c");

        let (a, b, c) = tokio::join!(
            run_daemon_once(&node_a, DaemonOptions::new(Duration::from_millis(3_000))),
            run_daemon_once(&node_b, DaemonOptions::new(Duration::from_millis(3_000))),
            run_daemon_once(&node_c, DaemonOptions::new(Duration::from_millis(3_000)))
        );
        let report_a = a.expect("node a daemon");
        let report_b = b.expect("node b daemon");
        let report_c = c.expect("node c daemon");

        let state_a = load_node(&node_a).expect("load node a");
        let state_b = load_node(&node_b).expect("load node b");
        let state_c = load_node(&node_c).expect("load node c");
        let trusted = &[state_a.clone(), state_b.clone(), state_c.clone()];
        let projected_a = projected_node_count(&node_a, trusted).await;
        let projected_b = projected_node_count(&node_b, trusted).await;
        let projected_c = projected_node_count(&node_c, trusted).await;

        assert_eq!(
            (projected_a, projected_b, projected_c),
            (3, 3, 3),
            "membership projection mismatch after reports: {report_a:?} {report_b:?} {report_c:?}"
        );
    }

    #[tokio::test]
    async fn admission_rejects_principal_that_does_not_match_node_id() {
        let temp = tempfile::tempdir().expect("tempdir");
        let node_a = temp.path().join("node-a");
        let node_b = temp.path().join("node-b");
        init_node(
            InitOptions::new(&node_a)
                .with_island("prod")
                .with_node_id("node-a"),
        )
        .expect("init node a");
        let token = create_invite(&node_a, Duration::from_secs(60)).expect("invite");
        join_from_token(&node_b, &token, Some("node-b".to_string()), super::now_ms())
            .expect("join node b");
        let admission = create_admission_request(&node_b).expect("admission request");
        let mut request = super::AdmissionRequest::decode(&admission).expect("decode admission");
        request.principal_id = "node:other".to_string();
        let admission = request.encode().expect("encode admission");

        let error = admit_joiner(&node_a, &admission, super::now_ms())
            .expect_err("principal mismatch fails");

        assert!(matches!(
            error,
            crate::NodeError::InvalidAdmissionPrincipal { .. }
        ));
    }

    #[tokio::test]
    async fn admission_rejects_conflicting_existing_peer_identity() {
        let temp = tempfile::tempdir().expect("tempdir");
        let node_a = temp.path().join("node-a");
        let node_b = temp.path().join("node-b");
        let node_c = temp.path().join("node-c");
        init_node(
            InitOptions::new(&node_a)
                .with_island("prod")
                .with_node_id("node-a"),
        )
        .expect("init node a");
        let token_b = create_invite(&node_a, Duration::from_secs(60)).expect("invite b");
        join_from_token(
            &node_b,
            &token_b,
            Some("node-b".to_string()),
            super::now_ms(),
        )
        .expect("join node b");
        let admission_b = create_admission_request(&node_b).expect("admission b");
        admit_joiner(&node_a, &admission_b, super::now_ms()).expect("admit node b");

        let token_c = create_invite(&node_a, Duration::from_secs(60)).expect("invite c");
        join_from_token(
            &node_c,
            &token_c,
            Some("node-c".to_string()),
            super::now_ms(),
        )
        .expect("join node c");
        let admission_c = create_admission_request(&node_c).expect("admission c");
        let mut conflicting =
            super::AdmissionRequest::decode(&admission_c).expect("decode admission c");
        conflicting.node_id = "node-b".to_string();
        conflicting.principal_id = "node:node-b".to_string();
        let conflicting = conflicting.encode().expect("encode conflicting admission");

        let error = admit_joiner(&node_a, &conflicting, super::now_ms())
            .expect_err("conflicting peer fails");

        assert!(matches!(
            error,
            crate::NodeError::AdmissionPeerConflict { .. }
        ));
    }

    async fn projected_node_count(
        path: &std::path::Path,
        trusted_states: &[crate::LoadedNodeState],
    ) -> usize {
        let state = load_node(path).expect("load node");
        let (raw_bus, authority) = InMemoryBus::new_with_authority();
        let session = authority.grant_in(
            IslandId::new(state.island_id()),
            PrincipalId::new("projection"),
            Grant::empty().with_fact_read(
                FactKeyPattern::parse("/facts/node/>").expect("valid fact pattern"),
            ),
        );
        for trusted in trusted_states {
            authority.grant_in(
                trusted.island(),
                trusted.principal(),
                Grant::empty()
                    .with_fact_write(
                        FactKeyPattern::parse("/facts/>").expect("valid product fact grant"),
                    )
                    .with_fact_read(
                        FactKeyPattern::parse("/facts/>").expect("valid product fact grant"),
                    ),
            );
        }
        let mut store_config = PandaSqliteOpenConfig::new(
            state.paths().fact_store.clone(),
            vec![IslandId::new(state.island_id())],
        );
        for trusted in trusted_states {
            let author = trusted.author().expect("trusted node author");
            store_config = store_config.with_trusted_author_key(PandaTrustedAuthorKey::new(
                trusted.island(),
                trusted.principal(),
                author.author_key(),
            ));
        }
        let store = PandaFactStore::open_sqlite(std::sync::Arc::new(raw_bus), store_config)
            .await
            .expect("open projection store");
        let pattern = FactKeyPattern::parse("/facts/node/>").expect("node pattern");
        let candidates = store
            .list_candidates(&state.island(), &pattern, &session)
            .expect("list candidates");
        let payloads = store
            .read_payloads(&state.island(), &candidates, &session)
            .expect("read payloads");
        reduce_facts(&state.island(), &candidates, &payloads)
            .nodes
            .len()
    }
}
