use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_bus::{
    BusError, BusSession, FactContentHash, FactKeyPattern, FactWriteOutcome, Grant, IslandId,
    PrincipalId, harness::InMemoryBus,
};
use mvp_identity::NodeId;
use mvp_projection::{
    BackendEndpoint, CandidateStatus, DnsCommitFact, DnsRecordFact, FactCandidate, FactKind,
    FactSource, FactSourceResult, GatewayCommitFact, NodeJoinedFact, ProjectionFactPayload,
    ProjectionIgnoreReason, RouteCommitFact, RouteId, ServiceName, ServiceRegistrationFact,
    SqliteProjectionStore, fixtures::BusFactFixtureSource, load_dns_snapshot,
    load_gateway_snapshot,
};
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::{fact_key, fact_pattern, write_projection_fact};
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::projection_harness::projection_actor;

const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Serialize)]
struct ProjectionContractReport {
    scenario: &'static str,
    projected_nodes: usize,
    projected_services: usize,
    projected_gateway_routes: usize,
    projected_dns_records: usize,
    ignored_unauthorized: usize,
    ignored_unverified: usize,
    ignored_conflicts: usize,
    gateway_snapshot_bytes: usize,
    dns_snapshot_bytes: usize,
    sqlite_rebuild_after_delete: bool,
    corrupt_sqlite_rebuilt: bool,
    dropped_hint_caught_up: bool,
    corrupt_snapshot_rejected: bool,
    unauthorized_write_rejected: bool,
    conflict_write_recorded: bool,
    payload_key_mismatch_rejected: bool,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|error| format!("create tokio runtime for projection contract: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("projection-contract");
    reset_dir(&root)?;

    let (bus, projection_session, intruder_session) = seed_base_facts()?;
    let actor = projection_actor(
        Arc::new(BusFactFixtureSource::new(bus.clone())),
        projection_session.clone(),
        &root,
    )?;

    let first = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("initial projection failed: {error}"))?;
    assert_eq_named("initial nodes", first.state.nodes.len(), 1)?;
    assert_eq_named("initial services", first.state.services.len(), 1)?;
    assert_eq_named(
        "initial gateway routes",
        gateway_route_count(&first.state),
        1,
    )?;
    assert_eq_named("initial dns records", dns_record_count(&first.state), 1)?;

    let sqlite = SqliteProjectionStore::new(root.join("projections.sqlite"));
    let loaded = sqlite
        .load()
        .map_err(|error| format!("load initial sqlite projection: {error}"))?;
    if loaded != first.state {
        return Err("initial sqlite projection did not match actor state".to_string());
    }
    let gateway_path = root.join("gateway.snapshot");
    let dns_path = root.join("dns.snapshot");
    let first_gateway_bytes =
        fs::read(&gateway_path).map_err(|error| format!("read gateway snapshot: {error}"))?;
    let first_dns_bytes =
        fs::read(&dns_path).map_err(|error| format!("read dns snapshot: {error}"))?;
    assert_eq_named(
        "loaded gateway routes",
        load_gateway_snapshot(&gateway_path, &IslandId::new("prod"))
            .map_err(|error| format!("load gateway snapshot: {error}"))?
            .routes
            .len(),
        1,
    )?;
    assert_eq_named(
        "loaded dns records",
        load_dns_snapshot(&dns_path, &IslandId::new("prod"))
            .map_err(|error| format!("load dns snapshot: {error}"))?
            .records
            .len(),
        1,
    )?;

