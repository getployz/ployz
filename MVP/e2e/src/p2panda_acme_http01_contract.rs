use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mvp_acme::{
    AcmeChallengeId, AcmeChallengeToken, AcmeHostname, AcmeHttp01PresentedFact,
    AcmeKeyAuthorization,
};
use mvp_acme_command::{
    AcmeClaimCommand, AcmeClearHttp01Command, AcmeCommandError, AcmeLeaseHandle,
    AcmePresentHttp01Command, PandaAcmeCommandAdapter,
};
use mvp_bus::{BusSession, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_lease::{LeaseEpoch, LeaseTimestamp};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactAuthorKey, PandaFactStore, PandaFactSyncError, PandaFactSyncScope,
    PandaFactSyncSide, PandaSqliteOpenConfig, PandaTrustedAuthorKey, sync_panda_fact_stores,
};
use mvp_projection::{
    DnsCommitFact, FactSource, ProjectionFactPayload, ProjectionIgnoreReason, SqliteProjectionStore,
};
use mvp_serving::{ServingActorHandle, ServingSnapshotPaths, WireServingState, spawn_http_gateway};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::fact_pattern;
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::p2panda_projection_fixture::{
    status_count, write_projection_fact as write_panda_projection_fact,
};
use crate::projection_harness::projection_actor;

const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);
const ACME_TOKEN: &str = "tokPanda0123456789abcdef";
const OTHER_TOKEN: &str = "tokOther0123456789abcdef";

#[derive(Debug, Serialize)]
struct P2pandaAcmeHttp01Report {
    scenario: &'static str,
    visible_nodes_at_decision: usize,
    initial_key_authorization: String,
    takeover_key_authorization: String,
    sync_ms: u128,
    projection_reload_ms: u128,
    http_request_us: u128,
    command_adapter_outage_serving_success_count: usize,
    stale_mutation_rejections: usize,
    scoped_grant_rejections: usize,
    stale_sync_preserved_winner: bool,
    release_fact_recorded: bool,
    trusted_replica_required: bool,
    duplicate_sync_noop: bool,
    sqlite_rebuild_after_delete: bool,
    http_404_after_clear: bool,
    superseded_count: usize,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create tokio runtime for p2panda ACME: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("p2panda-acme-http01-contract");
    reset_dir(&root)?;

    let prod = IslandId::new("prod");
    let challenge = AcmeChallengeId::new(
        AcmeHostname::parse("example.test").map_err(|error| error.to_string())?,
        AcmeChallengeToken::parse(ACME_TOKEN).map_err(|error| error.to_string())?,
    );
    let other_challenge = AcmeChallengeId::new(
        AcmeHostname::parse("example.test").map_err(|error| error.to_string())?,
        AcmeChallengeToken::parse(OTHER_TOKEN).map_err(|error| error.to_string())?,
    );
    let (bus, sessions) = acme_bus_sessions(&prod, &challenge)?;
    let bus = Arc::new(bus);
    let dns_author = PandaFactAuthor::new(sessions.dns_writer.principal().clone());
    let visible_nodes = VisibleNodes::new([NodeId::new("node-a"), NodeId::new("node-b")]);
    let mut adapter_a = PandaAcmeCommandAdapter::new(
        sessions.issuer_a.clone(),
        PandaFactAuthor::new(sessions.issuer_a.principal().clone()),
        visible_nodes.clone(),
    );
    let mut adapter_b = PandaAcmeCommandAdapter::new(
        sessions.issuer_b.clone(),
        PandaFactAuthor::new(sessions.issuer_b.principal().clone()),
        visible_nodes,
    );
    let trusted_authors = vec![
        (
            adapter_a.author().principal().clone(),
            adapter_a.author().author_key(),
        ),
        (
            adapter_b.author().principal().clone(),
            adapter_b.author().author_key(),
        ),
        (dns_author.principal().clone(), dns_author.author_key()),
    ];

