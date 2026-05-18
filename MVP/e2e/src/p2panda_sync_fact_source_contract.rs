use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_bus::{BusSession, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_identity::NodeId;
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactStore, PandaFactSyncReport, PandaFactSyncScope,
    PandaSqliteOpenConfig, PandaTrustedAuthorKey, sync_panda_fact_stores,
};
use mvp_projection::{
    BackendEndpoint, CandidateStatus, DnsCommitFact, DnsRecordFact, FactSource, GatewayCommitFact,
    NodeJoinedFact, ProjectionFactPayload, ProjectionIgnoreReason, RouteCommitFact, RouteId,
    ServiceName, ServiceRegistrationFact, SqliteProjectionStore, load_dns_snapshot,
    load_gateway_snapshot,
};
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::{fact_key, fact_pattern};
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::projection_harness::projection_actor;

const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize)]
struct P2pandaSyncFactSourceReport {
    scenario: &'static str,
    projected_nodes: usize,
    projected_services: usize,
    projected_gateway_routes: usize,
    projected_dns_records: usize,
    first_sync_received: u64,
    first_sync_imported: u64,
    first_sync_conflicts: u64,
    first_sync_bytes_received: u64,
    first_sync_bytes_sent: u64,
    repeat_sync_received: u64,
    conflict_status_count: usize,
    no_cross_island_leakage: bool,
    deleted_projection_rebuilt: bool,
    read_grants_preserved: bool,
    gateway_snapshot_bytes: usize,
    dns_snapshot_bytes: usize,
    first_sync_ms: u128,
    repeat_sync_ms: u128,
    projection_rebuild_ms: u128,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|error| {
            format!("create tokio runtime for p2panda sync fact source contract: {error}")
        })?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("p2panda-sync-fact-source-contract");
    reset_dir(&root)?;

    let (bus, sessions) = sync_bus_sessions();
    let left_author = PandaFactAuthor::new(sessions.left_writer.principal().clone());
    let right_author = PandaFactAuthor::new(sessions.right_writer.principal().clone());
    let prod = IslandId::new("prod");

    let mut left = open_sync_store(
        Arc::new(bus.clone()),
        root.join("left-p2panda-facts.sqlite"),
        &prod,
        &[&left_author, &right_author],
    )
    .await?;
    let mut right = open_sync_store(
        Arc::new(bus.clone()),
        root.join("right-p2panda-facts.sqlite"),
        &prod,
        &[&left_author, &right_author],
    )
    .await?;
    left.trust_replica_peer(&prod, sessions.left_replica.principal().clone());
    right.trust_replica_peer(&prod, sessions.right_replica.principal().clone());

    seed_projection_facts(&mut left, &sessions.left_writer, &left_author).await?;
    seed_left_conflict(&mut left, &sessions.left_writer, &left_author).await?;
    seed_right_conflict(&mut right, &sessions.right_writer, &right_author).await?;
    seed_laptop_fact(&mut left, &sessions.laptop_writer, &left_author).await?;

    let scope = PandaFactSyncScope::new(prod.clone())
        .with_trusted_author(left_author.principal().clone(), left_author.author_key())
        .with_trusted_author(right_author.principal().clone(), right_author.author_key());

    let first_sync_started = Instant::now();
    let first_sync = sync_panda_fact_stores(
        &mut left,
        &sessions.left_replica,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .map_err(|error| format!("run first p2panda sync: {error}"))?;
    let first_sync_ms = first_sync_started.elapsed().as_millis();
    assert_eq_named(
        "right sync received operations",
        first_sync.right.received,
        6,
    )?;
    assert_eq_named(
        "right sync imported operations",
        first_sync.right.imported,
        5,
    )?;
    assert_eq_named(
        "right sync conflict operations",
        first_sync.right.conflict,
        1,
    )?;
    assert_eq_named("left sync received operations", first_sync.left.received, 1)?;
    assert_eq_named("left sync conflict operations", first_sync.left.conflict, 1)?;

    let repeat_sync_started = Instant::now();
    let repeat_sync = sync_panda_fact_stores(
        &mut left,
        &sessions.left_replica,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .map_err(|error| format!("run repeat p2panda sync: {error}"))?;
    let repeat_sync_ms = repeat_sync_started.elapsed().as_millis();
    assert_eq_named(
        "repeat sync received operations",
        sync_received(&repeat_sync),
        0,
    )?;

    let laptop = IslandId::new("laptop");
    let no_cross_island_leakage = right
        .list_candidates(&laptop, &fact_pattern("/facts/>")?, &sessions.projection)
        .map_err(|error| format!("list cross-island candidates after sync: {error}"))?
        .iter()
        .all(|candidate| candidate.island() != &laptop);
    if !no_cross_island_leakage {
        return Err("prod p2panda sync leaked laptop island facts".to_string());
    }
    let read_grants_preserved = payload_read_grants_are_preserved(&right, &sessions)?;
    if !read_grants_preserved {
        return Err("synced p2panda store did not preserve payload read grants".to_string());
    }

    let actor = projection_actor(Arc::new(right), sessions.projection, &root)?;
    let first_projection = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("initial synced p2panda projection failed: {error}"))?;
    assert_projected_state(&first_projection.state)?;

    let gateway_path = root.join("gateway.snapshot");
    let dns_path = root.join("dns.snapshot");
    let gateway_bytes = fs::read(&gateway_path)
        .map_err(|error| format!("read synced p2panda gateway snapshot: {error}"))?;
    let dns_bytes = fs::read(&dns_path)
        .map_err(|error| format!("read synced p2panda dns snapshot: {error}"))?;
    assert_eq_named(
        "loaded synced p2panda gateway routes",
        load_gateway_snapshot(&gateway_path, &prod)
            .map_err(|error| format!("load synced p2panda gateway snapshot: {error}"))?
            .routes
            .len(),
        1,
    )?;
    assert_eq_named(
        "loaded synced p2panda dns records",
        load_dns_snapshot(&dns_path, &prod)
            .map_err(|error| format!("load synced p2panda dns snapshot: {error}"))?
            .records
            .len(),
        1,
    )?;

    let sqlite = SqliteProjectionStore::new(root.join("projections.sqlite"));
    let loaded = sqlite
        .load()
        .map_err(|error| format!("load synced p2panda sqlite projection: {error}"))?;
    if loaded != first_projection.state {
        return Err("synced p2panda sqlite projection did not match actor state".to_string());
    }

    fs::remove_file(root.join("projections.sqlite"))
        .map_err(|error| format!("delete synced p2panda sqlite projection: {error}"))?;
    let projection_rebuild_started = Instant::now();
    let rebuilt = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("rebuild synced p2panda projection failed: {error}"))?;
    let projection_rebuild_ms = projection_rebuild_started.elapsed().as_millis();
    if rebuilt.state != first_projection.state {
        return Err("synced p2panda projection changed after deleting sqlite".to_string());
    }

    let report = P2pandaSyncFactSourceReport {
        scenario: "p2panda-sync-fact-source-contract",
        projected_nodes: rebuilt.state.nodes.len(),
        projected_services: rebuilt.state.services.len(),
        projected_gateway_routes: rebuilt
            .state
            .gateway
            .as_ref()
            .map_or(0, |gateway| gateway.routes.len()),
        projected_dns_records: rebuilt
            .state
            .dns
            .as_ref()
            .map_or(0, |dns| dns.records.len()),
        first_sync_received: sync_received(&first_sync),
        first_sync_imported: first_sync.left.imported + first_sync.right.imported,
        first_sync_conflicts: first_sync.left.conflict + first_sync.right.conflict,
        first_sync_bytes_received: first_sync.left.bytes_received + first_sync.right.bytes_received,
        first_sync_bytes_sent: first_sync.left.bytes_sent + first_sync.right.bytes_sent,
        repeat_sync_received: sync_received(&repeat_sync),
        conflict_status_count: status_count(
            &rebuilt.state.statuses,
            ProjectionIgnoreReason::Conflict,
        ),
        no_cross_island_leakage,
        deleted_projection_rebuilt: true,
        read_grants_preserved,
        gateway_snapshot_bytes: gateway_bytes.len(),
        dns_snapshot_bytes: dns_bytes.len(),
        first_sync_ms,
        repeat_sync_ms,
        projection_rebuild_ms,
        elapsed_ms: started.elapsed().as_millis(),
    };
    if report.conflict_status_count != 2 {
        return Err(format!(
            "expected two synced p2panda conflict candidates, got {}",
            report.conflict_status_count
        ));
    }

    let path = root.join("p2panda-sync-fact-source-contract-metrics.json");
    let json = write_json(&path, &report)?;
    println!("{json}");
    eprintln!("PASS p2panda-sync-fact-source-contract");
    Ok(())
}

