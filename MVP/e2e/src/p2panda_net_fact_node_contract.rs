use std::fs;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_bus::{
    BusSession, FactKey, FactPayload, Grant, IslandId, PrincipalId, harness::InMemoryBus,
};
use mvp_identity::NodeId;
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactStore, PandaFactWireEnvelope, SharedPandaFactStore,
};
use mvp_p2panda_transport::{
    PandaNetFactImportOutcome, PandaNetFactImportRejection, PandaNetFactImportReport,
    PandaNetFactNode, PandaNetFactNodeConfig, PandaNetNetworkId, PandaNetNodeConfig,
    PandaNetNodeSeed, PandaNetTopic, PandaNetTransportError, import_fact_body_into_shared_store,
};
use mvp_projection::{
    BackendEndpoint, CandidateStatus, DnsCommitFact, DnsRecordFact, FactSource, GatewayCommitFact,
    NodeJoinedFact, ProjectionFactPayload, RouteCommitFact, RouteId, ServiceName,
    ServiceRegistrationFact, SqliteProjectionStore, load_dns_snapshot, load_gateway_snapshot,
};
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::fact_pattern;
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::projection_harness::projection_actor;

const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize)]
struct P2pandaNetFactNodeReport {
    scenario: &'static str,
    attempted_imports: usize,
    imported_operations: u64,
    duplicate_operations: u64,
    conflict_operations: u64,
    rejected_operations: usize,
    conflict_candidates: usize,
    unauthorized_replica_rejected: bool,
    untrusted_author_rejected: bool,
    cross_island_rejected: bool,
    author_mismatch_rejected: bool,
    no_cross_island_leakage: bool,
    projected_nodes: usize,
    projected_services: usize,
    projected_gateway_routes: usize,
    projected_dns_records: usize,
    startup_ms: u128,
    sync_import_ms: u128,
    projection_rebuild_ms: u128,
    restart_projection_rebuild_ms: u128,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create tokio runtime for p2panda-net fact node: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("p2panda-net-fact-node-contract");
    reset_dir(&root)?;

    let prod = IslandId::new("prod");
    let laptop = IslandId::new("laptop");
    let (bus, sessions) = fact_node_bus_sessions(&prod, &laptop)?;
    let bus = Arc::new(bus);
    let writer_a = PandaFactAuthor::new(sessions.writer_a.principal().clone());
    let writer_b = PandaFactAuthor::new(sessions.writer_b.principal().clone());
    let untrusted = PandaFactAuthor::new(sessions.untrusted_writer.principal().clone());
    let laptop_author = PandaFactAuthor::new(sessions.laptop_writer.principal().clone());
    let startup_started = Instant::now();
    let mut receiver = fact_node(
        &bus,
        &prod,
        &sessions.receiver_replica,
        &[
            (&sessions.writer_a, &writer_a),
            (&sessions.writer_b, &writer_b),
        ],
        [51; 32],
        Vec::new(),
    )
    .await?;
    let receiver_info = receiver.node_info();
    let mut sender = fact_node(
        &bus,
        &prod,
        &sessions.receiver_replica,
        &[
            (&sessions.writer_a, &writer_a),
            (&sessions.writer_b, &writer_b),
        ],
        [52; 32],
        vec![receiver_info.clone()],
    )
    .await?;
    sender
        .store()
        .trust_author_key(
            &prod,
            sessions.untrusted_writer.principal().clone(),
            untrusted.author_key(),
        )
        .await
        .map_err(|error| format!("trust p2panda-net untrusted sender author: {error}"))?;
    sender
        .store()
        .trust_author_key(
            &laptop,
            sessions.laptop_writer.principal().clone(),
            laptop_author.author_key(),
        )
        .await
        .map_err(|error| format!("trust p2panda-net laptop sender author: {error}"))?;
    let startup_ms = startup_started.elapsed().as_millis();

