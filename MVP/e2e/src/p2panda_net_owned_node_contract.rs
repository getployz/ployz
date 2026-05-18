use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_bus::{BusSession, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_identity::NodeId;
use mvp_p2panda_facts::{PandaFactAuthor, PandaFactStore, PandaFactWireEnvelope};
use mvp_p2panda_transport::harness::{
    PandaNetWireTransportConfig, import_fact_bodies, transport_wire_bodies,
};
use mvp_p2panda_transport::{
    PandaNetFactImportOutcome, PandaNetFactImportRejection, PandaNetNetworkId, PandaNetNodeSeed,
    PandaNetTopic, import_fact_body,
};
use mvp_projection::{CandidateStatus, FactSource, NodeJoinedFact, ProjectionFactPayload};
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::fact_pattern;
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::p2panda_projection_fixture::write_projection_fact;
use crate::projection_harness::projection_actor;

const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize)]
struct P2pandaNetOwnedNodeReport {
    scenario: &'static str,
    transported_operations: usize,
    imported_operations: u64,
    duplicate_operations: u64,
    conflict_candidates: usize,
    unauthorized_replica_rejected: bool,
    untrusted_author_rejected: bool,
    cross_island_rejected: bool,
    malformed_rejected: bool,
    no_cross_island_leakage: bool,
    projected_nodes: usize,
    projection_rebuild_ms: u128,
    network_sync_ms: u128,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create tokio runtime for owned p2panda-net proof: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("p2panda-net-owned-node-contract");
    reset_dir(&root)?;

    let prod = IslandId::new("prod");
    let laptop = IslandId::new("laptop");
    let (bus, sessions) = net_bus_sessions(&prod, &laptop)?;
    let bus = Arc::new(bus);
    let writer_a = PandaFactAuthor::new(sessions.writer_a.principal().clone());
    let writer_b = PandaFactAuthor::new(sessions.writer_b.principal().clone());
    let untrusted = PandaFactAuthor::new(sessions.untrusted_writer.principal().clone());
    let laptop_author = PandaFactAuthor::new(sessions.laptop_writer.principal().clone());

    let mut source_a = PandaFactStore::new(bus.clone());
    let mut source_b = PandaFactStore::new(bus.clone());
    let mut untrusted_source = PandaFactStore::new(bus.clone());
    let mut laptop_source = PandaFactStore::new(bus.clone());
    let mut canonical = PandaFactStore::new(bus);
    canonical
        .trust_author_key(
            &prod,
            sessions.writer_a.principal().clone(),
            writer_a.author_key(),
        )
        .map_err(|error| format!("trust owned-net writer A: {error}"))?;
    canonical
        .trust_author_key(
            &prod,
            sessions.writer_b.principal().clone(),
            writer_b.author_key(),
        )
        .map_err(|error| format!("trust owned-net writer B: {error}"))?;
    canonical.trust_replica_peer(&prod, sessions.right_replica.principal().clone());

    let first_wire = write_node_wire(
        &mut source_a,
        &sessions.writer_a,
        &writer_a,
        NodeWireFact {
            key: "/facts/node/node-net/joined/1",
            node_id: "node-net",
            overlay_ip: "fd00::20",
            iroh_endpoint_id: "iroh-owned-a",
            wg_public_key: "wg-owned-a",
        },
    )
    .await?;
    let projected_wire = write_node_wire(
        &mut source_a,
        &sessions.writer_a,
        &writer_a,
        NodeWireFact {
            key: "/facts/node/node-projected/joined/1",
            node_id: "node-projected",
            overlay_ip: "fd00::24",
            iroh_endpoint_id: "iroh-owned-projected",
            wg_public_key: "wg-owned-projected",
        },
    )
    .await?;
    let conflict_wire = write_node_wire(
        &mut source_b,
        &sessions.writer_b,
        &writer_b,
        NodeWireFact {
            key: "/facts/node/node-net/joined/1",
            node_id: "node-net",
            overlay_ip: "fd00::21",
            iroh_endpoint_id: "iroh-owned-b",
            wg_public_key: "wg-owned-b",
        },
    )
    .await?;
    let untrusted_wire = write_node_wire(
        &mut untrusted_source,
        &sessions.untrusted_writer,
        &untrusted,
        NodeWireFact {
            key: "/facts/node/node-untrusted/joined/1",
            node_id: "node-untrusted",
            overlay_ip: "fd00::22",
            iroh_endpoint_id: "iroh-owned-untrusted",
            wg_public_key: "wg-owned-untrusted",
        },
    )
    .await?;
    let laptop_wire = write_node_wire(
        &mut laptop_source,
        &sessions.laptop_writer,
        &laptop_author,
        NodeWireFact {
            key: "/facts/node/laptop-net/joined/1",
            node_id: "laptop-net",
            overlay_ip: "fd00::23",
            iroh_endpoint_id: "iroh-owned-laptop",
            wg_public_key: "wg-owned-laptop",
        },
    )
    .await?;

