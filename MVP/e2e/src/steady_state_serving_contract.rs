use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use mvp_bus::{Grant, IslandId, PrincipalId};
use mvp_deploy::{DnsCommitId, GatewayCommitId, RouteCommitId, ServingCommitId, ServingCommitPlan};
use mvp_identity::NodeId;
use mvp_projection::{
    BackendEndpoint, BusFactSource, DnsRecordFact, DnsSnapshotFile, RouteId, load_dns_snapshot,
};
use mvp_routing::write_serving_commit;
use mvp_serving::{
    ServingActorHandle, ServingError, ServingFailureKind, ServingFreshness, ServingSnapshotKind,
    ServingSnapshotPaths, ServingStatus,
};
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::projection_harness::projection_actor;

const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);
const STALE_AFTER: Duration = Duration::from_secs(300);

#[derive(Debug, Serialize)]
struct SteadyStateServingReport {
    scenario: &'static str,
    local_coordinator_available_for_mutation: bool,
    initial_projection_us: u128,
    remote_projection_us: u128,
    projection_rebuild_us: u128,
    reload_us: u128,
    restart_from_snapshot_us: u128,
    serving_query_probes_during_outage: usize,
    serving_reload_attempts: u64,
    stale_snapshot_age_us: u128,
    updated_gateway_revision: String,
    updated_dns_revision: String,
    corrupt_reload_preserved_last_good: bool,
    wrong_island_reload_preserved_last_good: bool,
    deleted_reload_preserved_last_good: bool,
    symlink_reload_preserved_last_good: Option<bool>,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_time()
        .build()
        .map_err(|error| format!("create tokio runtime for serving contract: {error}"))?;
    runtime.block_on(run_async())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("steady-state-serving-contract");
    reset_dir(&root)?;

    let (bus, authority, raw_bus) = mvp_bus::harness::actor_with_authority();
    let prod = IslandId::new("prod");
    let remote_coordinator = authority.grant_in(
        prod.clone(),
        PrincipalId::new("remote-coordinator"),
        Grant::allow_all(),
    );
    let projection_session = authority.grant_in(
        prod.clone(),
        PrincipalId::new("projection"),
        Grant::allow_all(),
    );
    let local_coordinator_available_for_mutation = false;

    let paths = ServingSnapshotPaths::new(root.join("gateway.snapshot"), root.join("dns.snapshot"));
    let projection = projection_actor(
        Arc::new(BusFactSource::new(raw_bus.clone())),
        projection_session.clone(),
        &root,
    )?;

    write_serving_commit(
        &bus,
        &remote_coordinator,
        &serving_commit("serving-1", "fd00::1:8080", "fd00::1", 1),
    )
    .await
    .map_err(|error| format!("write initial serving commit: {error}"))?;

