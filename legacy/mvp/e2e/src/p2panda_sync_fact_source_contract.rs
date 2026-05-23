use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_bus::{BusSession, FactPayload, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_identity::NodeId;
use mvp_p2panda_authz::IslandAuthoritySnapshot;
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactAuthoritySource, PandaFactStore, PandaFactSyncReport,
    PandaFactSyncScope, PandaSqliteOpenConfig, sync_panda_fact_stores,
};
use mvp_projection::{
    CandidateStatus, FactSource, NodeJoinedFact, ProjectionFactPayload, ProjectionIgnoreReason,
    SqliteProjectionStore, load_dns_snapshot, load_gateway_snapshot,
};
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::{fact_key, fact_pattern};
use crate::metrics::{MemorySnapshot, memory_snapshot, reset_dir, scenario_dir, write_json};
use crate::p2panda_projection_fixture::{
    P2pandaMembershipFixture, create_p2panda_membership_fixture, p2panda_read_replica_importers,
    p2panda_replica_importer_members, seed_projection_facts, status_count, write_projection_fact,
};
use crate::projection_harness::projection_actor;

const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);
const LOAD_FACT_COUNTS: [usize; 3] = [200, 1_000, 10_000];
const SQLITE_LOAD_FACT_COUNTS: [usize; 1] = [1_000];

#[derive(Clone, Copy)]
enum LoadStoreBackend {
    Memory,
    Sqlite,
}

impl LoadStoreBackend {
    fn label(self) -> &'static str {
        match self {
            Self::Memory => "memory",
            Self::Sqlite => "sqlite",
        }
    }
}

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
    load_runs: Vec<P2pandaSyncLoadRunReport>,
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

#[derive(Debug, Serialize)]
struct P2pandaSyncLoadRunReport {
    store_backend: &'static str,
    fact_count: usize,
    first_sync_received: u64,
    first_sync_imported: u64,
    first_sync_conflicts: u64,
    repeat_sync_received: u64,
    projected_candidates: usize,
    conflict_candidates: usize,
    no_cross_island_leakage: bool,
    sync_ms: u128,
    repeat_sync_ms: u128,
    memory_before: MemorySnapshot,
    memory_after: MemorySnapshot,
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
    let laptop = IslandId::new("laptop");
    let prod_replica_importers = p2panda_read_replica_importers([
        (&sessions.left_replica, [61; 32]),
        (&sessions.right_replica, [62; 32]),
    ]);
    let prod_replica_importer_members = p2panda_replica_importer_members(&prod_replica_importers);
    let memberships = SyncMembershipFixtures {
        prod_island: prod.clone(),
        laptop_island: laptop.clone(),
        prod: create_p2panda_membership_fixture(
            &root.join("prod-membership.sqlite"),
            &prod,
            &[&left_author, &right_author],
            &prod_replica_importer_members,
        )
        .await?,
        laptop: create_p2panda_membership_fixture(
            &root.join("laptop-membership.sqlite"),
            &laptop,
            &[&left_author],
            &[],
        )
        .await?,
    };

    let mut left = open_sync_store(
        Arc::new(bus.clone()),
        root.join("left-p2panda-facts.sqlite"),
        &prod,
        &memberships,
    )
    .await?;
    let mut right = open_sync_store(
        Arc::new(bus.clone()),
        root.join("right-p2panda-facts.sqlite"),
        &prod,
        &memberships,
    )
    .await?;

    seed_projection_facts(&mut left, &sessions.left_writer, &left_author).await?;
    seed_conflict(
        &mut left,
        &sessions.left_writer,
        &left_author,
        "fd00::10",
        "iroh-left",
        "wg-left",
    )
    .await?;
    seed_conflict(
        &mut right,
        &sessions.right_writer,
        &right_author,
        "fd00::11",
        "iroh-right",
        "wg-right",
    )
    .await?;
    seed_laptop_fact(&mut left, &sessions.laptop_writer, &left_author).await?;

    let scope = memberships.sync_scope(&prod).await?;

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
    let load_runs = run_large_load_sync_cases(
        &bus,
        &root,
        &prod,
        &sessions,
        &left_author,
        &right_author,
        &memberships,
    )
    .await?;