    let unauthorized =
        import_fact_body(&first_wire, &mut canonical, &sessions.untrusted_replica).await;
    let unauthorized_replica_rejected = matches!(
        unauthorized,
        PandaNetFactImportOutcome::Rejected(
            PandaNetFactImportRejection::UnauthorizedReplica { .. }
        )
    );
    if !unauthorized_replica_rejected {
        return Err(format!(
            "owned p2panda-net unauthorized replica produced {unauthorized:?}"
        ));
    }

    let wire_operations = vec![
        first_wire.clone(),
        first_wire,
        projected_wire,
        conflict_wire,
        untrusted_wire,
        laptop_wire,
        b"bad-envelope".to_vec(),
    ];
    let network_started = Instant::now();
    let transported = transport_wire_bodies(
        PandaNetWireTransportConfig::new(
            PandaNetNetworkId::new([73; 32]),
            PandaNetTopic::new([73; 32]),
            PandaNetNodeSeed::new([11; 32]),
            PandaNetNodeSeed::new([12; 32]),
        ),
        wire_operations,
    )
    .await
    .map_err(|error| format!("transport owned p2panda-net bodies: {error}"))?;
    let network_sync_ms = network_started.elapsed().as_millis();

    let import_report =
        import_fact_bodies(transported, &mut canonical, &sessions.right_replica).await;
    if !import_report.deferred.is_empty()
        || !import_report.failed.is_empty()
        || import_report.rejected.iter().any(|rejection| {
            !matches!(
                rejection,
                PandaNetFactImportRejection::UntrustedAuthor { principal, .. }
                    if principal == sessions.untrusted_writer.principal()
            ) && !matches!(
                rejection,
                PandaNetFactImportRejection::CrossIsland { operation, .. }
                    if operation == &laptop
            ) && !matches!(rejection, PandaNetFactImportRejection::MalformedEnvelope(_))
        })
    {
        return Err(format!(
            "unexpected owned p2panda-net import report: {import_report:?}"
        ));
    }
    let imported_operations = (import_report.imported + import_report.conflict) as u64;
    let duplicate_operations = import_report.duplicate as u64;
    let untrusted_author_rejected = import_report.rejected.iter().any(|rejection| {
        matches!(
            rejection,
            PandaNetFactImportRejection::UntrustedAuthor { principal, .. }
                if principal == sessions.untrusted_writer.principal()
        )
    });
    let cross_island_rejected = import_report.rejected.iter().any(|rejection| {
        matches!(
            rejection,
            PandaNetFactImportRejection::CrossIsland { operation, .. } if operation == &laptop
        )
    });
    let malformed_rejected = import_report
        .rejected
        .iter()
        .any(|rejection| matches!(rejection, PandaNetFactImportRejection::MalformedEnvelope(_)));

    assert_eq_named(
        "owned p2panda-net imported operations",
        imported_operations,
        3,
    )?;
    assert_eq_named(
        "owned p2panda-net duplicate operations",
        duplicate_operations,
        1,
    )?;
    if !untrusted_author_rejected {
        return Err("owned p2panda-net untrusted author operation was not rejected".to_string());
    }
    if !cross_island_rejected {
        return Err("owned p2panda-net cross-island operation was not rejected".to_string());
    }
    if !malformed_rejected {
        return Err("owned p2panda-net malformed operation was not rejected".to_string());
    }

    let conflict_candidates = canonical
        .list_candidates(
            &prod,
            &fact_pattern("/facts/node/node-net/joined/1")?,
            &sessions.projection,
        )
        .map_err(|error| format!("list owned p2panda-net conflict candidates: {error}"))?;
    assert_eq_named(
        "owned p2panda-net conflict candidate count",
        conflict_candidates.len(),
        2,
    )?;
    if conflict_candidates
        .iter()
        .any(|candidate| candidate.status() != CandidateStatus::Conflict)
    {
        return Err(
            "owned p2panda-net same-key race did not stay as conflict candidates".to_string(),
        );
    }
    let no_cross_island_leakage = canonical
        .list_candidates(&laptop, &fact_pattern("/facts/>")?, &sessions.laptop_writer)
        .map_err(|error| format!("list owned p2panda-net laptop candidates: {error}"))?
        .is_empty();
    if !no_cross_island_leakage {
        return Err("owned p2panda-net canonical store leaked cross-island candidates".to_string());
    }

