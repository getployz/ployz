use std::fs;
use std::time::Duration;

use mvp_bus::IslandId;
use mvp_projection::{
    BackendEndpoint, DnsRecordProjection, DnsSnapshotFile, GatewayRouteProjection,
    GatewaySnapshotFile, NodeId, RouteId,
};
use tempfile::TempDir;

use crate::{
    ServingActorHandle, ServingError, ServingFailureKind, ServingFreshness, ServingSnapshotBatch,
    ServingSnapshotKind, ServingSnapshotPaths, WireServingState,
};

fn snapshot_paths(root: &TempDir) -> ServingSnapshotPaths {
    ServingSnapshotPaths::new(
        root.path().join("gateway.snapshot"),
        root.path().join("dns.snapshot"),
    )
}

fn gateway_snapshot(
    island: &str,
    revision: &str,
    host: &str,
    backend: &str,
) -> GatewaySnapshotFile {
    GatewaySnapshotFile {
        schema_version: 1,
        island: island.to_string(),
        revision: revision.to_string(),
        gateway_commit_id: format!("{revision}-gateway"),
        route_commit_id: format!("{revision}-route"),
        routes: vec![GatewayRouteProjection {
            route_id: RouteId::new("web-http"),
            hostnames: vec![host.to_string()],
            backends: vec![BackendEndpoint {
                node_id: NodeId::new("node-web"),
                address: backend.to_string(),
            }],
            old_backends_to_drain: Vec::new(),
        }],
    }
}

fn dns_snapshot(island: &str, revision: &str, name: &str, value: &str) -> DnsSnapshotFile {
    DnsSnapshotFile {
        schema_version: 1,
        island: island.to_string(),
        revision: revision.to_string(),
        dns_commit_id: format!("{revision}-dns"),
        records: vec![DnsRecordProjection {
            name: name.to_string(),
            record_type: "AAAA".to_string(),
            value: value.to_string(),
            ttl_seconds: 30,
        }],
    }
}

fn empty_gateway_snapshot(island: &str, revision: &str) -> GatewaySnapshotFile {
    GatewaySnapshotFile {
        schema_version: 1,
        island: island.to_string(),
        revision: revision.to_string(),
        gateway_commit_id: format!("{revision}-gateway"),
        route_commit_id: format!("{revision}-route"),
        routes: Vec::new(),
    }
}

fn empty_dns_snapshot(island: &str, revision: &str) -> DnsSnapshotFile {
    DnsSnapshotFile {
        schema_version: 1,
        island: island.to_string(),
        revision: revision.to_string(),
        dns_commit_id: format!("{revision}-dns"),
        records: Vec::new(),
    }
}

fn write_snapshot_files(root: &TempDir, gateway: &GatewaySnapshotFile, dns: &DnsSnapshotFile) {
    let paths = snapshot_paths(root);
    fs::write(
        paths.gateway,
        serde_json::to_vec(gateway).expect("serialize gateway snapshot"),
    )
    .expect("write gateway snapshot");
    fs::write(
        paths.dns,
        serde_json::to_vec(dns).expect("serialize dns snapshot"),
    )
    .expect("write dns snapshot");
}

fn write_prod_snapshots(root: &TempDir, revision: &str, backend: &str, dns_value: &str) {
    write_snapshot_files(
        root,
        &gateway_snapshot("prod", revision, "web.example.test", backend),
        &dns_snapshot("prod", revision, "web.example.test", dns_value),
    );
}

fn prod() -> IslandId {
    IslandId::new("prod")
}

#[test]
fn batch_loads_valid_gateway_and_dns_snapshots() {
    let root = TempDir::new().expect("tempdir");
    write_prod_snapshots(&root, "rev-1", "fd00::1:8080", "fd00::1");

    let batch =
        ServingSnapshotBatch::load(&snapshot_paths(&root), &prod()).expect("load snapshot batch");

    assert_eq!(batch.revisions().gateway, "rev-1");
    assert_eq!(batch.revisions().dns, "rev-1");
}