    fs::remove_file(root.join("projections.sqlite"))
        .map_err(|error| format!("delete sqlite projection: {error}"))?;
    let rebuilt = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("rebuild projection failed: {error}"))?;
    if rebuilt.state != first.state {
        return Err("projection changed after deleting sqlite and rebuilding".to_string());
    }
    if fs::read(&gateway_path).map_err(|error| format!("read rebuilt gateway: {error}"))?
        != first_gateway_bytes
    {
        return Err("gateway snapshot changed after sqlite rebuild".to_string());
    }
    if fs::read(&dns_path).map_err(|error| format!("read rebuilt dns: {error}"))? != first_dns_bytes
    {
        return Err("dns snapshot changed after sqlite rebuild".to_string());
    }

    write_projection_fact(
        &bus,
        &projection_session,
        "/facts/service/queue/node-2/registered/1",
        ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
            service: ServiceName::new("queue"),
            node_id: NodeId::new("node-2"),
            version: "1.0.0".to_string(),
            endpoint_subject: "service.queue.changed".to_string(),
            epoch: 1,
        }),
    )?;
    let caught_up = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("dropped-hint projection failed: {error}"))?;
    assert_eq_named("dropped-hint services", caught_up.state.services.len(), 2)?;
    assert_eq_named(
        "projection hints stayed optional",
        actor
            .status()
            .await
            .map_err(|error| format!("read projection actor status: {error}"))?
            .hints_seen,
        0,
    )?;

    fs::write(root.join("projections.sqlite"), b"not-sqlite")
        .map_err(|error| format!("write corrupt sqlite projection: {error}"))?;
    let corrupt_sqlite = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("corrupt sqlite rebuild failed: {error}"))?;
    let corrupt_sqlite_rebuilt = corrupt_sqlite.state == caught_up.state;
    if !corrupt_sqlite_rebuilt {
        return Err("projection changed after corrupt sqlite rebuild".to_string());
    }
    if fs::read(&gateway_path).map_err(|error| format!("read corrupt-sqlite gateway: {error}"))?
        != first_gateway_bytes
    {
        return Err("gateway snapshot changed after corrupt sqlite rebuild".to_string());
    }
    if fs::read(&dns_path).map_err(|error| format!("read corrupt-sqlite dns: {error}"))?
        != first_dns_bytes
    {
        return Err("dns snapshot changed after corrupt sqlite rebuild".to_string());
    }

    write_projection_fact(
        &bus,
        &projection_session,
        "/facts/node/node-allowed/joined/1",
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-forged"),
            epoch: 1,
            overlay_ip: "fd00::bad".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    )?;
    let mismatch = actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("mismatched-payload projection failed: {error}"))?;
    let payload_key_mismatch_rejected = !mismatch
        .state
        .nodes
        .contains_key(&NodeId::new("node-forged"))
        && status_count(
            &mismatch.state.statuses,
            ProjectionIgnoreReason::MalformedPayload,
        ) == 1;
    if !payload_key_mismatch_rejected {
        return Err("payload identity mismatch was projected".to_string());
    }

    let unauthorized_write_rejected = matches!(
        bus.write_fact_payload(
            &intruder_session,
            fact_key("/facts/node/evil/joined/1")?,
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("evil"),
                epoch: 1,
                overlay_ip: "fd00::bad".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            })
            .to_fact_bytes()
            .map_err(|error| format!("serialize unauthorized payload: {error}"))?,
        )
        .unwrap_err(),
        BusError::UnauthorizedFactWrite { .. }
    );
    let conflict_key = fact_key("/facts/node/node-conflict/joined/1")?;
    bus.write_fact_payload(
        &projection_session,
        conflict_key.clone(),
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-conflict"),
            epoch: 1,
            overlay_ip: "fd00::10".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        })
        .to_fact_bytes()
        .map_err(|error| format!("serialize first conflict payload: {error}"))?,
    )
    .map_err(|error| format!("write first conflict candidate: {error}"))?;
    let conflict_write_recorded = matches!(
        bus.write_fact_payload(
            &projection_session,
            conflict_key,
            ProjectionFactPayload::NodeJoined(NodeJoinedFact {
                node_id: NodeId::new("node-conflict"),
                epoch: 1,
                overlay_ip: "fd00::11".to_string(),
                iroh_endpoint_id: "iroh-test".to_string(),
                wg_public_key: "wg-test".to_string(),
            })
            .to_fact_bytes()
            .map_err(|error| format!("serialize second conflict payload: {error}"))?,
        )
        .map_err(|error| format!("write second conflict candidate: {error}"))?,
        FactWriteOutcome::Conflict(_)
    );

    let redacted_actor = projection_actor(
        Arc::new(RedactingSource::new(
            BusFactFixtureSource::new(bus),
            redacted_candidates()?,
        )),
        projection_session,
        &root,
    )?;
    let redacted_report = redacted_actor
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("redacted projection failed: {error}"))?;
    assert_eq_named(
        "redacted projection services",
        redacted_report.state.services.len(),
        2,
    )?;
    assert_redacted_outputs(&root, &redacted_report.state.statuses)?;

    let original_gateway =
        fs::read(&gateway_path).map_err(|error| format!("read gateway before corrupt: {error}"))?;
    fs::write(&gateway_path, b"not-json")
        .map_err(|error| format!("write corrupt gateway snapshot: {error}"))?;
    let corrupt_snapshot_rejected =
        load_gateway_snapshot(&gateway_path, &IslandId::new("prod")).is_err();
    fs::write(&gateway_path, original_gateway)
        .map_err(|error| format!("restore gateway snapshot after corrupt test: {error}"))?;

    let report = ProjectionContractReport {
        scenario: "projection-contract",
        projected_nodes: redacted_report.state.nodes.len(),
        projected_services: redacted_report.state.services.len(),
        projected_gateway_routes: gateway_route_count(&redacted_report.state),
        projected_dns_records: dns_record_count(&redacted_report.state),
        ignored_unauthorized: status_count(
            &redacted_report.state.statuses,
            ProjectionIgnoreReason::Unauthorized,
        ),
        ignored_unverified: status_count(
            &redacted_report.state.statuses,
            ProjectionIgnoreReason::Unverified,
        ),
        ignored_conflicts: status_count(
            &redacted_report.state.statuses,
            ProjectionIgnoreReason::Conflict,
        ),
        gateway_snapshot_bytes: fs::metadata(&gateway_path)
            .map_err(|error| format!("gateway snapshot metadata: {error}"))?
            .len() as usize,
        dns_snapshot_bytes: fs::metadata(&dns_path)
            .map_err(|error| format!("dns snapshot metadata: {error}"))?
            .len() as usize,
        sqlite_rebuild_after_delete: true,
        corrupt_sqlite_rebuilt,
        dropped_hint_caught_up: caught_up.state.services.len() == 2,
        corrupt_snapshot_rejected,
        unauthorized_write_rejected,
        conflict_write_recorded,
        payload_key_mismatch_rejected,
        elapsed_ms: started.elapsed().as_millis(),
    };
    if !report.unauthorized_write_rejected {
        return Err("unauthorized fact write was not rejected".to_string());
    }
    if !report.conflict_write_recorded {
        return Err("conflicting fact write was not recorded".to_string());
    }
    if !report.corrupt_snapshot_rejected {
        return Err("corrupt gateway snapshot was accepted".to_string());
    }
    if !report.corrupt_sqlite_rebuilt {
        return Err("corrupt sqlite projection did not rebuild".to_string());
    }
    if !report.payload_key_mismatch_rejected {
        return Err("payload/key mismatch was not rejected".to_string());
    }

    let path = root.join("projection-contract-metrics.json");
    let json = write_json(&path, &report)?;
    println!("{json}");
    eprintln!("PASS projection-contract");
    Ok(())
}

