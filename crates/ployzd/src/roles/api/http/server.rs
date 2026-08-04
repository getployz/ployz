//! HTTP/1, JSON, and SSE serving for the API role.

use std::convert::Infallible;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::stream;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::Frame;
use hyper::header::{ALLOW, CONTENT_TYPE, HeaderValue, RETRY_AFTER};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use ployz_core::ids::ClusterId;
use ployz_core::{
    ApiFeature, ApiRefusal, ApiVersion, KNOWN_API_FEATURES, LensCollection, LensSnapshot,
    LensWatchEvent, V2Route,
};
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior};

use super::config::{ApiRoleConfig, ApiRoleConfigError};
use super::roster::{
    ApiListenerValidationError, corrosion_unavailable_refusal, resolve_peer_principal,
    validate_listener_identity,
};
use crate::corrosion::CorrosionClient;
use crate::roles::api::lenses::{LensEngineConfig, LensRecoveryPolicy, LensWatch, start_lens};

const LENS_INITIAL_WAIT: Duration = Duration::from_secs(15);
const SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const SERVER_SHUTDOWN_GRACE: Duration = Duration::from_secs(8);
const HTTP_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
const LENS_RECOVERY_MAX_ATTEMPTS: u32 = 5;
const LENS_RECOVERY_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const LENS_RECOVERY_MAX_BACKOFF: Duration = Duration::from_secs(1);

pub(super) type HttpBody = BoxBody<Bytes, Infallible>;
pub(super) type LensState = Result<LensSnapshot, ApiRefusal>;

