//! HTTP/1, JSON, and SSE serving for the API role.

use std::convert::Infallible;
use std::future::Future;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use bytes::Bytes;
use futures_util::stream;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::Frame;
use hyper::header::{ALLOW, AUTHORIZATION, CONTENT_TYPE, HeaderValue, RETRY_AFTER};
use hyper::server::conn::http1;
use hyper::service::service_fn;
use hyper::{Method, Response, StatusCode};
use hyper_util::rt::{TokioIo, TokioTimer};
use ployz_core::founding::{FoundingRefusal, FoundingRequest, FoundingResult};
use ployz_core::ids::{ClusterId, MachineRowId};
use ployz_core::{
    ApiFeature, ApiRefusal, ApiVersion, FOUNDING_ROUTE, KNOWN_API_FEATURES, LensCollection,
    LensSnapshot, LensWatchEvent, V2Method, V2Route,
};
use tokio::net::TcpListener;
use tokio::sync::{Mutex, OnceCell, mpsc, watch};
use tokio::task::JoinSet;
use tokio::time::{Instant, MissedTickBehavior};

use super::config::{ApiRoleConfig, ApiRoleConfigError, ApiRoleMode};
use super::founding::{FoundingWrite, ensure_endpoint_network, write_initial_rows};
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
const LENS_RECOVERY_MAX_ATTEMPTS: u32 = 5;
const LENS_RECOVERY_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const LENS_RECOVERY_MAX_BACKOFF: Duration = Duration::from_secs(1);
pub(super) const MAX_FOUNDING_REQUEST_BYTES: usize = 1024 * 1024;

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

struct ApiService {
    corrosion: CorrosionClient,
    cluster_id: ClusterId,
    local_machine_id: MachineRowId,
    listen_addr: SocketAddr,
    build: String,
    mode: ApiRoleMode,
    lenses: OnceCell<Arc<ApiLenses>>,
    lens_lifecycle: mpsc::UnboundedSender<LensCollection>,
    founding_lock: Mutex<()>,
}

