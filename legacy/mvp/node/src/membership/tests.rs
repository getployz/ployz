use std::io::{Read, Write};
use std::os::unix::net::UnixStream;
use std::time::Duration;
use std::time::Instant;

use mvp_bus::{FactKeyPattern, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_p2panda_facts::{PandaFactStore, PandaSqliteOpenConfig, PandaTrustedAuthorKey};
use mvp_projection::{FactSource, reduce_facts};

use crate::{
    DaemonOptions, InitOptions, admit_joiner, create_admission_request, create_invite, init_node,
    join_from_token, load_node, load_node_ticket, run_daemon_once,
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
        state
            .bootstrap_peers()
            .first()
            .and_then(|peer| peer.node_id.as_deref()),
        Some("node-a")
    );
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
        bootstrap_node_id: Some("bootstrap".to_string()),
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
async fn daemon_control_status_responds_while_import_is_idle() {
    let temp = tempfile::tempdir().expect("tempdir");
    let node_a = temp.path().join("node-a");
    let control_socket = temp.path().join("daemon.sock");
    init_node(
        InitOptions::new(&node_a)
            .with_island("prod")
            .with_node_id("node-a"),
    )
    .expect("init node a");
    let mut options = DaemonOptions::new(Duration::from_millis(100));
    options.import_idle = Duration::from_millis(750);
    options = options.with_control_socket(&control_socket);
    let daemon = tokio::spawn({
        let node_a = node_a.clone();
        async move { run_daemon_once(&node_a, options).await }
    });

    wait_for_path(&control_socket, Duration::from_secs(5))
        .await
        .expect("daemon control socket");
    let status = wait_daemon_control_status(&control_socket, Duration::from_millis(600))
        .await
        .expect("daemon status should respond before import idle timeout");

    assert_eq!(
        status.get("status").and_then(serde_json::Value::as_str),
        Some("ready")
    );
    assert_eq!(
        status.get("node").and_then(serde_json::Value::as_str),
        Some("node-a")
    );
    daemon.await.expect("daemon join").expect("daemon exits");
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

    let error =
        admit_joiner(&node_a, &admission, super::now_ms()).expect_err("principal mismatch fails");

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

    let error =
        admit_joiner(&node_a, &conflicting, super::now_ms()).expect_err("conflicting peer fails");

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
        Grant::empty()
            .with_fact_read(FactKeyPattern::parse("/facts/node/>").expect("valid fact pattern")),
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

async fn wait_for_path(socket: &std::path::Path, timeout: Duration) -> Result<(), String> {
    let deadline = Instant::now() + timeout;
    loop {
        if socket.exists() {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("socket did not appear: {}", socket.display()));
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

async fn wait_daemon_control_status(
    socket: &std::path::Path,
    timeout: Duration,
) -> Result<serde_json::Value, String> {
    let deadline = Instant::now() + timeout;
    loop {
        match daemon_control_status(socket) {
            Ok(value) => return Ok(value),
            Err(error) => {
                if Instant::now() >= deadline {
                    return Err(error);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        }
    }
}

fn daemon_control_status(socket: &std::path::Path) -> Result<serde_json::Value, String> {
    let mut stream =
        UnixStream::connect(socket).map_err(|error| format!("connect daemon control: {error}"))?;
    stream
        .set_read_timeout(Some(Duration::from_millis(100)))
        .map_err(|error| format!("set daemon control read timeout: {error}"))?;
    stream
        .write_all(b"status\n")
        .map_err(|error| format!("write daemon control: {error}"))?;
    stream
        .shutdown(std::net::Shutdown::Write)
        .map_err(|error| format!("shutdown daemon control write: {error}"))?;
    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .map_err(|error| format!("read daemon control: {error}"))?;
    serde_json::from_str(response.trim())
        .map_err(|error| format!("decode daemon control status: {error}; body={response}"))
}
