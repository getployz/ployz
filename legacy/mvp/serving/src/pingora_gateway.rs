use std::fmt::{self, Debug, Formatter};
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use async_trait::async_trait;
use hyper::http::{Method, Response, StatusCode};
use pingora::apps::{HttpServerApp, http_app::ServeHttp};
use pingora::protocols::http::ServerSession;
use pingora::protocols::raw_connect::ProxyDigest;
use pingora::protocols::{
    GetProxyDigest, GetSocketDigest, GetTimingDigest, Peek, Shutdown, SocketDigest, Ssl,
    TimingDigest, UniqueID, UniqueIDType,
};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt, ReadBuf};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;
use tokio_rustls::LazyConfigAcceptor;
use tokio_rustls::rustls::ServerConfig;
use tokio_rustls::rustls::pki_types::{CertificateDer, PrivateKeyDer, pem::PemObject};

use crate::gateway_request::{acme_http01_token, request_host};
use crate::http_gateway::{
    BackendProxyResponse, HttpGatewayError, HttpGatewayHandle, HttpGatewayResult, parse_backend,
    proxy_get_path,
};
use crate::{WireMetricsRecorder, WireServingState};

const HTTP_CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HTTP_CONNECTIONS: usize = 256;
const MAX_LATENCY_SAMPLES: usize = 256;
static NEXT_TLS_STREAM_ID: AtomicUsize = AtomicUsize::new(1);

pub async fn spawn_pingora_gateway(
    listen_addr: SocketAddr,
    tls_listen_addr: Option<SocketAddr>,
    state: WireServingState,
) -> HttpGatewayResult<HttpGatewayHandle> {
    let listener =
        TcpListener::bind(listen_addr)
            .await
            .map_err(|source| HttpGatewayError::Bind {
                addr: listen_addr,
                source,
            })?;
    let listen_addr = listener.local_addr().map_err(HttpGatewayError::LocalAddr)?;
    let tls_listener = match tls_listen_addr {
        Some(addr) => {
            let listener = TcpListener::bind(addr)
                .await
                .map_err(|source| HttpGatewayError::Bind { addr, source })?;
            Some(listener)
        }
        None => None,
    };
    let tls_listen_addr = tls_listener
        .as_ref()
        .map(TcpListener::local_addr)
        .transpose()
        .map_err(HttpGatewayError::LocalAddr)?;
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let (shutdown_watch_tx, shutdown_watch_rx) = watch::channel(false);
    let metrics = WireMetricsRecorder::new(MAX_LATENCY_SAMPLES);
    let app = Arc::new(PingoraGatewayApp {
        state,
        metrics: metrics.clone(),
    });
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        let mut tls_listener = tls_listener;
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    let _ = shutdown_watch_tx.send(true);
                    connections.abort_all();
                    while connections.join_next().await.is_some() {}
                    return Ok(());
                },
                Some(joined) = connections.join_next(), if !connections.is_empty() => {
                    let _ = joined;
                },
                accepted = async {
                    match tls_listener.as_mut() {
                        Some(listener) => Some(listener.accept().await),
                        None => None,
                    }
                }, if tls_listener.is_some() => {
                    let Some(accepted) = accepted else {
                        continue;
                    };
                    let (stream, _) = accepted.map_err(HttpGatewayError::Accept)?;
                    if connections.len() >= MAX_HTTP_CONNECTIONS {
                        continue;
                    }
                    let app = Arc::clone(&app);
                    let state = app.state.clone();
                    let shutdown_watch = shutdown_watch_rx.clone();
                    connections.spawn(async move {
                        let _ = timeout(
                            HTTP_CONNECTION_TIMEOUT,
                            process_tls_connection(stream, state, app, shutdown_watch),
                        )
                        .await;
                    });
                },
                accepted = listener.accept() => {
                    let (stream, _) = accepted.map_err(HttpGatewayError::Accept)?;
                    if connections.len() >= MAX_HTTP_CONNECTIONS {
                        continue;
                    }
                    let app = Arc::clone(&app);
                    let shutdown_watch = shutdown_watch_rx.clone();
                    connections.spawn(async move {
                        let stream = pingora::protocols::l4::stream::Stream::from(stream);
                        let stream: pingora::protocols::Stream = Box::new(stream);
                        let session = ServerSession::new_http1(stream);
                        let _ = timeout(
                            HTTP_CONNECTION_TIMEOUT,
                            app.process_new_http(session, &shutdown_watch),
                        )
                        .await;
                    });
                }
            }
        }
    });
    Ok(HttpGatewayHandle::from_parts(
        listen_addr,
        tls_listen_addr,
        shutdown_tx,
        task,
        metrics,
    ))
}

