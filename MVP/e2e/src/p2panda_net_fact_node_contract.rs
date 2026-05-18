use std::fs;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mvp_acme::{
    AcmeChallengeId, AcmeChallengeToken, AcmeHostname, AcmeHttp01ClearedFact,
    AcmeHttp01PresentedFact, AcmeKeyAuthorization,
};
use mvp_bus::{BusSession, FactPayload, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_identity::NodeId;
use mvp_lease::{
    LeaseClaimed, LeaseContentHash, LeaseEpoch, LeaseFact, LeaseHolder, LeaseResource,
    LeaseTimestamp,
};
use mvp_p2panda_facts::{PandaFactAuthor, PandaFactStore, SharedPandaFactStore};
use mvp_p2panda_transport::{
    PandaNetFactImportOutcome, PandaNetFactImportRejection, PandaNetFactImportReport,
    PandaNetFactNode, PandaNetFactNodeConfig, PandaNetNetworkId, PandaNetNodeConfig,
    PandaNetNodeSeed, PandaNetTopic, PandaNetTransportError, harness::import_p2panda_operation,
};
use mvp_projection::{
    BackendEndpoint, CandidateStatus, DnsCommitFact, DnsRecordFact, FactSource, GatewayCommitFact,
    NodeJoinedFact, ProjectionFactPayload, RouteCommitFact, RouteId, ServiceName,
    ServiceRegistrationFact, SqliteProjectionStore, load_dns_snapshot, load_gateway_snapshot,
};
use mvp_serving::{ServingActorHandle, ServingSnapshotPaths, WireServingState, spawn_http_gateway};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio::time::timeout;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::{fact_key, fact_pattern};
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::projection_harness::projection_actor;

const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);
const TEST_TOPIC: PandaNetTopic = PandaNetTopic::new([84; 32]);
const ACME_TOKEN: &str = "tokNet0123456789abcdef";

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
    projected_acme_challenges_before_clear: usize,
    projected_acme_challenges_after_clear: usize,
    gateway_snapshot_acme_challenges_before_clear: usize,
    http_served_before_clear: bool,
    http_404_after_clear: bool,
    startup_ms: u128,
    sync_import_ms: u128,
    acme_clear_import_ms: u128,
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
    let unauthorized_replica_probe = duplicate_operation
        .to_p2panda_operation()
        .map_err(|error| format!("convert duplicate operation for direct import: {error}"))?;
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
    let challenge = acme_challenge()?;
    let lease_epoch = LeaseEpoch::first();
    let lease_timeline = LeaseTimeline::fresh()?;
    let lease_holder = LeaseHolder::new(sessions.writer_a.principal().as_str());
    let claim = lease_claim(
        &challenge,
        lease_holder.clone(),
        lease_epoch,
        lease_timeline,
    );
    let claim_hash = LeaseFact::Claimed(claim.clone()).content_hash();
    let presented = presented_fact(
        challenge.clone(),
        lease_holder.clone(),
        lease_epoch,
        claim_hash,
        "thumbprintNet",
        lease_timeline,
    )?;
    let expected_key_authorization = presented.key_authorization().as_str().to_string();
    sender
        .publish_fact_payload(
            &sessions.writer_a,
            &writer_a,
            fact_key(&challenge.lease_claimed_fact_key(lease_epoch))?,
            projection_payload(ProjectionFactPayload::LeaseClaimed(claim))?,
        )
        .await
        .map_err(|error| format!("publish fact-node ACME lease fact: {error}"))?;
    sender
        .publish_fact_payload(
            &sessions.writer_a,
            &writer_a,
            fact_key(&challenge.presented_fact_key(lease_epoch))?,
            projection_payload(ProjectionFactPayload::AcmeHttp01Presented(presented))?,
        )
        .await
        .map_err(|error| format!("publish fact-node ACME presented fact: {error}"))?;
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
    let unauthorized = import_p2panda_operation(
        unauthorized_replica_probe,
        &unauthorized_store,
        &sessions.untrusted_replica,
        TEST_TOPIC,
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
    let mismatch_operation = mismatch_source
        .export_operations()
        .last()
        .ok_or_else(|| "author-key mismatch source produced no operation".to_string())?
        .to_p2panda_operation()
        .map_err(|error| format!("convert author-key mismatch operation: {error}"))?;
    let author_mismatch = import_p2panda_operation(
        mismatch_operation,
        &receiver.store(),
        &sessions.receiver_replica,
        TEST_TOPIC,
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
        8,
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
        "fact-node projected ACME challenges before clear",
        projection.state.acme_http01.len(),
        1,
    )?;
    let gateway_snapshot_before_clear = load_gateway_snapshot(root.join("gateway.snapshot"), &prod)
        .map_err(|error| format!("load fact-node gateway snapshot: {error}"))?;
    assert_eq_named(
        "fact-node loaded gateway routes",
        gateway_snapshot_before_clear.routes.len(),
        1,
    )?;
    assert_eq_named(
        "fact-node gateway snapshot ACME challenges before clear",
        gateway_snapshot_before_clear.acme_http01.len(),
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
    let serving = ServingActorHandle::spawn(
        prod.clone(),
        ServingSnapshotPaths::new(root.join("gateway.snapshot"), root.join("dns.snapshot")),
        Duration::from_secs(60),
    )
    .map_err(|error| format!("spawn fact-node serving actor: {error}"))?;
    let gateway = spawn_http_gateway(loopback_any(), WireServingState::new(serving.clone()))
        .await
        .map_err(|error| format!("spawn fact-node HTTP gateway: {error}"))?;
    let before_response = http_get(
        gateway.listen_addr(),
        "example.test",
        &format!("/.well-known/acme-challenge/{ACME_TOKEN}"),
    )
    .await?;
    let http_served_before_clear = before_response.starts_with("HTTP/1.1 200 OK")
        && before_response.ends_with(&expected_key_authorization);
    if !http_served_before_clear {
        return Err(format!(
            "fact-node ACME HTTP-01 response before clear was not the projected challenge: {before_response}"
        ));
    }

    let clear_started = Instant::now();
    sender
        .publish_fact_payload(
            &sessions.writer_a,
            &writer_a,
            fact_key(&challenge.cleared_fact_key(lease_epoch, claim_hash))?,
            projection_payload(ProjectionFactPayload::AcmeHttp01Cleared(
                AcmeHttp01ClearedFact::from_parts(
                    challenge.clone(),
                    lease_holder,
                    lease_epoch,
                    claim_hash,
                    lease_timeline.cleared_at,
                ),
            ))?,
        )
        .await
        .map_err(|error| format!("publish fact-node ACME cleared fact: {error}"))?;
    let clear_import_report = import_until_imports(&mut receiver, 1).await?;
    assert_eq_named(
        "fact-node ACME clear imported operations",
        clear_import_report.imported as u64,
        1,
    )?;
    let acme_clear_import_ms = clear_started.elapsed().as_millis();

    let after_clear = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project p2panda-net fact-node cleared ACME fact: {error}"))?;
    assert_projected_state(&after_clear.state)?;
    assert_eq_named(
        "fact-node projected ACME challenges after clear",
        after_clear.state.acme_http01.len(),
        0,
    )?;
    serving
        .reload()
        .await
        .map_err(|error| format!("reload fact-node serving after ACME clear: {error}"))?;
    let after_response = http_get(
        gateway.listen_addr(),
        "example.test",
        &format!("/.well-known/acme-challenge/{ACME_TOKEN}"),
    )
    .await?;
    let http_404_after_clear = after_response.starts_with("HTTP/1.1 404 Not Found");
    if !http_404_after_clear {
        return Err(format!(
            "fact-node ACME HTTP-01 response after clear did not become 404: {after_response}"
        ));
    }
    gateway
        .shutdown()
        .await
        .map_err(|error| format!("shutdown fact-node HTTP gateway: {error}"))?;

    let sqlite = SqliteProjectionStore::new(root.join("projections.sqlite"));
    let loaded = sqlite
        .load()
        .map_err(|error| format!("load fact-node sqlite projection: {error}"))?;
    if loaded != after_clear.state {
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
    if restarted.state != after_clear.state {
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
        projected_nodes: after_clear.state.nodes.len(),
        projected_services: after_clear.state.services.len(),
        projected_gateway_routes: after_clear
            .state
            .gateway
            .as_ref()
            .map_or(0, |gateway| gateway.routes.len()),
        projected_dns_records: after_clear
            .state
            .dns
            .as_ref()
            .map_or(0, |dns| dns.records.len()),
        projected_acme_challenges_before_clear: projection.state.acme_http01.len(),
        projected_acme_challenges_after_clear: after_clear.state.acme_http01.len(),
        gateway_snapshot_acme_challenges_before_clear: gateway_snapshot_before_clear
            .acme_http01
            .len(),
        http_served_before_clear,
        http_404_after_clear,
        startup_ms,
        sync_import_ms,
        acme_clear_import_ms,
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
    report.imported >= 8
        && report.duplicate == 0
        && report.conflict >= 1
        && report.rejected.len() >= 2
        && report.deferred.is_empty()
        && report.failed.is_empty()
}

async fn import_until_imports(
    receiver: &mut PandaNetFactNode,
    imported: usize,
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
        if report.imported >= imported && report.deferred.is_empty() && report.failed.is_empty() {
            return Ok(report);
        }
    }
    Err(format!(
        "p2panda-net fact-node imports did not arrive: {report:?}"
    ))
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
        TEST_TOPIC,
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

fn acme_challenge() -> Result<AcmeChallengeId, String> {
    Ok(AcmeChallengeId::new(
        AcmeHostname::parse("example.test").map_err(|error| error.to_string())?,
        AcmeChallengeToken::parse(ACME_TOKEN).map_err(|error| error.to_string())?,
    ))
}

#[derive(Debug, Clone, Copy)]
struct LeaseTimeline {
    acquired_at: LeaseTimestamp,
    published_at: LeaseTimestamp,
    cleared_at: LeaseTimestamp,
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
            expires_at: LeaseTimestamp::from_secs(now + 600),
        })
    }
}