    let initial_projection_started = Instant::now();
    projection
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("initial serving projection: {error}"))?;
    let initial_projection_us = initial_projection_started.elapsed().as_micros();

    let serving = ServingActorHandle::spawn(prod.clone(), paths.clone(), STALE_AFTER)
        .map_err(|error| format!("spawn serving actor from snapshots: {error}"))?;
    let mut serving_query_probes_during_outage = 0;
    assert_serving_answer(&serving, "fd00::1:8080", "fd00::1").await?;
    serving_query_probes_during_outage += 1;

    write_serving_commit(
        &bus,
        &remote_coordinator,
        &serving_commit("serving-2", "fd00::2:8080", "fd00::2", 2),
    )
    .await
    .map_err(|error| format!("write remote serving commit: {error}"))?;
    assert_serving_answer(&serving, "fd00::1:8080", "fd00::1").await?;
    serving_query_probes_during_outage += 1;

    let remote_projection_started = Instant::now();
    projection
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| format!("remote serving projection: {error}"))?;
    let remote_projection_us = remote_projection_started.elapsed().as_micros();

    let reload_started = Instant::now();
    let reloaded_status = serving
        .reload()
        .await
        .map_err(|error| format!("reload serving actor after remote projection: {error}"))?;
    let reload_us = reload_started.elapsed().as_micros();
    assert_serving_answer(&serving, "fd00::2:8080", "fd00::2").await?;
    serving_query_probes_during_outage += 1;

    fs::remove_file(root.join("projections.sqlite"))
        .map_err(|error| format!("delete projection sqlite during serving: {error}"))?;
    assert_serving_answer(&serving, "fd00::2:8080", "fd00::2").await?;
    serving_query_probes_during_outage += 1;

    let projection_rebuild_started = Instant::now();
    let rebuild_projection = projection_actor(
        Arc::new(BusFactSource::new(raw_bus.clone())),
        projection_session,
        &root,
    )?;
    let rebuild_task =
        tokio::spawn(async move { rebuild_projection.project_once(PROJECT_TIMEOUT).await });
    assert_serving_answer(&serving, "fd00::2:8080", "fd00::2").await?;
    serving_query_probes_during_outage += 1;
    rebuild_task
        .await
        .map_err(|error| format!("projection rebuild worker join: {error}"))?
        .map_err(|error| format!("projection rebuild during serving: {error}"))?;
    let projection_rebuild_us = projection_rebuild_started.elapsed().as_micros();
    serving
        .reload()
        .await
        .map_err(|error| format!("reload after projection rebuild: {error}"))?;
    assert_serving_answer(&serving, "fd00::2:8080", "fd00::2").await?;
    serving_query_probes_during_outage += 1;

    let restart_started = Instant::now();
    let restarted = ServingActorHandle::spawn(prod.clone(), paths.clone(), STALE_AFTER)
        .map_err(|error| format!("restart serving actor from snapshot files: {error}"))?;
    let restart_from_snapshot_us = restart_started.elapsed().as_micros();
    assert_serving_answer(&restarted, "fd00::2:8080", "fd00::2").await?;
    serving_query_probes_during_outage += 1;

    let valid_gateway = fs::read(&paths.gateway)
        .map_err(|error| format!("read valid gateway snapshot bytes: {error}"))?;
    let valid_dns =
        fs::read(&paths.dns).map_err(|error| format!("read valid dns snapshot bytes: {error}"))?;

    restore_valid_snapshots(&paths, &valid_gateway, &valid_dns)?;
    fs::write(&paths.gateway, b"not-json")
        .map_err(|error| format!("write corrupt gateway snapshot: {error}"))?;
    let corrupt_reload_preserved_last_good = assert_failed_reload_preserves(
        &restarted,
        ServingFailureKind::InvalidSnapshotJson,
        ServingSnapshotKind::Gateway,
    )
    .await?;

    restore_valid_snapshots(&paths, &valid_gateway, &valid_dns)?;
    write_wrong_island_dns(&paths.dns)?;
    let wrong_island_reload_preserved_last_good = assert_failed_reload_preserves(
        &restarted,
        ServingFailureKind::SnapshotPath,
        ServingSnapshotKind::Dns,
    )
    .await?;

    restore_valid_snapshots(&paths, &valid_gateway, &valid_dns)?;
    fs::remove_file(&paths.dns)
        .map_err(|error| format!("delete next dns snapshot before reload: {error}"))?;
    let deleted_reload_preserved_last_good = assert_failed_reload_preserves(
        &restarted,
        ServingFailureKind::MissingSnapshot,
        ServingSnapshotKind::Dns,
    )
    .await?;

    let symlink_reload_preserved_last_good =
        symlink_failure_variant(&restarted, &paths, &valid_gateway, &valid_dns).await?;

    let final_status = restarted
        .status()
        .await
        .map_err(|error| format!("read final serving status: {error}"))?;
    assert_eq_named(
        "final freshness after failed reload",
        final_status.freshness.clone(),
        ServingFreshness::ServingLastGoodAfterFailure,
    )?;
    let expected_final_failure_kind = if symlink_reload_preserved_last_good.is_some() {
        ServingFailureKind::SnapshotPath
    } else {
        ServingFailureKind::MissingSnapshot
    };
    assert_status_failure(
        &final_status,
        &expected_final_failure_kind,
        ServingSnapshotKind::Dns,
    )?;

    let report = SteadyStateServingReport {
        scenario: "steady-state-serving-contract",
        local_coordinator_available_for_mutation,
        initial_projection_us,
        remote_projection_us,
        projection_rebuild_us,
        reload_us,
        restart_from_snapshot_us,
        serving_query_probes_during_outage,
        serving_reload_attempts: final_status.reload_attempts,
        stale_snapshot_age_us: final_status.snapshot_age.as_micros(),
        updated_gateway_revision: reloaded_status.loaded_revisions.gateway,
        updated_dns_revision: reloaded_status.loaded_revisions.dns,
        corrupt_reload_preserved_last_good,
        wrong_island_reload_preserved_last_good,
        deleted_reload_preserved_last_good,
        symlink_reload_preserved_last_good,
        elapsed_ms: started.elapsed().as_millis(),
    };
    assert_eq_named(
        "serving query probes during outage",
        report.serving_query_probes_during_outage,
        7,
    )?;
    assert_eq_named(
        "local coordinator mutation availability",
        report.local_coordinator_available_for_mutation,
        false,
    )?;
    if !report.corrupt_reload_preserved_last_good
        || !report.wrong_island_reload_preserved_last_good
        || !report.deleted_reload_preserved_last_good
        || matches!(report.symlink_reload_preserved_last_good, Some(false))
    {
        return Err("one or more unsafe reload variants replaced last-good serving".to_string());
    }

    let json = write_json(
        &root.join("steady-state-serving-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS steady-state-serving-contract");
    Ok(())
}