    let no_cross_island_leakage = right
        .list_candidates(&laptop, &fact_pattern("/facts/>")?, &sessions.laptop_writer)
        .map_err(|error| format!("list cross-island candidates after sync: {error}"))?
        .is_empty();
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
        load_runs,
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

struct SyncMembershipFixtures {
    prod_island: IslandId,
    laptop_island: IslandId,
    prod: P2pandaMembershipFixture,
    laptop: P2pandaMembershipFixture,
}

impl SyncMembershipFixtures {
    async fn sync_scope(&self, island: &IslandId) -> Result<PandaFactSyncScope, String> {
        Ok(PandaFactSyncScope::from_authority(
            &self.snapshot_for(island).await?,
        ))
    }

    async fn authority_source(
        &self,
        island: &IslandId,
    ) -> Result<PandaFactAuthoritySource, String> {
        Ok(PandaFactAuthoritySource::from_snapshots(
            self.snapshots_for_store(island).await?,
        ))
    }

    async fn snapshots_for_store(
        &self,
        island: &IslandId,
    ) -> Result<Vec<IslandAuthoritySnapshot>, String> {
        let mut snapshots = Vec::new();
        for sync_island in trusted_sync_islands(island) {
            snapshots.push(self.snapshot_for(&sync_island).await?);
        }
        Ok(snapshots)
    }

