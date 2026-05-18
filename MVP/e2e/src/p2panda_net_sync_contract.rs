use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::Stream;
use p2panda_core_git::Topic;
use p2panda_sync_git::{FromSync, protocols::TopicLogSyncEvent};
use serde::Serialize;
use tokio::time::timeout;
use tokio_stream::StreamExt;

use mvp_bus::{BusSession, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_identity::NodeId;
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactError, PandaFactOperation, PandaFactStore, PandaFactWriteOutcome,
};
use mvp_projection::{CandidateStatus, FactSource, NodeJoinedFact, ProjectionFactPayload};

use crate::assertions::assert_eq_named;
use crate::bus_syntax::fact_pattern;
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::p2panda_projection_fixture::write_projection_fact;
use crate::projection_harness::projection_actor;

const NET_EVENT_TIMEOUT: Duration = Duration::from_secs(5);
const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize)]
struct P2pandaNetSyncReport {
    scenario: &'static str,
    transported_operations: usize,
    imported_operations: u64,
    duplicate_operations: u64,
    conflict_candidates: usize,
    untrusted_rejected: bool,
    cross_island_rejected: bool,
    no_cross_island_leakage: bool,
    projected_nodes: usize,
    projection_rebuild_ms: u128,
    network_sync_ms: u128,
    repeated_import_noop: bool,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create tokio runtime for p2panda-net sync proof: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("p2panda-net-sync-contract");
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
        .map_err(|error| format!("trust p2panda-net writer A: {error}"))?;
    canonical
        .trust_author_key(
            &prod,
            sessions.writer_b.principal().clone(),
            writer_b.author_key(),
        )
        .map_err(|error| format!("trust p2panda-net writer B: {error}"))?;

    let first_wire = write_node_wire(
        &mut source_a,
        &sessions.writer_a,
        &writer_a,
        NodeWireFact {
            key: "/facts/node/node-net/joined/1",
            node_id: "node-net",
            overlay_ip: "fd00::20",
            iroh_endpoint_id: "iroh-net-a",
            wg_public_key: "wg-net-a",
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
            iroh_endpoint_id: "iroh-net-projected",
            wg_public_key: "wg-net-projected",
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
            iroh_endpoint_id: "iroh-net-b",
            wg_public_key: "wg-net-b",
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
            iroh_endpoint_id: "iroh-net-untrusted",
            wg_public_key: "wg-net-untrusted",
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
            iroh_endpoint_id: "iroh-net-laptop",
            wg_public_key: "wg-net-laptop",
        },
    )
    .await?;

    let network_started = Instant::now();
    let transported = transport_wire_operations(vec![
        first_wire.clone(),
        projected_wire,
        conflict_wire,
        first_wire,
        untrusted_wire,
        laptop_wire,
    ])
    .await?;
    let network_sync_ms = network_started.elapsed().as_millis();

    let mut imported_operations = 0;
    let mut duplicate_operations = 0;
    let mut untrusted_rejected = false;
    let mut cross_island_rejected = false;
    for wire in transported {
        let operation = PandaFactOperation::from_wire_bytes(&wire)
            .map_err(|error| format!("decode transported p2panda fact operation: {error}"))?;
        match canonical
            .import_operation(&sessions.right_replica, &operation)
            .await
        {
            Ok(PandaFactWriteOutcome::Inserted(_)) | Ok(PandaFactWriteOutcome::Conflict(_)) => {
                imported_operations += 1;
            }
            Ok(PandaFactWriteOutcome::AlreadyPresent(_)) => {
                duplicate_operations += 1;
            }
            Err(PandaFactError::UntrustedAuthorKey { principal, .. })
                if principal == *sessions.untrusted_writer.principal() =>
            {
                untrusted_rejected = true;
            }
            Err(PandaFactError::ImportIslandMismatch { operation, .. }) if operation == laptop => {
                cross_island_rejected = true;
            }
            Err(error) => {
                return Err(format!(
                    "unexpected p2panda-net canonical import error: {error}"
                ));
            }
        }
    }

    assert_eq_named("p2panda-net imported operations", imported_operations, 3)?;
    assert_eq_named("p2panda-net duplicate operations", duplicate_operations, 1)?;
    if !untrusted_rejected {
        return Err("p2panda-net untrusted author operation was not rejected".to_string());
    }
    if !cross_island_rejected {
        return Err("p2panda-net cross-island operation was not rejected".to_string());
    }

    let conflict_candidates = canonical
        .list_candidates(
            &prod,
            &fact_pattern("/facts/node/node-net/joined/1")?,
            &sessions.projection,
        )
        .map_err(|error| format!("list p2panda-net conflict candidates: {error}"))?;
    assert_eq_named(
        "p2panda-net conflict candidate count",
        conflict_candidates.len(),
        2,
    )?;
    if conflict_candidates
        .iter()
        .any(|candidate| candidate.status() != CandidateStatus::Conflict)
    {
        return Err("p2panda-net same-key race did not stay as conflict candidates".to_string());
    }
    let no_cross_island_leakage = canonical
        .list_candidates(&laptop, &fact_pattern("/facts/>")?, &sessions.laptop_writer)
        .map_err(|error| format!("list p2panda-net laptop candidates: {error}"))?
        .is_empty();
    if !no_cross_island_leakage {
        return Err("p2panda-net canonical store leaked cross-island candidates".to_string());
    }