async fn process_tls_connection(
    stream: TcpStream,
    state: WireServingState,
    app: Arc<PingoraGatewayApp>,
    shutdown_watch: watch::Receiver<bool>,
) -> HttpGatewayResult<()> {
    let acceptor =
        LazyConfigAcceptor::new(tokio_rustls::rustls::server::Acceptor::default(), stream);
    tokio::pin!(acceptor);
    let handshake = acceptor
        .as_mut()
        .await
        .map_err(|error| HttpGatewayError::TlsConfig(format!("read TLS client hello: {error}")))?;
    let client_hello = handshake.client_hello();
    let server_name = client_hello
        .server_name()
        .ok_or_else(|| HttpGatewayError::TlsConfig("TLS client hello missing SNI".to_string()))?
        .to_string();
    let config = tls_config_for_host(&state, &server_name).await?;
    let stream = handshake
        .into_stream(config)
        .await
        .map_err(|error| HttpGatewayError::TlsConfig(format!("TLS handshake: {error}")))?;
    let stream: pingora::protocols::Stream = Box::new(PingoraTlsStream::new(stream));
    let session = ServerSession::new_http1(stream);
    app.process_new_http(session, &shutdown_watch).await;
    Ok(())
}

async fn tls_config_for_host(
    state: &WireServingState,
    server_name: &str,
) -> HttpGatewayResult<Arc<ServerConfig>> {
    ensure_rustls_crypto_provider();
    let certificate = state
        .certificate_for_host(server_name)
        .await
        .map_err(|error| HttpGatewayError::TlsConfig(format!("load certificate: {error}")))?
        .ok_or_else(|| {
            HttpGatewayError::TlsConfig(format!("no serving certificate for {server_name}"))
        })?;
    let certificates: Vec<CertificateDer<'static>> =
        CertificateDer::pem_slice_iter(certificate.fullchain_pem.as_bytes())
            .collect::<Result<Vec<_>, _>>()
            .map_err(|error| {
                HttpGatewayError::TlsConfig(format!("parse certificate PEM: {error}"))
            })?;
    if certificates.is_empty() {
        return Err(HttpGatewayError::TlsConfig(
            "certificate PEM did not contain any certificates".to_string(),
        ));
    }
    let private_key = PrivateKeyDer::from_pem_slice(certificate.private_key_pem.as_bytes())
        .map_err(|error| HttpGatewayError::TlsConfig(format!("parse private key PEM: {error}")))?;
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certificates, private_key)
        .map_err(|error| HttpGatewayError::TlsConfig(format!("build TLS config: {error}")))?;
    Ok(Arc::new(config))
}

fn ensure_rustls_crypto_provider() {
    let _ = tokio_rustls::rustls::crypto::aws_lc_rs::default_provider().install_default();
}

struct PingoraTlsStream {
    id: UniqueIDType,
    inner: tokio_rustls::server::TlsStream<TcpStream>,
}

impl PingoraTlsStream {
    fn new(inner: tokio_rustls::server::TlsStream<TcpStream>) -> Self {
        Self {
            id: NEXT_TLS_STREAM_ID.fetch_add(1, Ordering::Relaxed) as UniqueIDType,
            inner,
        }
    }
}

impl Debug for PingoraTlsStream {
    fn fmt(&self, formatter: &mut Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PingoraTlsStream")
            .field("id", &self.id)
            .finish_non_exhaustive()
    }
}

impl AsyncRead for PingoraTlsStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_read(cx, buf)
    }
}