#[test]
fn missing_snapshot_is_structured_failure() {
    let root = TempDir::new().expect("tempdir");
    let paths = snapshot_paths(&root);
    fs::write(
        &paths.gateway,
        serde_json::to_vec(&gateway_snapshot(
            "prod",
            "rev-1",
            "web.example.test",
            "fd00::1:8080",
        ))
        .expect("serialize gateway snapshot"),
    )
    .expect("write gateway snapshot");

    let error = ServingSnapshotBatch::load(&paths, &prod()).expect_err("dns snapshot is missing");

    assert!(matches!(
        error,
        ServingError::SnapshotLoad {
            failure
        } if failure.kind == ServingFailureKind::MissingSnapshot
            && failure.snapshot == Some(ServingSnapshotKind::Dns)
    ));
}

#[test]
fn empty_snapshot_batch_is_valid() {
    let root = TempDir::new().expect("tempdir");
    write_snapshot_files(
        &root,
        &empty_gateway_snapshot("prod", "rev-empty"),
        &empty_dns_snapshot("prod", "rev-empty"),
    );

    let batch =
        ServingSnapshotBatch::load(&snapshot_paths(&root), &prod()).expect("load empty batch");

    assert!(batch.gateway.routes.is_empty());
    assert!(batch.dns.records.is_empty());
    assert_eq!(batch.revisions().gateway, "rev-empty");
    assert_eq!(batch.revisions().dns, "rev-empty");
}

#[test]
fn wrong_island_snapshot_is_structured_failure() {
    let root = TempDir::new().expect("tempdir");
    write_snapshot_files(
        &root,
        &gateway_snapshot("laptop", "rev-1", "web.example.test", "fd00::1:8080"),
        &dns_snapshot("prod", "rev-1", "web.example.test", "fd00::1"),
    );

    let error =
        ServingSnapshotBatch::load(&snapshot_paths(&root), &prod()).expect_err("wrong island");

    assert!(matches!(
        error,
        ServingError::SnapshotLoad {
            failure
        } if failure.kind == ServingFailureKind::SnapshotPath
            && failure.snapshot == Some(ServingSnapshotKind::Gateway)
    ));
}

#[cfg(unix)]
#[test]
fn symlink_snapshot_is_structured_failure() {
    use std::os::unix::fs::symlink;

    let root = TempDir::new().expect("tempdir");
    let paths = snapshot_paths(&root);
    write_prod_snapshots(&root, "rev-1", "fd00::1:8080", "fd00::1");
    let target = root.path().join("dns-target.snapshot");
    fs::rename(&paths.dns, &target).expect("move dns snapshot");
    symlink(&target, &paths.dns).expect("link dns snapshot");

    let error = ServingSnapshotBatch::load(&paths, &prod()).expect_err("symlink rejected");

    assert!(matches!(
        error,
        ServingError::SnapshotLoad {
            failure
        } if failure.kind == ServingFailureKind::SnapshotPath
            && failure.snapshot == Some(ServingSnapshotKind::Dns)
    ));
}

#[tokio::test]
async fn actor_serves_gateway_and_dns_from_last_good_snapshots() {
    let root = TempDir::new().expect("tempdir");
    write_prod_snapshots(&root, "rev-1", "fd00::1:8080", "fd00::1");

    let actor = ServingActorHandle::spawn(prod(), snapshot_paths(&root), Duration::from_secs(60))
        .expect("spawn serving actor");
    let route = actor
        .gateway_route_for_host("WEB.EXAMPLE.TEST")
        .await
        .expect("query gateway")
        .expect("route");
    let records = actor
        .dns_records("web.example.test", "aaaa")
        .await
        .expect("query dns");
    let status = actor.status().await.expect("status");

    assert_eq!(route.backends[0].address, "fd00::1:8080");
    assert_eq!(records[0].value, "fd00::1");
    assert_eq!(status.loaded_revisions.gateway, "rev-1");
    assert_eq!(status.loaded_revisions.dns, "rev-1");
    assert_eq!(status.reload_attempts, 0);
    assert_eq!(status.freshness, ServingFreshness::Fresh);
}