    sender
        .publish_fact_payload(
            &sessions.writer_a,
            &writer_a,
            fact_key("/facts/node/node-net/joined/1")?,
            node_joined_payload("node-net", "fd00::20", "iroh-fact-a", "wg-fact-a")?,
        )
        .await
        .map_err(|error| format!("publish fact-node first fact: {error}"))?;
    let duplicate_operation = sender
        .store()
        .export_operations()
        .await
        .last()
        .cloned()
        .ok_or_else(|| "sender store did not export first fact-node operation".to_string())?;
    let unauthorized_replica_probe_body = PandaFactWireEnvelope::encode(&duplicate_operation);
    sender
        .publish_fact_payload(
            &sessions.writer_a,
            &writer_a,
            fact_key("/facts/node/node-projected/joined/1")?,
            node_joined_payload(
                "node-projected",
                "fd00::24",
                "iroh-fact-projected",
                "wg-fact-projected",
            )?,
        )
        .await
        .map_err(|error| format!("publish fact-node projected fact: {error}"))?;
    sender
        .publish_fact_payload(
            &sessions.writer_a,
            &writer_a,
            fact_key("/facts/service/web/node-projected/registered/1")?,
            service_payload()?,
        )
        .await
        .map_err(|error| format!("publish fact-node service fact: {error}"))?;
    sender
        .publish_fact_payload(
            &sessions.writer_a,
            &writer_a,
            fact_key("/facts/routes/route-net")?,
            route_payload()?,
        )
        .await
        .map_err(|error| format!("publish fact-node route fact: {error}"))?;
    sender
        .publish_fact_payload(
            &sessions.writer_a,
            &writer_a,
            fact_key("/facts/gateway/gateway-net")?,
            gateway_payload()?,
        )
        .await
        .map_err(|error| format!("publish fact-node gateway fact: {error}"))?;
    sender
        .publish_fact_payload(
            &sessions.writer_a,
            &writer_a,
            fact_key("/facts/dns/dns-net")?,
            dns_payload()?,
        )
        .await
        .map_err(|error| format!("publish fact-node dns fact: {error}"))?;
    sender
        .publish_fact_payload(
            &sessions.writer_b,
            &writer_b,
            fact_key("/facts/node/node-net/joined/1")?,
            node_joined_payload("node-net", "fd00::21", "iroh-fact-b", "wg-fact-b")?,
        )
        .await
        .map_err(|error| format!("publish fact-node conflict fact: {error}"))?;
    sender
        .publish_fact_payload(
            &sessions.untrusted_writer,
            &untrusted,
            fact_key("/facts/node/node-untrusted/joined/1")?,
            node_joined_payload(
                "node-untrusted",
                "fd00::22",
                "iroh-fact-untrusted",
                "wg-fact-untrusted",
            )?,
        )
        .await
        .map_err(|error| format!("publish fact-node untrusted fact: {error}"))?;
    sender
        .publish_fact_payload(
            &sessions.laptop_writer,
            &laptop_author,
            fact_key("/facts/node/laptop-net/joined/1")?,
            node_joined_payload(
                "laptop-net",
                "fd00::23",
                "iroh-fact-laptop",
                "wg-fact-laptop",
            )?,
        )
        .await
        .map_err(|error| format!("publish fact-node cross-island fact: {error}"))?;
    let unauthorized_store = trusted_store(
        &bus,
        &prod,
        &[
            (&sessions.writer_a, &writer_a),
            (&sessions.writer_b, &writer_b),
        ],
        None,
    )
    .await?;
    let unauthorized = import_fact_body_into_shared_store(
        &unauthorized_replica_probe_body,
        &unauthorized_store,
        &sessions.untrusted_replica,
    )
    .await;
    let unauthorized_replica_rejected = matches!(
        unauthorized,
        PandaNetFactImportOutcome::Rejected(
            PandaNetFactImportRejection::UnauthorizedReplica { .. }
        )
    );
    if !unauthorized_replica_rejected {
        return Err(format!(
            "p2panda-net fact-node unauthorized replica produced {unauthorized:?}"
        ));
    }

