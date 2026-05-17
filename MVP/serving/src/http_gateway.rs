use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use bytes::Bytes;
use http_body_util::Full;
use hyper::body::Incoming;
use hyper::header::{CONTENT_LENGTH, CONTENT_TYPE, HOST};
use hyper::http::{Method, Request, Response, StatusCode};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper_util::rt::TokioIo;
use mvp_projection::{BackendEndpoint, GatewayRouteProjection};
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::oneshot;
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;

use crate::{WireMetricsRecorder, WireRoleMetrics, WireServingState};

const BACKEND_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const BACKEND_READ_TIMEOUT: Duration = Duration::from_secs(2);
const BACKEND_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const HTTP_CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_BACKEND_RESPONSE_BYTES: usize = 64 * 1024;
const MAX_HTTP_CONNECTIONS: usize = 256;
const MAX_LATENCY_SAMPLES: usize = 256;

pub type HttpGatewayResult<T> = Result<T, HttpGatewayError>;

#[derive(Debug, Error)]
pub enum HttpGatewayError {
    #[error("bind HTTP gateway listener at {addr}: {source}")]
    Bind {
        addr: SocketAddr,
        source: std::io::Error,
    },
    #[error("read HTTP gateway local address: {0}")]
    LocalAddr(std::io::Error),
    #[error("accept HTTP gateway connection: {0}")]
    Accept(std::io::Error),
}

pub struct HttpGatewayHandle {
    listen_addr: SocketAddr,
    shutdown: Option<oneshot::Sender<()>>,
    task: JoinHandle<HttpGatewayResult<()>>,
    metrics: WireMetricsRecorder,
}

impl HttpGatewayHandle {
    #[must_use]
    pub fn listen_addr(&self) -> SocketAddr {
        self.listen_addr
    }

    #[must_use]
    pub fn metrics(&self) -> WireRoleMetrics {
        self.metrics.snapshot()
    }

    pub async fn shutdown(mut self) -> HttpGatewayResult<()> {
        if let Some(shutdown) = self.shutdown.take() {
            let _ = shutdown.send(());
        }
        match self.task.await {
            Ok(result) => result,
            Err(error) if error.is_cancelled() => Ok(()),
            Err(error) => Err(HttpGatewayError::Accept(std::io::Error::other(format!(
                "gateway task join: {error}"
            )))),
        }
    }
}

pub async fn spawn_http_gateway(
    listen_addr: SocketAddr,
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
    let (shutdown_tx, mut shutdown_rx) = oneshot::channel();
    let metrics = WireMetricsRecorder::new(MAX_LATENCY_SAMPLES);
    let server_metrics = metrics.clone();
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
        let state = Arc::new(state);
        loop {
            tokio::select! {
                _ = &mut shutdown_rx => {
                    connections.abort_all();
                    while connections.join_next().await.is_some() {}
                    return Ok(());
                },
                Some(joined) = connections.join_next(), if !connections.is_empty() => {
                    let _ = joined;
                },
                accepted = listener.accept() => {
                    let (stream, _) = accepted.map_err(HttpGatewayError::Accept)?;
                    if connections.len() >= MAX_HTTP_CONNECTIONS {
                        continue;
                    }
                    let io = TokioIo::new(stream);
                    let state = Arc::clone(&state);
                    let metrics = server_metrics.clone();
                    connections.spawn(async move {
                        let service = service_fn(move |request| {
                            handle_request(request, Arc::clone(&state), metrics.clone())
                        });
                        let _ = timeout(
                            HTTP_CONNECTION_TIMEOUT,
                            http1::Builder::new().serve_connection(io, service),
                        )
                        .await;
                    });
                }
            }
        }
    });
    Ok(HttpGatewayHandle {
        listen_addr,
        shutdown: Some(shutdown_tx),
        task,
        metrics,
    })
}

async fn handle_request(
    request: Request<Incoming>,
    state: Arc<WireServingState>,
    metrics: WireMetricsRecorder,
) -> Result<Response<Full<Bytes>>, Infallible> {
    let started = Instant::now();
    let response = match response_for_request(request, state.as_ref(), &metrics).await {
        Ok(response) => response,
        Err(response) => response,
    };
    metrics.record_request(started.elapsed());
    Ok(response)
}