    let projection_started = Instant::now();
    let actor = projection_actor(Arc::new(canonical), sessions.projection.clone(), &root)?;
    let projection = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project owned p2panda-net canonical facts: {error}"))?;
    let projection_rebuild_ms = projection_started.elapsed().as_millis();
    assert_eq_named(
        "owned p2panda-net projected nodes",
        projection.state.nodes.len(),
        1,
    )?;
    if !projection
        .state
        .nodes
        .contains_key(&NodeId::new("node-projected"))
    {
        return Err("owned p2panda-net projection missed the non-conflicting node".to_string());
    }

    let report = P2pandaNetOwnedNodeReport {
        scenario: "p2panda-net-owned-node-contract",
        transported_operations: 7,
        imported_operations,
        duplicate_operations,
        conflict_candidates: conflict_candidates.len(),
        unauthorized_replica_rejected,
        untrusted_author_rejected,
        cross_island_rejected,
        malformed_rejected,
        no_cross_island_leakage,
        projected_nodes: projection.state.nodes.len(),
        projection_rebuild_ms,
        network_sync_ms,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let json = write_json(
        &root.join("p2panda-net-owned-node-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS p2panda-net-owned-node-contract");
    Ok(())
}

struct NetBusSessions {
    writer_a: BusSession,
    writer_b: BusSession,
    untrusted_writer: BusSession,
    laptop_writer: BusSession,
    right_replica: BusSession,
    untrusted_replica: BusSession,
    projection: BusSession,
}

fn net_bus_sessions(
    prod: &IslandId,
    laptop: &IslandId,
) -> Result<(InMemoryBus, NetBusSessions), String> {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer_grant = Grant::empty()
        .with_fact_write(fact_pattern("/facts/>")?)
        .with_fact_read(fact_pattern("/facts/>")?);
    let sessions = NetBusSessions {
        writer_a: authority.grant_in(
            prod.clone(),
            PrincipalId::new("owned-net-writer-a"),
            writer_grant.clone(),
        ),
        writer_b: authority.grant_in(
            prod.clone(),
            PrincipalId::new("owned-net-writer-b"),
            writer_grant.clone(),
        ),
        untrusted_writer: authority.grant_in(
            prod.clone(),
            PrincipalId::new("owned-net-untrusted"),
            writer_grant,
        ),
        laptop_writer: authority.grant_in(
            laptop.clone(),
            PrincipalId::new("owned-net-laptop"),
            Grant::empty()
                .with_fact_write(fact_pattern("/facts/>")?)
                .with_fact_read(fact_pattern("/facts/>")?),
        ),
        right_replica: authority.grant_in(
            prod.clone(),
            PrincipalId::new("owned-net-right-replica"),
            Grant::empty(),
        ),
        untrusted_replica: authority.grant_in(
            prod.clone(),
            PrincipalId::new("owned-net-untrusted-replica"),
            Grant::empty(),
        ),
        projection: authority.grant_in(
            prod.clone(),
            PrincipalId::new("owned-net-projection"),
            Grant::empty().with_fact_read(fact_pattern("/facts/>")?),
        ),
    };
    Ok((bus, sessions))
}

struct NodeWireFact {
    key: &'static str,
    node_id: &'static str,
    overlay_ip: &'static str,
    iroh_endpoint_id: &'static str,
    wg_public_key: &'static str,
}

async fn write_node_wire(
    store: &mut PandaFactStore,
    session: &BusSession,
    author: &PandaFactAuthor,
    fact: NodeWireFact,
) -> Result<Vec<u8>, String> {
    write_projection_fact(
        store,
        session,
        author,
        fact.key,
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new(fact.node_id),
            epoch: 1,
            overlay_ip: fact.overlay_ip.to_string(),
            iroh_endpoint_id: fact.iroh_endpoint_id.to_string(),
            wg_public_key: fact.wg_public_key.to_string(),
        }),
    )
    .await?;
    latest_wire_operation(store)
}

fn latest_wire_operation(store: &PandaFactStore) -> Result<Vec<u8>, String> {
    store
        .export_operations()
        .last()
        .map(PandaFactWireEnvelope::encode)
        .ok_or_else(|| "source store did not export a p2panda fact operation".to_string())
}