    let mut left = open_store(
        bus.clone(),
        root.join("left-p2panda-facts.sqlite"),
        &prod,
        &trusted_authors,
    )
    .await?;
    let mut right = open_store(
        bus.clone(),
        root.join("right-p2panda-facts.sqlite"),
        &prod,
        &trusted_authors,
    )
    .await?;
    left.trust_replica_peer(&prod, sessions.left_replica.principal().clone());
    right.trust_replica_peer(&prod, sessions.right_replica.principal().clone());
    let timeline = LeaseTimeline::fresh()?;
    let scope = trusted_authors
        .iter()
        .fold(PandaFactSyncScope::new(prod.clone()), |scope, author| {
            scope.with_trusted_author(author.0.clone(), author.1)
        });

    write_panda_projection_fact(
        &mut left,
        &sessions.dns_writer,
        &dns_author,
        "/facts/dns/dns-acme-p2panda",
        ProjectionFactPayload::DnsCommit(DnsCommitFact {
            dns_commit_id: "dns-acme-p2panda".to_string(),
            epoch: 1,
            records: Vec::new(),
        }),
    )
    .await?;

    let claim_a = adapter_a
        .claim(
            &mut left,
            AcmeClaimCommand::new(challenge.clone(), timeline.acquired_at, timeline.expires_at),
        )
        .await
        .map_err(|error| error.to_string())?;
    let present_a = adapter_a
        .present(
            &mut left,
            AcmePresentHttp01Command::new(
                challenge.clone(),
                claim_a.lease().clone(),
                "thumbprint-a",
                timeline.published_at,
            ),
        )
        .await
        .map_err(|error| error.to_string())?;
    let stale_before_mutation = adapter_b
        .claim(
            &mut left,
            AcmeClaimCommand::new(
                challenge.clone(),
                timeline.published_at,
                timeline.expires_at,
            ),
        )
        .await
        .expect_err("locally visible lease conflict rejects second issuer");
    assert!(matches!(
        stale_before_mutation,
        AcmeCommandError::Conflict { .. }
    ));
    let wrong_scope = adapter_a
        .claim(
            &mut left,
            AcmeClaimCommand::new(
                other_challenge.clone(),
                timeline.acquired_at,
                timeline.expires_at,
            ),
        )
        .await
        .expect_err("ACME grant scoped to one challenge rejects another lease");
    assert!(matches!(
        wrong_scope,
        AcmeCommandError::UnauthorizedFactWrite { .. }
    ));