impl ApiService {
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
                Ok(()) => return self.founding_response(peer, request).await,
                Err(error) => {
                    return refusal_response_with_allow(error.refusal, error.allow);
                }
            }
        }

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
        if founding_route_disabled(&self.mode, request.uri().path()) {
            return refusal_response(ApiRefusal::UnsupportedRoute);
        }
        let route = match parse_route(request.method(), request.uri().path()) {
            Ok(route) => route,
            Err(error) => {
                return refusal_response_with_allow(error.refusal, error.allow);
            }
        };
        match route {
            V2Route::Version => version_response(&self.build),
            V2Route::Founding => unreachable!("founding routes are handled before roster auth"),
            V2Route::Lens(collection) => self.snapshot_response(collection).await,
            V2Route::LensWatch(collection) => self.watch_response(collection, shutdown).await,
        }
    }

    async fn founding_response(
        &self,
        peer: SocketAddr,
        request: hyper::Request<hyper::body::Incoming>,
    ) -> Response<HttpBody> {
        if let Err(refusal) = authorize_founding(
            &self.mode,
            self.listen_addr,
            peer,
            request.headers().get(AUTHORIZATION),
        ) {
            return refusal_response(refusal);
        }
        let body = match read_bounded_founding_body(request.into_body()).await {
            Ok(body) => body,
            Err(FoundingBodyError::TooLarge) => {
                return founding_http_error(StatusCode::PAYLOAD_TOO_LARGE, "request_too_large");
            }
            Err(FoundingBodyError::Read) => {
                return founding_http_error(StatusCode::BAD_REQUEST, "invalid_request");
            }
        };
        let request = match serde_json::from_slice::<FoundingRequest>(&body) {
            Ok(request) => request,
            Err(_) => return founding_http_error(StatusCode::BAD_REQUEST, "invalid_request"),
        };
        if request.cluster_id != self.cluster_id || request.machine_id != self.local_machine_id {
            return refusal_response(ApiRefusal::InvalidCluster);
        }
        let ployz_core::corrosion::MachineTransport::Wireguard { addr_v6, .. } =
            &request.machine.transport
        else {
            return founding_refusal_response(FoundingRefusal::from(
                ployz_core::founding::FoundingValidationError::MachineTransportProviderMismatch,
            ));
        };
        if self.listen_addr.ip() != IpAddr::V6(*addr_v6) {
            return refusal_response(ApiRefusal::InvalidCluster);
        }
        let validated = match request.try_validate() {
            Ok(validated) => validated,
            Err(reason) => {
                return founding_refusal_response(FoundingRefusal::InvalidRequest { reason });
            }
        };

        let _guard = self.founding_lock.lock().await;
        let postconditions = ApiFoundingPostconditions {
            service: self,
            validated: &validated,
        };
        match execute_founding(&postconditions).await {
            Ok(result) => founding_result_response(result),
            Err(refusal) => refusal_response(refusal),
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

fn bootstrap_authorized(
    authorization: Option<&HeaderValue>,
    secret: &super::config::BootstrapSecret,
) -> bool {
    let Some(authorization) = authorization.and_then(|value| value.to_str().ok()) else {
        return false;
    };
    let Some(candidate) = authorization.strip_prefix("Bearer ") else {
        return false;
    };
    !candidate.is_empty() && secret.verifies(candidate.as_bytes())
}

pub(super) fn authorize_founding(
    mode: &ApiRoleMode,
    listen_addr: SocketAddr,
    peer: SocketAddr,
    authorization: Option<&HeaderValue>,
) -> Result<(), ApiRefusal> {
    let ApiRoleMode::Founding(secret) = mode else {
        return Err(ApiRefusal::UnsupportedRoute);
    };
    if peer.ip() != listen_addr.ip() || !bootstrap_authorized(authorization, secret) {
        return Err(ApiRefusal::UnknownSource { source: peer.ip() });
    }
    Ok(())
}

#[derive(Debug)]
pub(super) enum FoundingBodyError {
    TooLarge,
    Read,
}

async fn read_bounded_founding_body(
    mut body: hyper::body::Incoming,
) -> Result<Vec<u8>, FoundingBodyError> {
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| FoundingBodyError::Read)?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        append_founding_body_chunk(&mut bytes, &data)?;
    }
    Ok(bytes)
}

pub(super) fn append_founding_body_chunk(
    body: &mut Vec<u8>,
    chunk: &[u8],
) -> Result<(), FoundingBodyError> {
    let Some(total) = body.len().checked_add(chunk.len()) else {
        return Err(FoundingBodyError::TooLarge);
    };
    if total > MAX_FOUNDING_REQUEST_BYTES {
        return Err(FoundingBodyError::TooLarge);
    }
    body.extend_from_slice(chunk);
    Ok(())
}

fn founding_http_error(status: StatusCode, kind: &'static str) -> Response<HttpBody> {
    json_response(status, format!("{{\"kind\":\"{kind}\"}}").into_bytes())
}

fn founding_refusal_response(refusal: FoundingRefusal) -> Response<HttpBody> {
    match serde_json::to_vec(&refusal) {
        Ok(body) => json_response(StatusCode::BAD_REQUEST, body),
        Err(error) => {
            tracing::error!(error = %error, "could not encode founding refusal");
            corrosion_unavailable_response()
        }
    }
}

fn founding_result_response(result: FoundingResult) -> Response<HttpBody> {
    match serde_json::to_vec(&result) {
        Ok(body) => json_response(StatusCode::OK, body),
        Err(error) => {
            tracing::error!(error = %error, "could not encode founding result");
            corrosion_unavailable_response()
        }
    }
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

#[async_trait]
pub(super) trait FoundingPostconditions: Sync {
    async fn write_and_read_back(&self) -> Result<FoundingWrite, ApiRefusal>;
    async fn ensure_endpoint_network(
        &self,
        subnet: &ployz_core::network::MachineEndpointSubnet,
    ) -> Result<(), ApiRefusal>;
    async fn start_lenses_and_observe_machine(&self) -> Result<(), ApiRefusal>;
}

pub(super) async fn execute_founding(
    postconditions: &impl FoundingPostconditions,
) -> Result<FoundingResult, ApiRefusal> {
    let write = postconditions.write_and_read_back().await?;
    postconditions
        .ensure_endpoint_network(&write.machine_subnet)
        .await?;
    postconditions.start_lenses_and_observe_machine().await?;
    Ok(write.result)
}

struct ApiFoundingPostconditions<'a> {
    service: &'a ApiService,
    validated: &'a ployz_core::founding::ValidatedFoundingRequest,
}