    let mut mismatch_source = PandaFactStore::new(bus.clone());
    let mismatched_writer_a = PandaFactAuthor::new(sessions.writer_a.principal().clone());
    mismatch_source
        .write_fact_payload(
            &sessions.writer_a,
            &mismatched_writer_a,
            fact_key("/facts/node/node-mismatch/joined/1")?,
            node_joined_payload(
                "node-mismatch",
                "fd00::25",
                "iroh-fact-mismatch",
                "wg-fact-mismatch",
            )?,
        )
        .await
        .map_err(|error| format!("write fact-node author-key mismatch source: {error}"))?;
    let mismatch_body = mismatch_source
        .export_operations()
        .last()
        .map(PandaFactWireEnvelope::encode)
        .ok_or_else(|| "author-key mismatch source produced no operation".to_string())?;
    let author_mismatch = import_fact_body_into_shared_store(
        &mismatch_body,
        &receiver.store(),
        &sessions.receiver_replica,
    )
    .await;
    let author_mismatch_rejected = matches!(
        author_mismatch,
        PandaNetFactImportOutcome::Rejected(PandaNetFactImportRejection::AuthorKeyMismatch {
            principal,
            ..
        }) if principal == *sessions.writer_a.principal()
    );

    let sync_started = Instant::now();
    let import_report = import_until_contract_terminal_outcomes(&mut receiver).await?;
    let sync_import_ms = sync_started.elapsed().as_millis();

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
    let unexpected_rejection = import_report.rejected.iter().any(|rejection| {
        !matches!(
            rejection,
            PandaNetFactImportRejection::UntrustedAuthor { principal, .. }
                if principal == sessions.untrusted_writer.principal()
        ) && !matches!(
            rejection,
            PandaNetFactImportRejection::CrossIsland { operation, .. } if operation == &laptop
        )
    });

    if !import_report.deferred.is_empty()
        || !import_report.failed.is_empty()
        || unexpected_rejection
    {
        return Err(format!(
            "unexpected p2panda-net fact-node import report: {import_report:?}"
        ));
    }

    assert_eq_named(
        "fact-node inserted operations",
        import_report.imported as u64,
        6,
    )?;
    assert_eq_named(
        "fact-node conflict operations",
        import_report.conflict as u64,
        1,
    )?;
    assert_eq_named(
        "fact-node duplicate operations",
        import_report.duplicate as u64,
        0,
    )?;
    if !untrusted_author_rejected || !cross_island_rejected || !author_mismatch_rejected {
        return Err(format!(
            "fact-node did not surface expected rejection classes: {import_report:?}"
        ));
    }

    let receiver_store = receiver.store();
    let conflict_candidates = receiver_store
        .list_candidates(
            &prod,
            &fact_pattern("/facts/node/node-net/joined/1")?,
            &sessions.projection,
        )
        .map_err(|error| format!("list fact-node conflict candidates: {error}"))?;
    assert_eq_named(
        "fact-node conflict candidate count",
        conflict_candidates.len(),
        2,
    )?;
    if conflict_candidates
        .iter()
        .any(|candidate| candidate.status() != CandidateStatus::Conflict)
    {
        return Err("fact-node same-key race did not stay as conflict candidates".to_string());
    }
    let no_cross_island_leakage = receiver_store
        .list_candidates(&laptop, &fact_pattern("/facts/>")?, &sessions.laptop_writer)
        .map_err(|error| format!("list fact-node laptop candidates: {error}"))?
        .is_empty();
    if !no_cross_island_leakage {
        return Err("fact-node receiver leaked cross-island candidates".to_string());
    }