async fn response_for_request(
    request: Request<Incoming>,
    state: &WireServingState,
    metrics: &WireMetricsRecorder,
) -> Result<Response<Full<Bytes>>, Response<Full<Bytes>>> {
    if request.method() != Method::GET {
        return Err(text_response(
            StatusCode::METHOD_NOT_ALLOWED,
            "method not allowed\n",
        ));
    }
    let Some(host) = request_host(&request) else {
        return Err(text_response(StatusCode::BAD_REQUEST, "missing host\n"));
    };
    let route = state.gateway_route_for_host(host).await.map_err(|error| {
        text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            format!("serving unavailable: {error}\n"),
        )
    })?;
    let Some(route) = route else {
        return Err(text_response(StatusCode::NOT_FOUND, "route not found\n"));
    };
    let Some(backend) = route.backends.first() else {
        metrics.record_backend_failure();
        return Err(text_response(
            StatusCode::SERVICE_UNAVAILABLE,
            "no backend\n",
        ));
    };
    let backend = match parse_backend(backend) {
        Ok(backend) => backend,
        Err(message) => {
            metrics.record_backend_failure();
            return Err(text_response(StatusCode::SERVICE_UNAVAILABLE, message));
        }
    };
    match proxy_get(&request, backend, &route).await {
        Ok(response) => Ok(response),
        Err(message) => {
            metrics.record_backend_failure();
            Err(text_response(StatusCode::SERVICE_UNAVAILABLE, message))
        }
    }
}

fn request_host(request: &Request<Incoming>) -> Option<String> {
    request
        .headers()
        .get(HOST)
        .and_then(|value| value.to_str().ok())
        .map(strip_port)
        .filter(|value| !value.trim().is_empty())
}

fn strip_port(value: &str) -> String {
    let value = value.trim();
    if value.starts_with('[') {
        return value
            .split(']')
            .next()
            .map_or(value, |host| host.trim_start_matches('['))
            .to_string();
    }
    value.split(':').next().unwrap_or(value).to_string()
}

fn parse_backend(backend: &BackendEndpoint) -> Result<SocketAddr, String> {
    let address = backend.address.trim();
    let addr = address
        .parse::<SocketAddr>()
        .map_err(|error| format!("invalid backend address '{address}': {error}\n"))?;
    if !addr.ip().is_loopback() {
        return Err(format!("backend address '{address}' is not loopback\n"));
    }
    Ok(addr)
}

async fn proxy_get(
    request: &Request<Incoming>,
    backend: SocketAddr,
    route: &GatewayRouteProjection,
) -> Result<Response<Full<Bytes>>, String> {
    let mut stream = timeout(BACKEND_CONNECT_TIMEOUT, TcpStream::connect(backend))
        .await
        .map_err(|_| format!("connect backend {backend} timed out\n"))?
        .map_err(|error| format!("connect backend {backend}: {error}\n"))?;
    let path = request
        .uri()
        .path_and_query()
        .map_or("/", |value| value.as_str());
    let upstream = format!(
        "GET {path} HTTP/1.1\r\nHost: {backend}\r\nConnection: close\r\nX-Ployz-Route: {}\r\n\r\n",
        route.route_id.as_str()
    );
    timeout(BACKEND_WRITE_TIMEOUT, stream.write_all(upstream.as_bytes()))
        .await
        .map_err(|_| format!("write backend {backend} timed out\n"))?
        .map_err(|error| format!("write backend {backend}: {error}\n"))?;
    let mut bytes = Vec::new();
    timeout(
        BACKEND_READ_TIMEOUT,
        read_limited(&mut stream, &mut bytes, MAX_BACKEND_RESPONSE_BYTES),
    )
    .await
    .map_err(|_| format!("read backend {backend} timed out\n"))?
    .map_err(|error| format!("read backend {backend}: {error}\n"))?;
    parse_backend_response(bytes)
}

async fn read_limited(
    stream: &mut TcpStream,
    bytes: &mut Vec<u8>,
    limit: usize,
) -> std::io::Result<()> {
    let mut chunk = [0_u8; 4096];
    loop {
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Ok(());
        }
        if bytes.len() + read > limit {
            return Err(std::io::Error::other("backend response exceeds limit"));
        }
        bytes.extend_from_slice(&chunk[..read]);
    }
}

fn parse_backend_response(bytes: Vec<u8>) -> Result<Response<Full<Bytes>>, String> {
    let split = bytes
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .ok_or_else(|| "backend response missing header terminator\n".to_string())?;
    let (header_bytes, body_bytes) = bytes.split_at(split + 4);
    let header = String::from_utf8_lossy(header_bytes);
    let status = header
        .lines()
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|value| value.parse::<u16>().ok())
        .and_then(|status| StatusCode::from_u16(status).ok())
        .unwrap_or(StatusCode::BAD_GATEWAY);
    let body = Bytes::copy_from_slice(body_bytes);
    Ok(Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain")
        .header(CONTENT_LENGTH, body.len())
        .body(Full::new(body))
        .expect("valid backend proxy response"))
}

fn text_response(status: StatusCode, body: impl Into<String>) -> Response<Full<Bytes>> {
    let body = body.into();
    Response::builder()
        .status(status)
        .header(CONTENT_TYPE, "text/plain")
        .header(CONTENT_LENGTH, body.len())
        .body(Full::new(Bytes::from(body)))
        .expect("valid text response")
}