fn seed_base_facts() -> Result<(InMemoryBus, BusSession, BusSession), String> {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let prod = IslandId::new("prod");
    let projection = authority.grant_in(
        prod.clone(),
        PrincipalId::new("projection"),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/>")?)
            .with_fact_read(fact_pattern("/facts/>")?),
    );
    let intruder = authority.grant_in(prod, PrincipalId::new("intruder"), Grant::empty());
    write_projection_fact(
        &bus,
        &projection,
        "/facts/node/node-1/joined/1",
        ProjectionFactPayload::NodeJoined(NodeJoinedFact {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            overlay_ip: "fd00::1".to_string(),
            iroh_endpoint_id: "iroh-test".to_string(),
            wg_public_key: "wg-test".to_string(),
        }),
    )?;
    write_projection_fact(
        &bus,
        &projection,
        "/facts/service/web/node-1/registered/1",
        ProjectionFactPayload::ServiceRegistered(ServiceRegistrationFact {
            service: ServiceName::new("web"),
            node_id: NodeId::new("node-1"),
            version: "1.0.0".to_string(),
            endpoint_subject: "service.web.changed".to_string(),
            epoch: 1,
        }),
    )?;
    write_projection_fact(
        &bus,
        &projection,
        "/facts/routes/route-1",
        ProjectionFactPayload::RouteCommit(RouteCommitFact {
            route_commit_id: "route-1".to_string(),
            route_id: RouteId::new("web-http"),
            hostnames: vec!["web.example.com".to_string()],
            backends: vec![BackendEndpoint {
                node_id: NodeId::new("node-1"),
                address: "10.0.0.1:8080".to_string(),
            }],
            old_backends_to_drain: vec![BackendEndpoint {
                node_id: NodeId::new("node-old"),
                address: "10.0.0.9:8080".to_string(),
            }],
        }),
    )?;
    write_projection_fact(
        &bus,
        &projection,
        "/facts/gateway/gateway-1",
        ProjectionFactPayload::GatewayCommit(GatewayCommitFact {
            gateway_commit_id: "gateway-1".to_string(),
            route_commit_id: "route-1".to_string(),
            epoch: 1,
        }),
    )?;
    write_projection_fact(
        &bus,
        &projection,
        "/facts/dns/dns-1",
        ProjectionFactPayload::DnsCommit(DnsCommitFact {
            dns_commit_id: "dns-1".to_string(),
            epoch: 1,
            records: vec![DnsRecordFact {
                name: "web.example.com".to_string(),
                record_type: "AAAA".to_string(),
                value: "fd00::1".to_string(),
                ttl_seconds: 30,
            }],
        }),
    )?;
    Ok((bus, projection, intruder))
}

