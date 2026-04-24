use std::sync::Arc;

use async_trait::async_trait;
use pingora::listeners::{TlsAccept, tls::TlsSettings};
use pingora::prelude::*;
use pingora::protocols::tls::TlsRef;
#[cfg(unix)]
use pingora::server::{RunArgs, ShutdownSignal, ShutdownSignalWatch};
use pingora::services::listening::Service as ListeningService;
use pingora::tls::{ext, pkey::PKey, ssl, x509::X509};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tracing::info;

use crate::config::{GatewayConfig, GatewayError};
use crate::proxy::GatewayApp;
use crate::snapshot::SharedSnapshot;
use crate::sync::load_projected_snapshot_from_store;

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
        let Some(server_name) = ssl.servername(ssl::NameType::HOST_NAME) else {
            return;
        };
        let state = self.shared_snapshot.load();
        let Some(certificate) = state
            .snapshot
            .certificates
            .iter()
            .find(|certificate| certificate.hostname == server_name)
        else {
            return;
        };
        let Ok(certificates) = X509::stack_from_pem(certificate.fullchain_pem.as_bytes()) else {
            return;
        };
        let [leaf, chain @ ..] = certificates.as_slice() else {
            return;
        };
        let Ok(private_key) = PKey::private_key_from_pem(certificate.private_key_pem.as_bytes())
        else {
            return;
        };
        if ext::ssl_use_certificate(ssl, leaf).is_err() {
            return;
        }
        if ext::ssl_use_private_key(ssl, &private_key).is_err() {
            return;
        }
        for certificate in chain {
            if ext::ssl_add_chain_cert(ssl, certificate).is_err() {
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
        gateway_tls_listener(&config),
        config.threads,
        config.metrics_listen_addr.as_deref(),
        shared_snapshot,
        None,
    )
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
    use crate::routes::{BackendView, GatewaySnapshot, HttpRouteView};
    use pingora::prelude::Opt;
    use ployz_types::model::{InstanceId, MachineId};
    use ployz_types::spec::Namespace;
    use std::io::{Read, Write};
    use std::net::{SocketAddr, TcpListener};
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
            acme_challenges: Vec::new(),
            certificates: Vec::new(),
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

    async fn wait_for_metrics_listener(addr: SocketAddr) {
        for _ in 0..50 {
            if tokio::net::TcpStream::connect(addr).await.is_ok() {
                return;
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for listener {addr}");
    }

    async fn send_http_request(addr: SocketAddr, host: Option<&str>) -> String {
        let mut stream = tokio::net::TcpStream::connect(addr)
            .await
            .expect("http connection should succeed");
        let host = host.unwrap_or("127.0.0.1");
        let request = format!("GET / HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
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