    let projection_started = Instant::now();
    let actor = projection_actor(
        Arc::new(receiver_store.clone()),
        sessions.projection.clone(),
        &root,
    )?;
    let projection = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project p2panda-net fact-node facts: {error}"))?;
    let projection_rebuild_ms = projection_started.elapsed().as_millis();
    assert_projected_state(&projection.state)?;
    assert_eq_named(
        "fact-node loaded gateway routes",
        load_gateway_snapshot(root.join("gateway.snapshot"), &prod)
            .map_err(|error| format!("load fact-node gateway snapshot: {error}"))?
            .routes
            .len(),
        1,
    )?;
    assert_eq_named(
        "fact-node loaded dns records",
        load_dns_snapshot(root.join("dns.snapshot"), &prod)
            .map_err(|error| format!("load fact-node dns snapshot: {error}"))?
            .records
            .len(),
        1,
    )?;
    let sqlite = SqliteProjectionStore::new(root.join("projections.sqlite"));
    let loaded = sqlite
        .load()
        .map_err(|error| format!("load fact-node sqlite projection: {error}"))?;
    if loaded != projection.state {
        return Err("fact-node sqlite projection did not match actor state".to_string());
    }

    let restart_projection_started = Instant::now();
    fs::remove_file(root.join("projections.sqlite"))
        .map_err(|error| format!("delete fact-node sqlite projection: {error}"))?;
    fs::remove_file(root.join("gateway.snapshot"))
        .map_err(|error| format!("delete fact-node gateway snapshot: {error}"))?;
    fs::remove_file(root.join("dns.snapshot"))
        .map_err(|error| format!("delete fact-node dns snapshot: {error}"))?;
    let restarted = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("rebuild p2panda-net fact-node facts: {error}"))?;
    let restart_projection_rebuild_ms = restart_projection_started.elapsed().as_millis();
    if restarted.state != projection.state {
        return Err("fact-node projection changed after sqlite rebuild".to_string());
    }
    let reloaded = sqlite
        .load()
        .map_err(|error| format!("load rebuilt fact-node sqlite projection: {error}"))?;
    if reloaded != restarted.state {
        return Err("rebuilt fact-node sqlite projection did not match actor state".to_string());
    }
    assert_eq_named(
        "rebuilt fact-node gateway routes",
        load_gateway_snapshot(root.join("gateway.snapshot"), &prod)
            .map_err(|error| format!("load rebuilt fact-node gateway snapshot: {error}"))?
            .routes
            .len(),
        1,
    )?;
    assert_eq_named(
        "rebuilt fact-node dns records",
        load_dns_snapshot(root.join("dns.snapshot"), &prod)
            .map_err(|error| format!("load rebuilt fact-node dns snapshot: {error}"))?
            .records
            .len(),
        1,
    )?;

    let report = P2pandaNetFactNodeReport {
        scenario: "p2panda-net-fact-node-contract",
        attempted_imports: import_report.attempted,
        imported_operations: import_report.imported as u64,
        duplicate_operations: import_report.duplicate as u64,
        conflict_operations: import_report.conflict as u64,
        rejected_operations: import_report.rejected.len(),
        conflict_candidates: conflict_candidates.len(),
        unauthorized_replica_rejected,
        untrusted_author_rejected,
        cross_island_rejected,
        author_mismatch_rejected,
        no_cross_island_leakage,
        projected_nodes: projection.state.nodes.len(),
        projected_services: projection.state.services.len(),
        projected_gateway_routes: projection
            .state
            .gateway
            .as_ref()
            .map_or(0, |gateway| gateway.routes.len()),
        projected_dns_records: projection
            .state
            .dns
            .as_ref()
            .map_or(0, |dns| dns.records.len()),
        startup_ms,
        sync_import_ms,
        projection_rebuild_ms,
        restart_projection_rebuild_ms,
        elapsed_ms: started.elapsed().as_millis(),
    };
    let json = write_json(
        &root.join("p2panda-net-fact-node-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS p2panda-net-fact-node-contract");
    Ok(())
}