fn json_response(status: StatusCode, body: Vec<u8>) -> Response<HttpBody> {
    let mut response = Response::new(Full::new(Bytes::from(body)).boxed());
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

fn sse_response(body: HttpBody) -> Response<HttpBody> {
    let mut response = Response::new(body);
    response.headers_mut().insert(
        CONTENT_TYPE,
        HeaderValue::from_static("text/event-stream; charset=utf-8"),
    );
    response.headers_mut().insert(
        hyper::header::CACHE_CONTROL,
        HeaderValue::from_static("no-cache"),
    );
    response
}

pub(super) fn sse_data(json: &[u8]) -> Bytes {
    let mut frame = Vec::with_capacity(json.len() + 7);
    frame.extend_from_slice(b"data: ");
    frame.extend_from_slice(json);
    frame.extend_from_slice(b"\n\n");
    Bytes::from(frame)
}

pub(super) fn sse_keepalive() -> Bytes {
    Bytes::from_static(b": keepalive\n\n")
}

pub(super) fn version_response(build: &str) -> Response<HttpBody> {
    let version = ApiVersion::new(
        build,
        KNOWN_API_FEATURES.iter().copied().map(ApiFeature::from),
    );
    match serde_json::to_vec(&version) {
        Ok(body) => json_response(StatusCode::OK, body),
        Err(error) => {
            tracing::error!(error = %error, "could not encode API version response");
            corrosion_unavailable_response()
        }
    }
}

pub(super) fn refusal_response(refusal: ApiRefusal) -> Response<HttpBody> {
    let status = refusal_status(&refusal);
    let retry_after = match &refusal {
        ApiRefusal::CorrosionUnavailable {
            retry_after_seconds,
        } => Some(*retry_after_seconds),
        ApiRefusal::UnknownSource { .. }
        | ApiRefusal::AmbiguousSource { .. }
        | ApiRefusal::UnsupportedRoute
        | ApiRefusal::UnsupportedMethod { .. }
        | ApiRefusal::MissingCluster
        | ApiRefusal::InvalidCluster => None,
    };
    let unsupported_method = matches!(&refusal, ApiRefusal::UnsupportedMethod { .. });
    let body = match serde_json::to_vec(&refusal) {
        Ok(body) => body,
        Err(error) => {
            tracing::error!(error = %error, "could not encode API refusal response");
            return corrosion_unavailable_response();
        }
    };
    let mut response = json_response(status, body);
    if unsupported_method {
        response
            .headers_mut()
            .insert(ALLOW, HeaderValue::from_static("GET"));
    }
    if let Some(retry_after) = retry_after
        && let Ok(value) = HeaderValue::from_str(&retry_after.get().to_string())
    {
        response.headers_mut().insert(RETRY_AFTER, value);
    }
    response
}

fn corrosion_unavailable_response() -> Response<HttpBody> {
    let refusal = corrosion_unavailable_refusal();
    let body = match serde_json::to_vec(&refusal) {
        Ok(body) => body,
        Err(error) => {
            tracing::error!(error = %error, "could not encode fallback API refusal response");
            b"{\"kind\":\"corrosion_unavailable\",\"retry_after_seconds\":1}".to_vec()
        }
    };
    let mut response = json_response(StatusCode::SERVICE_UNAVAILABLE, body);
    response
        .headers_mut()
        .insert(RETRY_AFTER, HeaderValue::from_static("1"));
    response
}

fn refusal_status(refusal: &ApiRefusal) -> StatusCode {
    match refusal {
        ApiRefusal::UnknownSource { .. } | ApiRefusal::AmbiguousSource { .. } => {
            StatusCode::FORBIDDEN
        }
        ApiRefusal::UnsupportedRoute => StatusCode::NOT_FOUND,
        ApiRefusal::UnsupportedMethod { .. } => StatusCode::METHOD_NOT_ALLOWED,
        ApiRefusal::MissingCluster
        | ApiRefusal::InvalidCluster
        | ApiRefusal::CorrosionUnavailable { .. } => StatusCode::SERVICE_UNAVAILABLE,
    }
}

pub(super) fn sse_event(event: &LensWatchEvent) -> Result<Bytes, serde_json::Error> {
    let json = serde_json::to_vec(event)?;
    let event_name = event.event_name();
    let data = sse_data(&json);
    let mut frame = Vec::with_capacity(event_name.len() + data.len() + 8);
    frame.extend_from_slice(b"event: ");
    frame.extend_from_slice(event_name.as_bytes());
    frame.push(b'\n');
    frame.extend_from_slice(&data);
    Ok(Bytes::from(frame))
}

pub(super) fn parse_get_route(method: &Method, path: &str) -> Result<V2Route, ApiRefusal> {
    if method != Method::GET {
        return Err(ApiRefusal::UnsupportedMethod {
            method: method.as_str().to_owned(),
        });
    }
    V2Route::parse(path).ok_or(ApiRefusal::UnsupportedRoute)
}

pub(super) fn source_from_peer<Body>(peer: SocketAddr, _request: &hyper::Request<Body>) -> IpAddr {
    peer.ip()
}

struct ApiLenses {
    machines: LensWatch,
    services: LensWatch,
    containers: LensWatch,
    machine_status: LensWatch,
    operations: LensWatch,
}

impl ApiLenses {
    fn start(corrosion: CorrosionClient, cluster_id: ClusterId) -> Self {
        let config = lens_engine_config(cluster_id);
        Self {
            machines: start_lens(corrosion.clone(), LensCollection::Machines, config.clone()),
            services: start_lens(corrosion.clone(), LensCollection::Services, config.clone()),
            containers: start_lens(
                corrosion.clone(),
                LensCollection::Containers,
                config.clone(),
            ),
            machine_status: start_lens(
                corrosion.clone(),
                LensCollection::MachineStatus,
                config.clone(),
            ),
            operations: start_lens(corrosion, LensCollection::Operations, config),
        }
    }

    fn watch(&self, collection: LensCollection) -> &LensWatch {
        match collection {
            LensCollection::Machines => &self.machines,
            LensCollection::Services => &self.services,
            LensCollection::Containers => &self.containers,
            LensCollection::MachineStatus => &self.machine_status,
            LensCollection::Operations => &self.operations,
        }
    }

    async fn shutdown(self) {
        let Self {
            machines,
            services,
            containers,
            machine_status,
            operations,
        } = self;
        tokio::join!(
            machines.shutdown(),
            services.shutdown(),
            containers.shutdown(),
            machine_status.shutdown(),
            operations.shutdown(),
        );
    }
}

fn lens_engine_config(cluster_id: ClusterId) -> LensEngineConfig {
    let Some(max_attempts) = NonZeroU32::new(LENS_RECOVERY_MAX_ATTEMPTS) else {
        unreachable!("the fixed lens recovery attempt count is nonzero");
    };
    let recovery = LensRecoveryPolicy::try_new(
        max_attempts,
        LENS_RECOVERY_INITIAL_BACKOFF,
        LENS_RECOVERY_MAX_BACKOFF,
    )
    .expect("fixed lens recovery policy is valid");
    LensEngineConfig::new(cluster_id, recovery)
}

struct ApiService {
    corrosion: CorrosionClient,
    cluster_id: ClusterId,
    build: String,
    lenses: Arc<ApiLenses>,
}

impl ApiService {
    async fn handle(
        &self,
        peer: SocketAddr,
        request: hyper::Request<hyper::body::Incoming>,
        shutdown: watch::Receiver<bool>,
    ) -> Response<HttpBody> {
        let _principal = match resolve_peer_principal(
            &self.corrosion,
            &self.cluster_id,
            source_from_peer(peer, &request),
        )
        .await
        {
            Ok(principal) => principal,
            Err(refusal) => return refusal_response(refusal),
        };
        let route = match parse_get_route(request.method(), request.uri().path()) {
            Ok(route) => route,
            Err(refusal) => return refusal_response(refusal),
        };

        match route {
            V2Route::Version => version_response(&self.build),
            V2Route::Lens(collection) => self.snapshot_response(collection).await,
            V2Route::LensWatch(collection) => self.watch_response(collection, shutdown).await,
        }
    }

    async fn snapshot_response(&self, collection: LensCollection) -> Response<HttpBody> {
        let mut updates = self.lenses.watch(collection).subscribe();
        let state =
            match tokio::time::timeout(LENS_INITIAL_WAIT, await_lens_state(&mut updates)).await {
                Ok(Ok(state)) => state,
                Ok(Err(refusal)) => return refusal_response(refusal),
                Err(_) => return refusal_response(corrosion_unavailable_refusal()),
            };
        lens_snapshot_response(state)
    }

    async fn watch_response(
        &self,
        collection: LensCollection,
        shutdown: watch::Receiver<bool>,
    ) -> Response<HttpBody> {
        let mut updates = self.lenses.watch(collection).subscribe();
        let initial =
            match tokio::time::timeout(LENS_INITIAL_WAIT, await_lens_state(&mut updates)).await {
                Ok(Ok(state)) => initial_watch_event(state),
                Ok(Err(refusal)) => return refusal_response(refusal),
                Err(_) => return refusal_response(corrosion_unavailable_refusal()),
            };
        sse_response(sse_watch_body(updates, initial, shutdown))
    }
}

pub(super) fn lens_snapshot_response(
    state: Result<LensSnapshot, ApiRefusal>,
) -> Response<HttpBody> {
    match state {
        Ok(snapshot) => match serde_json::to_vec(&snapshot) {
            Ok(body) => json_response(StatusCode::OK, body),
            Err(error) => {
                tracing::error!(error = %error, "could not encode lens snapshot response");
                corrosion_unavailable_response()
            }
        },
        Err(refusal) => refusal_response(refusal),
    }
}

pub(super) async fn await_lens_state(
    updates: &mut watch::Receiver<Option<LensState>>,
) -> Result<LensState, ApiRefusal> {
    loop {
        if let Some(state) = updates.borrow_and_update().clone() {
            return Ok(state);
        }
        if updates.changed().await.is_err() {
            return Err(corrosion_unavailable_refusal());
        }
    }
}

pub(super) fn initial_watch_event(state: Result<LensSnapshot, ApiRefusal>) -> LensWatchEvent {
    match state {
        Ok(snapshot) => LensWatchEvent::snapshot(snapshot),
        Err(refusal) => LensWatchEvent::terminal(refusal),
    }
}

pub(super) fn subsequent_watch_event(state: Result<LensSnapshot, ApiRefusal>) -> LensWatchEvent {
    match state {
        Ok(snapshot) => LensWatchEvent::state(snapshot),
        Err(refusal) => LensWatchEvent::terminal(refusal),
    }
}

struct SseWatchState {
    updates: watch::Receiver<Option<LensState>>,
    initial: Option<LensWatchEvent>,
    shutdown: watch::Receiver<bool>,
    keepalive: tokio::time::Interval,
    terminal_sent: bool,
}

pub(super) fn sse_watch_body(
    updates: watch::Receiver<Option<LensState>>,
    initial: LensWatchEvent,
    shutdown: watch::Receiver<bool>,
) -> HttpBody {
    let mut keepalive = tokio::time::interval_at(
        Instant::now() + SSE_KEEPALIVE_INTERVAL,
        SSE_KEEPALIVE_INTERVAL,
    );
    keepalive.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let stream = stream::unfold(
        SseWatchState {
            updates,
            initial: Some(initial),
            shutdown,
            keepalive,
            terminal_sent: false,
        },
        |mut state| async move {
            if state.terminal_sent {
                return None;
            }
            if let Some(event) = state.initial.take() {
                state.terminal_sent = matches!(&event, LensWatchEvent::Terminal { .. });
                return Some((
                    Ok::<_, Infallible>(Frame::data(encoded_sse_event(&event))),
                    state,
                ));
            }

            tokio::select! {
                changed = state.updates.changed() => {
                    if changed.is_err() {
                        return None;
                    }
                    let Some(lens_state) = state.updates.borrow_and_update().clone() else {
                        return Some((Ok(Frame::data(sse_keepalive())), state));
                    };
                    let event = subsequent_watch_event(lens_state);
                    state.terminal_sent = matches!(&event, LensWatchEvent::Terminal { .. });
                    Some((Ok(Frame::data(encoded_sse_event(&event))), state))
                }
                changed = state.shutdown.changed() => {
                    match changed {
                        Ok(()) if *state.shutdown.borrow() => None,
                        Ok(()) => Some((Ok(Frame::data(sse_keepalive())), state)),
                        Err(_) => None,
                    }
                }
                _ = state.keepalive.tick() => Some((Ok(Frame::data(sse_keepalive())), state)),
            }
        },
    );
    StreamBody::new(stream).boxed()
}

fn encoded_sse_event(event: &LensWatchEvent) -> Bytes {
    match sse_event(event) {
        Ok(frame) => frame,
        Err(error) => {
            tracing::error!(error = %error, "could not encode lens SSE event");
            let terminal = LensWatchEvent::terminal(corrosion_unavailable_refusal());
            match sse_event(&terminal) {
                Ok(frame) => frame,
                Err(fallback_error) => {
                    tracing::error!(error = %fallback_error, "could not encode fallback lens SSE event");
                    fallback_terminal_sse_event()
                }
            }
        }
    }
}

pub(super) fn fallback_terminal_sse_event() -> Bytes {
    Bytes::from_static(
        b"event: terminal\ndata: {\"kind\":\"terminal\",\"refusal\":{\"kind\":\"corrosion_unavailable\",\"retry_after_seconds\":1}}\n\n",
    )
}

/// A bound public API listener and its owned lens tasks.
pub struct ApiServer {
    listener: TcpListener,
    service: Arc<ApiService>,
    lenses: Arc<ApiLenses>,
}

impl ApiServer {
    /// Validates the configured mesh listener and starts one local lens per
    /// public collection.
    pub async fn bind(config: ApiRoleConfig) -> Result<Self, ApiServerError> {
        let listen_addr = config.listen_addr();
        let (corrosion, cluster_id, build) = Self::validate_configuration(config).await?;
        let listener =
            TcpListener::bind(listen_addr)
                .await
                .map_err(|source| ApiServerError::Bind {
                    listen_addr,
                    source,
                })?;
        Ok(Self::from_validated_listener(
            listener, corrosion, cluster_id, build,
        ))
    }

    async fn validate_configuration(
        config: ApiRoleConfig,
    ) -> Result<(CorrosionClient, ClusterId, String), ApiServerError> {
        let corrosion = CorrosionClient::new(config.corrosion().clone())
            .map_err(ApiServerError::CorrosionClientConfiguration)?;
        validate_listener_identity(
            &corrosion,
            config.cluster_id(),
            config.local_machine_id(),
            config.listen_addr(),
        )
        .await
        .map_err(ApiServerError::ListenerIdentity)?;
        Ok((
            corrosion,
            config.cluster_id().clone(),
            config.build().to_owned(),
        ))
    }

    fn from_validated_listener(
        listener: TcpListener,
        corrosion: CorrosionClient,
        cluster_id: ClusterId,
        build: String,
    ) -> Self {
        let lenses = Arc::new(ApiLenses::start(corrosion.clone(), cluster_id.clone()));
        let service = Arc::new(ApiService {
            corrosion,
            cluster_id,
            build,
            lenses: Arc::clone(&lenses),
        });
        Self {
            listener,
            service,
            lenses,
        }
    }

    /// Serves accepted TCP peers until the caller requests controlled shutdown.
    pub async fn serve<Shutdown>(self, shutdown: Shutdown)
    where
        Shutdown: Future<Output = ()> + Send,
    {
        let Self {
            listener,
            service,
            lenses,
        } = self;
        let (shutdown_tx, _) = watch::channel(false);
        let mut connections = JoinSet::new();
        tokio::pin!(shutdown);

        loop {
            tokio::select! {
                () = &mut shutdown => {
                    let _ = shutdown_tx.send(true);
                    break;
                }
                accepted = listener.accept() => match accepted {
                    Ok((stream, peer)) => {
                        let service = Arc::clone(&service);
                        let shutdown = shutdown_tx.subscribe();
                        connections.spawn(async move {
                            serve_connection(stream, peer, service, shutdown).await;
                        });
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "API listener accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                },
                Some(result) = connections.join_next(), if !connections.is_empty() => {
                    if let Err(error) = result {
                        tracing::warn!(error = %error, "API connection task failed");
                    }
                }
            }
        }

        if tokio::time::timeout(SERVER_SHUTDOWN_GRACE, drain_connections(&mut connections))
            .await
            .is_err()
        {
            connections.abort_all();
            drain_connections(&mut connections).await;
        }
        drop(service);
        match Arc::try_unwrap(lenses) {
            Ok(lenses) => lenses.shutdown().await,
            Err(_) => tracing::warn!("API lenses retained a connection reference during shutdown"),
        }
    }
}

async fn drain_connections(connections: &mut JoinSet<()>) {
    while let Some(result) = connections.join_next().await {
        if let Err(error) = result {
            tracing::warn!(error = %error, "API connection task failed during shutdown");
        }
    }
}

async fn serve_connection(
    stream: tokio::net::TcpStream,
    peer: SocketAddr,
    service: Arc<ApiService>,
    mut shutdown: watch::Receiver<bool>,
) {
    let service_for_requests = Arc::clone(&service);
    let shutdown_for_requests = shutdown.clone();
    let mut builder = http1::Builder::new();
    builder
        .timer(TokioTimer::new())
        .header_read_timeout(HTTP_HEADER_READ_TIMEOUT);
    let connection = builder.serve_connection(
        TokioIo::new(stream),
        service_fn(move |request| {
            let service = Arc::clone(&service_for_requests);
            let shutdown = shutdown_for_requests.clone();
            async move { Ok::<_, Infallible>(service.handle(peer, request, shutdown).await) }
        }),
    );
    tokio::pin!(connection);
    tokio::select! {
        result = &mut connection => log_connection_result(result),
        changed = shutdown.changed() => {
            if changed.is_ok() && *shutdown.borrow() {
                connection.as_mut().graceful_shutdown();
                log_connection_result(connection.await);
            }
        }
    }
}

fn log_connection_result(result: Result<(), hyper::Error>) {
    if let Err(error) = result {
        tracing::debug!(error = %error, "API HTTP/1 connection ended");
    }
}

/// Runs the API role using its supervisor-loaded environment file.
pub async fn run_from_environment() -> Result<(), ApiRoleRuntimeError> {
    let config = ApiRoleConfig::from_environment().map_err(ApiRoleRuntimeError::Configuration)?;
    let server = ApiServer::bind(config)
        .await
        .map_err(ApiRoleRuntimeError::Server)?;
    server.serve(wait_for_process_shutdown()).await;
    Ok(())
}

async fn wait_for_process_shutdown() {
    #[cfg(unix)]
    {
        let terminate = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate());
        let interrupt = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt());
        match (terminate, interrupt) {
            (Ok(mut terminate), Ok(mut interrupt)) => {
                tokio::select! {
                    _ = terminate.recv() => {}
                    _ = interrupt.recv() => {}
                }
            }
            (Err(error), _) | (_, Err(error)) => {
                tracing::warn!(error = %error, "could not install API shutdown signal handler");
                if let Err(error) = tokio::signal::ctrl_c().await {
                    tracing::warn!(error = %error, "could not wait for API shutdown signal");
                }
            }
        }
    }
    #[cfg(not(unix))]
    {
        if let Err(error) = tokio::signal::ctrl_c().await {
            tracing::warn!(error = %error, "could not wait for API shutdown signal");
        }
    }
}

/// A bounded API role startup failure.
#[derive(Debug, thiserror::Error)]
pub enum ApiServerError {
    #[error("could not build the local Corrosion client: {0}")]
    CorrosionClientConfiguration(crate::corrosion::CorrosionClientConfigError),
    #[error(transparent)]
    ListenerIdentity(ApiListenerValidationError),
    #[error("could not bind API listener {listen_addr}: {source}")]
    Bind {
        listen_addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
}

/// A bounded API role process startup failure.
#[derive(Debug, thiserror::Error)]
pub enum ApiRoleRuntimeError {
    #[error(transparent)]
    Configuration(ApiRoleConfigError),
    #[error(transparent)]
    Server(ApiServerError),
}
