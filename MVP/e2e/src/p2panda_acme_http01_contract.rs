use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use mvp_acme::{
    AcmeChallengeId, AcmeChallengeToken, AcmeHostname, AcmeHttp01ClearedFact,
    AcmeHttp01PresentedFact, AcmeKeyAuthorization,
};
use mvp_bus::{BusSession, FactPayload, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_identity::{NodeId, VisibleNodes};
use mvp_lease::{
    LeaseClaimed, LeaseContentHash, LeaseEpoch, LeaseFact, LeaseHolder, LeaseReleased, LeaseState,
    LeaseTimestamp,
};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactStore, PandaFactSyncScope, PandaFactWriteOutcome,
    PandaSqliteOpenConfig, PandaTrustedAuthorKey, sync_panda_fact_stores,
};
use mvp_projection::{
    CandidateStatus, DnsCommitFact, FactCandidate, FactKind, FactSource, ProjectionFactPayload,
    ProjectionIgnoreReason, SqliteProjectionStore,
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
const ACME_TOKEN: &str = "tokPanda0123456789abcdef";
const OTHER_TOKEN: &str = "tokOther0123456789abcdef";

#[derive(Debug, Serialize)]
struct P2pandaAcmeHttp01Report {
    scenario: &'static str,
    visible_nodes_at_decision: usize,
    initial_key_authorization: String,
    takeover_key_authorization: String,
    sync_ms: u128,
    projection_reload_ms: u128,
    http_request_us: u128,
    command_adapter_outage_serving_success_count: usize,
    stale_mutation_rejections: usize,
    scoped_grant_rejections: usize,
    stale_sync_preserved_winner: bool,
    release_fact_recorded: bool,
    trusted_replica_required: bool,
    duplicate_sync_noop: bool,
    sqlite_rebuild_after_delete: bool,
    http_404_after_clear: bool,
    superseded_count: usize,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create tokio runtime for p2panda ACME: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("p2panda-acme-http01-contract");
    reset_dir(&root)?;

    let prod = IslandId::new("prod");
    let (bus, sessions) = acme_bus_sessions(&prod)?;
    let bus = Arc::new(bus);
    let issuer_a_author = PandaFactAuthor::new(sessions.issuer_a.principal().clone());
    let issuer_b_author = PandaFactAuthor::new(sessions.issuer_b.principal().clone());
    let dns_author = PandaFactAuthor::new(sessions.dns_writer.principal().clone());
    let authors = [&issuer_a_author, &issuer_b_author, &dns_author];

    let mut left = open_store(
        bus.clone(),
        root.join("left-p2panda-facts.sqlite"),
        &prod,
        &authors,
    )
    .await?;
    let mut right = open_store(
        bus.clone(),
        root.join("right-p2panda-facts.sqlite"),
        &prod,
        &authors,
    )
    .await?;
    left.trust_replica_peer(&prod, sessions.left_replica.principal().clone());
    right.trust_replica_peer(&prod, sessions.right_replica.principal().clone());

    let challenge = AcmeChallengeId::new(
        AcmeHostname::parse("example.test").map_err(|error| error.to_string())?,
        AcmeChallengeToken::parse(ACME_TOKEN).map_err(|error| error.to_string())?,
    );
    let other_challenge = AcmeChallengeId::new(
        AcmeHostname::parse("example.test").map_err(|error| error.to_string())?,
        AcmeChallengeToken::parse(OTHER_TOKEN).map_err(|error| error.to_string())?,
    );
    let timeline = LeaseTimeline::fresh()?;
    let visible_nodes = VisibleNodes::new([NodeId::new("node-a"), NodeId::new("node-b")]);
    let scope = authors
        .iter()
        .fold(PandaFactSyncScope::new(prod.clone()), |scope, author| {
            scope.with_trusted_author(author.principal().clone(), author.author_key())
        });

    write_projection_fact(
        &mut left,
        &sessions.dns_writer,
        &dns_author,
        "/facts/dns/dns-acme-p2panda",
        ProjectionFactPayload::DnsCommit(DnsCommitFact {
            dns_commit_id: "dns-acme-p2panda".to_string(),
            epoch: 1,
            records: Vec::new(),
        }),
    )
    .await?;

    let mut adapter_a =
        AcmeP2pandaCommandAdapter::new(sessions.issuer_a.clone(), visible_nodes.clone());
    let mut adapter_b =
        AcmeP2pandaCommandAdapter::new(sessions.issuer_b.clone(), visible_nodes.clone());

    let claim_a = adapter_a
        .claim(
            &mut left,
            &issuer_a_author,
            &challenge,
            timeline.acquired_at,
            timeline.expires_at,
        )
        .await
        .map_err(|error| error.to_string())?;
    let present_a = adapter_a
        .present(
            &mut left,
            &issuer_a_author,
            &challenge,
            &claim_a.lease,
            "thumbprint-a",
            timeline.published_at,
        )
        .await
        .map_err(|error| error.to_string())?;
    let stale_before_mutation = adapter_b
        .claim(
            &mut left,
            &issuer_b_author,
            &challenge,
            timeline.published_at,
            timeline.expires_at,
        )
        .await
        .expect_err("locally visible lease conflict rejects second issuer");
    assert!(matches!(
        stale_before_mutation,
        AcmeAdapterError::Conflict { .. }
    ));
    let other_claim = adapter_a
        .claim(
            &mut left,
            &issuer_a_author,
            &other_challenge,
            timeline.acquired_at,
            timeline.expires_at,
        )
        .await
        .map_err(|error| error.to_string())?;
    let wrong_scope = adapter_a
        .present(
            &mut left,
            &issuer_a_author,
            &other_challenge,
            &other_claim.lease,
            "thumbprint-wrong-scope",
            timeline.published_at,
        )
        .await
        .expect_err("ACME grant scoped to one challenge rejects another");
    assert!(matches!(wrong_scope, AcmeAdapterError::Fact(_)));

    let sync_started = Instant::now();
    let first_sync = sync_panda_fact_stores(
        &mut left,
        &sessions.left_replica,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .map_err(|error| format!("run initial ACME p2panda sync: {error}"))?;
    let sync_ms = sync_started.elapsed().as_millis();
    if first_sync.right.imported < 3 {
        return Err(format!(
            "expected initial sync to import lease, ACME, and DNS facts, got {}",
            first_sync.right.imported
        ));
    }

    let rejected_sync = sync_panda_fact_stores(
        &mut left,
        &sessions.projection,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .expect_err("projection-only principal cannot run replica sync");
    let trusted_replica_required = rejected_sync.to_string().contains("not a trusted replica");
    if !trusted_replica_required {
        return Err(format!(
            "projection-only sync failed for the wrong reason: {rejected_sync}"
        ));
    }
    let repeat = sync_panda_fact_stores(
        &mut left,
        &sessions.left_replica,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .map_err(|error| format!("run repeat ACME sync: {error}"))?;
    let duplicate_sync_noop = repeat.left.received + repeat.right.received == 0;

    let mut projection =
        project_from_reopened_store(&root, bus.clone(), &prod, &authors, &sessions).await?;
    assert_eq_named(
        "initial p2panda ACME projection count",
        projection.state.acme_http01.len(),
        1,
    )?;
    let initial_key_authorization = projected_key_authorization(&projection)?;

    let serving = ServingActorHandle::spawn(
        prod.clone(),
        ServingSnapshotPaths::new(root.join("gateway.snapshot"), root.join("dns.snapshot")),
        Duration::from_secs(60),
    )
    .map_err(|error| format!("spawn p2panda ACME serving actor: {error}"))?;
    let gateway = spawn_http_gateway(loopback_any(), WireServingState::new(serving.clone()))
        .await
        .map_err(|error| format!("spawn p2panda ACME HTTP gateway: {error}"))?;
    let initial_http = timed_http_get(
        gateway.listen_addr(),
        "example.test",
        &format!("/.well-known/acme-challenge/{ACME_TOKEN}"),
    )
    .await?;
    let mut outage_success_count = 0;
    if initial_http.response.starts_with("HTTP/1.1 200 OK")
        && initial_http
            .response
            .ends_with(present_a.key_authorization.as_str())
    {
        outage_success_count += 1;
    }

    let pending_clear = adapter_a
        .clear(
            &mut left,
            &issuer_a_author,
            &challenge,
            &claim_a.lease,
            timeline.cleared_at,
        )
        .await
        .map_err(|error| error.to_string())?;
    drop(adapter_a);
    let outage_http = timed_http_get(
        gateway.listen_addr(),
        "example.test",
        &format!("/.well-known/acme-challenge/{ACME_TOKEN}"),
    )
    .await?;
    if outage_http.response.starts_with("HTTP/1.1 200 OK")
        && outage_http.response.ends_with(&initial_key_authorization)
    {
        outage_success_count += 1;
    }

    sync_panda_fact_stores(
        &mut left,
        &sessions.left_replica,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .map_err(|error| format!("sync pending clear after adapter drop: {error}"))?;
    projection =
        project_from_reopened_store(&root, bus.clone(), &prod, &authors, &sessions).await?;
    serving
        .reload()
        .await
        .map_err(|error| format!("reload after synced clear: {error}"))?;
    assert_eq_named(
        "projection cleared after dropped adapter sync",
        projection.state.acme_http01.len(),
        0,
    )?;

    let claim_b = adapter_b
        .claim(
            &mut left,
            &issuer_b_author,
            &challenge,
            timeline.takeover_at,
            timeline.takeover_expires_at,
        )
        .await
        .map_err(|error| error.to_string())?;
    let present_b = adapter_b
        .present(
            &mut left,
            &issuer_b_author,
            &challenge,
            &claim_b.lease,
            "thumbprint-b",
            timeline.takeover_presented_at,
        )
        .await
        .map_err(|error| error.to_string())?;
    let stale_present = stale_presented_fact(
        challenge.clone(),
        &claim_a.lease,
        "thumbprint-stale",
        timeline.stale_arrival_at,
    )?;
    write_projection_fact(
        &mut left,
        &sessions.issuer_b,
        &issuer_b_author,
        &challenge.presented_fact_key(LeaseEpoch::first()),
        ProjectionFactPayload::AcmeHttp01Presented(stale_present),
    )
    .await
    .map_err(|error| format!("write stale fixture fact through p2panda: {error}"))?;

    sync_panda_fact_stores(
        &mut left,
        &sessions.left_replica,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .map_err(|error| format!("sync takeover and stale fact: {error}"))?;
    let projection_reload_started = Instant::now();
    projection =
        project_from_reopened_store(&root, bus.clone(), &prod, &authors, &sessions).await?;
    serving
        .reload()
        .await
        .map_err(|error| format!("reload after takeover: {error}"))?;
    let projection_reload_ms = projection_reload_started.elapsed().as_millis();
    let takeover_key_authorization = projected_key_authorization(&projection)?;
    let takeover_http = timed_http_get(
        gateway.listen_addr(),
        "example.test",
        &format!("/.well-known/acme-challenge/{ACME_TOKEN}"),
    )
    .await?;
    let stale_sync_preserved_winner = takeover_http.response.starts_with("HTTP/1.1 200 OK")
        && takeover_http
            .response
            .ends_with(present_b.key_authorization.as_str())
        && takeover_key_authorization == present_b.key_authorization.as_str();
    if !stale_sync_preserved_winner {
        return Err(format!(
            "stale synced ACME fact rolled back serving: {}",
            takeover_http.response
        ));
    }

    let final_clear = adapter_b
        .clear(
            &mut left,
            &issuer_b_author,
            &challenge,
            &claim_b.lease,
            timeline.final_cleared_at,
        )
        .await
        .map_err(|error| error.to_string())?;
    sync_panda_fact_stores(
        &mut left,
        &sessions.left_replica,
        &mut right,
        &sessions.right_replica,
        &scope,
    )
    .await
    .map_err(|error| format!("sync final clear: {error}"))?;
    projection =
        project_from_reopened_store(&root, bus.clone(), &prod, &authors, &sessions).await?;
    serving
        .reload()
        .await
        .map_err(|error| format!("reload after final clear: {error}"))?;
    let after_clear_http = timed_http_get(
        gateway.listen_addr(),
        "example.test",
        &format!("/.well-known/acme-challenge/{ACME_TOKEN}"),
    )
    .await?;
    let http_404_after_clear = after_clear_http
        .response
        .starts_with("HTTP/1.1 404 Not Found");
    if !http_404_after_clear {
        return Err(format!(
            "ACME p2panda HTTP-01 response after final clear did not become 404: {}",
            after_clear_http.response
        ));
    }

    let sqlite_path = root.join("projections.sqlite");
    std::fs::remove_file(&sqlite_path)
        .map_err(|error| format!("delete p2panda ACME projection sqlite: {error}"))?;
    let rebuilt =
        project_from_reopened_store(&root, bus.clone(), &prod, &authors, &sessions).await?;
    let sqlite = SqliteProjectionStore::new(sqlite_path);
    let row_counts = sqlite
        .row_counts()
        .map_err(|error| format!("read rebuilt p2panda ACME sqlite rows: {error}"))?;
    let sqlite_rebuild_after_delete =
        rebuilt.state.acme_http01.is_empty() && row_counts.acme_http01_challenges == 0;

    gateway
        .shutdown()
        .await
        .map_err(|error| format!("shutdown p2panda ACME HTTP gateway: {error}"))?;

    let visible_nodes_at_decision = [
        claim_a.visible_nodes.len(),
        present_a.visible_nodes.len(),
        pending_clear.visible_nodes.len(),
        claim_b.visible_nodes.len(),
        present_b.visible_nodes.len(),
        final_clear.visible_nodes.len(),
    ]
    .into_iter()
    .min()
    .unwrap_or(0);

    let report = P2pandaAcmeHttp01Report {
        scenario: "p2panda-acme-http01-contract",
        visible_nodes_at_decision,
        initial_key_authorization,
        takeover_key_authorization,
        sync_ms,
        projection_reload_ms,
        http_request_us: initial_http.elapsed_us,
        command_adapter_outage_serving_success_count: outage_success_count,
        stale_mutation_rejections: 1,
        scoped_grant_rejections: 1,
        stale_sync_preserved_winner,
        release_fact_recorded: matches!(pending_clear.kind, AcmeFactWriteKind::Cleared)
            && matches!(final_clear.kind, AcmeFactWriteKind::Cleared),
        trusted_replica_required,
        duplicate_sync_noop,
        sqlite_rebuild_after_delete,
        http_404_after_clear,
        superseded_count: status_count(
            &projection.state.statuses,
            ProjectionIgnoreReason::Superseded,
        ),
        elapsed_ms: started.elapsed().as_millis(),
    };
    if report.visible_nodes_at_decision != 2 {
        return Err(format!(
            "expected visible nodes in ACME command result, got {}",
            report.visible_nodes_at_decision
        ));
    }
    if !report.duplicate_sync_noop {
        return Err("repeat p2panda sync was not a no-op".to_string());
    }
    if !report.sqlite_rebuild_after_delete {
        return Err("p2panda ACME sqlite rebuild after delete failed".to_string());
    }

    let json = write_json(
        &root.join("p2panda-acme-http01-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS p2panda-acme-http01-contract");
    Ok(())
}

struct AcmeBusSessions {
    issuer_a: BusSession,
    issuer_b: BusSession,
    dns_writer: BusSession,
    projection: BusSession,
    left_replica: BusSession,
    right_replica: BusSession,
}

fn acme_bus_sessions(prod: &IslandId) -> Result<(InMemoryBus, AcmeBusSessions), String> {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let acme_pattern = fact_pattern(&format!("/facts/acme/http01/example.test/{ACME_TOKEN}/>"))?;
    let issuer_grant = Grant::empty()
        .with_fact_write(fact_pattern("/facts/lease/>")?)
        .with_fact_read(fact_pattern("/facts/lease/>")?)
        .with_fact_write(acme_pattern.clone())
        .with_fact_read(acme_pattern);
    let sessions = AcmeBusSessions {
        issuer_a: authority.grant_in(prod.clone(), PrincipalId::new("issuer-a"), issuer_grant),
        issuer_b: authority.grant_in(
            prod.clone(),
            PrincipalId::new("issuer-b"),
            Grant::empty()
                .with_fact_write(fact_pattern("/facts/lease/>")?)
                .with_fact_read(fact_pattern("/facts/lease/>")?)
                .with_fact_write(fact_pattern(&format!(
                    "/facts/acme/http01/example.test/{ACME_TOKEN}/>"
                ))?)
                .with_fact_read(fact_pattern(&format!(
                    "/facts/acme/http01/example.test/{ACME_TOKEN}/>"
                ))?),
        ),
        dns_writer: authority.grant_in(
            prod.clone(),
            PrincipalId::new("dns-writer"),
            Grant::empty().with_fact_write(fact_pattern("/facts/dns/>")?),
        ),
        projection: authority.grant_in(
            prod.clone(),
            PrincipalId::new("projection"),
            Grant::empty().with_fact_read(fact_pattern("/facts/>")?),
        ),
        left_replica: authority.grant_in(
            prod.clone(),
            PrincipalId::new("left-replica"),
            Grant::empty(),
        ),
        right_replica: authority.grant_in(
            prod.clone(),
            PrincipalId::new("right-replica"),
            Grant::empty(),
        ),
    };
    Ok((bus, sessions))
}

struct AcmeP2pandaCommandAdapter {
    session: BusSession,
    visible_nodes: VisibleNodes,
}

impl AcmeP2pandaCommandAdapter {
    fn new(session: BusSession, visible_nodes: VisibleNodes) -> Self {
        Self {
            session,
            visible_nodes,
        }
    }

    async fn claim(
        &mut self,
        store: &mut PandaFactStore,
        author: &PandaFactAuthor,
        challenge: &AcmeChallengeId,
        acquired_at: LeaseTimestamp,
        expires_at: LeaseTimestamp,
    ) -> Result<ClaimCommandResult, AcmeAdapterError> {
        let state = lease_state_from_store(store, &self.session, challenge, acquired_at)?;
        let epoch = match state {
            LeaseState::Vacant { next_epoch, .. }
            | LeaseState::Expired {
                next_epoch: Some(next_epoch),
                ..
            }
            | LeaseState::Released {
                next_epoch: Some(next_epoch),
                ..
            } => next_epoch,
            LeaseState::Active { current, .. } => {
                return Err(AcmeAdapterError::Conflict {
                    holder: current.holder().clone(),
                    epoch: current.epoch(),
                });
            }
            LeaseState::Expired {
                next_epoch: None, ..
            }
            | LeaseState::Released {
                next_epoch: None, ..
            } => {
                return Err(AcmeAdapterError::EpochOverflow);
            }
        };
        let fact = LeaseClaimed::new(
            challenge.lease_resource().clone(),
            LeaseHolder::new(self.session.principal().as_str()),
            epoch,
            acquired_at,
            expires_at,
        );
        let claim_hash = LeaseFact::Claimed(fact.clone()).content_hash();
        write_projection_fact(
            store,
            &self.session,
            author,
            &challenge.lease_claimed_fact_key(epoch),
            ProjectionFactPayload::LeaseClaimed(fact),
        )
        .await?;
        Ok(ClaimCommandResult {
            lease: AcmeLeaseHandle {
                challenge: challenge.clone(),
                holder: LeaseHolder::new(self.session.principal().as_str()),
                epoch,
                claim_hash,
            },
            visible_nodes: self.visible_nodes.clone(),
        })
    }

    async fn present(
        &mut self,
        store: &mut PandaFactStore,
        author: &PandaFactAuthor,
        challenge: &AcmeChallengeId,
        lease: &AcmeLeaseHandle,
        thumbprint: &str,
        published_at: LeaseTimestamp,
    ) -> Result<AcmeWriteResult, AcmeAdapterError> {
        lease.assert_challenge(challenge)?;
        assert_current_lease(store, &self.session, lease, published_at)?;
        let key_authorization = AcmeKeyAuthorization::parse_for_token(
            challenge.token(),
            format!("{}.{thumbprint}", challenge.token()),
        )
        .map_err(|error| AcmeAdapterError::Acme(error.to_string()))?;
        let fact = AcmeHttp01PresentedFact::from_parts(
            challenge.clone(),
            key_authorization.clone(),
            lease.holder.clone(),
            lease.epoch,
            lease.claim_hash,
            published_at,
        )
        .map_err(|error| AcmeAdapterError::Acme(error.to_string()))?;
        write_projection_fact(
            store,
            &self.session,
            author,
            &challenge.presented_fact_key(lease.epoch),
            ProjectionFactPayload::AcmeHttp01Presented(fact),
        )
        .await?;
        Ok(AcmeWriteResult {
            kind: AcmeFactWriteKind::Presented,
            key_authorization,
            visible_nodes: self.visible_nodes.clone(),
        })
    }

    async fn clear(
        &mut self,
        store: &mut PandaFactStore,
        author: &PandaFactAuthor,
        challenge: &AcmeChallengeId,
        lease: &AcmeLeaseHandle,
        cleared_at: LeaseTimestamp,
    ) -> Result<AcmeWriteResult, AcmeAdapterError> {
        lease.assert_challenge(challenge)?;
        assert_current_lease(store, &self.session, lease, cleared_at)?;
        let release = LeaseReleased::new_at(
            challenge.lease_resource().clone(),
            lease.holder.clone(),
            lease.epoch,
            lease.claim_hash,
            cleared_at,
        );
        write_projection_fact(
            store,
            &self.session,
            author,
            &challenge.lease_released_fact_key(lease.epoch, lease.claim_hash, release.release()),
            ProjectionFactPayload::LeaseReleased(release),
        )
        .await?;
        let clear = AcmeHttp01ClearedFact::from_parts(
            challenge.clone(),
            lease.holder.clone(),
            lease.epoch,
            lease.claim_hash,
            cleared_at,
        );
        write_projection_fact(
            store,
            &self.session,
            author,
            &challenge.cleared_fact_key(lease.epoch, lease.claim_hash),
            ProjectionFactPayload::AcmeHttp01Cleared(clear),
        )
        .await?;
        Ok(AcmeWriteResult {
            kind: AcmeFactWriteKind::Cleared,
            key_authorization: AcmeKeyAuthorization::parse_for_token(
                challenge.token(),
                format!("{}.cleared", challenge.token()),
            )
            .map_err(|error| AcmeAdapterError::Acme(error.to_string()))?,
            visible_nodes: self.visible_nodes.clone(),
        })
    }
}

#[derive(Debug)]
enum AcmeAdapterError {
    Conflict {
        holder: LeaseHolder,
        epoch: LeaseEpoch,
    },
    ChallengeMismatch {
        expected: String,
        actual: String,
    },
    StaleLease,
    EpochOverflow,
    Acme(String),
    Fact(String),
}

impl std::fmt::Display for AcmeAdapterError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Conflict { holder, epoch } => {
                write!(
                    formatter,
                    "ACME lease conflict with {holder} at epoch {epoch}"
                )
            }
            Self::ChallengeMismatch { expected, actual } => {
                write!(
                    formatter,
                    "ACME challenge mismatch: expected {expected}, got {actual}"
                )
            }
            Self::StaleLease => formatter.write_str("ACME lease is stale"),
            Self::EpochOverflow => formatter.write_str("ACME lease epoch overflow"),
            Self::Acme(message) | Self::Fact(message) => formatter.write_str(message),
        }
    }
}

impl std::error::Error for AcmeAdapterError {}

impl From<String> for AcmeAdapterError {
    fn from(value: String) -> Self {
        Self::Fact(value)
    }
}

#[derive(Debug)]
struct ClaimCommandResult {
    lease: AcmeLeaseHandle,
    visible_nodes: VisibleNodes,
}

#[derive(Debug)]
struct AcmeWriteResult {
    kind: AcmeFactWriteKind,
    key_authorization: AcmeKeyAuthorization,
    visible_nodes: VisibleNodes,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum AcmeFactWriteKind {
    Presented,
    Cleared,
}

#[derive(Debug)]
struct AcmeLeaseHandle {
    challenge: AcmeChallengeId,
    holder: LeaseHolder,
    epoch: LeaseEpoch,
    claim_hash: LeaseContentHash,
}

impl AcmeLeaseHandle {
    fn assert_challenge(&self, challenge: &AcmeChallengeId) -> Result<(), AcmeAdapterError> {
        if &self.challenge == challenge {
            return Ok(());
        }
        Err(AcmeAdapterError::ChallengeMismatch {
            expected: challenge_label(&self.challenge),
            actual: challenge_label(challenge),
        })
    }
}

fn challenge_label(challenge: &AcmeChallengeId) -> String {
    format!("{} {}", challenge.hostname(), challenge.token())
}

async fn write_projection_fact(
    store: &mut PandaFactStore,
    session: &BusSession,
    author: &PandaFactAuthor,
    key: &str,
    payload: ProjectionFactPayload,
) -> Result<PandaFactWriteOutcome, String> {
    let bytes = payload
        .to_fact_bytes()
        .map_err(|error| format!("serialize p2panda ACME fact {key}: {error}"))?;
    store
        .write_fact_payload(session, author, fact_key(key)?, FactPayload::from(bytes))
        .await
        .map_err(|error| format!("write p2panda ACME fact {key}: {error}"))
}

fn assert_current_lease(
    store: &PandaFactStore,
    session: &BusSession,
    lease: &AcmeLeaseHandle,
    now: LeaseTimestamp,
) -> Result<(), AcmeAdapterError> {
    match lease_state_from_store(store, session, &lease.challenge, now)? {
        LeaseState::Active { current, .. }
            if current.holder() == &lease.holder
                && current.epoch() == lease.epoch
                && current.content_hash() == lease.claim_hash =>
        {
            Ok(())
        }
        LeaseState::Active { .. }
        | LeaseState::Vacant { .. }
        | LeaseState::Expired { .. }
        | LeaseState::Released { .. } => Err(AcmeAdapterError::StaleLease),
    }
}

fn lease_state_from_store(
    store: &PandaFactStore,
    session: &BusSession,
    challenge: &AcmeChallengeId,
    now: LeaseTimestamp,
) -> Result<LeaseState, AcmeAdapterError> {
    let pattern = fact_pattern(&format!("/facts/lease/{}/>", challenge.lease_resource()))?;
    let candidates = store
        .list_candidates(session.island(), &pattern, session)
        .map_err(|error| AcmeAdapterError::Fact(error.to_string()))?;
    let payloads = store
        .read_payloads(session.island(), &candidates, session)
        .map_err(|error| AcmeAdapterError::Fact(error.to_string()))?;
    let book = mvp_lease::LeaseBook::new();
    let importer = book.importer();
    for candidate in lease_candidates(&candidates) {
        let Some(payload) = payloads.get(candidate.content_hash()) else {
            continue;
        };
        match ProjectionFactPayload::from_fact_bytes(payload.as_bytes())
            .map_err(|error| AcmeAdapterError::Fact(error.to_string()))?
        {
            ProjectionFactPayload::LeaseClaimed(fact) => {
                importer.record(LeaseFact::Claimed(fact));
            }
            ProjectionFactPayload::LeaseRenewed(fact) => {
                importer.record(LeaseFact::Renewed(fact));
            }
            ProjectionFactPayload::LeaseReleased(fact) => {
                importer.record(LeaseFact::Released(fact));
            }
            ProjectionFactPayload::NodeJoined(_)
            | ProjectionFactPayload::NodeRemovalStarted(_)
            | ProjectionFactPayload::NodeTombstoned(_)
            | ProjectionFactPayload::ServiceRegistered(_)
            | ProjectionFactPayload::ServingCommit(_)
            | ProjectionFactPayload::RouteCommit(_)
            | ProjectionFactPayload::GatewayCommit(_)
            | ProjectionFactPayload::DnsCommit(_)
            | ProjectionFactPayload::AcmeHttp01Presented(_)
            | ProjectionFactPayload::AcmeHttp01Cleared(_) => {}
        }
    }
    Ok(book.state(challenge.lease_resource(), now))
}

fn lease_candidates(candidates: &[FactCandidate]) -> impl Iterator<Item = &FactCandidate> {
    candidates.iter().filter(|candidate| {
        matches!(
            candidate.status(),
            CandidateStatus::Verified | CandidateStatus::Conflict
        ) && matches!(
            candidate.kind(),
            FactKind::LeaseClaimed | FactKind::LeaseRenewed | FactKind::LeaseReleased
        )
    })
}

async fn open_store(
    bus: Arc<InMemoryBus>,
    path: PathBuf,
    island: &IslandId,
    authors: &[&PandaFactAuthor],
) -> Result<PandaFactStore, String> {
    let config = authors.iter().fold(
        PandaSqliteOpenConfig::new(path, vec![island.clone()]),
        |config, author| {
            config.with_trusted_author_key(PandaTrustedAuthorKey::new(
                island.clone(),
                author.principal().clone(),
                author.author_key(),
            ))
        },
    );
    PandaFactStore::open_sqlite(bus, config)
        .await
        .map_err(|error| format!("open p2panda ACME store: {error}"))
}

async fn project_from_reopened_store(
    root: &Path,
    bus: Arc<InMemoryBus>,
    island: &IslandId,
    authors: &[&PandaFactAuthor],
    sessions: &AcmeBusSessions,
) -> Result<mvp_projection::ProjectionReport, String> {
    let source = open_store(
        bus,
        root.join("right-p2panda-facts.sqlite"),
        island,
        authors,
    )
    .await?;
    let actor = projection_actor(Arc::new(source), sessions.projection.clone(), root)?;
    actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("project p2panda ACME facts: {error}"))
}

fn projected_key_authorization(
    report: &mvp_projection::ProjectionReport,
) -> Result<String, String> {
    report
        .state
        .acme_http01
        .values()
        .next()
        .map(|challenge| challenge.key_authorization.as_str().to_string())
        .ok_or_else(|| "p2panda ACME projection did not expose a challenge".to_string())
}

#[derive(Debug, Clone, Copy)]
struct LeaseTimeline {
    acquired_at: LeaseTimestamp,
    published_at: LeaseTimestamp,
    cleared_at: LeaseTimestamp,
    takeover_at: LeaseTimestamp,
    takeover_presented_at: LeaseTimestamp,
    takeover_expires_at: LeaseTimestamp,
    stale_arrival_at: LeaseTimestamp,
    final_cleared_at: LeaseTimestamp,
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
            takeover_at: LeaseTimestamp::from_secs(now + 2),
            takeover_presented_at: LeaseTimestamp::from_secs(now + 3),
            takeover_expires_at: LeaseTimestamp::from_secs(now + 600),
            stale_arrival_at: LeaseTimestamp::from_secs(now + 4),
            final_cleared_at: LeaseTimestamp::from_secs(now + 5),
            expires_at: LeaseTimestamp::from_secs(now + 600),
        })
    }
}

fn stale_presented_fact(
    id: AcmeChallengeId,
    lease: &AcmeLeaseHandle,
    thumbprint: &str,
    published_at: LeaseTimestamp,
) -> Result<AcmeHttp01PresentedFact, String> {
    let key_authorization =
        AcmeKeyAuthorization::parse_for_token(id.token(), format!("{}.{thumbprint}", id.token()))
            .map_err(|error| error.to_string())?;
    AcmeHttp01PresentedFact::from_parts(
        id,
        key_authorization,
        lease.holder.clone(),
        lease.epoch,
        lease.claim_hash,
        published_at,
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

struct TimedHttpResponse {
    response: String,
    elapsed_us: u128,
}

async fn timed_http_get(
    addr: SocketAddr,
    host: &str,
    path: &str,
) -> Result<TimedHttpResponse, String> {
    let started = Instant::now();
    let response = http_get(addr, host, path).await?;
    Ok(TimedHttpResponse {
        response,
        elapsed_us: started.elapsed().as_micros(),
    })
}

async fn http_get(addr: SocketAddr, host: &str, path: &str) -> Result<String, String> {
    let mut stream = TcpStream::connect(addr)
        .await
        .map_err(|error| format!("connect p2panda ACME HTTP gateway: {error}"))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .map_err(|error| format!("write p2panda ACME HTTP request: {error}"))?;
    let mut response = Vec::new();
    timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
        .await
        .map_err(|_| "read p2panda ACME HTTP response timed out".to_string())?
        .map_err(|error| format!("read p2panda ACME HTTP response: {error}"))?;
    String::from_utf8(response)
        .map_err(|error| format!("p2panda ACME HTTP response was not UTF-8: {error}"))
}

fn loopback_any() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
}