fn serving_commit(
    serving_commit_id: &str,
    backend_address: &str,
    dns_value: &str,
    epoch: u64,
) -> ServingCommitPlan {
    ServingCommitPlan {
        serving_commit_id: ServingCommitId::new(serving_commit_id),
        route_commit_id: RouteCommitId::new(format!("{serving_commit_id}-route")),
        gateway_commit_id: GatewayCommitId::new(format!("{serving_commit_id}-gateway")),
        dns_commit_id: DnsCommitId::new(format!("{serving_commit_id}-dns")),
        route_id: RouteId::new("web-http"),
        hostnames: vec!["web.example.test".to_string()],
        active_backends: vec![BackendEndpoint {
            node_id: NodeId::new("node-web"),
            address: backend_address.to_string(),
        }],
        old_backends_to_drain: vec![BackendEndpoint {
            node_id: NodeId::new("node-old"),
            address: "fd00::old:8080".to_string(),
        }],
        dns_records: vec![DnsRecordFact {
            name: "web.example.test".to_string(),
            record_type: "AAAA".to_string(),
            value: dns_value.to_string(),
            ttl_seconds: 30,
        }],
        epoch,
    }
}

async fn assert_serving_answer(
    serving: &ServingActorHandle,
    backend_address: &str,
    dns_value: &str,
) -> Result<(), String> {
    let route = serving
        .gateway_route_for_host("WEB.EXAMPLE.TEST")
        .await
        .map_err(|error| format!("query gateway route: {error}"))?
        .ok_or_else(|| "gateway route missing".to_string())?;
    let [backend] = route.backends.as_slice() else {
        return Err(format!(
            "expected exactly one gateway backend, got {:?}",
            route.backends
        ));
    };
    assert_eq_named(
        "serving gateway backend",
        backend.address.as_str(),
        backend_address,
    )?;
    let dns_records = serving
        .dns_records("web.example.test", "aaaa")
        .await
        .map_err(|error| format!("query dns records: {error}"))?;
    let [record] = dns_records.as_slice() else {
        return Err(format!(
            "expected exactly one dns record, got {dns_records:?}"
        ));
    };
    assert_eq_named("serving dns value", record.value.as_str(), dns_value)
}

async fn assert_failed_reload_preserves(
    serving: &ServingActorHandle,
    expected_kind: ServingFailureKind,
    expected_snapshot: ServingSnapshotKind,
) -> Result<bool, String> {
    let error = match serving.reload().await {
        Ok(status) => {
            return Err(format!(
                "unsafe reload succeeded with revisions {:?}",
                status.loaded_revisions
            ));
        }
        Err(error) => error,
    };
    assert_serving_error(error, &expected_kind, expected_snapshot)?;
    assert_serving_answer(serving, "fd00::2:8080", "fd00::2").await?;
    let status = serving
        .status()
        .await
        .map_err(|error| format!("status after failed reload: {error}"))?;
    assert_eq_named(
        "freshness after failed reload",
        status.freshness.clone(),
        ServingFreshness::ServingLastGoodAfterFailure,
    )?;
    assert_status_failure(&status, &expected_kind, expected_snapshot)?;
    assert_eq_named(
        "loaded gateway revision",
        status.loaded_revisions.gateway,
        expected_gateway_revision("serving-2"),
    )?;
    assert_eq_named(
        "loaded dns revision",
        status.loaded_revisions.dns,
        "dns:serving-2-dns".to_string(),
    )?;
    Ok(true)
}