struct SyncBusSessions {
    left_writer: BusSession,
    right_writer: BusSession,
    laptop_writer: BusSession,
    projection: BusSession,
    blind_reader: BusSession,
    left_replica: BusSession,
    right_replica: BusSession,
}

fn sync_bus_sessions() -> (InMemoryBus, SyncBusSessions) {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let write_read = Grant::empty()
        .with_fact_write(fact_pattern("/facts/>").expect("fact pattern parses"))
        .with_fact_read(fact_pattern("/facts/>").expect("fact pattern parses"));
    let read_only =
        Grant::empty().with_fact_read(fact_pattern("/facts/>").expect("fact pattern parses"));
    let sessions = SyncBusSessions {
        left_writer: authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("writer-left"),
            write_read.clone(),
        ),
        right_writer: authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("writer-right"),
            write_read,
        ),
        laptop_writer: authority.grant_in(
            IslandId::new("laptop"),
            PrincipalId::new("writer-left"),
            Grant::empty().with_fact_write(fact_pattern("/facts/>").expect("fact pattern parses")),
        ),
        projection: authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("projection"),
            read_only,
        ),
        blind_reader: authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("blind-reader"),
            Grant::empty(),
        ),
        left_replica: authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("left-replica"),
            Grant::empty(),
        ),
        right_replica: authority.grant_in(
            IslandId::new("prod"),
            PrincipalId::new("right-replica"),
            Grant::empty(),
        ),
    };
    (bus, sessions)
}

