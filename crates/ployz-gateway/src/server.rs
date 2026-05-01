use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use pingora::listeners::{TlsAccept, tls::TlsSettings};
use pingora::prelude::*;
use pingora::protocols::tls::TlsRef;
#[cfg(unix)]
use pingora::server::{RunArgs, ShutdownSignal, ShutdownSignalWatch};
use pingora::services::listening::Service as ListeningService;
use pingora::tls::{ext, ssl};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tokio::time::{sleep, timeout};
use tracing::{error, info, warn};

use crate::config::{GatewayConfig, GatewayError};
use crate::proxy::GatewayApp;
use crate::routes::GatewaySnapshot;
#[cfg(test)]
use crate::routes::ProjectedTlsMaterial;
use crate::snapshot::SharedSnapshot;
use crate::sync::load_projected_snapshot_from_store;
use ployz_types::model::MachineId;

const STORE_READY_TIMEOUT: Duration = Duration::from_secs(30);
const STORE_READY_ATTEMPT_TIMEOUT: Duration = Duration::from_secs(5);
const STORE_READY_POLL: Duration = Duration::from_millis(250);

// ---------------------------------------------------------------------------
// Managed-TLS material resolution (pure, testable without pingora)
// ---------------------------------------------------------------------------

/// SNI -> snapshot lookup result. PEM parsing happens at projection time, so
/// this enum no longer carries parse-error variants — malformed material never
/// reaches the handshake path.
#[cfg(test)]
pub(crate) enum TlsResolution {
    Ready(Arc<ProjectedTlsMaterial>),
    MissingSni,
    HostnameMiss(String),
}

#[cfg(test)]
pub(crate) fn resolve_tls_material(
    snapshot: &GatewaySnapshot,
    server_name: Option<&str>,
) -> TlsResolution {
    let Some(server_name) = server_name else {
        return TlsResolution::MissingSni;
    };
    // Project stores certificate keys via `normalize_request_host` (lowercase,
    // trimmed trailing dot/port), so the SNI must be normalized the same way
    // or mixed-case clients miss an otherwise valid certificate.
    let hostname = crate::routes::normalize_request_host(server_name);
    match snapshot.certificates.get(&hostname) {
        Some(certificate) => TlsResolution::Ready(Arc::clone(&certificate.tls)),
        None => TlsResolution::HostnameMiss(hostname),
    }
}

pub struct GatewayTlsListener<'a> {
    pub listen_addr: &'a str,
    pub static_cert_path: Option<&'a str>,
    pub static_key_path: Option<&'a str>,
}

struct ManagedTlsCallbacks {
    shared_snapshot: SharedSnapshot,
}

#[async_trait]
impl TlsAccept for ManagedTlsCallbacks {
    async fn certificate_callback(&self, ssl: &mut TlsRef) -> () {
        let state = self.shared_snapshot.load();
        let server_name = ssl.servername(ssl::NameType::HOST_NAME);
        let material = match server_name {
            Some(server_name) => match state.certificate(server_name) {
                Some(certificate) => Arc::clone(&certificate.tls),
                None => {
                    warn!(
                        hostname = crate::routes::normalize_request_host(server_name),
                        "managed TLS certificate was not found for SNI hostname"
                    );
                    return;
                }
            },
            None => {
                warn!("managed TLS handshake did not include SNI");
                return;
            }
        };
        let hostname = server_name.map(ToString::to_string).unwrap_or_default();
        if let Err(error) = ext::ssl_use_certificate(ssl, &material.leaf) {
            error!(
                hostname = hostname,
                ?error,
                "managed TLS leaf certificate could not be installed"
            );
            return;
        }
        if let Err(error) = ext::ssl_use_private_key(ssl, &material.private_key) {
            error!(
                hostname = hostname,
                ?error,
                "managed TLS private key could not be installed"
            );
            return;
        }
        for certificate in material.chain.iter() {
            if let Err(error) = ext::ssl_add_chain_cert(ssl, certificate) {
                error!(
                    hostname = hostname,
                    ?error,
                    "managed TLS chain certificate could not be installed"
                );
                return;
            }
        }
    }
}

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
    tls_listener: Option<GatewayTlsListener<'_>>,
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

    let mut service = http_proxy_service(
        &server.configuration,
        GatewayApp::new(shared_snapshot.clone()),
    );
    service.add_tcp(listen_addr);
    if let Some(tls_listener) = tls_listener {
        let mut tls_settings = if let (Some(cert_path), Some(key_path)) =
            (tls_listener.static_cert_path, tls_listener.static_key_path)
        {
            TlsSettings::intermediate(cert_path, key_path)
                .map_err(|err| GatewayError::Runtime(format!("tls settings: {err}")))?
        } else {
            TlsSettings::with_callbacks(Box::new(ManagedTlsCallbacks {
                shared_snapshot: shared_snapshot.clone(),
            }))
            .map_err(|err| GatewayError::Runtime(format!("tls settings: {err}")))?
        };
        tls_settings.enable_h2();
        service.add_tls_with_settings(tls_listener.listen_addr, None, tls_settings);
        info!(
            listen = tls_listener.listen_addr,
            "gateway https listener running"
        );
    }
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
    S: crate::sync::RoutingSnapshotReader + Send + Sync + 'static,
{
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("ployz-gateway-async")
        .worker_threads(2)
        .build()
        .map_err(|err| GatewayError::Runtime(err.to_string()))?;
    run_gateway_process_on_runtime(runtime, config, store)
}

