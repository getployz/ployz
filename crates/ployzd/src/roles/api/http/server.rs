//! HTTP/1, JSON, and SSE serving for the API role.

use std::convert::Infallible;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::path::PathBuf;
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
use ployz_core::ids::{ClusterId, MachineRowId};
use ployz_core::join::{JoinDoorMaterial, JoinMachineSubstrate};
use ployz_core::{
    ApiFeature, ApiRefusal, ApiVersion, FOUNDING_ROUTE, KNOWN_API_FEATURES, LensCollection,
    LensSnapshot, LensWatchEvent, V2Method, V2Route,
};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, OnceCell, Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior};

use super::config::{ApiRoleConfig, ApiRoleConfigError, ApiRoleMode};
use super::door::{
    JoinDoorBindError, JoinDoorListener, accept_join_connection, serve_join_connection,
};
use super::endpoint_network::{self, EndpointNetworkFoldError};
use super::roster::{
    ApiListenerValidationError, corrosion_unavailable_refusal, resolve_peer_principal,
    validate_listener_identity,
};
use crate::corrosion::CorrosionClient;
use crate::roles::api::lenses::{
    LensEngineConfig, LensRecoveryPolicy, LensWatch, start_lens_with_lifecycle,
};

const LENS_INITIAL_WAIT: Duration = Duration::from_secs(15);
const SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const SERVER_SHUTDOWN_GRACE: Duration = Duration::from_secs(8);
const HTTP_HEADER_READ_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_JOIN_SUBSTRATE_BYTES: usize = 1024 * 1024;
const MAX_JOIN_DOOR_CONNECTIONS: usize = 256;
const LENS_RECOVERY_MAX_ATTEMPTS: u32 = 5;
const LENS_RECOVERY_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const LENS_RECOVERY_MAX_BACKOFF: Duration = Duration::from_secs(1);

pub(super) type HttpBody = BoxBody<Bytes, Infallible>;
pub(super) type LensState = Result<LensSnapshot, ApiRefusal>;

pub(super) fn json_response(status: StatusCode, body: Vec<u8>) -> Response<HttpBody> {
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
    refusal_response_with_allow(refusal, Some(V2Method::Get))
}

pub(super) fn refusal_response_with_allow(
    refusal: ApiRefusal,
    allow: Option<V2Method>,
) -> Response<HttpBody> {
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
    let body = match serde_json::to_vec(&refusal) {
        Ok(body) => body,
        Err(error) => {
            tracing::error!(error = %error, "could not encode API refusal response");
            return corrosion_unavailable_response();
        }
    };
    let mut response = json_response(status, body);
    if matches!(&refusal, ApiRefusal::UnsupportedMethod { .. })
        && let Some(method) = allow
    {
        response.headers_mut().insert(
            ALLOW,
            HeaderValue::from_static(match method {
                V2Method::Get => "GET",
                V2Method::Post => "POST",
            }),
        );
    }
    if let Some(retry_after) = retry_after
        && let Ok(value) = HeaderValue::from_str(&retry_after.get().to_string())
    {
        response.headers_mut().insert(RETRY_AFTER, value);
    }
    response
}

