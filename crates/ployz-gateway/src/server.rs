use std::sync::Arc;

use async_trait::async_trait;
use pingora::prelude::*;
#[cfg(unix)]
use pingora::server::{RunArgs, ShutdownSignal, ShutdownSignalWatch};
use pingora::services::listening::Service as ListeningService;
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tracing::info;

use crate::config::{GatewayConfig, GatewayError};
use crate::proxy::GatewayApp;
use crate::snapshot::SharedSnapshot;
use crate::sync::load_projected_snapshot_from_store;

// ---------------------------------------------------------------------------
// EmbeddedShutdownWatch
// ---------------------------------------------------------------------------

#[cfg(unix)]
pub struct EmbeddedShutdownWatch {
    receiver: AsyncMutex<Option<oneshot::Receiver<()>>>,
}

#[cfg(unix)]
impl EmbeddedShutdownWatch {
    #[must_use]
    pub fn new(receiver: oneshot::Receiver<()>) -> Self {
        Self {
            receiver: AsyncMutex::new(Some(receiver)),
        }
    }
}

#[cfg(unix)]
#[async_trait]
impl ShutdownSignalWatch for EmbeddedShutdownWatch {
    async fn recv(&self) -> ShutdownSignal {
        let mut guard = self.receiver.lock().await;
        let Some(receiver) = guard.take() else {
            return ShutdownSignal::FastShutdown;
        };
        let _ = receiver.await;
        ShutdownSignal::GracefulTerminate
    }
}

// ---------------------------------------------------------------------------
// Server bootstrap
// ---------------------------------------------------------------------------

pub fn run_server(
    opt: Opt,
    listen_addr: &str,
    threads: usize,
    metrics_listen_addr: Option<&str>,
    shared_snapshot: SharedSnapshot,
    #[cfg(unix)] shutdown_signal: Option<Box<dyn ShutdownSignalWatch>>,
    #[cfg(not(unix))] _shutdown_signal: Option<()>,
) -> Result<(), GatewayError> {
    let mut server = Server::new(Some(opt))
        .map_err(|err| GatewayError::Runtime(format!("server init: {err}")))?;
    let Some(configuration) = Arc::get_mut(&mut server.configuration) else {
        return Err(GatewayError::Runtime(
            "server configuration was unexpectedly shared".into(),
        ));
    };
    configuration.threads = threads;
    if cfg!(test) {
        configuration.grace_period_seconds.get_or_insert(0);
        configuration
            .graceful_shutdown_timeout_seconds
            .get_or_insert(0);
    }
    server.bootstrap();

    let mut service = http_proxy_service(&server.configuration, GatewayApp::new(shared_snapshot));
    service.add_tcp(listen_addr);
    server.add_service(service);

    if let Some(metrics_listen_addr) = metrics_listen_addr {
        let mut metrics_service = ListeningService::prometheus_http_service();
        metrics_service.add_tcp(metrics_listen_addr);
        server.add_service(metrics_service);
        info!(
            listen = metrics_listen_addr,
            "gateway metrics listener running"
        );
    }

    info!(listen = listen_addr, threads, "gateway listening");
    #[cfg(unix)]
    if let Some(shutdown_signal) = shutdown_signal {
        server.run(RunArgs { shutdown_signal });
        return Ok(());
    }
    server.run_forever()
}

// ---------------------------------------------------------------------------
// Standalone process entry point
// ---------------------------------------------------------------------------