async fn open_sync_store(
    bus: Arc<InMemoryBus>,
    path: std::path::PathBuf,
    island: &IslandId,
    authors: &[&PandaFactAuthor],
) -> Result<PandaFactStore, String> {
    let config = authors.iter().fold(
        PandaSqliteOpenConfig::new(path, vec![island.clone()]),
        |config, author| {
            config.with_trusted_author_key(PandaTrustedAuthorKey::new(
                island.clone(),
                author.principal().clone(),
                author.author_key(),
            ))
        },
    );
    PandaFactStore::open_sqlite(bus, config)
        .await
        .map_err(|error| format!("open p2panda sync store: {error}"))
}

async fn seed_projection_facts(
    store: &mut PandaFactStore,
    session: &BusSession,
    author: &PandaFactAuthor,
) -> Result<(), String> {
    write_projection_fact(
        store,
        session,
        author,
        "/facts/node/node-1/joined/1",
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            overlay_ip: "fd00::1".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    )
    .await?;
    write_projection_fact(
        store,
        session,
        author,
        "/facts/service/web/node-1/registered/1",
        ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
            service: ServiceName::new("web"),
            node_id: NodeId::new("node-1"),
            version: "1.0.0".to_string(),
            endpoint_subject: "service.web.changed".to_string(),
            epoch: 1,
        }),
    )
    .await?;
    write_projection_fact(
        store,
        session,
        author,
        "/facts/routes/route-1",
        ProjectionFactPayload::RouteCommit(RouteCommitFact {
            route_commit_id: "route-1".to_string(),
            route_id: RouteId::new("web-http"),
            hostnames: vec!["web.example.com".to_string()],
            backends: vec![BackendEndpoint {
                node_id: NodeId::new("node-1"),
                address: "10.0.0.1:8080".to_string(),
            }],
            old_backends_to_drain: vec![BackendEndpoint {
                node_id: NodeId::new("node-old"),
                address: "10.0.0.9:8080".to_string(),
            }],
        }),
    )
    .await?;
    write_projection_fact(
        store,
        session,
        author,
        "/facts/gateway/gateway-1",
        ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
            gateway_commit_id: "gateway-1".to_string(),
            route_commit_id: "route-1".to_string(),
            epoch: 1,
        }),
    )
    .await?;
    write_projection_fact(
        store,
        session,
        author,
        "/facts/dns/dns-1",
        ProjectionFactPayload::DnsCommit(DnsCommitFact {
            dns_commit_id: "dns-1".to_string(),
            epoch: 1,
            records: vec![DnsRecordFact {
                name: "web.example.com".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::1".to_string(),
                ttl_seconds: 30,
            }],
        }),
    )
    .await
    .map(|_| ())
}