/// Run the gateway using an externally-provided runtime.
///
/// Run store sync and Pingora against the same runtime so store subscriptions
/// keep polling while Pingora owns the main thread.
pub fn run_gateway_process_on_runtime<S>(
    runtime: tokio::runtime::Runtime,
    config: GatewayConfig,
    store: S,
) -> Result<(), GatewayError>
where
    S: crate::sync::RoutingSnapshotReader + Send + Sync + 'static,
{
    let initial_snapshot = runtime.block_on(wait_for_initial_snapshot(&store))?;
    crate::metrics::update_route_counts(&initial_snapshot);
    let shared_snapshot = SharedSnapshot::new(initial_snapshot);

    // The multi-thread runtime's worker threads keep polling store events while
    // Pingora blocks the main thread.
    let sync_snapshot = shared_snapshot.clone();
    let sync_machine_id = MachineId(config.machine_id.clone());
    runtime.spawn(async move {
        if let Err(err) = crate::sync::run_sync_loop(store, sync_snapshot, sync_machine_id).await {
            tracing::warn!(?err, "gateway sync loop exited");
        }
    });

    let opt = Opt::parse_args();
    let result = run_server(
        opt,
        config.listen_addr.as_str(),
        gateway_tls_listener(&config),
        config.threads,
        config.metrics_listen_addr.as_deref(),
        shared_snapshot,
        None,
    );

    // Explicitly tear down the runtime so in-flight store tasks get a chance to
    // finish before the process exits.
    runtime.shutdown_background();
    result
}

async fn wait_for_initial_snapshot<S>(store: &S) -> Result<GatewaySnapshot, GatewayError>
where
    S: crate::sync::RoutingSnapshotReader + Send + Sync,
{
    let deadline = tokio::time::Instant::now() + STORE_READY_TIMEOUT;
    loop {
        match timeout(
            STORE_READY_ATTEMPT_TIMEOUT,
            load_projected_snapshot_from_store(store),
        )
        .await
        {
            Ok(Ok(snapshot)) => return Ok(snapshot),
            Ok(Err(error)) if tokio::time::Instant::now() < deadline => {
                warn!(?error, "gateway waiting for store readiness");
            }
            Err(_) if tokio::time::Instant::now() < deadline => {
                warn!("gateway timed out loading initial store snapshot; retrying");
            }
            Ok(Err(error)) => {
                return Err(GatewayError::Store(format!(
                    "store did not become ready within {:?}: {error}",
                    STORE_READY_TIMEOUT
                )));
            }
            Err(_) => {
                return Err(GatewayError::Store(format!(
                    "store did not return initial snapshot within {:?}",
                    STORE_READY_TIMEOUT
                )));
            }
        }
        sleep(STORE_READY_POLL).await;
    }
}

