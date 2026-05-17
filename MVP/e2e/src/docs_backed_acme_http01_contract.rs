use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_acme::{
    AcmeChallengeId, AcmeChallengeToken, AcmeHostname, AcmeHttp01ClearedFact,
    AcmeHttp01PresentedFact, AcmeKeyAuthorization,
};
use mvp_bus::{FactContentHash, FactKey, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_iroh::{IrohDocsFactSource, IrohFactAuthor, IrohFactDoc, IrohFactNode};
use mvp_lease::{LeaseClaimed, LeaseEpoch, LeaseFact, LeaseHolder, LeaseResource, LeaseTimestamp};
use mvp_projection::{
    CandidateStatus, DnsCommitFact, FactSource, ProjectionFactPayload, ProjectionIgnoreReason,
    SqliteProjectionStore, load_gateway_snapshot,
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
const DOC_TIMEOUT: Duration = Duration::from_secs(5);
const ACME_TOKEN: &str = "tokAcme0123456789abcdef";

#[derive(Debug, Serialize)]
struct DocsBackedAcmeHttp01Report {
    scenario: &'static str,
    selected_holder: String,
    selected_key_authorization: String,
    initial_conflict_candidates: usize,
    projected_challenges_before_clear: usize,
    projected_challenges_after_clear: usize,
    superseded_count_before_clear: usize,
    superseded_count_after_clear: usize,
    sqlite_challenges_before_clear: usize,
    sqlite_challenges_after_clear: usize,
    gateway_snapshot_challenges_before_clear: usize,
    http_served_before_clear: bool,
    http_404_after_clear: bool,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create tokio runtime for docs-backed ACME: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("docs-backed-acme-http01-contract");
    reset_dir(&root)?;

    let prod = IslandId::new("prod");
    let (bus, authority) = InMemoryBus::new_with_authority();
    let issuer_a = authority.grant_in(
        prod.clone(),
        PrincipalId::new("issuer-a"),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/lease/>")?)
            .with_fact_write(fact_pattern("/facts/acme/>")?),
    );
    let issuer_b = authority.grant_in(
        prod.clone(),
        PrincipalId::new("issuer-b"),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/lease/>")?)
            .with_fact_write(fact_pattern("/facts/acme/>")?),
    );
    let dns_writer = authority.grant_in(
        prod.clone(),
        PrincipalId::new("dns-writer"),
        Grant::empty().with_fact_write(fact_pattern("/facts/dns/>")?),
    );
    let projection_session = authority.grant_in(
        prod.clone(),
        PrincipalId::new("projection"),
        Grant::empty().with_fact_read(fact_pattern("/facts/>")?),
    );

    let node = IrohFactNode::memory()
        .await
        .map_err(|error| format!("spawn iroh fact node: {error}"))?;
    let doc = node
        .create_fact_doc(prod.clone())
        .await
        .map_err(|error| format!("create fact doc: {error}"))?;
    let author_a = node
        .create_author(issuer_a.principal().clone())
        .await
        .map_err(|error| format!("create issuer-a author: {error}"))?;
    let author_b = node
        .create_author(issuer_b.principal().clone())
        .await
        .map_err(|error| format!("create issuer-b author: {error}"))?;
    let dns_author = node
        .create_author(dns_writer.principal().clone())
        .await
        .map_err(|error| format!("create dns author: {error}"))?;

    let challenge = AcmeChallengeId::new(
        AcmeHostname::parse("example.test").map_err(|error| error.to_string())?,
        AcmeChallengeToken::parse(ACME_TOKEN).map_err(|error| error.to_string())?,
    );
    let epoch = LeaseEpoch::first();
    let claim_a = lease_claim(&challenge, "issuer-a", epoch);
    let claim_b = lease_claim(&challenge, "issuer-b", epoch);
    let claim_hash_a = LeaseFact::Claimed(claim_a.clone()).content_hash();
    let claim_hash_b = LeaseFact::Claimed(claim_b.clone()).content_hash();
    let claim_key = fact_key(&challenge.lease_claimed_fact_key(epoch))?;
    write_doc_fact(
        &doc,
        &author_a,
        claim_key.clone(),
        ProjectionFactPayload::LeaseClaimed(claim_a),
        &bus,
    )
    .await?;
    write_doc_fact(
        &doc,
        &author_b,
        claim_key.clone(),
        ProjectionFactPayload::LeaseClaimed(claim_b),
        &bus,
    )
    .await?;

    write_doc_fact(
        &doc,
        &dns_author,
        fact_key("/facts/dns/dns-acme")?,
        ProjectionFactPayload::DnsCommit(DnsCommitFact {
            dns_commit_id: "dns-acme".to_string(),
            epoch: 1,
            records: Vec::new(),
        }),
        &bus,
    )
    .await?;

    let presented_a = presented_fact(challenge.clone(), "issuer-a", claim_hash_a, "thumbprintA")?;
    let presented_b = presented_fact(challenge.clone(), "issuer-b", claim_hash_b, "thumbprintB")?;
    let presented_key = fact_key(&challenge.presented_fact_key(epoch))?;
    write_doc_fact(
        &doc,
        &author_a,
        presented_key.clone(),
        ProjectionFactPayload::AcmeHttp01Presented(presented_a.clone()),
        &bus,
    )
    .await?;
    let presented_hash_b = write_doc_fact(
        &doc,
        &author_b,
        presented_key.clone(),
        ProjectionFactPayload::AcmeHttp01Presented(presented_b.clone()),
        &bus,
    )
    .await?;
    doc.wait_for_content_hash(&presented_key, &presented_hash_b, DOC_TIMEOUT)
        .await
        .map_err(|error| format!("wait for presented docs fact: {error}"))?;

    let source = IrohDocsFactSource::new(node.local_view(), Arc::new(bus.clone()));
    let actor = projection_actor(Arc::new(source.clone()), projection_session.clone(), &root)?;
    let before_clear = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project docs-backed ACME facts: {error}"))?;
    assert_eq_named(
        "projected challenges before clear",
        before_clear.state.acme_http01.len(),
        1,
    )?;
    let initial_candidates = source
        .list_candidates(&prod, &fact_pattern("/facts/acme/>")?, &projection_session)
        .map_err(|error| format!("list ACME candidates: {error}"))?;
    let initial_conflict_candidates = initial_candidates
        .iter()
        .filter(|candidate| candidate.status() == CandidateStatus::Conflict)
        .count();
    assert_eq_named("ACME conflict candidates", initial_conflict_candidates, 2)?;

    let selected = before_clear
        .state
        .acme_http01
        .values()
        .next()
        .cloned()
        .ok_or_else(|| "ACME projection did not expose a selected challenge".to_string())?;
    let selected_key_authorization = selected.key_authorization.as_str().to_string();
    let selected_holder = selected.holder.as_str().to_string();

    let sqlite = SqliteProjectionStore::new(root.join("projections.sqlite"));
    let sqlite_before = sqlite
        .row_counts()
        .map_err(|error| format!("read sqlite row counts before clear: {error}"))?;
    assert_eq_named(
        "sqlite ACME challenge rows before clear",
        sqlite_before.acme_http01_challenges,
        1,
    )?;
    let gateway_snapshot = load_gateway_snapshot(root.join("gateway.snapshot"), &prod)
        .map_err(|error| format!("load gateway snapshot before clear: {error}"))?;
    assert_eq_named(
        "gateway snapshot ACME challenges",
        gateway_snapshot.acme_http01.len(),
        1,
    )?;

    let serving = ServingActorHandle::spawn(
        prod.clone(),
        ServingSnapshotPaths::new(root.join("gateway.snapshot"), root.join("dns.snapshot")),
        Duration::from_secs(60),
    )
    .map_err(|error| format!("spawn serving actor: {error}"))?;
    let gateway = spawn_http_gateway(loopback_any(), WireServingState::new(serving.clone()))
        .await
        .map_err(|error| format!("spawn HTTP gateway: {error}"))?;
    let before_response = http_get(
        gateway.listen_addr(),
        "example.test",
        &format!("/.well-known/acme-challenge/{ACME_TOKEN}"),
    )
    .await?;
    let http_served_before_clear = before_response.starts_with("HTTP/1.1 200 OK")
        && before_response.ends_with(&selected_key_authorization);
    if !http_served_before_clear {
        return Err(format!(
            "ACME HTTP-01 response before clear was not the projected winner: {before_response}"
        ));
    }

    let clear = AcmeHttp01ClearedFact::from_parts(
        challenge.clone(),
        selected.holder.clone(),
        selected.lease_epoch,
        selected.claim_hash,
        LeaseTimestamp::from_secs(120),
    );
    let clear_key =
        fact_key(&challenge.cleared_fact_key(selected.lease_epoch, selected.claim_hash))?;
    let clear_hash = write_doc_fact(
        &doc,
        if selected.holder.as_str() == "issuer-a" {
            &author_a
        } else {
            &author_b
        },
        clear_key.clone(),
        ProjectionFactPayload::AcmeHttp01Cleared(clear),
        &bus,
    )
    .await?;
    doc.wait_for_content_hash(&clear_key, &clear_hash, DOC_TIMEOUT)
        .await
        .map_err(|error| format!("wait for cleared docs fact: {error}"))?;

    let after_clear = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project cleared ACME fact: {error}"))?;
    assert_eq_named(
        "projected challenges after clear",
        after_clear.state.acme_http01.len(),
        0,
    )?;
    serving
        .reload()
        .await
        .map_err(|error| format!("reload serving after ACME clear: {error}"))?;
    let after_response = http_get(
        gateway.listen_addr(),
        "example.test",
        &format!("/.well-known/acme-challenge/{ACME_TOKEN}"),
    )
    .await?;
    let http_404_after_clear = after_response.starts_with("HTTP/1.1 404 Not Found");
    if !http_404_after_clear {
        return Err(format!(
            "ACME HTTP-01 response after clear did not become 404: {after_response}"
        ));
    }
    let sqlite_after = sqlite
        .row_counts()
        .map_err(|error| format!("read sqlite row counts after clear: {error}"))?;

    gateway
        .shutdown()
        .await
        .map_err(|error| format!("shutdown HTTP gateway: {error}"))?;
    node.shutdown()
        .await
        .map_err(|error| format!("shutdown iroh fact node: {error}"))?;

    let report = DocsBackedAcmeHttp01Report {
        scenario: "docs-backed-acme-http01-contract",
        selected_holder,
        selected_key_authorization,
        initial_conflict_candidates,
        projected_challenges_before_clear: before_clear.state.acme_http01.len(),
        projected_challenges_after_clear: after_clear.state.acme_http01.len(),
        superseded_count_before_clear: status_count(
            &before_clear.state.statuses,
            ProjectionIgnoreReason::Superseded,
        ),
        superseded_count_after_clear: status_count(
            &after_clear.state.statuses,
            ProjectionIgnoreReason::Superseded,
        ),
        sqlite_challenges_before_clear: sqlite_before.acme_http01_challenges,
        sqlite_challenges_after_clear: sqlite_after.acme_http01_challenges,
        gateway_snapshot_challenges_before_clear: gateway_snapshot.acme_http01.len(),
        http_served_before_clear,
        http_404_after_clear,
        elapsed_ms: started.elapsed().as_millis(),
    };
    assert_eq_named(
        "superseded before clear",
        report.superseded_count_before_clear,
        2,
    )?;
    assert_eq_named(
        "sqlite ACME rows after clear",
        report.sqlite_challenges_after_clear,
        0,
    )?;

    let json = write_json(
        &root.join("docs-backed-acme-http01-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS docs-backed-acme-http01-contract");
    Ok(())
}

async fn write_doc_fact(
    doc: &IrohFactDoc,
    author: &IrohFactAuthor,
    key: FactKey,
    payload: ProjectionFactPayload,
    bus: &InMemoryBus,
) -> Result<FactContentHash, String> {
    let bytes = payload
        .to_fact_bytes()
        .map_err(|error| format!("serialize docs fact {}: {error}", key.as_str()))?;
    doc.write_fact_payload(author, key.clone(), bytes.into(), bus)
        .await
        .map_err(|error| format!("write docs fact {}: {error}", key.as_str()))
}

fn lease_claim(id: &AcmeChallengeId, holder: &str, epoch: LeaseEpoch) -> LeaseClaimed {
    LeaseClaimed::new(
        LeaseResource::from_segments([
            "acme",
            "http01",
            id.hostname().as_str(),
            id.token().as_str(),
        ]),
        LeaseHolder::new(holder),
        epoch,
        LeaseTimestamp::from_secs(100),
        LeaseTimestamp::from_secs(160),
    )
}

fn presented_fact(
    id: AcmeChallengeId,
    holder: &str,
    claim_hash: mvp_lease::LeaseContentHash,
    thumbprint: &str,
) -> Result<AcmeHttp01PresentedFact, String> {
    let key_authorization =
        AcmeKeyAuthorization::parse_for_token(id.token(), format!("{}.{thumbprint}", id.token()))
            .map_err(|error| error.to_string())?;
    AcmeHttp01PresentedFact::from_parts(
        id,
        key_authorization,
        LeaseHolder::new(holder),
        LeaseEpoch::first(),
        claim_hash,
        LeaseTimestamp::from_secs(110),
    )
    .map_err(|error| error.to_string())
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

async fn http_get(addr: SocketAddr, host: &str, path: &str) -> Result<String, String> {
    let mut stream = TcpStream::connect(addr)
        .await
        .map_err(|error| format!("connect HTTP gateway: {error}"))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("write HTTP request: {error}"))?;
    let mut response = Vec::new();
    timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
        .await
        .map_err(|_| "read HTTP response timed out".to_string())?
        .map_err(|error| format!("read HTTP response: {error}"))?;
    String::from_utf8(response).map_err(|error| format!("HTTP response was not UTF-8: {error}"))
}

fn loopback_any() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
}
