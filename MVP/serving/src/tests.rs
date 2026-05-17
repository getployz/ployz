use std::fs;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::time::Duration;

use hickory_proto::op::{Message, Query, ResponseCode};
use hickory_proto::rr::{Name, RData, RecordType};
use mvp_bus::IslandId;
use mvp_projection::{
    BackendEndpoint, DnsRecordProjection, DnsSnapshotFile, GatewayRouteProjection,
    GatewaySnapshotFile, NodeId, RouteId,
};
use tempfile::TempDir;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::{
    ServingActorHandle, ServingError, ServingFailureKind, ServingFreshness, ServingSnapshotBatch,
    ServingSnapshotKind, ServingSnapshotPaths, WireServingState, spawn_dns_server,
    spawn_http_gateway,
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

fn loopback_any() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
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
async fn http_gateway_routes_to_selected_backend() {
    let backend = TestBackend::spawn("backend-1").await;
    let root = TempDir::new().expect("tempdir");
    write_prod_snapshots(&root, "rev-1", &backend.addr().to_string(), "fd00::1");
    let actor = ServingActorHandle::spawn(prod(), snapshot_paths(&root), Duration::from_secs(60))
        .expect("spawn serving actor");
    let gateway = spawn_http_gateway(loopback_any(), WireServingState::new(actor))
        .await
        .expect("spawn http gateway");

    let response = http_get(gateway.listen_addr(), "web.example.test", "/health").await;

    assert!(response.starts_with("HTTP/1.1 200 OK"), "{response}");
    assert!(response.contains("backend-1 /health"), "{response}");
    assert_eq!(gateway.metrics().request_count, 1);
    gateway.shutdown().await.expect("shutdown gateway");
    backend.shutdown().await;
}

#[tokio::test]
async fn http_gateway_unknown_host_returns_not_found() {
    let root = TempDir::new().expect("tempdir");
    write_prod_snapshots(&root, "rev-1", "127.0.0.1:1", "fd00::1");
    let actor = ServingActorHandle::spawn(prod(), snapshot_paths(&root), Duration::from_secs(60))
        .expect("spawn serving actor");
    let gateway = spawn_http_gateway(loopback_any(), WireServingState::new(actor))
        .await
        .expect("spawn http gateway");

    let response = http_get(gateway.listen_addr(), "missing.example.test", "/").await;

    assert!(response.starts_with("HTTP/1.1 404 Not Found"), "{response}");
    assert_eq!(gateway.metrics().request_count, 1);
    gateway.shutdown().await.expect("shutdown gateway");
}

#[tokio::test]
async fn http_gateway_invalid_backend_returns_503_and_records_failure() {
    let root = TempDir::new().expect("tempdir");
    write_prod_snapshots(&root, "rev-1", "not-a-socket", "fd00::1");
    let actor = ServingActorHandle::spawn(prod(), snapshot_paths(&root), Duration::from_secs(60))
        .expect("spawn serving actor");
    let gateway = spawn_http_gateway(loopback_any(), WireServingState::new(actor))
        .await
        .expect("spawn http gateway");

    let response = http_get(gateway.listen_addr(), "web.example.test", "/").await;
    let metrics = gateway.metrics();

    assert!(
        response.starts_with("HTTP/1.1 503 Service Unavailable"),
        "{response}"
    );
    assert_eq!(metrics.request_count, 1);
    assert_eq!(metrics.backend_failure_count, 1);
    gateway.shutdown().await.expect("shutdown gateway");
}

#[tokio::test]
async fn dns_server_answers_aaaa_from_snapshot() {
    let root = TempDir::new().expect("tempdir");
    write_prod_snapshots(&root, "rev-1", "127.0.0.1:1", "fd00::1");
    let actor = ServingActorHandle::spawn(prod(), snapshot_paths(&root), Duration::from_secs(60))
        .expect("spawn serving actor");
    let server = spawn_dns_server(loopback_any(), WireServingState::new(actor))
        .await
        .expect("spawn dns server");

    let response = dns_query(server.listen_addr(), "web.example.test.", RecordType::AAAA).await;

    assert_eq!(response.metadata.response_code, ResponseCode::NoError);
    assert_eq!(response.answers.len(), 1);
    assert_eq!(response.answers[0].ttl, 30);
    assert!(matches!(
        &response.answers[0].data,
        RData::AAAA(addr) if addr.to_string() == "fd00::1"
    ));
    assert_eq!(server.metrics().request_count, 1);
    server.shutdown().await.expect("shutdown dns server");
}

#[tokio::test]
async fn dns_server_lookup_is_case_insensitive() {
    let root = TempDir::new().expect("tempdir");
    write_prod_snapshots(&root, "rev-1", "127.0.0.1:1", "fd00::1");
    let actor = ServingActorHandle::spawn(prod(), snapshot_paths(&root), Duration::from_secs(60))
        .expect("spawn serving actor");
    let server = spawn_dns_server(loopback_any(), WireServingState::new(actor))
        .await
        .expect("spawn dns server");

    let response = dns_query(server.listen_addr(), "WEB.EXAMPLE.TEST.", RecordType::AAAA).await;

    assert_eq!(response.metadata.response_code, ResponseCode::NoError);
    assert_eq!(response.answers.len(), 1);
    server.shutdown().await.expect("shutdown dns server");
}

#[tokio::test]
async fn dns_server_unknown_name_returns_nxdomain() {
    let root = TempDir::new().expect("tempdir");
    write_prod_snapshots(&root, "rev-1", "127.0.0.1:1", "fd00::1");
    let actor = ServingActorHandle::spawn(prod(), snapshot_paths(&root), Duration::from_secs(60))
        .expect("spawn serving actor");
    let server = spawn_dns_server(loopback_any(), WireServingState::new(actor))
        .await
        .expect("spawn dns server");

    let response = dns_query(
        server.listen_addr(),
        "missing.example.test.",
        RecordType::AAAA,
    )
    .await;

    assert_eq!(response.metadata.response_code, ResponseCode::NXDomain);
    assert!(response.answers.is_empty());
    server.shutdown().await.expect("shutdown dns server");
}

#[tokio::test]
async fn dns_server_records_malformed_packets_without_crashing() {
    let root = TempDir::new().expect("tempdir");
    write_prod_snapshots(&root, "rev-1", "127.0.0.1:1", "fd00::1");
    let actor = ServingActorHandle::spawn(prod(), snapshot_paths(&root), Duration::from_secs(60))
        .expect("spawn serving actor");
    let server = spawn_dns_server(loopback_any(), WireServingState::new(actor))
        .await
        .expect("spawn dns server");
    let client = UdpSocket::bind(loopback_any()).await.expect("bind client");

    client
        .send_to(b"not dns", server.listen_addr())
        .await
        .expect("send malformed");
    let response = dns_query(server.listen_addr(), "web.example.test.", RecordType::AAAA).await;
    let metrics = server.metrics();

    assert_eq!(response.metadata.response_code, ResponseCode::NoError);
    assert_eq!(metrics.malformed_dns_count, 1);
    assert_eq!(metrics.request_count, 2);
    server.shutdown().await.expect("shutdown dns server");
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

struct TestBackend {
    addr: SocketAddr,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl TestBackend {
    async fn spawn(id: &'static str) -> Self {
        let listener = TcpListener::bind(loopback_any())
            .await
            .expect("bind test backend");
        let addr = listener.local_addr().expect("backend local addr");
        let (shutdown, mut shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => return,
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else {
                            return;
                        };
                        tokio::spawn(async move {
                            let mut request = Vec::new();
                            let mut chunk = [0_u8; 1024];
                            loop {
                                let read = stream.read(&mut chunk).await.expect("read backend request");
                                if read == 0 {
                                    return;
                                }
                                request.extend_from_slice(&chunk[..read]);
                                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            let request_line = String::from_utf8_lossy(&request)
                                .lines()
                                .next()
                                .unwrap_or("GET / HTTP/1.1")
                                .to_string();
                            let path = request_line
                                .split_whitespace()
                                .nth(1)
                                .unwrap_or("/");
                            let body = format!("{id} {path}\n");
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                body.len(),
                                body
                            );
                            stream.write_all(response.as_bytes()).await.expect("write backend response");
                        });
                    }
                }
            }
        });
        Self {
            addr,
            shutdown,
            task,
        }
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }

    async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}

async fn http_get(addr: SocketAddr, host: &str, path: &str) -> String {
    let mut stream = TcpStream::connect(addr).await.expect("connect gateway");
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    stream
        .write_all(request.as_bytes())
        .await
        .expect("write request");
    let mut response = Vec::new();
    timeout(Duration::from_secs(2), stream.read_to_end(&mut response))
        .await
        .expect("read response timeout")
        .expect("read response");
    String::from_utf8(response).expect("utf8 response")
}

async fn dns_query(addr: SocketAddr, name: &str, record_type: RecordType) -> Message {
    let socket = UdpSocket::bind(loopback_any())
        .await
        .expect("bind dns client");
    let mut query = Message::query();
    query.add_query(Query::query(
        Name::from_ascii(name).expect("query name"),
        record_type,
    ));
    let bytes = query.to_vec().expect("encode query");
    socket.send_to(&bytes, addr).await.expect("send dns query");
    let mut response = [0_u8; 2048];
    let len = timeout(Duration::from_secs(2), socket.recv(&mut response))
        .await
        .expect("dns response timeout")
        .expect("receive dns response");
    Message::from_vec(&response[..len]).expect("decode dns response")
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
