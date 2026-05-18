use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_bus::{BusSession, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_identity::NodeId;
use mvp_p2panda_facts::{PandaFactAuthor, PandaFactStore, PandaFactWriteOutcome};
use mvp_projection::{
    BackendEndpoint, DnsCommitFact, DnsRecordFact, GatewayCommitFact, NodeJoinedFact,
    ProjectionFactPayload, ProjectionIgnoreReason, RouteCommitFact, RouteId, ServiceName,
    ServiceRegistrationFact, SqliteProjectionStore, load_dns_snapshot, load_gateway_snapshot,
};
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::{fact_key, fact_pattern};
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::projection_harness::projection_actor;

const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize)]
struct P2pandaFactSourceReport {
    scenario: &'static str,
    projected_nodes: usize,
    projected_services: usize,
    projected_gateway_routes: usize,
    projected_dns_records: usize,
    conflict_write_recorded: bool,
    conflict_status_count: usize,
    sqlite_rebuild_after_delete: bool,
    gateway_snapshot_bytes: usize,
    dns_snapshot_bytes: usize,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|error| {
            format!("create tokio runtime for p2panda fact source contract: {error}")
        })?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("p2panda-fact-source-contract");
    reset_dir(&root)?;

    let (mut store, projection_session) = p2panda_store_with_projection_session();
    seed_projection_facts(&mut store, &projection_session).await?;
    let conflict_write_recorded = seed_conflict_facts(&mut store, &projection_session).await?;

    let actor = projection_actor(Arc::new(store), projection_session, &root)?;
    let first = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("initial p2panda projection failed: {error}"))?;
    assert_eq_named("p2panda projected nodes", first.state.nodes.len(), 1)?;
    assert_eq_named("p2panda projected services", first.state.services.len(), 1)?;
    assert_eq_named(
        "p2panda projected gateway routes",
        first
            .state
            .gateway
            .as_ref()
            .map_or(0, |gateway| gateway.routes.len()),
        1,
    )?;
    assert_eq_named(
        "p2panda projected dns records",
        first.state.dns.as_ref().map_or(0, |dns| dns.records.len()),
        1,
    )?;

    let sqlite = SqliteProjectionStore::new(root.join("projections.sqlite"));
    let loaded = sqlite
        .load()
        .map_err(|error| format!("load p2panda sqlite projection: {error}"))?;
    if loaded != first.state {
        return Err("p2panda sqlite projection did not match actor state".to_string());
    }
    let gateway_path = root.join("gateway.snapshot");
    let dns_path = root.join("dns.snapshot");
    let gateway_bytes = fs::read(&gateway_path)
        .map_err(|error| format!("read p2panda gateway snapshot: {error}"))?;
    let dns_bytes =
        fs::read(&dns_path).map_err(|error| format!("read p2panda dns snapshot: {error}"))?;

    assert_eq_named(
        "loaded p2panda gateway routes",
        load_gateway_snapshot(&gateway_path, &IslandId::new("prod"))
            .map_err(|error| format!("load p2panda gateway snapshot: {error}"))?
            .routes
            .len(),
        1,
    )?;
    assert_eq_named(
        "loaded p2panda dns records",
        load_dns_snapshot(&dns_path, &IslandId::new("prod"))
            .map_err(|error| format!("load p2panda dns snapshot: {error}"))?
            .records
            .len(),
        1,
    )?;

    fs::remove_file(root.join("projections.sqlite"))
        .map_err(|error| format!("delete p2panda sqlite projection: {error}"))?;
    let rebuilt = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("rebuild p2panda projection failed: {error}"))?;
    if rebuilt.state != first.state {
        return Err("p2panda projection changed after deleting sqlite".to_string());
    }

    let report = P2pandaFactSourceReport {
        scenario: "p2panda-fact-source-contract",
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
        conflict_write_recorded,
        conflict_status_count: status_count(
            &rebuilt.state.statuses,
            ProjectionIgnoreReason::Conflict,
        ),
        sqlite_rebuild_after_delete: true,
        gateway_snapshot_bytes: gateway_bytes.len(),
        dns_snapshot_bytes: dns_bytes.len(),
        elapsed_ms: started.elapsed().as_millis(),
    };
    if !report.conflict_write_recorded {
        return Err("p2panda conflicting write did not return a conflict outcome".to_string());
    }
    if report.conflict_status_count != 2 {
        return Err(format!(
            "expected two p2panda conflict candidates, got {}",
            report.conflict_status_count
        ));
    }
    let path = root.join("p2panda-fact-source-contract-metrics.json");
    let json = write_json(&path, &report)?;
    println!("{json}");
    eprintln!("PASS p2panda-fact-source-contract");
    Ok(())
}

fn p2panda_store_with_projection_session() -> (PandaFactStore, BusSession) {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let session = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("projection"),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/>").expect("fact pattern parses"))
            .with_fact_read(fact_pattern("/facts/>").expect("fact pattern parses")),
    );
    (PandaFactStore::new(Arc::new(bus)), session)
}

async fn seed_projection_facts(
    store: &mut PandaFactStore,
    session: &BusSession,
) -> Result<(), String> {
    let author = PandaFactAuthor::new(session.principal().clone());
    write_projection_fact(
        store,
        session,
        &author,
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
        &author,
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
        &author,
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
        &author,
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
        &author,
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

async fn seed_conflict_facts(
    store: &mut PandaFactStore,
    session: &BusSession,
) -> Result<bool, String> {
    let author = PandaFactAuthor::new(session.principal().clone());
    let key = "/facts/node/node-conflict/joined/1";
    write_projection_fact(
        store,
        session,
        &author,
        key,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-conflict"),
            epoch: 1,
            overlay_ip: "fd00::10".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    )
    .await?;
    let outcome = write_projection_fact(
        store,
        session,
        &author,
        key,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-conflict"),
            epoch: 1,
            overlay_ip: "fd00::11".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    )
    .await?;
    Ok(matches!(outcome, PandaFactWriteOutcome::Conflict(_)))
}

async fn write_projection_fact(
    store: &mut PandaFactStore,
    session: &BusSession,
    author: &PandaFactAuthor,
    key: &str,
    payload: ProjectionFactPayload,
) -> Result<PandaFactWriteOutcome, String> {
    let fact_key = fact_key(key)?;
    let bytes = payload
        .to_fact_bytes()
        .map_err(|error| format!("serialize p2panda projection fact '{key}': {error}"))?;
    store
        .write_fact_payload(session, author, fact_key, bytes.into())
        .await
        .map_err(|error| format!("write p2panda projection fact '{key}': {error}"))
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