fn expected_gateway_revision(serving_commit_id: &str) -> String {
    format!("gateway:{serving_commit_id}-gateway:{serving_commit_id}-route:acme:none")
}

fn assert_status_failure(
    status: &ServingStatus,
    expected_kind: &ServingFailureKind,
    expected_snapshot: ServingSnapshotKind,
) -> Result<(), String> {
    let Some(failure) = &status.last_failure else {
        return Err("serving status did not preserve last reload failure".to_string());
    };
    if failure.kind == *expected_kind && failure.snapshot == Some(expected_snapshot) {
        return Ok(());
    }
    Err(format!(
        "unexpected serving status failure: expected {expected_kind:?}/{expected_snapshot:?}, got {failure:?}"
    ))
}

fn assert_serving_error(
    error: ServingError,
    expected_kind: &ServingFailureKind,
    expected_snapshot: ServingSnapshotKind,
) -> Result<(), String> {
    match error {
        ServingError::SnapshotLoad { failure }
            if failure.kind == *expected_kind && failure.snapshot == Some(expected_snapshot) =>
        {
            Ok(())
        }
        other => Err(format!("unexpected reload error: {other:?}")),
    }
}

fn restore_valid_snapshots(
    paths: &ServingSnapshotPaths,
    gateway_bytes: &[u8],
    dns_bytes: &[u8],
) -> Result<(), String> {
    remove_snapshot_if_present(&paths.gateway)?;
    remove_snapshot_if_present(&paths.dns)?;
    fs::write(&paths.gateway, gateway_bytes)
        .map_err(|error| format!("restore gateway snapshot: {error}"))?;
    fs::write(&paths.dns, dns_bytes).map_err(|error| format!("restore dns snapshot: {error}"))
}

fn remove_snapshot_if_present(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove snapshot '{}': {error}", path.display())),
    }
}

fn write_wrong_island_dns(path: &Path) -> Result<(), String> {
    let mut dns = load_dns_snapshot(path, &IslandId::new("prod"))
        .map_err(|error| format!("load valid dns before wrong-island mutation: {error}"))?;
    dns.island = "laptop".to_string();
    write_dns_snapshot(path, &dns)
}

fn write_dns_snapshot(path: &Path, snapshot: &DnsSnapshotFile) -> Result<(), String> {
    let bytes =
        serde_json::to_vec(snapshot).map_err(|error| format!("serialize dns snapshot: {error}"))?;
    fs::write(path, bytes).map_err(|error| format!("write dns snapshot: {error}"))
}

#[cfg(unix)]
async fn symlink_failure_variant(
    serving: &ServingActorHandle,
    paths: &ServingSnapshotPaths,
    gateway_bytes: &[u8],
    dns_bytes: &[u8],
) -> Result<Option<bool>, String> {
    use std::os::unix::fs::symlink;

    restore_valid_snapshots(paths, gateway_bytes, dns_bytes)?;
    let target = paths
        .dns
        .parent()
        .ok_or_else(|| "dns snapshot path has no parent".to_string())?
        .join("dns-target.snapshot");
    fs::write(&target, dns_bytes).map_err(|error| format!("write symlink target: {error}"))?;
    fs::remove_file(&paths.dns).map_err(|error| format!("remove dns before symlink: {error}"))?;
    symlink(&target, &paths.dns).map_err(|error| format!("create dns symlink: {error}"))?;
    assert_failed_reload_preserves(
        serving,
        ServingFailureKind::SnapshotPath,
        ServingSnapshotKind::Dns,
    )
    .await
    .map(Some)
}

#[cfg(not(unix))]
async fn symlink_failure_variant(
    _serving: &ServingActorHandle,
    _paths: &ServingSnapshotPaths,
    _gateway_bytes: &[u8],
    _dns_bytes: &[u8],
) -> Result<Option<bool>, String> {
    Ok(None)
}