#[tokio::test]
async fn wire_state_delegates_queries_and_status_to_serving_actor() {
    let root = TempDir::new().expect("tempdir");
    write_prod_snapshots(&root, "rev-1", "fd00::1:8080", "fd00::1");
    let actor = ServingActorHandle::spawn(prod(), snapshot_paths(&root), Duration::from_secs(60))
        .expect("spawn serving actor");
    let wire = WireServingState::new(actor);

    let route = wire
        .gateway_route_for_host("WEB.EXAMPLE.TEST")
        .await
        .expect("query route")
        .expect("route");
    let records = wire
        .dns_records("web.example.test", "aaaa")
        .await
        .expect("query dns");
    let status = wire.status().await.expect("status");

    assert_eq!(route.backends[0].address, "fd00::1:8080");
    assert_eq!(records[0].value, "fd00::1");
    assert_eq!(status.loaded_revisions.gateway, "rev-1");
    assert_eq!(status.loaded_revisions.dns, "rev-1");
}

#[tokio::test]
async fn wire_state_reload_preserves_last_good_failure_semantics() {
    let root = TempDir::new().expect("tempdir");
    let paths = snapshot_paths(&root);
    write_prod_snapshots(&root, "rev-1", "fd00::1:8080", "fd00::1");
    let actor = ServingActorHandle::spawn(prod(), paths.clone(), Duration::from_secs(60))
        .expect("spawn serving actor");
    let wire = WireServingState::new(actor);
    fs::write(&paths.dns, b"not json").expect("corrupt dns snapshot");

    let error = wire.reload().await.expect_err("reload should fail");
    let route = wire
        .gateway_route_for_host("web.example.test")
        .await
        .expect("query route")
        .expect("route");
    let records = wire
        .dns_records("web.example.test", "AAAA")
        .await
        .expect("query dns");
    let status = wire.status().await.expect("status");

    assert!(matches!(
        error,
        ServingError::SnapshotLoad {
            failure
        } if failure.kind == ServingFailureKind::InvalidSnapshotJson
            && failure.snapshot == Some(ServingSnapshotKind::Dns)
    ));
    assert_eq!(route.backends[0].address, "fd00::1:8080");
    assert_eq!(records[0].value, "fd00::1");
    assert_eq!(status.loaded_revisions.gateway, "rev-1");
    assert_eq!(status.loaded_revisions.dns, "rev-1");
    assert!(status.last_failure.is_some());
    assert_eq!(
        status.freshness,
        ServingFreshness::ServingLastGoodAfterFailure
    );
}

#[tokio::test]
async fn successful_reload_replaces_gateway_and_dns_together() {
    let root = TempDir::new().expect("tempdir");
    write_prod_snapshots(&root, "rev-1", "fd00::1:8080", "fd00::1");
    let actor = ServingActorHandle::spawn(prod(), snapshot_paths(&root), Duration::from_secs(60))
        .expect("spawn serving actor");
    write_prod_snapshots(&root, "rev-2", "fd00::2:8080", "fd00::2");

    let status = actor.reload().await.expect("reload");
    let route = actor
        .gateway_route_for_host("web.example.test")
        .await
        .expect("query gateway")
        .expect("route");
    let records = actor
        .dns_records("web.example.test", "AAAA")
        .await
        .expect("query dns");

    assert_eq!(route.backends[0].address, "fd00::2:8080");
    assert_eq!(records[0].value, "fd00::2");
    assert_eq!(status.loaded_revisions.gateway, "rev-2");
    assert_eq!(status.loaded_revisions.dns, "rev-2");
    assert_eq!(status.reload_attempts, 1);
    assert!(status.last_reload_attempt_at.is_some());
    assert!(status.last_reload_success_at.is_some());
    assert!(status.last_failure.is_none());
}

#[tokio::test]
async fn failed_reload_preserves_last_good_and_records_failure() {
    let root = TempDir::new().expect("tempdir");
    let paths = snapshot_paths(&root);
    write_prod_snapshots(&root, "rev-1", "fd00::1:8080", "fd00::1");
    let actor =
        ServingActorHandle::spawn(prod(), paths.clone(), Duration::from_secs(60)).expect("spawn");
    fs::write(&paths.dns, b"not json").expect("corrupt dns snapshot");

    let error = actor.reload().await.expect_err("reload should fail");
    let route = actor
        .gateway_route_for_host("web.example.test")
        .await
        .expect("query gateway")
        .expect("route");
    let records = actor
        .dns_records("web.example.test", "AAAA")
        .await
        .expect("query dns");
    let status = actor.status().await.expect("status");

    assert!(matches!(
        error,
        ServingError::SnapshotLoad {
            failure
        } if failure.kind == ServingFailureKind::InvalidSnapshotJson
            && failure.snapshot == Some(ServingSnapshotKind::Dns)
    ));
    assert_eq!(route.backends[0].address, "fd00::1:8080");
    assert_eq!(records[0].value, "fd00::1");
    assert_eq!(status.loaded_revisions.gateway, "rev-1");
    assert_eq!(status.loaded_revisions.dns, "rev-1");
    assert_eq!(status.reload_attempts, 1);
    assert!(status.last_reload_attempt_at.is_some());
    assert!(status.last_reload_success_at.is_some());
    assert!(status.last_failure.is_some());
    assert_eq!(
        status.freshness,
        ServingFreshness::ServingLastGoodAfterFailure
    );
}