fn gateway_tls_listener(config: &GatewayConfig) -> Option<GatewayTlsListener<'_>> {
    let Some(listen_addr) = config.https_listen_addr.as_deref() else {
        return None;
    };
    Some(GatewayTlsListener {
        listen_addr,
        static_cert_path: config
            .tls_cert_path
            .as_deref()
            .and_then(std::path::Path::to_str),
        static_key_path: config
            .tls_key_path
            .as_deref()
            .and_then(std::path::Path::to_str),
    })
}

#[cfg(test)]
mod tests {
    use super::{EmbeddedShutdownWatch, run_server};
    use crate::SharedSnapshot;
    use crate::routes::{BackendView, GatewaySnapshot, HttpRouteView, RouteId, ServiceKey};
    use pingora::prelude::Opt;
    use ployz_types::model::{InstanceId, MachineId, MachineTopology};
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
        let _metrics_guard = crate::metrics::ROUTE_METRICS_TEST_LOCK
            .lock()
            .expect("route metrics test lock should not be poisoned");
        ployz_metrics::set_build_info("ployz-gateway", env!("CARGO_PKG_VERSION"));
        let gateway_addr = free_local_addr();
        let metrics_addr = free_local_addr();
        let upstream_addr = free_local_addr();

        let upstream = thread::spawn(move || serve_single_http_request(upstream_addr));

        let snapshot = GatewaySnapshot {
            http_routes: vec![HttpRouteView {
                route_id: RouteId::http(
                    &ServiceKey::new(Namespace("prod".into()), "web".into()),
                    0,
                ),
                namespace: Namespace("prod".into()),
                service: "web".into(),
                revision_hash: "rev-1".into(),
                hostnames: vec!["example.com".into()],
                path_prefix: "/".into(),
                backends: vec![BackendView {
                    instance_id: InstanceId("inst-1".into()),
                    machine_id: MachineId("machine-1".into()),
                    topology: MachineTopology::local(),
                    service_port: "http".into(),
                    address: upstream_addr,
                }],
            }],
            tcp_routes: Vec::new(),
            acme_challenges: std::collections::HashMap::new(),
            certificates: std::collections::HashMap::new(),
        };
        crate::metrics::update_route_counts(&snapshot);

        let shared_snapshot = SharedSnapshot::new(snapshot);
        let (shutdown_tx, shutdown_rx) = oneshot::channel();
        let gateway_thread = thread::spawn(move || {
            run_server(
                Opt::default(),
                &gateway_addr.to_string(),
                None,
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
                None,
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
                None,
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
                ("X-Forwarded-For", "198.51.100.20"),
                ("Via", "1.0 previous-proxy"),
                ("Via", "1.1 second-proxy"),
            ],
        )
        .await;
        assert!(response.starts_with("HTTP/1.1 200"));

        let captured = request_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("upstream request should be captured");
        assert_eq!(
            captured.headers.get("x-forwarded-for"),
            Some(&"203.0.113.10, 198.51.100.20, 127.0.0.1".to_string())
        );
        assert_eq!(
            captured.headers.get("via"),
            Some(&"1.0 previous-proxy, 1.1 second-proxy, 1.1 ployz-gateway".to_string())
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
                route_id: RouteId::http(
                    &ServiceKey::new(Namespace("prod".into()), "web".into()),
                    0,
                ),
                namespace: Namespace("prod".into()),
                service: "web".into(),
                revision_hash: "rev-1".into(),
                hostnames: vec!["example.com".into()],
                path_prefix: "/".into(),
                backends: vec![BackendView {
                    instance_id: InstanceId("inst-1".into()),
                    machine_id: MachineId("machine-1".into()),
                    topology: MachineTopology::local(),
                    service_port: "http".into(),
                    address: upstream_addr,
                }],
            }],
            tcp_routes: Vec::new(),
            acme_challenges: std::collections::HashMap::new(),
            certificates: std::collections::HashMap::new(),
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

    // -----------------------------------------------------------------------
    // resolve_tls_material — pure SNI selection (parsing happens at projection)
    // -----------------------------------------------------------------------

    mod resolve_tls_material_tests {
        use crate::routes::{GatewaySnapshot, ProjectedTlsMaterial, project_certificates};
        use crate::server::{TlsResolution, resolve_tls_material};
        use ployz_types::model::{CertificateRecord, CertificateState, CertificateVersion};
        use std::collections::HashMap;
        use std::sync::Arc;