async fn seed_left_conflict(
    store: &mut PandaFactStore,
    session: &BusSession,
    author: &PandaFactAuthor,
) -> Result<(), String> {
    write_projection_fact(
        store,
        session,
        author,
        "/facts/node/node-conflict/joined/1",
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-conflict"),
            epoch: 1,
            overlay_ip: "fd00::10".to_string(),
            iroh_endpoint_id: "iroh-left".to_string(),
            wg_public_key: "wg-left".to_string(),
        }),
    )
    .await
    .map(|_| ())
}

async fn seed_right_conflict(
    store: &mut PandaFactStore,
    session: &BusSession,
    author: &PandaFactAuthor,
) -> Result<(), String> {
    write_projection_fact(
        store,
        session,
        author,
        "/facts/node/node-conflict/joined/1",
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-conflict"),
            epoch: 1,
            overlay_ip: "fd00::11".to_string(),
            iroh_endpoint_id: "iroh-right".to_string(),
            wg_public_key: "wg-right".to_string(),
        }),
    )
    .await
    .map(|_| ())
}

async fn seed_laptop_fact(
    store: &mut PandaFactStore,
    session: &BusSession,
    author: &PandaFactAuthor,
) -> Result<(), String> {
    write_projection_fact(
        store,
        session,
        author,
        "/facts/node/laptop-only/joined/1",
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("laptop-only"),
            epoch: 1,
            overlay_ip: "fd00::99".to_string(),
            iroh_endpoint_id: "iroh-laptop".to_string(),
            wg_public_key: "wg-laptop".to_string(),
        }),
    )
    .await
    .map(|_| ())
}

async fn write_projection_fact(
    store: &mut PandaFactStore,
    session: &BusSession,
    author: &PandaFactAuthor,
    key: &str,
    payload: ProjectionFactPayload,
) -> Result<mvp_p2panda_facts::PandaFactWriteOutcome, String> {
    let fact_key = fact_key(key)?;
    let bytes = payload
        .to_fact_bytes()
        .map_err(|error| format!("serialize p2panda sync projection fact '{key}': {error}"))?;
    store
        .write_fact_payload(session, author, fact_key, bytes.into())
        .await
        .map_err(|error| format!("write p2panda sync projection fact '{key}': {error}"))
}

fn payload_read_grants_are_preserved(
    store: &PandaFactStore,
    sessions: &SyncBusSessions,
) -> Result<bool, String> {
    let candidates = store
        .list_candidates(
            sessions.projection.island(),
            &fact_pattern("/facts/node/>")?,
            &sessions.projection,
        )
        .map_err(|error| format!("list synced payload candidates: {error}"))?;
    if !candidates
        .iter()
        .any(|candidate| candidate.status() == CandidateStatus::Verified)
    {
        return Ok(false);
    }
    let visible = store
        .read_payloads(
            sessions.projection.island(),
            &candidates,
            &sessions.projection,
        )
        .map_err(|error| format!("read synced payloads with projection grant: {error}"))?;
    let blind = store
        .read_payloads(
            sessions.projection.island(),
            &candidates,
            &sessions.blind_reader,
        )
        .map_err(|error| format!("read synced payloads without grant: {error}"))?;
    Ok(!visible.is_empty() && blind.is_empty())
}

fn assert_projected_state(state: &mvp_projection::ProjectionState) -> Result<(), String> {
    assert_eq_named("synced p2panda projected nodes", state.nodes.len(), 1)?;
    assert_eq_named("synced p2panda projected services", state.services.len(), 1)?;
    assert_eq_named(
        "synced p2panda projected gateway routes",
        state
            .gateway
            .as_ref()
            .map_or(0, |gateway| gateway.routes.len()),
        1,
    )?;
    assert_eq_named(
        "synced p2panda projected dns records",
        state.dns.as_ref().map_or(0, |dns| dns.records.len()),
        1,
    )
}

fn status_count(
    statuses: &[mvp_projection::ProjectionStatus],
    reason: ProjectionIgnoreReason,
) -> usize {
    statuses
        .iter()
        .find(|status| status.reason == reason)
        .map_or(0, |status| status.count)
}

fn sync_received(report: &PandaFactSyncReport) -> u64 {
    report.left.received + report.right.received
}