    async fn snapshot_for(&self, island: &IslandId) -> Result<IslandAuthoritySnapshot, String> {
        if island == &self.prod_island {
            self.prod.authority_snapshot(island).await
        } else if island == &self.laptop_island {
            self.laptop.authority_snapshot(island).await
        } else {
            Err(format!(
                "no p2panda membership fixture for island {}",
                island.as_str()
            ))
        }
    }
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
            Grant::empty()
                .with_fact_write(fact_pattern("/facts/>").expect("fact pattern parses"))
                .with_fact_read(fact_pattern("/facts/>").expect("fact pattern parses")),
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

async fn run_large_load_sync_cases(
    bus: &InMemoryBus,
    root: &Path,
    island: &IslandId,
    sessions: &SyncBusSessions,
    left_author: &PandaFactAuthor,
    right_author: &PandaFactAuthor,
    memberships: &SyncMembershipFixtures,
) -> Result<Vec<P2pandaSyncLoadRunReport>, String> {
    let context = LoadSyncCaseContext {
        bus,
        root,
        island,
        sessions,
        left_author,
        right_author,
        memberships,
    };
    let mut reports = Vec::with_capacity(LOAD_FACT_COUNTS.len() + SQLITE_LOAD_FACT_COUNTS.len());
    for fact_count in LOAD_FACT_COUNTS {
        reports
            .push(run_large_load_sync_case(&context, LoadStoreBackend::Memory, fact_count).await?);
    }
    for fact_count in SQLITE_LOAD_FACT_COUNTS {
        reports
            .push(run_large_load_sync_case(&context, LoadStoreBackend::Sqlite, fact_count).await?);
    }
    Ok(reports)
}

struct LoadSyncCaseContext<'a> {
    bus: &'a InMemoryBus,
    root: &'a Path,
    island: &'a IslandId,
    sessions: &'a SyncBusSessions,
    left_author: &'a PandaFactAuthor,
    right_author: &'a PandaFactAuthor,
    memberships: &'a SyncMembershipFixtures,
}

async fn run_large_load_sync_case(
    context: &LoadSyncCaseContext<'_>,
    backend: LoadStoreBackend,
    fact_count: usize,
) -> Result<P2pandaSyncLoadRunReport, String> {
    let LoadSyncCaseContext {
        bus,
        root,
        island,
        sessions,
        left_author,
        right_author,
        memberships,
    } = context;
    let memory_before = memory_snapshot();
    let mut left = load_store(backend, bus, root, "left", fact_count, island, memberships).await?;
    let mut right =
        load_store(backend, bus, root, "right", fact_count, island, memberships).await?;

    for index in 0..fact_count {
        let key = fact_key(&format!("/facts/load/{fact_count}/item/{index}"))?;
        left.write_fact_payload(
            &sessions.left_writer,
            left_author,
            key,
            FactPayload::from(format!("payload-{fact_count}-{index}").into_bytes()),
        )
        .await
        .map_err(|error| format!("write load fact {index} of {fact_count}: {error}"))?;
    }
    let conflict_key = fact_key(&format!("/facts/load/{fact_count}/conflict"))?;
    left.write_fact_payload(
        &sessions.left_writer,
        left_author,
        conflict_key.clone(),
        FactPayload::from(format!("left-conflict-{fact_count}").into_bytes()),
    )
    .await
    .map_err(|error| format!("write left load conflict for {fact_count}: {error}"))?;
    right
        .write_fact_payload(
            &sessions.right_writer,
            right_author,
            conflict_key,
            FactPayload::from(format!("right-conflict-{fact_count}").into_bytes()),
        )
        .await
        .map_err(|error| format!("write right load conflict for {fact_count}: {error}"))?;
    left.write_fact_payload(
        &sessions.laptop_writer,
        left_author,
        fact_key(&format!("/facts/load/{fact_count}/laptop-only"))?,
        FactPayload::from(format!("laptop-only-{fact_count}").into_bytes()),
    )
    .await
    .map_err(|error| format!("write laptop-only load fact for {fact_count}: {error}"))?;

    let scope = memberships.sync_scope(island).await?;
    let sync_started = Instant::now();
    let sync = sync_panda_fact_stores(
        &mut left,
        &sessions.left_replica,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .map_err(|error| format!("run load sync for {fact_count} facts: {error}"))?;
    let sync_ms = sync_started.elapsed().as_millis();
    assert_eq_named(
        "load sync received operations",
        sync.right.received,
        fact_count as u64 + 1,
    )?;
    assert_eq_named(
        "load sync imported operations",
        sync.right.imported,
        fact_count as u64,
    )?;
    assert_eq_named("right load sync conflicts", sync.right.conflict, 1)?;
    assert_eq_named("left load sync received operations", sync.left.received, 1)?;
    assert_eq_named("left load sync conflicts", sync.left.conflict, 1)?;

    let candidates = right
        .list_candidates(
            island,
            &fact_pattern(&format!("/facts/load/{fact_count}/item/>"))?,
            &sessions.left_writer,
        )
        .map_err(|error| format!("list load sync candidates for {fact_count}: {error}"))?;
    assert_eq_named(
        "load sync projected candidates",
        candidates.len(),
        fact_count,
    )?;
    if candidates
        .iter()
        .any(|candidate| candidate.status() != CandidateStatus::Verified)
    {
        return Err(format!(
            "load sync for {fact_count} facts produced non-verified candidates"
        ));
    }
    let conflict_candidates = right
        .list_candidates(
            island,
            &fact_pattern(&format!("/facts/load/{fact_count}/conflict"))?,
            &sessions.left_writer,
        )
        .map_err(|error| format!("list load sync conflict candidates for {fact_count}: {error}"))?;
    assert_eq_named(
        "load sync conflict candidates",
        conflict_candidates.len(),
        2,
    )?;
    if conflict_candidates
        .iter()
        .any(|candidate| candidate.status() != CandidateStatus::Conflict)
    {
        return Err(format!(
            "load sync for {fact_count} facts did not preserve conflict status"
        ));
    }
    let laptop = IslandId::new("laptop");
    let no_cross_island_leakage = right
        .list_candidates(
            &laptop,
            &fact_pattern(&format!("/facts/load/{fact_count}/>"))?,
            &sessions.laptop_writer,
        )
        .map_err(|error| format!("list load sync laptop leakage for {fact_count}: {error}"))?
        .is_empty();
    if !no_cross_island_leakage {
        return Err(format!(
            "load sync for {fact_count} facts leaked laptop island data"
        ));
    }

    let repeat_started = Instant::now();
    let repeat = sync_panda_fact_stores(
        &mut left,
        &sessions.left_replica,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .map_err(|error| format!("run repeat load sync for {fact_count} facts: {error}"))?;
    let repeat_sync_ms = repeat_started.elapsed().as_millis();
    assert_eq_named(
        "repeat load sync received operations",
        sync_received(&repeat),
        0,
    )?;
    let memory_after = memory_snapshot();

    Ok(P2pandaSyncLoadRunReport {
        store_backend: backend.label(),
        fact_count,
        first_sync_received: sync_received(&sync),
        first_sync_imported: sync.left.imported + sync.right.imported,
        first_sync_conflicts: sync.left.conflict + sync.right.conflict,
        repeat_sync_received: sync_received(&repeat),
        projected_candidates: candidates.len(),
        conflict_candidates: conflict_candidates.len(),
        no_cross_island_leakage,
        sync_ms,
        repeat_sync_ms,
        memory_before,
        memory_after,
    })
}

async fn load_store(
    backend: LoadStoreBackend,
    bus: &InMemoryBus,
    root: &Path,
    side: &str,
    fact_count: usize,
    island: &IslandId,
    memberships: &SyncMembershipFixtures,
) -> Result<PandaFactStore, String> {
    match backend {
        LoadStoreBackend::Memory => load_memory_store(bus, island, memberships).await,
        LoadStoreBackend::Sqlite => {
            let path = root.join(format!(
                "load-{}-{fact_count}-{side}-p2panda-facts.sqlite",
                backend.label()
            ));
            open_sync_store(Arc::new(bus.clone()), path, island, memberships).await
        }
    }
}

async fn load_memory_store(
    bus: &InMemoryBus,
    island: &IslandId,
    memberships: &SyncMembershipFixtures,
) -> Result<PandaFactStore, String> {
    let mut store = PandaFactStore::new(Arc::new(bus.clone()));
    for snapshot in memberships.snapshots_for_store(island).await? {
        store.install_authority_snapshot(snapshot);
    }
    Ok(store)
}

async fn open_sync_store(
    bus: Arc<InMemoryBus>,
    path: std::path::PathBuf,
    island: &IslandId,
    memberships: &SyncMembershipFixtures,
) -> Result<PandaFactStore, String> {
    remove_stale_sqlite_store_files(&path)?;
    let islands = trusted_sync_islands(island);
    let config = PandaSqliteOpenConfig::new(path, islands)
        .with_authority_source(memberships.authority_source(island).await?);
    PandaFactStore::open_sqlite(bus, config)
        .await
        .map_err(|error| format!("open p2panda sync store: {error}"))
}

fn remove_stale_sqlite_store_files(path: &Path) -> Result<(), String> {
    for candidate in [
        path.to_path_buf(),
        sqlite_sidecar_path(path, "-wal"),
        sqlite_sidecar_path(path, "-shm"),
    ] {
        match fs::remove_file(&candidate) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(format!(
                    "remove stale p2panda sync store '{}': {error}",
                    candidate.display()
                ));
            }
        }
    }
    Ok(())
}

fn sqlite_sidecar_path(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut value = path.as_os_str().to_owned();
    value.push(suffix);
    std::path::PathBuf::from(value)
}

fn trusted_sync_islands(island: &IslandId) -> Vec<IslandId> {
    let laptop = IslandId::new("laptop");
    if island == &laptop {
        vec![island.clone()]
    } else {
        vec![island.clone(), laptop]
    }
}

async fn seed_conflict(
    store: &mut PandaFactStore,
    session: &BusSession,
    author: &PandaFactAuthor,
    overlay_ip: &str,
    iroh_endpoint_id: &str,
    wg_public_key: &str,
) -> Result<(), String> {
    write_projection_fact(
        store,
        session,
        author,
        "/facts/node/node-conflict/joined/1",
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-conflict"),
            epoch: 1,
            overlay_ip: overlay_ip.to_string(),
            iroh_endpoint_id: iroh_endpoint_id.to_string(),
            wg_public_key: wg_public_key.to_string(),
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

fn sync_received(report: &PandaFactSyncReport) -> u64 {
    report.left.received + report.right.received
}