        fn make_self_signed() -> (String, String) {
            let key = rcgen::KeyPair::generate().expect("keypair");
            let params = rcgen::CertificateParams::new(vec!["test.example.com".into()])
                .expect("cert params");
            let cert = params.self_signed(&key).expect("self-signed cert");
            (cert.pem(), key.serialize_pem())
        }

        fn projected_snapshot_with_cert(hostname: &str) -> GatewaySnapshot {
            let (fullchain_pem, private_key_pem) = make_self_signed();
            let record = CertificateRecord {
                hostname: hostname.into(),
                issuer_url: "https://acme.test/directory".into(),
                account_id: "acct-test".into(),
                state: CertificateState::Active,
                active_version_id: Some("v1".into()),
                versions: vec![CertificateVersion {
                    version_id: "v1".into(),
                    fullchain_pem,
                    private_key_pem,
                    not_before: Some(0),
                    not_after: Some(100),
                    issued_at: 0,
                }],
                order_url: None,
                last_error: None,
                requested_at: 0,
                updated_at: 0,
                next_renewal_at: None,
            };
            GatewaySnapshot {
                http_routes: Vec::new(),
                tcp_routes: Vec::new(),
                acme_challenges: HashMap::new(),
                certificates: project_certificates(&[record]),
            }
        }

        #[test]
        fn missing_sni_returns_missing_sni() {
            let snapshot = GatewaySnapshot::empty();
            assert!(matches!(
                resolve_tls_material(&snapshot, None),
                TlsResolution::MissingSni
            ));
        }

        #[test]
        fn unknown_hostname_returns_hostname_miss() {
            let snapshot = GatewaySnapshot::empty();
            match resolve_tls_material(&snapshot, Some("api.example.com")) {
                TlsResolution::HostnameMiss(hostname) => {
                    assert_eq!(hostname, "api.example.com");
                }
                other => panic!(
                    "expected HostnameMiss, got other variant: {}",
                    label(&other)
                ),
            }
        }

        #[test]
        fn projected_snapshot_returns_ready_arc_with_empty_chain() {
            let snapshot = projected_snapshot_with_cert("api.example.com");
            let entry = snapshot
                .certificates
                .get("api.example.com")
                .expect("cert projected");
            let entry_tls_ptr: *const ProjectedTlsMaterial = Arc::as_ptr(&entry.tls);
            match resolve_tls_material(&snapshot, Some("api.example.com")) {
                TlsResolution::Ready(material) => {
                    // The handshake-path lookup must hand back the *same* Arc
                    // that lives in the snapshot — no clone of the underlying
                    // X509/PKey.
                    assert!(
                        std::ptr::eq(Arc::as_ptr(&material), entry_tls_ptr),
                        "resolved material must share the snapshot's Arc"
                    );
                    assert!(
                        material.chain.is_empty(),
                        "self-signed fullchain has only the leaf"
                    );
                    assert!(
                        material
                            .leaf
                            .public_key()
                            .expect("leaf pubkey")
                            .public_eq(&material.private_key),
                        "leaf and private key must match"
                    );
                }
                other => panic!("expected Ready, got {}", label(&other)),
            }
        }

        fn label(resolution: &TlsResolution) -> &'static str {
            match resolution {
                TlsResolution::Ready(_) => "Ready",
                TlsResolution::MissingSni => "MissingSni",
                TlsResolution::HostnameMiss(_) => "HostnameMiss",
            }
        }

        #[test]
        fn mixed_case_sni_matches_lowercase_certificate_key() {
            let snapshot = projected_snapshot_with_cert("api.example.com");
            match resolve_tls_material(&snapshot, Some("API.Example.COM")) {
                TlsResolution::Ready(_) => {}
                other => panic!(
                    "mixed-case SNI should match lowercase cert key, got {}",
                    label(&other)
                ),
            }
        }

        #[test]
        fn sni_with_trailing_dot_matches_certificate_key() {
            let snapshot = projected_snapshot_with_cert("api.example.com");
            match resolve_tls_material(&snapshot, Some("api.example.com.")) {
                TlsResolution::Ready(_) => {}
                other => panic!(
                    "SNI with trailing dot should match cert key, got {}",
                    label(&other)
                ),
            }
        }
    }
}