pub fn run_gateway_process_with_store<S>(
    config: GatewayConfig,
    store: S,
) -> Result<(), GatewayError>
where
    S: crate::sync::RoutingStore + Send + Sync + 'static,
{
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| GatewayError::Runtime(err.to_string()))?;
    let initial_snapshot = runtime.block_on(load_projected_snapshot_from_store(&store))?;
    crate::metrics::update_route_counts(&initial_snapshot);
    let shared_snapshot = SharedSnapshot::new(initial_snapshot);
    crate::sync::spawn_sync_thread_with_store(store, shared_snapshot.clone())?;
    let opt = Opt::parse_args();
    run_server(
        opt,
        config.listen_addr.as_str(),
        config.threads,
        config.metrics_listen_addr.as_deref(),
        shared_snapshot,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::{EmbeddedShutdownWatch, run_server};
    use crate::SharedSnapshot;
    use crate::routes::{BackendView, GatewaySnapshot, HttpRouteView};
    use pingora::prelude::Opt;
    use ployz_types::model::{InstanceId, MachineId};
    use ployz_types::spec::Namespace;
    use std::collections::HashMap;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn gateway_metrics_listener_reports_requests_and_route_counts() {
        ployz_metrics::set_build_info("ployz-gateway", env!("CARGO_PKG_VERSION"));
        let gateway_addr = free_local_addr();
        let metrics_addr = free_local_addr();
        let upstream_addr = free_local_addr();

        let upstream = thread::spawn(move || serve_single_http_request(upstream_addr));

        let snapshot = GatewaySnapshot {
            http_routes: vec![HttpRouteView {
                route_id: "http:prod:web:0".into(),
                namespace: Namespace("prod".into()),
                service: "web".into(),
                revision_hash: "rev-1".into(),
                hostnames: vec!["example.com".into()],
                path_prefix: "/".into(),
                backends: vec![BackendView {
                    instance_id: InstanceId("inst-1".into()),
                    machine_id: MachineId("machine-1".into()),
                    service_port: "http".into(),
                    address: upstream_addr,
                }],
            }],
            tcp_routes: Vec::new(),
        };
        crate::metrics::update_route_counts(&snapshot);

        let shared_snapshot = SharedSnapshot::new(snapshot);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let gateway_thread = thread::spawn(move || {
            run_server(
                Opt::default(),
                &gateway_addr.to_string(),
                1,
                Some(&metrics_addr.to_string()),
                shared_snapshot,
                Some(Box::new(EmbeddedShutdownWatch::new(shutdown_rx))),
            )
            .expect("gateway server should run");
        });

        wait_for_metrics_listener(metrics_addr).await;

        let matched = send_http_request(gateway_addr, Some("example.com")).await;
        assert!(matched.starts_with("HTTP/1.1 200"));
        let unmatched = send_http_request(gateway_addr, Some("missing.example.com")).await;
        assert!(unmatched.starts_with("HTTP/1.1 404"));

        let metrics = fetch_http_body(metrics_addr, "/").await;
        assert!(metrics.contains(
            "ployz_gateway_requests_total{matched=\"true\",method=\"GET\",status_class=\"2xx\"}"
        ));
        assert!(metrics.contains(
            "ployz_gateway_requests_total{matched=\"false\",method=\"GET\",status_class=\"4xx\"}"
        ));
        assert!(metrics.contains(
            "ployz_gateway_request_duration_seconds_count{matched=\"true\",method=\"GET\",status_class=\"2xx\"}"
        ));
        assert!(metrics.contains("ployz_gateway_routes{protocol=\"http\"} 1"));
        assert!(metrics.contains("ployz_gateway_routes{protocol=\"tcp\"} 0"));

        let _ = shutdown_tx.send(());
        gateway_thread.join().expect("gateway thread should join");
        upstream.join().expect("upstream thread should join");
    }

    #[tokio::test]
    async fn gateway_adds_forwarded_headers_to_upstream_request() {
        let gateway_addr = free_local_addr();
        let upstream_addr = free_local_addr();
        let (request_tx, request_rx) = mpsc::channel();

        let upstream =
            thread::spawn(move || capture_single_http_request(upstream_addr, request_tx));
        let shared_snapshot = SharedSnapshot::new(gateway_snapshot(upstream_addr));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let gateway_thread = thread::spawn(move || {
            run_server(
                Opt::default(),
                &gateway_addr.to_string(),
                1,
                None,
                shared_snapshot,
                Some(Box::new(EmbeddedShutdownWatch::new(shutdown_rx))),
            )
            .expect("gateway server should run");
        });

        wait_for_listener(gateway_addr).await;

        let response = send_http_request_with_headers(gateway_addr, "example.com", &[]).await;
        assert!(response.starts_with("HTTP/1.1 200"));

        let captured = request_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("upstream request should be captured");
        assert_eq!(
            captured.headers.get("host"),
            Some(&"example.com".to_string())
        );
        assert_eq!(
            captured.headers.get("x-forwarded-for"),
            Some(&"127.0.0.1".to_string())
        );
        assert_eq!(
            captured.headers.get("x-forwarded-proto"),
            Some(&"http".to_string())
        );
        assert_eq!(
            captured.headers.get("x-forwarded-host"),
            Some(&"example.com".to_string())
        );
        assert_eq!(
            captured.headers.get("x-forwarded-port"),
            Some(&gateway_addr.port().to_string())
        );
        assert_eq!(
            captured.headers.get("via"),
            Some(&"1.1 ployz-gateway".to_string())
        );

        let _ = shutdown_tx.send(());
        gateway_thread.join().expect("gateway thread should join");
        upstream.join().expect("upstream thread should join");
    }

    #[tokio::test]
    async fn gateway_appends_existing_forwarded_headers_to_upstream_request() {
        let gateway_addr = free_local_addr();
        let upstream_addr = free_local_addr();
        let (request_tx, request_rx) = mpsc::channel();

        let upstream =
            thread::spawn(move || capture_single_http_request(upstream_addr, request_tx));
        let shared_snapshot = SharedSnapshot::new(gateway_snapshot(upstream_addr));
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let gateway_thread = thread::spawn(move || {
            run_server(
                Opt::default(),
                &gateway_addr.to_string(),
                1,
                None,
                shared_snapshot,
                Some(Box::new(EmbeddedShutdownWatch::new(shutdown_rx))),
            )
            .expect("gateway server should run");
        });

        wait_for_listener(gateway_addr).await;

        let response = send_http_request_with_headers(
            gateway_addr,
            "example.com",
            &[
                ("X-Forwarded-For", "203.0.113.10"),
                ("Via", "1.0 previous-proxy"),
            ],
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"));

        let captured = request_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("upstream request should be captured");
        assert_eq!(
            captured.headers.get("x-forwarded-for"),
            Some(&"203.0.113.10, 127.0.0.1".to_string())
        );
        assert_eq!(
            captured.headers.get("via"),
            Some(&"1.0 previous-proxy, 1.1 ployz-gateway".to_string())
        );

        let _ = shutdown_tx.send(());
        gateway_thread.join().expect("gateway thread should join");
        upstream.join().expect("upstream thread should join");
    }

    fn serve_single_http_request(addr: SocketAddr) {
        let listener = TcpListener::bind(addr).expect("upstream listener should bind");
        let (mut stream, _) = listener
            .accept()
            .expect("upstream connection should arrive");
        let mut buffer = [0_u8; 1024];
        let _ = stream
            .read(&mut buffer)
            .expect("upstream request should read");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .expect("upstream response should write");
    }

    fn capture_single_http_request(addr: SocketAddr, sender: mpsc::Sender<CapturedRequest>) {
        let listener = TcpListener::bind(addr).expect("upstream listener should bind");
        let (mut stream, _) = listener
            .accept()
            .expect("upstream connection should arrive");
        let mut buffer = [0_u8; 4096];
        let read = stream
            .read(&mut buffer)
            .expect("upstream request should read");
        let raw = String::from_utf8_lossy(&buffer[..read]).into_owned();
        sender
            .send(CapturedRequest {
                headers: parse_headers(&raw),
            })
            .expect("captured request should send");
        stream
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
            .expect("upstream response should write");
    }

    struct CapturedRequest {
        headers: HashMap<String, String>,
    }

    fn parse_headers(raw: &str) -> HashMap<String, String> {
        raw.split("\r\n")
            .skip(1)
            .take_while(|line| !line.is_empty())
            .filter_map(|line| {
                let (name, value) = line.split_once(':')?;
                Some((name.to_ascii_lowercase(), value.trim().to_string()))
            })
            .collect()
    }

    async fn wait_for_metrics_listener(addr: SocketAddr) {
        wait_for_listener(addr).await;
    }

    async fn wait_for_listener(addr: SocketAddr) {
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(addr).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for listener {addr}");
    }

    async fn send_http_request(addr: SocketAddr, host: Option<&str>) -> String {
        send_http_request_with_headers(addr, host.unwrap_or("127.0.0.1"), &[]).await
    }

    async fn send_http_request_with_headers(
        addr: SocketAddr,
        host: &str,
        headers: &[(&str, &str)],
    ) -> String {
        let mut stream = tokio::net::TcpStream::connect(addr)
            .await
            .expect("http connection should succeed");
        let mut request = format!("GET / HTTP/1.1\r\nHost: {host}\r\n");
        for (name, value) in headers {
            request.push_str(name);
            request.push_str(": ");
            request.push_str(value);
            request.push_str("\r\n");
        }
        request.push_str("Connection: close\r\n\r\n");
        stream
            .write_all(request.as_bytes())
            .await
            .expect("request write should succeed");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("response read should succeed");
        String::from_utf8(response).expect("response should be utf-8")
    }

    fn gateway_snapshot(upstream_addr: SocketAddr) -> GatewaySnapshot {
        GatewaySnapshot {
            http_routes: vec![HttpRouteView {
                route_id: "http:prod:web:0".into(),
                namespace: Namespace("prod".into()),
                service: "web".into(),
                revision_hash: "rev-1".into(),
                hostnames: vec!["example.com".into()],
                path_prefix: "/".into(),
                backends: vec![BackendView {
                    instance_id: InstanceId("inst-1".into()),
                    machine_id: MachineId("machine-1".into()),
                    service_port: "http".into(),
                    address: upstream_addr,
                }],
            }],
            tcp_routes: Vec::new(),
        }
    }

    async fn fetch_http_body(addr: SocketAddr, path: &str) -> String {
        let mut stream = tokio::net::TcpStream::connect(addr)
            .await
            .expect("metrics connection should succeed");
        let request = format!(
            "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
            addr
        );
        stream
            .write_all(request.as_bytes())
            .await
            .expect("metrics request write should succeed");
        let mut response = Vec::new();
        stream
            .read_to_end(&mut response)
            .await
            .expect("metrics response read should succeed");
        let response = String::from_utf8(response).expect("metrics response should be utf-8");
        let Some((_, body)) = response.split_once("\r\n\r\n") else {
            panic!("http response should contain headers");
        };
        body.to_string()
    }

    fn free_local_addr() -> SocketAddr {
        TcpListener::bind("127.0.0.1:0")
            .expect("should bind ephemeral port")
            .local_addr()
            .expect("ephemeral listener should have local addr")
    }
}
