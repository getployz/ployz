use std::net::{Ipv4Addr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_bus::{BusSession, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_identity::NodeId;
use mvp_p2panda_facts::{PandaFactAuthor, PandaFactStore, PandaFactWireEnvelope};
use mvp_p2panda_transport::{
    PandaNetBindConfig, PandaNetFactImportOutcome, PandaNetFactImportRejection, PandaNetNode,
    PandaNetNodeConfig, PandaNetNodeInfo, import_next_fact,
};
use mvp_projection::{CandidateStatus, FactSource, NodeJoinedFact, ProjectionFactPayload};
use p2panda_core_git::{SigningKey, Topic};
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
    let projected_duplicate_wire = projected_wire.clone();
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

    let wire_operations = vec![
        first_wire.clone(),
        first_wire,
        projected_wire,
        conflict_wire,
        projected_duplicate_wire,
        untrusted_wire,
        laptop_wire,
        b"bad-envelope".to_vec(),
    ];
    let network_started = Instant::now();
    let mut net = OwnedNetHarness::spawn(wire_operations).await?;
    let network_sync_ms = network_started.elapsed().as_millis();

    let unauthorized = import_next_fact(
        net.stream_mut(),
        &mut canonical,
        &sessions.untrusted_replica,
    )
    .await
    .map_err(|error| format!("import unauthorized owned-net replica body: {error}"))?;
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

    let mut imported_operations = 0;
    let mut duplicate_operations = 0;
    let mut untrusted_author_rejected = false;
    let mut cross_island_rejected = false;
    let mut malformed_rejected = false;
    for _ in 0..7 {
        match import_next_fact(net.stream_mut(), &mut canonical, &sessions.right_replica)
            .await
            .map_err(|error| format!("import owned p2panda-net fact body: {error}"))?
        {
            PandaNetFactImportOutcome::Imported | PandaNetFactImportOutcome::Conflict => {
                imported_operations += 1;
            }
            PandaNetFactImportOutcome::Duplicate => {
                duplicate_operations += 1;
            }
            PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::UntrustedAuthor {
                principal,
                ..
            }) if principal == *sessions.untrusted_writer.principal() => {
                untrusted_author_rejected = true;
            }
            PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::CrossIsland {
                operation,
                ..
            }) if operation == laptop => {
                cross_island_rejected = true;
            }
            PandaNetFactImportOutcome::Rejected(
                PandaNetFactImportRejection::MalformedEnvelope(_),
            ) => {
                malformed_rejected = true;
            }
            outcome => {
                return Err(format!(
                    "unexpected owned p2panda-net import outcome: {outcome:?}"
                ));
            }
        }
    }

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
        transported_operations: 8,
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

struct OwnedNetHarness {
    _receiver: PandaNetNode,
    _sender: PandaNetNode,
    _sender_stream: mvp_p2panda_transport::PandaNetStream,
    receiver_stream: mvp_p2panda_transport::PandaNetStream,
}

impl OwnedNetHarness {
    async fn spawn(wire_operations: Vec<Vec<u8>>) -> Result<Self, String> {
        let topic: Topic = [73; 32].into();
        let receiver = PandaNetNode::spawn(node_config([11; 32], free_port(), Vec::new()))
            .await
            .map_err(|error| format!("spawn owned p2panda-net receiver: {error}"))?;
        let receiver_info = receiver.node_info();
        let receiver_stream = receiver
            .open_stream(topic, true)
            .await
            .map_err(|error| format!("open owned p2panda-net receiver stream: {error}"))?;

        let mut sender =
            PandaNetNode::spawn(node_config([12; 32], free_port(), vec![receiver_info]))
                .await
                .map_err(|error| format!("spawn owned p2panda-net sender: {error}"))?;
        for wire in wire_operations {
            sender
                .append_to_topic(&topic, &wire)
                .await
                .map_err(|error| format!("append owned p2panda-net wire body: {error}"))?;
        }
        let sender_stream = sender
            .open_stream(topic, true)
            .await
            .map_err(|error| format!("open owned p2panda-net sender stream: {error}"))?;

        Ok(Self {
            _receiver: receiver,
            _sender: sender,
            _sender_stream: sender_stream,
            receiver_stream,
        })
    }

    fn stream_mut(&mut self) -> &mut mvp_p2panda_transport::PandaNetStream {
        &mut self.receiver_stream
    }
}

fn node_config(
    seed: [u8; 32],
    port: u16,
    bootstrap_nodes: Vec<PandaNetNodeInfo>,
) -> PandaNetNodeConfig {
    PandaNetNodeConfig {
        network_id: [73; 32],
        signing_key: SigningKey::from_bytes(&seed),
        bind: PandaNetBindConfig::localhost(port, free_port()),
        bootstrap_nodes,
    }
}

fn free_port() -> u16 {
    UdpSocket::bind((Ipv4Addr::LOCALHOST, 0))
        .expect("bind UDP port probe")
        .local_addr()
        .expect("read UDP port probe")
        .port()
}
