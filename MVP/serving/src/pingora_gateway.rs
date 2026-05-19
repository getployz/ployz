use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use async_trait::async_trait;
use hyper::http::{Method, Response, StatusCode};
use pingora::apps::{HttpServerApp, http_app::ServeHttp};
use pingora::protocols::http::ServerSession;
use tokio::net::TcpListener;
use tokio::sync::{oneshot, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::timeout;

use crate::gateway_request::{acme_http01_token, request_host};
use crate::http_gateway::{
    BackendProxyResponse, HttpGatewayError, HttpGatewayHandle, HttpGatewayResult, parse_backend,
    proxy_get_path,
};
use crate::{WireMetricsRecorder, WireServingState};

const HTTP_CONNECTION_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_HTTP_CONNECTIONS: usize = 256;
const MAX_LATENCY_SAMPLES: usize = 256;
pub async fn spawn_pingora_gateway(
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
    let (shutdown_watch_tx, shutdown_watch_rx) = watch::channel(false);
    let metrics = WireMetricsRecorder::new(MAX_LATENCY_SAMPLES);
    let app = Arc::new(PingoraGatewayApp {
        state,
        metrics: metrics.clone(),
    });
    let task = tokio::spawn(async move {
        let mut connections = JoinSet::new();
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
        shutdown_tx,
        task,
        metrics,
    ))
}

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
        shutdown: oneshot::Sender<()>,
        task: JoinHandle<HttpGatewayResult<()>>,
        metrics: WireMetricsRecorder,
    ) -> Self {
        Self {
            listen_addr,
            shutdown: Some(shutdown),
            task: Some(task),
            metrics,
        }
    }
}