async fn import_until_contract_terminal_outcomes(
    receiver: &mut PandaNetFactNode,
) -> Result<PandaNetFactImportReport, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut report = PandaNetFactImportReport::new(0);
    while Instant::now() < deadline {
        let batch = match receiver
            .import_next_fact_batch_with_idle_timeout(Duration::from_millis(250))
            .await
        {
            Ok(batch) => batch,
            Err(PandaNetTransportError::StreamEnded { .. }) => {
                receiver
                    .refresh_stream()
                    .await
                    .map_err(|error| format!("refresh p2panda-net fact-node stream: {error}"))?;
                continue;
            }
            Err(error) => return Err(format!("import p2panda-net fact-node stream: {error}")),
        };
        let Some(batch) = batch else {
            receiver
                .refresh_stream()
                .await
                .map_err(|error| format!("refresh p2panda-net fact-node stream: {error}"))?;
            continue;
        };
        report.attempted += 1;
        for outcome in batch {
            report.record(outcome);
        }
        if fact_node_contract_terminal_outcomes_arrived(&report) {
            return Ok(report);
        }
    }
    Err(format!(
        "p2panda-net fact-node terminal outcomes did not arrive: {report:?}"
    ))
}

fn fact_node_contract_terminal_outcomes_arrived(report: &PandaNetFactImportReport) -> bool {
    report.imported >= 6
        && report.duplicate == 0
        && report.conflict >= 1
        && report.rejected.len() >= 2
        && report.deferred.is_empty()
        && report.failed.is_empty()
}

struct FactNodeBusSessions {
    writer_a: BusSession,
    writer_b: BusSession,
    untrusted_writer: BusSession,
    laptop_writer: BusSession,
    receiver_replica: BusSession,
    untrusted_replica: BusSession,
    projection: BusSession,
}

fn fact_node_bus_sessions(
    prod: &IslandId,
    laptop: &IslandId,
) -> Result<(InMemoryBus, FactNodeBusSessions), String> {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let writer_grant = Grant::empty()
        .with_fact_write(fact_pattern("/facts/>")?)
        .with_fact_read(fact_pattern("/facts/>")?);
    let sessions = FactNodeBusSessions {
        writer_a: authority.grant_in(
            prod.clone(),
            PrincipalId::new("fact-node-writer-a"),
            writer_grant.clone(),
        ),
        writer_b: authority.grant_in(
            prod.clone(),
            PrincipalId::new("fact-node-writer-b"),
            writer_grant.clone(),
        ),
        untrusted_writer: authority.grant_in(
            prod.clone(),
            PrincipalId::new("fact-node-untrusted"),
            writer_grant,
        ),
        laptop_writer: authority.grant_in(
            laptop.clone(),
            PrincipalId::new("fact-node-laptop"),
            Grant::empty()
                .with_fact_write(fact_pattern("/facts/>")?)
                .with_fact_read(fact_pattern("/facts/>")?),
        ),
        receiver_replica: authority.grant_in(
            prod.clone(),
            PrincipalId::new("fact-node-receiver-replica"),
            Grant::empty(),
        ),
        untrusted_replica: authority.grant_in(
            prod.clone(),
            PrincipalId::new("fact-node-untrusted-replica"),
            Grant::empty(),
        ),
        projection: authority.grant_in(
            prod.clone(),
            PrincipalId::new("fact-node-projection"),
            Grant::empty().with_fact_read(fact_pattern("/facts/>")?),
        ),
    };
    Ok((bus, sessions))
}

async fn fact_node(
    bus: &Arc<InMemoryBus>,
    island: &IslandId,
    replica_session: &BusSession,
    trusted_authors: &[(&BusSession, &PandaFactAuthor)],
    seed: [u8; 32],
    bootstrap: Vec<mvp_p2panda_transport::PandaNetNodeInfo>,
) -> Result<PandaNetFactNode, String> {
    let store = trusted_store(bus, island, trusted_authors, Some(replica_session)).await?;
    PandaNetFactNode::spawn(PandaNetFactNodeConfig::new(
        PandaNetNodeConfig::localhost_ephemeral(
            PandaNetNetworkId::new([83; 32]),
            PandaNetNodeSeed::new(seed),
            bootstrap,
        ),
        PandaNetTopic::new([84; 32]),
        store,
        replica_session.clone(),
    ))
    .await
    .map_err(|error| format!("spawn p2panda-net fact node: {error}"))
}

