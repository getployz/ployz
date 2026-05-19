use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mvp_bus::{FactKeyPattern, Grant, PrincipalId, harness::InMemoryBus};
use mvp_mesh::{IrohEndpointId, MeshError, WireGuardPublicKey, joined_fact_key};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactStore, PandaSqliteOpenConfig, PandaTrustedAuthorKey,
    SharedPandaFactStore,
};
use mvp_p2panda_transport::{
    PandaNetFactImportOutcome, PandaNetFactNode, PandaNetFactNodeConfig, PandaNetNodeConfig,
    PandaNetNodeTicket,
};
use mvp_projection::{NodeJoinedFact, ProjectionFactPayload};
use serde::{Deserialize, Serialize};

use crate::config::{JoinedInitOptions, TrustedFactAuthorConfig};
use crate::error::{NodeError, NodeResult};
use crate::state::{
    LoadedNodeState, init_joined_node, load_node, load_node_ticket, write_node_ticket,
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
    let bootstrap_ticket = load_node_ticket(&state).map_err(|error| match error {
        NodeError::ReadState { .. } => NodeError::MissingNodeTicket,
        other => other,
    })?;
    let now = now_ms();
    InviteToken {
        island_id: state.island_id().to_string(),
        p2panda_network_id_hex: state.p2panda_network_id_hex().to_string(),
        p2panda_topic_hex: state.p2panda_topic_hex().to_string(),
        bootstrap_ticket,
        bootstrap_principal_id: state.principal_id().to_string(),
        bootstrap_author_key_hex: author.author_key().as_hex(),
        invite_id: format!("invite-{}", stable_suffix(now, state.node_id_str())),
        invite_secret: stable_suffix(now, state.principal_id()),
        expires_at_ms: now + millis_u64(ttl),
    }
    .encode()
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
    let state = init_joined_node(JoinedInitOptions::new(
        state_dir,
        token.island_id,
        node_id.unwrap_or_else(|| format!("node-{}", stable_suffix(now_ms, "join"))),
        token.p2panda_network_id_hex,
        token.p2panda_topic_hex,
        token.bootstrap_ticket,
        TrustedFactAuthorConfig::new(token.bootstrap_principal_id, token.bootstrap_author_key_hex),
    ))?;
    Ok(state)
}

pub async fn run_daemon_once(
    state_dir: impl AsRef<std::path::Path>,
    options: DaemonOptions,
) -> NodeResult<DaemonReport> {
    let state = load_node(state_dir)?;
    let (mut fact_node, writer_session, author) = spawn_fact_node(&state).await?;
    let ticket = fact_node
        .node_info()
        .to_ticket()
        .map_err(|source| NodeError::InvalidBootstrapTicket { source })?
        .as_str()
        .to_string();
    write_node_ticket(&state, &ticket)?;
    publish_self_join(&mut fact_node, &state, &writer_session, &author).await?;
    let started = std::time::Instant::now();
    let mut last_advertised = started;
    let mut imported_batches = 0;
    let mut imported_operations = 0;
    while started.elapsed() < options.run_for {
        if last_advertised.elapsed() >= Duration::from_millis(100) {
            publish_self_join(&mut fact_node, &state, &writer_session, &author).await?;
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
            }
            Ok(None) => {
                let _ = fact_node.refresh_stream().await;
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

async fn spawn_fact_node(
    state: &LoadedNodeState,
) -> NodeResult<(PandaNetFactNode, mvp_bus::BusSession, PandaFactAuthor)> {
    let (raw_bus, authority) = InMemoryBus::new_with_authority();
    let fact_grant = || {
        Grant::empty()
            .with_fact_write(FactKeyPattern::parse("/facts/>").expect("valid product fact grant"))
            .with_fact_read(FactKeyPattern::parse("/facts/>").expect("valid product fact grant"))
    };
    let trusted_authors = state.trusted_fact_authors()?;
    let writer_session = authority.grant_in(state.island(), state.principal(), fact_grant());
    let replica_session = authority.grant_in(
        writer_session.island().clone(),
        PrincipalId::new(format!("{}:replica", writer_session.principal().as_str())),
        Grant::empty()
            .with_fact_read(FactKeyPattern::parse("/facts/>").expect("valid product fact grant")),
    );
    for trusted in &trusted_authors {
        authority.grant_in(state.island(), trusted.principal().clone(), fact_grant());
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
    let node_config = PandaNetNodeConfig::localhost_ephemeral(
        state.p2panda_network_id()?,
        state.p2panda_node_seed()?,
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
    Ok((fact_node, writer_session, author))
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
        DaemonOptions, InitOptions, create_invite, init_node, join_from_token, load_node,
        run_daemon_once,
    };

    #[test]
    fn invite_requires_daemon_ticket() {
        let temp = tempfile::tempdir().expect("tempdir");
        let state_dir = temp.path().join("node-a");
        init_node(InitOptions::new(&state_dir)).expect("init node");

        let error = create_invite(&state_dir, Duration::from_secs(60)).expect_err("invite fails");

        assert!(matches!(error, crate::NodeError::MissingNodeTicket));
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
        run_daemon_once(&node_a, DaemonOptions::new(Duration::from_millis(50)))
            .await
            .expect("node a daemon");
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
    async fn product_daemons_publish_reopenable_join_facts() {
        let temp = tempfile::tempdir().expect("tempdir");
        let node_a = temp.path().join("node-a");
        let node_b = temp.path().join("node-b");
        init_node(
            InitOptions::new(&node_a)
                .with_island("prod")
                .with_node_id("node-a"),
        )
        .expect("init node a");
        run_daemon_once(&node_a, DaemonOptions::new(Duration::from_millis(150)))
            .await
            .expect("node a first daemon run");
        let token = create_invite(&node_a, Duration::from_secs(60)).expect("invite");
        join_from_token(&node_b, &token, Some("node-b".to_string()), super::now_ms())
            .expect("join node b");

        let (a, b) = tokio::join!(
            run_daemon_once(&node_a, DaemonOptions::new(Duration::from_millis(700))),
            run_daemon_once(&node_b, DaemonOptions::new(Duration::from_millis(700)))
        );
        let report_a = a.expect("node a daemon");
        let report_b = b.expect("node b daemon");

        let state_a = load_node(&node_a).expect("load node a");
        let state_b = load_node(&node_b).expect("load node b");
        let projected_a = projected_node_count(&node_a, &[state_a.clone(), state_b.clone()]).await;
        let projected_b = projected_node_count(&node_b, &[state_a, state_b]).await;

        assert_eq!(
            (projected_a, projected_b),
            (1, 1),
            "self-join projection mismatch after reports: {report_a:?} {report_b:?}"
        );
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