    let sync_started = Instant::now();
    let first_sync = sync_panda_fact_stores(
        &mut left,
        &sessions.left_replica,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .map_err(|error| format!("run initial ACME p2panda sync: {error}"))?;
    let sync_ms = sync_started.elapsed().as_millis();
    if first_sync.right.imported < 3 {
        return Err(format!(
            "expected initial sync to import lease, ACME, and DNS facts, got {}",
            first_sync.right.imported
        ));
    }

    let rejected_sync = sync_panda_fact_stores(
        &mut left,
        &sessions.projection,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .expect_err("projection-only principal cannot run replica sync");
    let trusted_replica_required = matches!(
        rejected_sync,
        PandaFactSyncError::UnauthorizedReplica {
            side: PandaFactSyncSide::Left,
            ..
        }
    );
    if !trusted_replica_required {
        return Err(format!(
            "projection-only sync failed for the wrong reason: {rejected_sync}"
        ));
    }
    let repeat = sync_panda_fact_stores(
        &mut left,
        &sessions.left_replica,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .map_err(|error| format!("run repeat ACME sync: {error}"))?;
    let duplicate_sync_noop = repeat.left.received + repeat.right.received == 0;

    let mut projection =
        project_from_reopened_store(&root, bus.clone(), &prod, &trusted_authors, &sessions).await?;
    assert_eq_named(
        "initial p2panda ACME projection count",
        projection.state.acme_http01.len(),
        1,
    )?;
    let initial_key_authorization = projected_key_authorization(&projection)?;

    let serving = ServingActorHandle::spawn(
        prod.clone(),
        ServingSnapshotPaths::new(root.join("gateway.snapshot"), root.join("dns.snapshot")),
        Duration::from_secs(60),
    )
    .map_err(|error| format!("spawn p2panda ACME serving actor: {error}"))?;
    let gateway = spawn_http_gateway(loopback_any(), WireServingState::new(serving.clone()))
        .await
        .map_err(|error| format!("spawn p2panda ACME HTTP gateway: {error}"))?;
    let initial_http = timed_http_get(
        gateway.listen_addr(),
        "example.test",
        &format!("/.well-known/acme-challenge/{ACME_TOKEN}"),
    )
    .await?;
    let mut outage_success_count = 0;
    if initial_http.response.starts_with("HTTP/1.1 200 OK")
        && initial_http
            .response
            .ends_with(present_a.key_authorization().as_str())
    {
        outage_success_count += 1;
    } else {
        return Err(format!(
            "initial p2panda ACME HTTP response did not serve the challenge: {}",
            initial_http.response
        ));
    }

    let pending_clear = adapter_a
        .clear(
            &mut left,
            AcmeClearHttp01Command::new(
                challenge.clone(),
                claim_a.lease().clone(),
                timeline.cleared_at,
            ),
        )
        .await
        .map_err(|error| error.to_string())?;
    let outage_http = timed_http_get(
        gateway.listen_addr(),
        "example.test",
        &format!("/.well-known/acme-challenge/{ACME_TOKEN}"),
    )
    .await?;
    if outage_http.response.starts_with("HTTP/1.1 200 OK")
        && outage_http.response.ends_with(&initial_key_authorization)
    {
        // This proves the last-good snapshot is still serving before synced
        // projection observes the clear.
    } else {
        return Err(format!(
            "p2panda ACME serving did not preserve last-good response before clear sync: {}",
            outage_http.response
        ));
    }

    sync_panda_fact_stores(
        &mut left,
        &sessions.left_replica,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .map_err(|error| format!("sync pending clear after adapter drop: {error}"))?;
    projection =
        project_from_reopened_store(&root, bus.clone(), &prod, &trusted_authors, &sessions).await?;
    serving
        .reload()
        .await
        .map_err(|error| format!("reload after synced clear: {error}"))?;
    assert_eq_named(
        "projection cleared after dropped adapter sync",
        projection.state.acme_http01.len(),
        0,
    )?;

    let claim_b = adapter_b
        .claim(
            &mut left,
            AcmeClaimCommand::new(
                challenge.clone(),
                timeline.takeover_at,
                timeline.takeover_expires_at,
            ),
        )
        .await
        .map_err(|error| error.to_string())?;
    let present_b = adapter_b
        .present(
            &mut left,
            AcmePresentHttp01Command::new(
                challenge.clone(),
                claim_b.lease().clone(),
                "thumbprint-b",
                timeline.takeover_presented_at,
            ),
        )
        .await
        .map_err(|error| error.to_string())?;
    let before_stale_present =
        visible_candidate_count(&left, &sessions.issuer_a).map_err(|error| error.to_string())?;
    let stale_present_attempt = adapter_a
        .present(
            &mut left,
            AcmePresentHttp01Command::new(
                challenge.clone(),
                claim_a.lease().clone(),
                "thumbprint-stale-local",
                timeline.stale_arrival_at,
            ),
        )
        .await
        .expect_err("stale local ACME presenter is rejected before mutation");
    assert!(matches!(
        stale_present_attempt,
        AcmeCommandError::StaleLease
    ));
    assert_eq_named(
        "stale local ACME present did not append facts",
        visible_candidate_count(&left, &sessions.issuer_a).map_err(|error| error.to_string())?,
        before_stale_present,
    )?;
    drop(adapter_a);
    let stale_present = stale_presented_fact(
        challenge.clone(),
        claim_a.lease(),
        "thumbprint-stale",
        timeline.stale_arrival_at,
    )
    .map_err(|error| error.to_string())?;
    write_panda_projection_fact(
        &mut left,
        &sessions.issuer_b,
        adapter_b.author(),
        &challenge.presented_fact_key(LeaseEpoch::first()),
        ProjectionFactPayload::AcmeHttp01Presented(stale_present),
    )
    .await
    .map_err(|error| format!("write stale fixture fact through p2panda: {error}"))?;

    sync_panda_fact_stores(
        &mut left,
        &sessions.left_replica,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .map_err(|error| format!("sync takeover and stale fact: {error}"))?;
    let projection_reload_started = Instant::now();
    projection =
        project_from_reopened_store(&root, bus.clone(), &prod, &trusted_authors, &sessions).await?;
    serving
        .reload()
        .await
        .map_err(|error| format!("reload after takeover: {error}"))?;
    let projection_reload_ms = projection_reload_started.elapsed().as_millis();
    let takeover_key_authorization = projected_key_authorization(&projection)?;
    let takeover_http = timed_http_get(
        gateway.listen_addr(),
        "example.test",
        &format!("/.well-known/acme-challenge/{ACME_TOKEN}"),
    )
    .await?;
    let stale_sync_preserved_winner = takeover_http.response.starts_with("HTTP/1.1 200 OK")
        && takeover_http
            .response
            .ends_with(present_b.key_authorization().as_str())
        && takeover_key_authorization == present_b.key_authorization().as_str();
    if !stale_sync_preserved_winner {
        return Err(format!(
            "stale synced ACME fact rolled back serving: {}",
            takeover_http.response
        ));
    }
    let adapter_dropped_http = timed_http_get(
        gateway.listen_addr(),
        "example.test",
        &format!("/.well-known/acme-challenge/{ACME_TOKEN}"),
    )
    .await?;
    if adapter_dropped_http.response.starts_with("HTTP/1.1 200 OK")
        && adapter_dropped_http
            .response
            .ends_with(present_b.key_authorization().as_str())
    {
        outage_success_count += 1;
    } else {
        return Err(format!(
            "p2panda ACME serving did not preserve winner after adapter drop: {}",
            adapter_dropped_http.response
        ));
    }

    let final_clear = adapter_b
        .clear(
            &mut left,
            AcmeClearHttp01Command::new(
                challenge.clone(),
                claim_b.lease().clone(),
                timeline.final_cleared_at,
            ),
        )
        .await
        .map_err(|error| error.to_string())?;
    sync_panda_fact_stores(
        &mut left,
        &sessions.left_replica,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .map_err(|error| format!("sync final clear: {error}"))?;
    projection =
        project_from_reopened_store(&root, bus.clone(), &prod, &trusted_authors, &sessions).await?;
    serving
        .reload()
        .await
        .map_err(|error| format!("reload after final clear: {error}"))?;
    let after_clear_http = timed_http_get(
        gateway.listen_addr(),
        "example.test",
        &format!("/.well-known/acme-challenge/{ACME_TOKEN}"),
    )
    .await?;
    let http_404_after_clear = after_clear_http
        .response
        .starts_with("HTTP/1.1 404 Not Found");
    if !http_404_after_clear {
        return Err(format!(
            "ACME p2panda HTTP-01 response after final clear did not become 404: {}",
            after_clear_http.response
        ));
    }

    let sqlite_path = root.join("projections.sqlite");
    std::fs::remove_file(&sqlite_path)
        .map_err(|error| format!("delete p2panda ACME projection sqlite: {error}"))?;
    let rebuilt =
        project_from_reopened_store(&root, bus.clone(), &prod, &trusted_authors, &sessions).await?;
    let sqlite = SqliteProjectionStore::new(sqlite_path);
    let row_counts = sqlite
        .row_counts()
        .map_err(|error| format!("read rebuilt p2panda ACME sqlite rows: {error}"))?;
    let sqlite_rebuild_after_delete =
        rebuilt.state.acme_http01.is_empty() && row_counts.acme_http01_challenges == 0;

    gateway
        .shutdown()
        .await
        .map_err(|error| format!("shutdown p2panda ACME HTTP gateway: {error}"))?;

    let visible_node_counts = [
        claim_a.visible_nodes().len(),
        present_a.visible_nodes().len(),
        pending_clear.visible_nodes().len(),
        claim_b.visible_nodes().len(),
        present_b.visible_nodes().len(),
        final_clear.visible_nodes().len(),
    ];
    if visible_node_counts
        .iter()
        .any(|visible_nodes| *visible_nodes != 2)
    {
        return Err(format!(
            "expected every ACME command result to include exactly two visible nodes, got {visible_node_counts:?}"
        ));
    }

    let report = P2pandaAcmeHttp01Report {
        scenario: "p2panda-acme-http01-contract",
        visible_nodes_at_decision: 2,
        initial_key_authorization,
        takeover_key_authorization,
        sync_ms,
        projection_reload_ms,
        http_request_us: initial_http.elapsed_us,
        command_adapter_outage_serving_success_count: outage_success_count,
        stale_mutation_rejections: 2,
        scoped_grant_rejections: 1,
        stale_sync_preserved_winner,
        release_fact_recorded: pending_clear.release_recorded() && final_clear.release_recorded(),
        trusted_replica_required,
        duplicate_sync_noop,
        sqlite_rebuild_after_delete,
        http_404_after_clear,
        superseded_count: status_count(
            &projection.state.statuses,
            ProjectionIgnoreReason::Superseded,
        ),
        elapsed_ms: started.elapsed().as_millis(),
    };
    if report.visible_nodes_at_decision != 2 {
        return Err(format!(
            "expected visible nodes in ACME command result, got {}",
            report.visible_nodes_at_decision
        ));
    }
    if !report.duplicate_sync_noop {
        return Err("repeat p2panda sync was not a no-op".to_string());
    }
    if !report.sqlite_rebuild_after_delete {
        return Err("p2panda ACME sqlite rebuild after delete failed".to_string());
    }

    let json = write_json(
        &root.join("p2panda-acme-http01-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS p2panda-acme-http01-contract");
    Ok(())
}

struct AcmeBusSessions {
    issuer_a: BusSession,
    issuer_b: BusSession,
    dns_writer: BusSession,
    projection: BusSession,
    left_replica: BusSession,
    right_replica: BusSession,
}

fn acme_bus_sessions(
    prod: &IslandId,
    challenge: &AcmeChallengeId,
) -> Result<(InMemoryBus, AcmeBusSessions), String> {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let acme_pattern = fact_pattern(&format!("/facts/acme/http01/example.test/{ACME_TOKEN}/>"))?;
    let lease_pattern = fact_pattern(&format!("/facts/lease/{}/>", challenge.lease_resource()))?;
    let issuer_grant = Grant::empty()
        .with_fact_write(lease_pattern.clone())
        .with_fact_read(lease_pattern)
        .with_fact_write(acme_pattern.clone())
        .with_fact_read(acme_pattern);
    let sessions = AcmeBusSessions {
        issuer_a: authority.grant_in(
            prod.clone(),
            PrincipalId::new("issuer-a"),
            issuer_grant.clone(),
        ),
        issuer_b: authority.grant_in(prod.clone(), PrincipalId::new("issuer-b"), issuer_grant),
        dns_writer: authority.grant_in(
            prod.clone(),
            PrincipalId::new("dns-writer"),
            Grant::empty().with_fact_write(fact_pattern("/facts/dns/>")?),
        ),
        projection: authority.grant_in(
            prod.clone(),
            PrincipalId::new("projection"),
            Grant::empty().with_fact_read(fact_pattern("/facts/>")?),
        ),
        left_replica: authority.grant_in(
            prod.clone(),
            PrincipalId::new("left-replica"),
            Grant::empty(),
        ),
        right_replica: authority.grant_in(
            prod.clone(),
            PrincipalId::new("right-replica"),
            Grant::empty(),
        ),
    };
    Ok((bus, sessions))
}

async fn open_store(
    bus: Arc<InMemoryBus>,
    path: PathBuf,
    island: &IslandId,
    trusted_authors: &[(PrincipalId, PandaFactAuthorKey)],
) -> Result<PandaFactStore, String> {
    let config = trusted_authors.iter().fold(
        PandaSqliteOpenConfig::new(path, vec![island.clone()]),
        |config, (principal, author_key)| {
            config.with_trusted_author_key(PandaTrustedAuthorKey::new(
                island.clone(),
                principal.clone(),
                *author_key,
            ))
        },
    );
    PandaFactStore::open_sqlite(bus, config)
        .await
        .map_err(|error| format!("open p2panda ACME store: {error}"))
}

async fn project_from_reopened_store(
    root: &Path,
    bus: Arc<InMemoryBus>,
    island: &IslandId,
    trusted_authors: &[(PrincipalId, PandaFactAuthorKey)],
    sessions: &AcmeBusSessions,
) -> Result<mvp_projection::ProjectionReport, String> {
    let source = open_store(
        bus,
        root.join("right-p2panda-facts.sqlite"),
        island,
        trusted_authors,
    )
    .await?;
    let actor = projection_actor(Arc::new(source), sessions.projection.clone(), root)?;
    actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project p2panda ACME facts: {error}"))
}

fn projected_key_authorization(
    report: &mvp_projection::ProjectionReport,
) -> Result<String, String> {
    report
        .state
        .acme_http01
        .values()
        .next()
        .map(|challenge| challenge.key_authorization.as_str().to_string())
        .ok_or_else(|| "p2panda ACME projection did not expose a challenge".to_string())
}

fn visible_candidate_count(store: &PandaFactStore, session: &BusSession) -> Result<usize, String> {
    store
        .list_candidates(session.island(), &fact_pattern("/facts/>")?, session)
        .map(|candidates| candidates.len())
        .map_err(|error| error.to_string())
}

fn stale_presented_fact(
    id: AcmeChallengeId,
    lease: &AcmeLeaseHandle,
    thumbprint: &str,
    published_at: LeaseTimestamp,
) -> Result<AcmeHttp01PresentedFact, String> {
    let key_authorization =
        AcmeKeyAuthorization::parse_for_token(id.token(), format!("{}.{thumbprint}", id.token()))
            .map_err(|error| error.to_string())?;
    AcmeHttp01PresentedFact::from_parts(
        id,
        key_authorization,
        lease.holder().clone(),
        lease.epoch(),
        lease.claim_hash(),
        published_at,
    )
    .map_err(|error| error.to_string())
}

#[derive(Debug, Clone, Copy)]
struct LeaseTimeline {
    acquired_at: LeaseTimestamp,
    published_at: LeaseTimestamp,
    cleared_at: LeaseTimestamp,
    takeover_at: LeaseTimestamp,
    takeover_presented_at: LeaseTimestamp,
    takeover_expires_at: LeaseTimestamp,
    stale_arrival_at: LeaseTimestamp,
    final_cleared_at: LeaseTimestamp,
    expires_at: LeaseTimestamp,
}

impl LeaseTimeline {
    fn fresh() -> Result<Self, String> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|error| format!("system clock before unix epoch: {error}"))?
            .as_secs();
        Ok(Self {
            acquired_at: LeaseTimestamp::from_secs(now.saturating_sub(5)),
            published_at: LeaseTimestamp::from_secs(now),
            cleared_at: LeaseTimestamp::from_secs(now + 1),
            takeover_at: LeaseTimestamp::from_secs(now + 2),
            takeover_presented_at: LeaseTimestamp::from_secs(now + 3),
            takeover_expires_at: LeaseTimestamp::from_secs(now + 600),
            stale_arrival_at: LeaseTimestamp::from_secs(now + 4),
            final_cleared_at: LeaseTimestamp::from_secs(now + 5),
            expires_at: LeaseTimestamp::from_secs(now + 600),
        })
    }
}

struct TimedHttpResponse {
    response: String,
    elapsed_us: u128,
}

async fn timed_http_get(
    addr: SocketAddr,
    host: &str,
    path: &str,
) -> Result<TimedHttpResponse, String> {
    let started = Instant::now();
    let response = http_get(addr, host, path).await?;
    Ok(TimedHttpResponse {
        response,
        elapsed_us: started.elapsed().as_micros(),
    })
}

async fn http_get(addr: SocketAddr, host: &str, path: &str) -> Result<String, String> {
    let mut stream = TcpStream::connect(addr)
        .await
        .map_err(|error| format!("connect p2panda ACME HTTP gateway: {error}"))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("write p2panda ACME HTTP request: {error}"))?;
    let mut response = Vec::new();
    timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
        .await
        .map_err(|_| "read p2panda ACME HTTP response timed out".to_string())?
        .map_err(|error| format!("read p2panda ACME HTTP response: {error}"))?;
    String::from_utf8(response)
        .map_err(|error| format!("p2panda ACME HTTP response was not UTF-8: {error}"))
}

fn loopback_any() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
}