pub(super) fn corrosion_unavailable_response() -> Response<HttpBody> {
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

pub(super) fn parse_route(method: &Method, path: &str) -> Result<V2Route, RouteRefusal> {
    let Some(route) = V2Route::parse(path) else {
        return Err(RouteRefusal {
            refusal: ApiRefusal::UnsupportedRoute,
            allow: None,
        });
    };
    let expected = route.method();
    let accepted = match expected {
        V2Method::Get => method == Method::GET,
        V2Method::Post => method == Method::POST,
    };
    if !accepted {
        return Err(RouteRefusal {
            refusal: ApiRefusal::UnsupportedMethod {
                method: method.as_str().to_owned(),
            },
            allow: Some(expected),
        });
    }
    Ok(route)
}

pub(super) struct RouteRefusal {
    pub(super) refusal: ApiRefusal,
    pub(super) allow: Option<V2Method>,
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
    fn start(
        corrosion: CorrosionClient,
        cluster_id: ClusterId,
        lifecycle_sender: mpsc::UnboundedSender<LensCollection>,
    ) -> Self {
        let config = lens_engine_config(cluster_id);
        Self {
            machines: start_lens_with_lifecycle(
                corrosion.clone(),
                LensCollection::Machines,
                config.clone(),
                lifecycle_sender.clone(),
            ),
            services: start_lens_with_lifecycle(
                corrosion.clone(),
                LensCollection::Services,
                config.clone(),
                lifecycle_sender.clone(),
            ),
            containers: start_lens_with_lifecycle(
                corrosion.clone(),
                LensCollection::Containers,
                config.clone(),
                lifecycle_sender.clone(),
            ),
            machine_status: start_lens_with_lifecycle(
                corrosion.clone(),
                LensCollection::MachineStatus,
                config.clone(),
                lifecycle_sender.clone(),
            ),
            operations: start_lens_with_lifecycle(
                corrosion,
                LensCollection::Operations,
                config,
                lifecycle_sender,
            ),
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

pub(super) struct ApiService {
    pub(super) corrosion: CorrosionClient,
    pub(super) cluster_id: ClusterId,
    pub(super) local_machine_id: MachineRowId,
    pub(super) listen_addr: SocketAddr,
    pub(super) door_material: Option<Arc<JoinDoorMaterial>>,
    pub(super) join_substrate: Option<Arc<JoinMachineSubstrate>>,
    pub(super) corrosion_gossip_port: u16,
    build: String,
    pub(super) mode: ApiRoleMode,
    lenses: OnceCell<Arc<ApiLenses>>,
    lens_lifecycle: mpsc::UnboundedSender<LensCollection>,
    pub(super) founding_lock: Mutex<()>,
}

impl ApiService {
    pub(super) async fn handle_join_door(
        &self,
        peer: SocketAddr,
        request: hyper::Request<hyper::body::Incoming>,
    ) -> Response<HttpBody> {
        super::join::handle_join(self, peer, request).await
    }

    async fn handle(
        &self,
        peer: SocketAddr,
        request: hyper::Request<hyper::body::Incoming>,
        shutdown: watch::Receiver<bool>,
    ) -> Response<HttpBody> {
        if let Some(founding) =
            parse_founding_route(&self.mode, request.method(), request.uri().path())
        {
            match founding {
                Ok(()) => return super::founding::handle_founding(self, peer, request).await,
                Err(error) => {
                    return refusal_response_with_allow(error.refusal, error.allow);
                }
            }
        }

        let principal = match resolve_peer_principal(
            &self.corrosion,
            &self.cluster_id,
            source_from_peer(peer, &request),
        )
        .await
        {
            Ok(principal) => principal,
            Err(refusal) => return refusal_response(refusal),
        };
        if founding_route_disabled(&self.mode, request.uri().path()) {
            return refusal_response(ApiRefusal::UnsupportedRoute);
        }
        let route = match parse_route(request.method(), request.uri().path()) {
            Ok(route) => route,
            Err(error) => {
                return refusal_response_with_allow(error.refusal, error.allow);
            }
        };
        if !route.accepts_principal(&principal) {
            return refusal_response(ApiRefusal::UnsupportedRoute);
        }
        match route {
            V2Route::Version => version_response(&self.build),
            V2Route::Founding => unreachable!("founding routes are handled before roster auth"),
            V2Route::Join => refusal_response(ApiRefusal::UnsupportedRoute),
            V2Route::TokenCreate
            | V2Route::TokenList
            | V2Route::TokenRevoke
            | V2Route::MachineEndpointSet => {
                super::mutations::handle_mutation(self, route, principal, request).await
            }
            V2Route::Lens(collection) => self.snapshot_response(collection).await,
            V2Route::LensWatch(collection) => self.watch_response(collection, shutdown).await,
        }
    }

    async fn start_lenses(&self) -> &Arc<ApiLenses> {
        self.lenses
            .get_or_init(|| async {
                Arc::new(ApiLenses::start(
                    self.corrosion.clone(),
                    self.cluster_id.clone(),
                    self.lens_lifecycle.clone(),
                ))
            })
            .await
    }

    async fn snapshot_response(&self, collection: LensCollection) -> Response<HttpBody> {
        let Some(lenses) = self.lenses.get() else {
            return refusal_response(corrosion_unavailable_refusal());
        };
        let mut updates = lenses.watch(collection).subscribe();
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
        let Some(lenses) = self.lenses.get() else {
            return refusal_response(corrosion_unavailable_refusal());
        };
        let mut updates = lenses.watch(collection).subscribe();
        let initial =
            match tokio::time::timeout(LENS_INITIAL_WAIT, await_lens_state(&mut updates)).await {
                Ok(Ok(state)) => initial_watch_event(state),
                Ok(Err(refusal)) => return refusal_response(refusal),
                Err(_) => return refusal_response(corrosion_unavailable_refusal()),
            };
        sse_response(sse_watch_body(updates, initial, shutdown))
    }

    async fn shutdown(self) {
        let Some(lenses) = self.lenses.into_inner() else {
            return;
        };
        match Arc::try_unwrap(lenses) {
            Ok(lenses) => lenses.shutdown().await,
            Err(_) => tracing::warn!("API lenses retained a connection reference during shutdown"),
        }
    }

    pub(super) async fn start_founding_lenses_and_observe_machine(&self) -> bool {
        let lenses = self.start_lenses().await;
        machines_lens_contains(lenses, &self.local_machine_id).await
    }
}

pub(super) fn parse_founding_route(
    mode: &ApiRoleMode,
    method: &Method,
    path: &str,
) -> Option<Result<(), RouteRefusal>> {
    if path != FOUNDING_ROUTE {
        return None;
    }
    if matches!(mode, ApiRoleMode::Ordinary) {
        return None;
    }
    Some(parse_route(method, path).map(|route| {
        if route != V2Route::Founding {
            unreachable!("the exact founding path parses as the founding route");
        }
    }))
}

pub(super) fn founding_route_disabled(mode: &ApiRoleMode, path: &str) -> bool {
    matches!(mode, ApiRoleMode::Ordinary) && path == FOUNDING_ROUTE
}

async fn machines_lens_contains(lenses: &ApiLenses, machine_id: &MachineRowId) -> bool {
    let mut updates = lenses.watch(LensCollection::Machines).subscribe();
    let state = tokio::time::timeout(LENS_INITIAL_WAIT, await_lens_state(&mut updates)).await;
    matches!(
        state,
        Ok(Ok(Ok(LensSnapshot::Machines { rows, .. })))
            if rows.iter().any(|row| &row.id == machine_id)
    )
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
                biased;
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
    join_door: Option<JoinDoorListener>,
    service: Arc<ApiService>,
    lifecycle_failures: mpsc::UnboundedReceiver<LensCollection>,
}

struct ValidatedApiRuntime {
    corrosion: CorrosionClient,
    cluster_id: ClusterId,
    local_machine_id: MachineRowId,
    listen_addr: SocketAddr,
    join_substrate: Option<Arc<JoinMachineSubstrate>>,
    corrosion_gossip_port: u16,
    build: String,
    mode: ApiRoleMode,
}

impl ApiServer {
    /// Validates the configured mesh listener and starts one local lens per
    /// public collection.
    pub async fn bind(config: ApiRoleConfig) -> Result<Self, ApiServerError> {
        Self::bind_inner(config, false).await
    }

    /// Binds the mesh API and the public, join-only HTTPS door.
    pub async fn bind_with_join_door(config: ApiRoleConfig) -> Result<Self, ApiServerError> {
        Self::bind_inner(config, true).await
    }

    async fn bind_inner(
        config: ApiRoleConfig,
        bind_join_door: bool,
    ) -> Result<Self, ApiServerError> {
        let listen_addr = config.listen_addr();
        let join_substrate_path = config.join_substrate_path().to_path_buf();
        let corrosion_gossip_port = config.corrosion_gossip_port();
        let corrosion = CorrosionClient::new(config.corrosion().clone())
            .map_err(ApiServerError::CorrosionClientConfiguration)?;
        if matches!(config.mode(), ApiRoleMode::Ordinary) {
            validate_listener_identity(
                &corrosion,
                config.cluster_id(),
                config.local_machine_id(),
                config.listen_addr(),
            )
            .await
            .map_err(ApiServerError::ListenerIdentity)?;
        }
        let listener =
            TcpListener::bind(listen_addr)
                .await
                .map_err(|source| ApiServerError::Bind {
                    listen_addr,
                    source,
                })?;
        let join_door = if bind_join_door {
            Some(
                JoinDoorListener::bind(
                    config.door_listen_addr().get(),
                    config.door_private_key_path(),
                    config.door_certificate_path(),
                    config.door_fingerprint_path(),
                )
                .await
                .map_err(ApiServerError::JoinDoor)?,
            )
        } else {
            None
        };
        let join_substrate = if bind_join_door {
            Some(Arc::new(load_join_substrate(&join_substrate_path).await?))
        } else {
            None
        };
        Ok(Self::from_validated_listener(
            listener,
            join_door,
            ValidatedApiRuntime {
                corrosion,
                cluster_id: config.cluster_id().clone(),
                local_machine_id: config.local_machine_id().clone(),
                listen_addr: config.listen_addr(),
                join_substrate,
                corrosion_gossip_port,
                build: config.build().to_owned(),
                mode: config.mode().clone(),
            },
        ))
    }

    fn from_validated_listener(
        listener: TcpListener,
        join_door: Option<JoinDoorListener>,
        runtime: ValidatedApiRuntime,
    ) -> Self {
        let ValidatedApiRuntime {
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
            join_substrate,
            corrosion_gossip_port,
            build,
            mode,
        } = runtime;
        let (lifecycle_sender, lifecycle_failures) = mpsc::unbounded_channel();
        let lenses = OnceCell::new();
        let ordinary_lenses = matches!(mode, ApiRoleMode::Ordinary).then(|| {
            Arc::new(ApiLenses::start(
                corrosion.clone(),
                cluster_id.clone(),
                lifecycle_sender.clone(),
            ))
        });
        if ordinary_lenses.is_some_and(|ordinary_lenses| lenses.set(ordinary_lenses).is_err()) {
            unreachable!("a new API lens cell is empty");
        }
        let door_material = join_door.as_ref().map(JoinDoorListener::material);
        let service = Arc::new(ApiService {
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
            door_material,
            join_substrate,
            corrosion_gossip_port,
            build,
            mode,
            lenses,
            lens_lifecycle: lifecycle_sender,
            founding_lock: Mutex::new(()),
        });
        Self {
            listener,
            join_door,
            service,
            lifecycle_failures,
        }
    }

    /// Serves accepted TCP peers until the caller requests controlled shutdown.
    pub async fn serve<Shutdown>(self, shutdown: Shutdown) -> Result<(), ApiServerServeError>
    where
        Shutdown: Future<Output = ()> + Send,
    {
        let Self {
            listener,
            join_door,
            service,
            mut lifecycle_failures,
        } = self;
        let (shutdown_tx, _) = watch::channel(false);
        let (endpoint_failure_tx, mut endpoint_failures) = mpsc::unbounded_channel();
        let endpoint_task = service.lenses.get().map(|lenses| {
            let updates = lenses.watch(LensCollection::Machines).subscribe();
            let local_machine_id = service.local_machine_id.clone();
            let shutdown = shutdown_tx.subscribe();
            tokio::spawn(async move {
                if let Err(error) = endpoint_network::run(updates, local_machine_id, shutdown).await
                {
                    let _ = endpoint_failure_tx.send(error);
                }
            })
        });
        let mut connections = JoinSet::new();
        let join_connection_slots = Arc::new(Semaphore::new(MAX_JOIN_DOOR_CONNECTIONS));
        let stop = await_server_stop_with_endpoint(
            shutdown,
            &mut lifecycle_failures,
            &mut endpoint_failures,
        );
        tokio::pin!(stop);

        let serve_result = loop {
            tokio::select! {
                biased;
                result = &mut stop => {
                    if let Err(error) = &result {
                        match &error {
                            ApiServerServeError::LensRecoveryExhausted { collection } => {
                                tracing::error!(collection = collection.as_str(), "API lens recovery budget exhausted; stopping API role for supervisor restart");
                            }
                            ApiServerServeError::EndpointNetworkConvergence { detail } => {
                                tracing::error!(%detail, "API endpoint-network convergence failed; stopping API role for supervisor restart");
                            }
                        }
                    }
                    let _ = shutdown_tx.send(true);
                    break result;
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
                accepted = accept_join_connection(&join_door), if join_door.is_some() => match accepted {
                    Ok((stream, peer)) => {
                        let Some(door) = join_door.as_ref() else {
                            unreachable!("the guarded join accept branch has a listener");
                        };
                        match Arc::clone(&join_connection_slots).try_acquire_owned() {
                            Ok(permit) => {
                                let acceptor = door.acceptor();
                                let service = Arc::clone(&service);
                                let shutdown = shutdown_tx.subscribe();
                                connections.spawn(async move {
                                    serve_join_connection(
                                        stream,
                                        peer,
                                        acceptor,
                                        service,
                                        shutdown,
                                        permit,
                                    )
                                    .await;
                                });
                            }
                            Err(_) => {
                                tracing::warn!(
                                    maximum = MAX_JOIN_DOOR_CONNECTIONS,
                                    "join door connection bound reached"
                                );
                            }
                        }
                    }
                    Err(error) => {
                        tracing::warn!(error = %error, "join door listener accept failed");
                        tokio::time::sleep(Duration::from_millis(100)).await;
                    }
                },
                Some(result) = connections.join_next(), if !connections.is_empty() => {
                    if let Err(error) = result {
                        tracing::warn!(error = %error, "API connection task failed");
                    }
                }
            }
        };

        if tokio::time::timeout(SERVER_SHUTDOWN_GRACE, drain_connections(&mut connections))
            .await
            .is_err()
        {
            connections.abort_all();
            drain_connections(&mut connections).await;
        }
        if let Some(endpoint_task) = endpoint_task {
            endpoint_task.abort();
            let _ = endpoint_task.await;
        }
        match Arc::try_unwrap(service) {
            Ok(service) => service.shutdown().await,
            Err(_) => tracing::warn!("API service retained a connection reference during shutdown"),
        }
        serve_result
    }
}

pub(super) async fn await_lens_lifecycle_failure(
    lifecycle_failures: &mut mpsc::UnboundedReceiver<LensCollection>,
) -> Option<ApiServerServeError> {
    lifecycle_failures
        .recv()
        .await
        .map(|collection| ApiServerServeError::LensRecoveryExhausted { collection })
}

#[cfg(test)]
pub(super) async fn await_server_stop<Shutdown>(
    shutdown: Shutdown,
    lifecycle_failures: &mut mpsc::UnboundedReceiver<LensCollection>,
) -> Result<(), ApiServerServeError>
where
    Shutdown: Future<Output = ()>,
{
    tokio::pin!(shutdown);
    tokio::select! {
        biased;
        () = &mut shutdown => Ok(()),
        Some(error) = await_lens_lifecycle_failure(lifecycle_failures) => Err(error),
    }
}

async fn await_server_stop_with_endpoint<Shutdown>(
    shutdown: Shutdown,
    lifecycle_failures: &mut mpsc::UnboundedReceiver<LensCollection>,
    endpoint_failures: &mut mpsc::UnboundedReceiver<EndpointNetworkFoldError>,
) -> Result<(), ApiServerServeError>
where
    Shutdown: Future<Output = ()>,
{
    tokio::pin!(shutdown);
    tokio::select! {
        biased;
        () = &mut shutdown => Ok(()),
        Some(error) = await_lens_lifecycle_failure(lifecycle_failures) => Err(error),
        Some(error) = endpoint_failures.recv() => Err(ApiServerServeError::EndpointNetworkConvergence {
            detail: error.to_string(),
        }),
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

pub(super) async fn load_join_substrate(
    path: &std::path::Path,
) -> Result<JoinMachineSubstrate, ApiServerError> {
    let metadata =
        tokio::fs::metadata(path)
            .await
            .map_err(|source| ApiServerError::JoinSubstrateRead {
                path: path.to_path_buf(),
                source,
            })?;
    if metadata.len() > MAX_JOIN_SUBSTRATE_BYTES as u64 {
        return Err(ApiServerError::JoinSubstrateTooLarge {
            path: path.to_path_buf(),
            limit: MAX_JOIN_SUBSTRATE_BYTES,
        });
    }
    let bytes =
        tokio::fs::read(path)
            .await
            .map_err(|source| ApiServerError::JoinSubstrateRead {
                path: path.to_path_buf(),
                source,
            })?;
    if bytes.len() > MAX_JOIN_SUBSTRATE_BYTES {
        return Err(ApiServerError::JoinSubstrateTooLarge {
            path: path.to_path_buf(),
            limit: MAX_JOIN_SUBSTRATE_BYTES,
        });
    }
    serde_json::from_slice::<JoinMachineSubstrate>(&bytes).map_err(|error| {
        ApiServerError::JoinSubstrateInvalid {
            path: path.to_path_buf(),
            detail: error.to_string(),
        }
    })
}

/// Runs the API role using its supervisor-loaded environment file.
pub async fn run_from_environment() -> Result<(), ApiRoleRuntimeError> {
    let config = ApiRoleConfig::from_environment().map_err(ApiRoleRuntimeError::Configuration)?;
    let server = ApiServer::bind_with_join_door(config)
        .await
        .map_err(ApiRoleRuntimeError::Server)?;
    server
        .serve(wait_for_process_shutdown())
        .await
        .map_err(ApiRoleRuntimeError::Serve)
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
    #[error(transparent)]
    JoinDoor(JoinDoorBindError),
    #[error("could not read join substrate {path:?}: {source}")]
    JoinSubstrateRead {
        path: PathBuf,
        #[source]
        source: std::io::Error,
    },
    #[error("join substrate {path:?} exceeds the {limit}-byte limit")]
    JoinSubstrateTooLarge { path: PathBuf, limit: usize },
    #[error("join substrate {path:?} is invalid: {detail}")]
    JoinSubstrateInvalid { path: PathBuf, detail: String },
    #[error("could not bind API listener {listen_addr}: {source}")]
    Bind {
        listen_addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
}

/// A bounded API role serving failure that requires supervisor restart.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ApiServerServeError {
    #[error("API lens {collection:?} exhausted its Corrosion recovery budget")]
    LensRecoveryExhausted { collection: LensCollection },
    #[error("API endpoint-network convergence failed: {detail}")]
    EndpointNetworkConvergence { detail: String },
}

/// A bounded API role process failure.
#[derive(Debug, thiserror::Error)]
pub enum ApiRoleRuntimeError {
    #[error(transparent)]
    Configuration(ApiRoleConfigError),
    #[error(transparent)]
    Server(ApiServerError),
    #[error(transparent)]
    Serve(ApiServerServeError),
}
