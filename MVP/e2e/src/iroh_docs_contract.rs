use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_bus::{FactKey, FactKeyPattern, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_identity::NodeId;
use mvp_iroh::{IrohDocsFactSource, IrohFactDoc, IrohFactNode};
use mvp_projection::{
    CandidateStatus, FactCandidate, FactSource, NodeJoinedFact, ProjectionFactPayload,
};
use serde::Serialize;
use tokio::time::sleep;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::{fact_key, fact_pattern};
use crate::metrics::{reset_dir, scenario_dir, write_json};

const SYNC_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Serialize)]
struct IrohDocsContractReport {
    scenario: &'static str,
    sync_timeout_ms: u128,
    initial_sync_ms: u128,
    conflict_sync_ms: u128,
    unauthorized_sync_ms: u128,
    verified_candidates: usize,
    conflict_candidates: usize,
    unauthorized_candidates: usize,
    conflict_payloads_read: usize,
    unauthorized_payloads_read: usize,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_io()
        .enable_time()
        .build()
        .map_err(|error| format!("create tokio runtime for iroh docs contract: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("iroh-docs-contract");
    reset_dir(&root)?;

    let (bus, authority) = InMemoryBus::new_with_authority();
    let prod = island("prod");
    let writer_one = authority.grant_in(
        prod.clone(),
        principal("node-1"),
        Grant::empty().with_fact_write(fact_pattern("/facts/node/>")?),
    );
    let writer_two = authority.grant_in(
        prod.clone(),
        principal("node-2"),
        Grant::empty().with_fact_write(fact_pattern("/facts/node/>")?),
    );
    let projection = authority.grant_in(
        prod.clone(),
        principal("projection"),
        Grant::empty().with_fact_read(fact_pattern("/facts/node/>")?),
    );

    let node_a = IrohFactNode::memory()
        .await
        .map_err(|error| format!("spawn iroh node a: {error}"))?;
    let node_b = IrohFactNode::memory()
        .await
        .map_err(|error| format!("spawn iroh node b: {error}"))?;
    let doc_a = node_a
        .create_fact_doc(prod.clone())
        .await
        .map_err(|error| format!("create fact doc: {error}"))?;
    let author_one = node_a
        .create_author(writer_one.principal().clone())
        .await
        .map_err(|error| format!("create writer one author: {error}"))?;
    let joined_key = fact_key("/facts/node/node-1/joined/1")?;
    let initial_hash = doc_a
        .write_fact_payload(
            &author_one,
            joined_key.clone(),
            node_joined_payload("node-1", 1, "fd00::1")?,
            &bus,
        )
        .await
        .map_err(|error| format!("write initial docs fact: {error}"))?;
    let ticket = doc_a
        .share()
        .await
        .map_err(|error| format!("share docs fact set: {error}"))?;
    let doc_b = node_b
        .import_fact_doc(ticket)
        .await
        .map_err(|error| format!("import docs fact set: {error}"))?;
    let source = IrohDocsFactSource::new(node_b.local_view(), Arc::new(bus.clone()));

    let initial_started = Instant::now();
    doc_b
        .wait_for_content_hash(&joined_key, &initial_hash, SYNC_TIMEOUT)
        .await
        .map_err(|error| format!("wait for initial docs sync: {error}"))?;
    let initial_sync_ms = initial_started.elapsed().as_millis();
    let initial_candidates = wait_for_candidates(
        &doc_b,
        &joined_key,
        &source,
        &prod,
        &fact_pattern("/facts/node/node-1/joined/1")?,
        &projection,
        |candidates| candidates.len() == 1,
    )
    .await?;
    assert_eq_named(
        "initial candidate status",
        initial_candidates[0].status(),
        CandidateStatus::Verified,
    )?;
    let initial_payloads = source
        .read_payloads(&prod, &initial_candidates, &projection)
        .map_err(|error| format!("read initial payloads: {error}"))?;
    if !initial_payloads.contains_key(&initial_hash) {
        return Err("initial docs payload was not readable through fact source".to_string());
    }

    let author_two = node_a
        .create_author(writer_two.principal().clone())
        .await
        .map_err(|error| format!("create writer two author: {error}"))?;
    doc_b
        .bind_author(&author_two)
        .map_err(|error| format!("bind writer two on imported doc: {error}"))?;

    let conflict_hash = doc_a
        .write_fact_payload(
            &author_two,
            joined_key.clone(),
            node_joined_payload("node-1", 1, "fd00::2")?,
            &bus,
        )
        .await
        .map_err(|error| format!("write conflicting docs fact: {error}"))?;
    let conflict_started = Instant::now();
    doc_b
        .wait_for_content_hash(&joined_key, &conflict_hash, SYNC_TIMEOUT)
        .await
        .map_err(|error| format!("wait for conflict docs sync: {error}"))?;
    let conflict_candidates = wait_for_candidates(
        &doc_b,
        &joined_key,
        &source,
        &prod,
        &fact_pattern("/facts/node/node-1/joined/1")?,
        &projection,
        |candidates| {
            candidates.len() == 2
                && candidates
                    .iter()
                    .all(|candidate| candidate.status() == CandidateStatus::Conflict)
        },
    )
    .await?;
    let conflict_sync_ms = conflict_started.elapsed().as_millis();
    let conflict_payloads = source
        .read_payloads(&prod, &conflict_candidates, &projection)
        .map_err(|error| format!("read conflict payloads: {error}"))?;

    let unauthorized_key = fact_key("/facts/node/node-unauthorized/joined/1")?;
    let unauthorized_hash = doc_a
        .write_fact_payload(
            &author_two,
            unauthorized_key.clone(),
            node_joined_payload("node-unauthorized", 1, "fd00::3")?,
            &bus,
        )
        .await
        .map_err(|error| format!("write soon-unauthorized docs fact: {error}"))?;
    let unauthorized_started = Instant::now();
    doc_b
        .wait_for_content_hash(&unauthorized_key, &unauthorized_hash, SYNC_TIMEOUT)
        .await
        .map_err(|error| format!("wait for unauthorized-key sync: {error}"))?;
    assert!(authority.revoke(&writer_two));
    let unauthorized_candidates = wait_for_candidates(
        &doc_b,
        &unauthorized_key,
        &source,
        &prod,
        &fact_pattern("/facts/node/node-unauthorized/joined/1")?,
        &projection,
        |candidates| {
            candidates.len() == 1
                && candidates[0].status() == CandidateStatus::Unauthorized
                && *candidates[0].content_hash() == unauthorized_hash
        },
    )
    .await?;
    let unauthorized_sync_ms = unauthorized_started.elapsed().as_millis();
    let unauthorized_payloads = source
        .read_payloads(&prod, &unauthorized_candidates, &projection)
        .map_err(|error| format!("read unauthorized payloads: {error}"))?;

    let report = IrohDocsContractReport {
        scenario: "iroh-docs-contract",
        sync_timeout_ms: SYNC_TIMEOUT.as_millis(),
        initial_sync_ms,
        conflict_sync_ms,
        unauthorized_sync_ms,
        verified_candidates: initial_candidates
            .iter()
            .filter(|candidate| candidate.status() == CandidateStatus::Verified)
            .count(),
        conflict_candidates: conflict_candidates
            .iter()
            .filter(|candidate| candidate.status() == CandidateStatus::Conflict)
            .count(),
        unauthorized_candidates: unauthorized_candidates
            .iter()
            .filter(|candidate| candidate.status() == CandidateStatus::Unauthorized)
            .count(),
        conflict_payloads_read: conflict_payloads.len(),
        unauthorized_payloads_read: unauthorized_payloads.len(),
        elapsed_ms: started.elapsed().as_millis(),
    };

    assert_eq_named("verified candidates", report.verified_candidates, 1)?;
    assert_eq_named("conflict candidates", report.conflict_candidates, 2)?;
    assert_eq_named("unauthorized candidates", report.unauthorized_candidates, 1)?;
    assert_eq_named("conflict payloads read", report.conflict_payloads_read, 2)?;
    assert_eq_named(
        "unauthorized payloads read",
        report.unauthorized_payloads_read,
        0,
    )?;

    node_a
        .shutdown()
        .await
        .map_err(|error| format!("shutdown iroh node a: {error}"))?;
    node_b
        .shutdown()
        .await
        .map_err(|error| format!("shutdown iroh node b: {error}"))?;

    let json = write_json(&root.join("iroh-docs-contract-metrics.json"), &report)?;
    println!("{json}");
    eprintln!("PASS iroh-docs-contract");
    Ok(())
}

async fn wait_for_candidates(
    doc: &IrohFactDoc,
    key: &FactKey,
    source: &IrohDocsFactSource,
    island: &IslandId,
    pattern: &FactKeyPattern,
    session: &mvp_bus::BusSession,
    ready: impl Fn(&[FactCandidate]) -> bool,
) -> Result<Vec<FactCandidate>, String> {
    let started = Instant::now();
    let mut last_candidates = Vec::new();
    while started.elapsed() < SYNC_TIMEOUT {
        doc.refresh_fact_key(key)
            .await
            .map_err(|error| format!("refresh docs local view: {error}"))?;
        let candidates = source
            .list_candidates(island, pattern, session)
            .map_err(|error| format!("list docs candidates: {error}"))?;
        last_candidates = candidates
            .iter()
            .map(|candidate| {
                format!(
                    "{} {} {:?}",
                    candidate.key(),
                    candidate.author(),
                    candidate.status()
                )
            })
            .collect::<Vec<_>>();
        if ready(&candidates) {
            return Ok(candidates);
        }
        sleep(Duration::from_millis(25)).await;
    }
    Err(format!(
        "timed out after {SYNC_TIMEOUT:?} waiting for candidates matching {}; last candidates: {last_candidates:?}",
        pattern.as_str(),
    ))
}

fn island(value: &str) -> IslandId {
    IslandId::new(value)
}

fn principal(value: &str) -> PrincipalId {
    PrincipalId::new(value)
}

fn node_joined_payload(
    node_id: &str,
    epoch: u64,
    overlay_ip: &str,
) -> Result<mvp_bus::Payload, String> {
    ProjectionFactPayload::NodeJoined(NodeJoinedFact {
        node_id: NodeId::new(node_id),
        epoch,
        overlay_ip: overlay_ip.to_string(),
        iroh_endpoint_id: "iroh-test".to_string(),
        wg_public_key: "wg-test".to_string(),
    })
    .to_fact_bytes()
    .map(Into::into)
    .map_err(|error| format!("serialize node joined payload for {node_id}: {error}"))
}