fn redacted_candidates() -> Result<Vec<FactCandidate>, String> {
    Ok(vec![
        FactCandidate::new(
            IslandId::new("prod"),
            fact_key("/facts/secret/unauthorized-node")?,
            PrincipalId::new("intruder"),
            FactContentHash::new("secret-unauthorized-hash"),
            FactKind::NodeJoined,
            0,
            CandidateStatus::Unauthorized,
        ),
        FactCandidate::new(
            IslandId::new("prod"),
            fact_key("/facts/secret/unverified-node")?,
            PrincipalId::new("unsigned"),
            FactContentHash::new("secret-unverified-hash"),
            FactKind::NodeJoined,
            0,
            CandidateStatus::Unverified,
        ),
        FactCandidate::new(
            IslandId::new("prod"),
            fact_key("/facts/secret/conflict-node")?,
            PrincipalId::new("projection"),
            FactContentHash::new("secret-conflict-hash"),
            FactKind::NodeJoined,
            0,
            CandidateStatus::Conflict,
        ),
    ])
}

fn assert_redacted_outputs(
    root: &Path,
    statuses: &[mvp_projection::ProjectionStatus],
) -> Result<(), String> {
    let status_json = serde_json::to_string(statuses)
        .map_err(|error| format!("serialize projection statuses: {error}"))?;
    let gateway = fs::read_to_string(root.join("gateway.snapshot"))
        .map_err(|error| format!("read redacted gateway snapshot: {error}"))?;
    let dns = fs::read_to_string(root.join("dns.snapshot"))
        .map_err(|error| format!("read redacted dns snapshot: {error}"))?;
    for output_name in ["statuses", "gateway", "dns"] {
        let output = match output_name {
            "statuses" => &status_json,
            "gateway" => &gateway,
            "dns" => &dns,
            _ => unreachable!(),
        };
        if output.contains("secret-") {
            return Err(format!("{output_name} exposed raw rejected fact metadata"));
        }
    }
    if status_count(statuses, ProjectionIgnoreReason::Unauthorized) != 1 {
        return Err("expected one redacted unauthorized projection status".to_string());
    }
    if status_count(statuses, ProjectionIgnoreReason::Unverified) != 1 {
        return Err("expected one redacted unverified projection status".to_string());
    }
    if status_count(statuses, ProjectionIgnoreReason::Conflict) != 3 {
        return Err(
            "expected two stored conflict candidates and one redacted conflict status".to_string(),
        );
    }
    Ok(())
}

fn gateway_route_count(state: &mvp_projection::ProjectionState) -> usize {
    state
        .gateway
        .as_ref()
        .map_or(0, |gateway| gateway.routes.len())
}

fn dns_record_count(state: &mvp_projection::ProjectionState) -> usize {
    state.dns.as_ref().map_or(0, |dns| dns.records.len())
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

struct RedactingSource {
    inner: BusFactFixtureSource,
    extra_candidates: Vec<FactCandidate>,
}

impl RedactingSource {
    fn new(inner: BusFactFixtureSource, extra_candidates: Vec<FactCandidate>) -> Self {
        Self {
            inner,
            extra_candidates,
        }
    }
}

impl FactSource for RedactingSource {
    fn list_candidates(
        &self,
        island: &IslandId,
        pattern: &FactKeyPattern,
        session: &BusSession,
    ) -> FactSourceResult<Vec<FactCandidate>> {
        let mut candidates = self.inner.list_candidates(island, pattern, session)?;
        candidates.extend(self.extra_candidates.clone());
        Ok(candidates)
    }

    fn read_payloads(
        &self,
        island: &IslandId,
        candidates: &[FactCandidate],
        session: &BusSession,
    ) -> FactSourceResult<std::collections::BTreeMap<FactContentHash, mvp_bus::FactPayload>> {
        self.inner.read_payloads(island, candidates, session)
    }
}