#[tokio::test]
async fn wrong_island_reload_preserves_last_good() {
    let root = TempDir::new().expect("tempdir");
    write_prod_snapshots(&root, "rev-1", "fd00::1:8080", "fd00::1");
    let actor = ServingActorHandle::spawn(prod(), snapshot_paths(&root), Duration::from_secs(60))
        .expect("spawn serving actor");
    write_snapshot_files(
        &root,
        &gateway_snapshot("prod", "rev-2", "web.example.test", "fd00::2:8080"),
        &dns_snapshot("laptop", "rev-2", "web.example.test", "fd00::2"),
    );

    let error = actor.reload().await.expect_err("wrong-island reload fails");
    let route = actor
        .gateway_route_for_host("web.example.test")
        .await
        .expect("query gateway")
        .expect("route");
    let status = actor.status().await.expect("status");

    assert!(matches!(
        error,
        ServingError::SnapshotLoad {
            failure
        } if failure.kind == ServingFailureKind::SnapshotPath
            && failure.snapshot == Some(ServingSnapshotKind::Dns)
    ));
    assert_eq!(route.backends[0].address, "fd00::1:8080");
    assert_eq!(status.loaded_revisions.gateway, "rev-1");
    assert_eq!(status.loaded_revisions.dns, "rev-1");
    assert_eq!(status.reload_attempts, 1);
}

#[tokio::test]
async fn deleted_snapshot_reload_preserves_last_good_without_partial_replace() {
    let root = TempDir::new().expect("tempdir");
    let paths = snapshot_paths(&root);
    write_prod_snapshots(&root, "rev-1", "fd00::1:8080", "fd00::1");
    let actor =
        ServingActorHandle::spawn(prod(), paths.clone(), Duration::from_secs(60)).expect("spawn");
    write_snapshot_files(
        &root,
        &gateway_snapshot("prod", "rev-2", "web.example.test", "fd00::2:8080"),
        &dns_snapshot("prod", "rev-2", "web.example.test", "fd00::2"),
    );
    fs::remove_file(&paths.dns).expect("delete dns snapshot");

    let error = actor.reload().await.expect_err("missing dns reload fails");
    let route = actor
        .gateway_route_for_host("web.example.test")
        .await
        .expect("query gateway")
        .expect("route");
    let records = actor
        .dns_records("web.example.test", "AAAA")
        .await
        .expect("query dns");
    let status = actor.status().await.expect("status");

    assert!(matches!(
        error,
        ServingError::SnapshotLoad {
            failure
        } if failure.kind == ServingFailureKind::MissingSnapshot
            && failure.snapshot == Some(ServingSnapshotKind::Dns)
    ));
    assert_eq!(route.backends[0].address, "fd00::1:8080");
    assert_eq!(records[0].value, "fd00::1");
    assert_eq!(status.loaded_revisions.gateway, "rev-1");
    assert_eq!(status.loaded_revisions.dns, "rev-1");
    assert_eq!(status.reload_attempts, 1);
}

#[tokio::test]
async fn stale_threshold_marks_aged_snapshot_without_failure() {
    let root = TempDir::new().expect("tempdir");
    write_prod_snapshots(&root, "rev-1", "fd00::1:8080", "fd00::1");
    let actor =
        ServingActorHandle::spawn(prod(), snapshot_paths(&root), Duration::ZERO).expect("spawn");

    let status = actor.status().await.expect("status");

    assert_eq!(status.freshness, ServingFreshness::ServingAgedSnapshot);
}