impl AsyncWrite for PingoraTlsStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<std::io::Result<usize>> {
        Pin::new(&mut self.inner).poll_write(cx, buf)
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<std::io::Result<()>> {
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[async_trait]
impl Shutdown for PingoraTlsStream {
    async fn shutdown(&mut self) {
        let _ = AsyncWriteExt::shutdown(&mut self.inner).await;
    }
}

impl UniqueID for PingoraTlsStream {
    fn id(&self) -> UniqueIDType {
        self.id
    }
}

impl Ssl for PingoraTlsStream {}

impl GetTimingDigest for PingoraTlsStream {
    fn get_timing_digest(&self) -> Vec<Option<TimingDigest>> {
        Vec::new()
    }
}

impl GetProxyDigest for PingoraTlsStream {
    fn get_proxy_digest(&self) -> Option<Arc<ProxyDigest>> {
        None
    }
}

impl GetSocketDigest for PingoraTlsStream {
    fn get_socket_digest(&self) -> Option<Arc<SocketDigest>> {
        None
    }
}

#[async_trait]
impl Peek for PingoraTlsStream {}

struct PingoraGatewayApp {
    state: WireServingState,
    metrics: WireMetricsRecorder,
}

#[async_trait]
impl ServeHttp for PingoraGatewayApp {
    async fn response(&self, session: &mut ServerSession) -> Response<Vec<u8>> {
        let started = Instant::now();
        let response = response_for_session(session, &self.state, &self.metrics).await;
        self.metrics.record_request(started.elapsed());
        response
    }
}

async fn response_for_session(
    session: &mut ServerSession,
    state: &WireServingState,
    metrics: &WireMetricsRecorder,
) -> Response<Vec<u8>> {
    let request = session.req_header();
    if request.method != Method::GET {
        return text_response(StatusCode::METHOD_NOT_ALLOWED, "method not allowed\n");
    }
    let Some(host) = request_host(&request.headers) else {
        return text_response(StatusCode::BAD_REQUEST, "missing host\n");
    };
    let path = request
        .uri
        .path_and_query()
        .map_or("/", |value| value.as_str());
    if let Some(token) = acme_http01_token(path) {
        return match state.acme_http01_challenge(host.as_str(), token).await {
            Ok(Some(key_authorization)) => {
                text_response(StatusCode::OK, key_authorization.as_str())
            }
            Ok(None) => text_response(StatusCode::NOT_FOUND, "challenge not found\n"),
            Err(error) => text_response(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("serving unavailable: {error}\n"),
            ),
        };
    }
    let route = match state.gateway_route_for_host(host).await {
        Ok(Some(route)) => route,
        Ok(None) => return text_response(StatusCode::NOT_FOUND, "route not found\n"),
        Err(error) => {
            return text_response(
                StatusCode::SERVICE_UNAVAILABLE,
                format!("serving unavailable: {error}\n"),
            );
        }
    };
    let Some(backend) = route.backends.first() else {
        metrics.record_backend_failure();
        return text_response(StatusCode::SERVICE_UNAVAILABLE, "no backend\n");
    };
    let backend = match parse_backend(backend) {
        Ok(backend) => backend,
        Err(message) => {
            metrics.record_backend_failure();
            return text_response(StatusCode::SERVICE_UNAVAILABLE, message);
        }
    };
    match proxy_get_path(path, backend, &route).await {
        Ok(response) => response.into_pingora_response(),
        Err(message) => {
            metrics.record_backend_failure();
            text_response(StatusCode::SERVICE_UNAVAILABLE, message)
        }
    }
}

fn text_response(status: StatusCode, body: impl Into<String>) -> Response<Vec<u8>> {
    let body = body.into().into_bytes();
    Response::builder()
        .status(status)
        .header(hyper::http::header::CONTENT_TYPE, "text/plain")
        .header(hyper::http::header::CONTENT_LENGTH, body.len())
        .body(body)
        .expect("valid pingora text response")
}

impl BackendProxyResponse {
    fn into_pingora_response(self) -> Response<Vec<u8>> {
        Response::builder()
            .status(self.status)
            .header(hyper::http::header::CONTENT_TYPE, "text/plain")
            .header(hyper::http::header::CONTENT_LENGTH, self.body.len())
            .body(self.body.to_vec())
            .expect("valid pingora proxy response")
    }
}

impl HttpGatewayHandle {
    pub(crate) fn from_parts(
        listen_addr: SocketAddr,
        tls_listen_addr: Option<SocketAddr>,
        shutdown: oneshot::Sender<()>,
        task: JoinHandle<HttpGatewayResult<()>>,
        metrics: WireMetricsRecorder,
    ) -> Self {
        Self {
            listen_addr,
            tls_listen_addr,
            shutdown: Some(shutdown),
            task: Some(task),
            metrics,
        }
    }
}