fn lease_claim(
    id: &AcmeChallengeId,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    timeline: LeaseTimeline,
) -> LeaseClaimed {
    LeaseClaimed::new(
        LeaseResource::from_segments([
            "acme",
            "http01",
            id.hostname().as_str(),
            id.token().as_str(),
        ]),
        holder,
        epoch,
        timeline.acquired_at,
        timeline.expires_at,
    )
}

fn presented_fact(
    id: AcmeChallengeId,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    claim_hash: LeaseContentHash,
    thumbprint: &str,
    timeline: LeaseTimeline,
) -> Result<AcmeHttp01PresentedFact, String> {
    let key_authorization =
        AcmeKeyAuthorization::parse_for_token(id.token(), format!("{}.{thumbprint}", id.token()))
            .map_err(|error| error.to_string())?;
    AcmeHttp01PresentedFact::from_parts(
        id,
        key_authorization,
        holder,
        epoch,
        claim_hash,
        timeline.published_at,
    )
    .map_err(|error| error.to_string())
}

async fn http_get(addr: SocketAddr, host: &str, path: &str) -> Result<String, String> {
    let mut stream = TcpStream::connect(addr)
        .await
        .map_err(|error| format!("connect fact-node HTTP gateway: {error}"))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("write fact-node HTTP request: {error}"))?;
    let mut response = Vec::new();
    timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
        .await
        .map_err(|_| "read fact-node HTTP response timed out".to_string())?
        .map_err(|error| format!("read fact-node HTTP response: {error}"))?;
    String::from_utf8(response)
        .map_err(|error| format!("fact-node HTTP response was not UTF-8: {error}"))
}

fn loopback_any() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
}

fn projection_payload(payload: ProjectionFactPayload) -> Result<FactPayload, String> {
    payload
        .to_fact_bytes()
        .map(FactPayload::from)
        .map_err(|error| format!("serialize p2panda-net projection fact: {error}"))
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