    let projection_started = Instant::now();
    let actor = projection_actor(Arc::new(canonical), sessions.projection.clone(), &root)?;
    let projection = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project p2panda-net canonical facts: {error}"))?;
    let projection_rebuild_ms = projection_started.elapsed().as_millis();

    let report = P2pandaNetSyncReport {
        scenario: "p2panda-net-sync-contract",
        transported_operations: 6,
        imported_operations,
        duplicate_operations,
        conflict_candidates: conflict_candidates.len(),
        untrusted_rejected,
        cross_island_rejected,
        no_cross_island_leakage,
        projected_nodes: projection.state.nodes.len(),
        projection_rebuild_ms,
        network_sync_ms,
        repeated_import_noop: duplicate_operations == 1,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let json = write_json(
        &root.join("p2panda-net-sync-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS p2panda-net-sync-contract");
    Ok(())
}

struct NetBusSessions {
    writer_a: BusSession,
    writer_b: BusSession,
    untrusted_writer: BusSession,
    laptop_writer: BusSession,
    right_replica: BusSession,
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
            PrincipalId::new("net-writer-a"),
            writer_grant.clone(),
        ),
        writer_b: authority.grant_in(
            prod.clone(),
            PrincipalId::new("net-writer-b"),
            writer_grant.clone(),
        ),
        untrusted_writer: authority.grant_in(
            prod.clone(),
            PrincipalId::new("net-untrusted"),
            writer_grant,
        ),
        laptop_writer: authority.grant_in(
            laptop.clone(),
            PrincipalId::new("net-laptop"),
            Grant::empty()
                .with_fact_write(fact_pattern("/facts/>")?)
                .with_fact_read(fact_pattern("/facts/>")?),
        ),
        right_replica: authority.grant_in(
            prod.clone(),
            PrincipalId::new("net-right-replica"),
            Grant::empty(),
        ),
        projection: authority.grant_in(
            prod.clone(),
            PrincipalId::new("net-projection"),
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
        .map(PandaFactOperation::to_wire_bytes)
        .ok_or_else(|| "source store did not export a p2panda fact operation".to_string())
}

async fn transport_wire_operations(wire_operations: Vec<Vec<u8>>) -> Result<Vec<Vec<u8>>, String> {
    let topic: Topic = [88; 32].into();
    let mut receiver = p2panda_net::test_utils::TestNode::spawn([88; 32], None).await;
    let receiver_info = receiver.node_info();
    let mut sender =
        p2panda_net::test_utils::TestNode::spawn([89; 32], Some(receiver_info.bootstrap())).await;

    let receiver_handle = receiver
        .log_sync
        .stream(topic, true)
        .await
        .map_err(|error| format!("open p2panda-net receiver stream: {error}"))?;
    let mut receiver_events = receiver_handle
        .subscribe()
        .await
        .map_err(|error| format!("subscribe p2panda-net receiver stream: {error}"))?;

    for wire in &wire_operations {
        sender.client.create_operation(wire, 0).await;
    }
    sender
        .client
        .associate(&topic, &HashMap::from([(sender.client_id(), vec![0])]))
        .await;
    let _sender_handle = sender
        .log_sync
        .stream(topic, true)
        .await
        .map_err(|error| format!("open p2panda-net sender stream: {error}"))?;

    collect_transported_operations(&mut receiver_events, wire_operations.len()).await
}

async fn collect_transported_operations(
    events: &mut (
             impl Stream<
        Item = Result<
            FromSync<TopicLogSyncEvent>,
            tokio_stream::wrappers::errors::BroadcastStreamRecvError,
        >,
    > + Unpin
         ),
    expected: usize,
) -> Result<Vec<Vec<u8>>, String> {
    let mut received = Vec::with_capacity(expected);
    while received.len() < expected {
        let event = next_net_event(events, "operation").await?;
        match event.event {
            TopicLogSyncEvent::OperationReceived { operation, .. } => {
                let Some(body) = operation.body else {
                    return Err("p2panda-net delivered operation without a body".to_string());
                };
                received.push(body.to_bytes());
            }
            TopicLogSyncEvent::Failed { error } => {
                return Err(format!("p2panda-net sync failed: {error}"));
            }
            TopicLogSyncEvent::SyncFinished { .. }
            | TopicLogSyncEvent::SessionFinished { .. }
            | TopicLogSyncEvent::LiveModeStarted
            | TopicLogSyncEvent::SessionStarted
            | TopicLogSyncEvent::SyncStarted { .. } => {}
        }
    }
    Ok(received)
}

async fn next_net_event(
    events: &mut (
             impl Stream<
        Item = Result<
            FromSync<TopicLogSyncEvent>,
            tokio_stream::wrappers::errors::BroadcastStreamRecvError,
        >,
    > + Unpin
         ),
    label: &'static str,
) -> Result<FromSync<TopicLogSyncEvent>, String> {
    timeout(NET_EVENT_TIMEOUT, events.next())
        .await
        .map_err(|_| format!("timed out waiting for p2panda-net {label} event"))?
        .ok_or_else(|| format!("p2panda-net {label} stream ended"))?
        .map_err(|error| format!("p2panda-net {label} stream lagged: {error}"))
}