async fn trusted_store(
    bus: &Arc<InMemoryBus>,
    island: &IslandId,
    trusted_authors: &[(&BusSession, &PandaFactAuthor)],
    replica_session: Option<&BusSession>,
) -> Result<SharedPandaFactStore, String> {
    let store = SharedPandaFactStore::new(PandaFactStore::new(bus.clone()));
    for &(session, author) in trusted_authors {
        store
            .trust_author_key(island, session.principal().clone(), author.author_key())
            .await
            .map_err(|error| format!("trust p2panda-net fact-node author: {error}"))?;
    }
    if let Some(replica_session) = replica_session {
        store
            .trust_replica_peer(island, replica_session.principal().clone())
            .await;
    }
    Ok(store)
}

fn node_joined_payload(
    node_id: &str,
    overlay_ip: &str,
    iroh_endpoint_id: &str,
    wg_public_key: &str,
) -> Result<FactPayload, String> {
    projection_payload(ProjectionFactPayload::NodeJoined(NodeJoinedFact {
        node_id: NodeId::new(node_id),
        epoch: 1,
        overlay_ip: overlay_ip.to_string(),
        iroh_endpoint_id: iroh_endpoint_id.to_string(),
        wg_public_key: wg_public_key.to_string(),
    }))
}

fn service_payload() -> Result<FactPayload, String> {
    projection_payload(ProjectionFactPayload::ServiceRegistered(
        ServiceRegistrationFact {
            service: ServiceName::new("web"),
            node_id: NodeId::new("node-projected"),
            version: "1.0.0".to_string(),
            endpoint_subject: "service.web.changed".to_string(),
            epoch: 1,
        },
    ))
}

fn route_payload() -> Result<FactPayload, String> {
    projection_payload(ProjectionFactPayload::RouteCommit(RouteCommitFact {
        route_commit_id: "route-net".to_string(),
        route_id: RouteId::new("web-http"),
        hostnames: vec!["web.example.com".to_string()],
        backends: vec![BackendEndpoint {
            node_id: NodeId::new("node-projected"),
            address: "10.0.0.24:8080".to_string(),
        }],
        old_backends_to_drain: Vec::new(),
    }))
}

fn gateway_payload() -> Result<FactPayload, String> {
    projection_payload(ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
        gateway_commit_id: "gateway-net".to_string(),
        route_commit_id: "route-net".to_string(),
        epoch: 1,
    }))
}

fn dns_payload() -> Result<FactPayload, String> {
    projection_payload(ProjectionFactPayload::DnsCommit(DnsCommitFact {
        dns_commit_id: "dns-net".to_string(),
        epoch: 1,
        records: vec![DnsRecordFact {
            name: "web.example.com".to_string(),
            record_type: "AAAA".to_string(),
            value: "fd00::24".to_string(),
            ttl_seconds: 30,
        }],
    }))
}

fn projection_payload(payload: ProjectionFactPayload) -> Result<FactPayload, String> {
    payload
        .to_fact_bytes()
        .map(FactPayload::from)
        .map_err(|error| format!("serialize p2panda-net projection fact: {error}"))
}

fn fact_key(value: &str) -> Result<FactKey, String> {
    FactKey::parse(value).map_err(|error| format!("parse p2panda-net fact key {value}: {error}"))
}

fn assert_projected_state(state: &mvp_projection::ProjectionState) -> Result<(), String> {
    assert_eq_named("fact-node projected nodes", state.nodes.len(), 1)?;
    assert_eq_named("fact-node projected services", state.services.len(), 1)?;
    assert_eq_named(
        "fact-node projected gateway routes",
        state
            .gateway
            .as_ref()
            .map_or(0, |gateway| gateway.routes.len()),
        1,
    )?;
    assert_eq_named(
        "fact-node projected dns records",
        state.dns.as_ref().map_or(0, |dns| dns.records.len()),
        1,
    )?;
    if !state.nodes.contains_key(&NodeId::new("node-projected")) {
        return Err("fact-node projection missed the non-conflicting node".to_string());
    }
    if state.nodes.contains_key(&NodeId::new("node-net")) {
        return Err("fact-node projection included conflicted node-net".to_string());
    }
    Ok(())
}