#[async_trait]
impl FoundingPostconditions for ApiFoundingPostconditions<'_> {
    async fn write_and_read_back(&self) -> Result<FoundingWrite, ApiRefusal> {
        match write_initial_rows(&self.service.corrosion, self.validated).await {
            Ok(write) => Ok(write),
            Err(error) if error.is_state_mismatch() => {
                tracing::warn!(error = %error, "founding rows did not match persisted state");
                Err(ApiRefusal::InvalidCluster)
            }
            Err(error) => {
                tracing::warn!(error = %error, "could not commit founding rows");
                Err(corrosion_unavailable_refusal())
            }
        }
    }

    async fn ensure_endpoint_network(
        &self,
        subnet: &ployz_core::network::MachineEndpointSubnet,
    ) -> Result<(), ApiRefusal> {
        ensure_endpoint_network(subnet).await.map_err(|error| {
            tracing::warn!(error = %error, "could not fold founding rows into the endpoint network");
            corrosion_unavailable_refusal()
        })
    }

    async fn start_lenses_and_observe_machine(&self) -> Result<(), ApiRefusal> {
        let lenses = self.service.start_lenses().await;
        if machines_lens_contains(lenses, &self.service.local_machine_id).await {
            Ok(())
        } else {
            tracing::warn!("machines lens did not observe machine one after founding");
            Err(corrosion_unavailable_refusal())
        }
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
    service: Arc<ApiService>,
    lifecycle_failures: mpsc::UnboundedReceiver<LensCollection>,
}

impl ApiServer {
    /// Validates the configured mesh listener and starts one local lens per
    /// public collection.
    pub async fn bind(config: ApiRoleConfig) -> Result<Self, ApiServerError> {
        let listen_addr = config.listen_addr();
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
        Ok(Self::from_validated_listener(
            listener,
            corrosion,
            config.cluster_id().clone(),
            config.local_machine_id().clone(),
            config.listen_addr(),
            config.build().to_owned(),
            config.mode().clone(),
        ))
    }

    fn from_validated_listener(
        listener: TcpListener,
        corrosion: CorrosionClient,
        cluster_id: ClusterId,
        local_machine_id: MachineRowId,
        listen_addr: SocketAddr,
        build: String,
        mode: ApiRoleMode,
    ) -> Self {
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
        let service = Arc::new(ApiService {
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
            build,
            mode,
            lenses,
            lens_lifecycle: lifecycle_sender,
            founding_lock: Mutex::new(()),
        });
        Self {
            listener,
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
            service,
            mut lifecycle_failures,
        } = self;
        let (shutdown_tx, _) = watch::channel(false);
        let mut connections = JoinSet::new();
        let stop = await_server_stop(shutdown, &mut lifecycle_failures);
        tokio::pin!(stop);

        let serve_result = loop {
            tokio::select! {
                biased;
                result = &mut stop => {
                    if let Err(error) = result {
                        tracing::error!(
                            collection = error.collection().as_str(),
                            "API lens recovery budget exhausted; stopping API role for supervisor restart"
                        );
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
    #[error("could not bind API listener {listen_addr}: {source}")]
    Bind {
        listen_addr: SocketAddr,
        #[source]
        source: std::io::Error,
    },
}

/// A bounded API role serving failure that requires supervisor restart.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum ApiServerServeError {
    #[error("API lens {collection:?} exhausted its Corrosion recovery budget")]
    LensRecoveryExhausted { collection: LensCollection },
}

impl ApiServerServeError {
    const fn collection(self) -> LensCollection {
        match self {
            Self::LensRecoveryExhausted { collection } => collection,
        }
    }
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
