use std::fs;
use std::num::NonZeroU64;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_bus::{BusSession, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_identity::NodeId;
use mvp_p2panda_authz::{
    IslandAuthoritySnapshot, IslandAuthzMemoryLog, IslandMemberAuthorKey, IslandMemberEpoch,
    IslandMemberKeyBinding, ReplicaImportAccess,
};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactStore, PandaFactWriteOutcome, PandaSqliteOpenConfig,
};
use mvp_projection::{
    NodeJoinedFact, ProjectionFactPayload, ProjectionIgnoreReason, SqliteProjectionStore,
    load_dns_snapshot, load_gateway_snapshot,
};
use p2panda_core::PrivateKey;
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::fact_pattern;
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::p2panda_projection_fixture::{
    seed_projection_facts, status_count, write_projection_fact,
};
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
    persistent_reopen: bool,
    persistent_import_reopen: bool,
    authority_snapshot_import: bool,
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

    let (bus, projection_session, replica_session) = p2panda_bus_with_projection_sessions();
    let author = PandaFactAuthor::new(projection_session.principal().clone());
    let authority_snapshot = p2panda_authority_snapshot(
        projection_session.island(),
        &author,
        replica_session.principal(),
    )
    .await?;
    let store_path = root.join("p2panda-facts.sqlite");
    let mut store = PandaFactStore::open_sqlite(
        Arc::new(bus.clone()),
        authority_sqlite_config(
            &store_path,
            projection_session.island(),
            authority_snapshot.clone(),
        ),
    )
    .await
    .map_err(|error| format!("open persistent p2panda fact store: {error}"))?;
    seed_projection_facts(&mut store, &projection_session, &author).await?;
    let conflict_write_recorded =
        seed_conflict_facts(&mut store, &projection_session, &author).await?;
    drop(store);

    let reopened = PandaFactStore::open_sqlite(
        Arc::new(bus.clone()),
        authority_sqlite_config(
            &store_path,
            projection_session.island(),
            authority_snapshot.clone(),
        ),
    )
    .await
    .map_err(|error| format!("reopen persistent p2panda fact store: {error}"))?;
    let exported = reopened.export_operations().cloned().collect::<Vec<_>>();
    if exported.is_empty() {
        return Err("persistent p2panda reopen produced no exported operations".to_string());
    }

    let import_path = root.join("imported-p2panda-facts.sqlite");
    let mut imported_store = PandaFactStore::open_sqlite(
        Arc::new(bus.clone()),
        authority_sqlite_config(
            &import_path,
            projection_session.island(),
            authority_snapshot.clone(),
        ),
    )
    .await
    .map_err(|error| format!("open persistent p2panda import store: {error}"))?;
    for operation in &exported {
        imported_store
            .import_replica_operation(&replica_session, operation)
            .await
            .map_err(|error| format!("import p2panda operation for projection: {error}"))?;
    }
    drop(imported_store);

    let reopened_import = PandaFactStore::open_sqlite(
        Arc::new(bus),
        authority_sqlite_config(
            &import_path,
            projection_session.island(),
            authority_snapshot,
        ),
    )
    .await
    .map_err(|error| format!("reopen persistent imported p2panda fact store: {error}"))?;

    let actor = projection_actor(Arc::new(reopened_import), projection_session, &root)?;
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
        persistent_reopen: true,
        persistent_import_reopen: true,
        authority_snapshot_import: true,
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

fn p2panda_bus_with_projection_sessions() -> (InMemoryBus, BusSession, BusSession) {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let projection_session = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("projection"),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/>").expect("fact pattern parses"))
            .with_fact_read(fact_pattern("/facts/>").expect("fact pattern parses")),
    );
    let replica_session = authority.grant_in(
        IslandId::new("prod"),
        PrincipalId::new("projection-replica"),
        Grant::empty(),
    );
    (bus, projection_session, replica_session)
}

async fn p2panda_authority_snapshot(
    island: &IslandId,
    writer: &PandaFactAuthor,
    replica: &PrincipalId,
) -> Result<IslandAuthoritySnapshot, String> {
    let root_private_key = PrivateKey::from_bytes(&[9; 32]);
    let replica_private_key = PrivateKey::from_bytes(&[8; 32]);
    let root = authority_binding(
        island,
        &PrincipalId::new("root"),
        1,
        IslandMemberAuthorKey::from_public_key(root_private_key.public_key()),
    )?;
    let writer = IslandMemberKeyBinding::new(
        island.clone(),
        writer.principal().clone(),
        island_epoch(1)?,
        writer.author_key().into(),
    );
    let replica = authority_binding(
        island,
        replica,
        1,
        IslandMemberAuthorKey::from_public_key(replica_private_key.public_key()),
    )?;

    let mut log = IslandAuthzMemoryLog::new(island.clone());
    let mut authz = log
        .create_root(root.clone(), &root_private_key)
        .await
        .map_err(|error| format!("create p2panda authority root: {error}"))?;
    log.add_writer(&mut authz, &root, &root_private_key, writer)
        .await
        .map_err(|error| format!("authorize p2panda projection writer: {error}"))?;
    log.add_replica_importer(
        &mut authz,
        &root,
        &root_private_key,
        replica,
        ReplicaImportAccess::Read,
    )
    .await
    .map_err(|error| format!("authorize p2panda projection replica importer: {error}"))?;
    Ok(authz.authority_snapshot())
}

fn authority_binding(
    island: &IslandId,
    principal: &PrincipalId,
    epoch: u64,
    author_key: IslandMemberAuthorKey,
) -> Result<IslandMemberKeyBinding, String> {
    Ok(IslandMemberKeyBinding::new(
        island.clone(),
        principal.clone(),
        island_epoch(epoch)?,
        author_key,
    ))
}

fn island_epoch(epoch: u64) -> Result<IslandMemberEpoch, String> {
    NonZeroU64::new(epoch)
        .map(IslandMemberEpoch::new)
        .ok_or_else(|| "authority member epoch must be non-zero".to_string())
}

fn authority_sqlite_config(
    path: &std::path::Path,
    island: &IslandId,
    authority: IslandAuthoritySnapshot,
) -> PandaSqliteOpenConfig {
    PandaSqliteOpenConfig::new(path, vec![island.clone()]).with_authority_snapshot(authority)
}

async fn seed_conflict_facts(
    store: &mut PandaFactStore,
    session: &BusSession,
    author: &PandaFactAuthor,
) -> Result<bool, String> {
    let key = "/facts/node/node-conflict/joined/1";
    write_projection_fact(
        store,
        session,
        author,
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
        author,
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
