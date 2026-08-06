//! HTTP/1, JSON, and SSE serving for the API role.

use std::convert::Infallible;
use std::net::{IpAddr, SocketAddr};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use futures_util::stream;
use http_body_util::{BodyExt, Full, StreamBody, combinators::BoxBody};
use hyper::body::Frame;
use hyper::header::{ALLOW, CONTENT_TYPE, HeaderValue, RETRY_AFTER};
use hyper::{Method, Response, StatusCode};
use ployz_core::MachineUpgradeSupervisor;
use ployz_core::ids::{ClusterId, MachineRowId};
use ployz_core::{
    ApiFeature, ApiRefusal, ApiVersion, FOUNDING_ROUTE, KNOWN_API_FEATURES, LensCollection,
    LensSnapshot, LensWatchEvent, V2Method, V2Route,
};
use ployz_host_runner::PloyzdArtifactStore;
use tokio::sync::{Mutex, OnceCell, mpsc, watch};
use tokio::time::{Instant, MissedTickBehavior};

use super::config::ApiRoleMode;
use super::roster::{PeerPrincipalError, corrosion_unavailable_refusal, resolve_peer_principal};
use super::runtime::JoinDoorRuntime;
use crate::corrosion::CorrosionClient;
use crate::roles::api::lenses::{
    LensEngineConfig, LensRecoveryPolicy, LensWatch, start_lens_with_lifecycle,
};

const LENS_INITIAL_WAIT: Duration = Duration::from_secs(15);
const SSE_KEEPALIVE_INTERVAL: Duration = Duration::from_secs(15);
const LENS_RECOVERY_MAX_ATTEMPTS: u32 = 5;
const LENS_RECOVERY_INITIAL_BACKOFF: Duration = Duration::from_millis(100);
const LENS_RECOVERY_MAX_BACKOFF: Duration = Duration::from_secs(1);

pub(super) type HttpBody = BoxBody<Bytes, Infallible>;
pub(super) type LensState = Result<LensSnapshot, ApiRefusal>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum BoundedBodyError {
    TooLarge,
    Deadline,
    Read,
}

pub(super) async fn read_bounded_body<Payload>(
    body: Payload,
    limit: usize,
    timeout: Duration,
) -> Result<Vec<u8>, BoundedBodyError>
where
    Payload: hyper::body::Body<Data = Bytes> + Unpin,
{
    tokio::time::timeout(timeout, collect_bounded_body(body, limit))
        .await
        .map_err(|_| BoundedBodyError::Deadline)?
}

async fn collect_bounded_body<Payload>(
    mut body: Payload,
    limit: usize,
) -> Result<Vec<u8>, BoundedBodyError>
where
    Payload: hyper::body::Body<Data = Bytes> + Unpin,
{
    let mut bytes = Vec::new();
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|_| BoundedBodyError::Read)?;
        let Ok(data) = frame.into_data() else {
            continue;
        };
        append_bounded_body_chunk(&mut bytes, &data, limit)?;
    }
    Ok(bytes)
}

pub(super) fn append_bounded_body_chunk(
    body: &mut Vec<u8>,
    chunk: &[u8],
    limit: usize,
) -> Result<(), BoundedBodyError> {
    let Some(total) = body.len().checked_add(chunk.len()) else {
        return Err(BoundedBodyError::TooLarge);
    };
    if total > limit {
        return Err(BoundedBodyError::TooLarge);
    }
    body.extend_from_slice(chunk);
    Ok(())
}

pub(super) fn json_response(status: StatusCode, body: Vec<u8>) -> Response<HttpBody> {
    let mut response = Response::new(Full::new(Bytes::from(body)).boxed());
    *response.status_mut() = status;
    response
        .headers_mut()
        .insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    response
}

pub(super) fn sse_response(body: HttpBody) -> Response<HttpBody> {
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

pub(super) struct ApiLenses {
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

    pub(super) fn watch(&self, collection: LensCollection) -> &LensWatch {
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
    pub(super) join_door: Arc<JoinDoorRuntime>,
    pub(super) corrosion_gossip_port: u16,
    build: String,
    pub(super) mode: ApiRoleMode,
    pub(super) upgrade_store: PloyzdArtifactStore,
    pub(super) keeper_upgrade_socket_path: std::path::PathBuf,
    pub(super) upgrade_supervisor: MachineUpgradeSupervisor,
    pub(super) operations: Arc<super::operation_http::OperationRuntime>,
    pub(super) first_deploy: Option<super::first_deploy::FirstDeployDriver>,
    pub(super) container_runner:
        Option<Arc<crate::roles::api::execution::docker::runner::DockerManagedContainerRunner>>,
    lenses: OnceCell<Arc<ApiLenses>>,
    lens_lifecycle: mpsc::UnboundedSender<LensCollection>,
    pub(super) founding_lock: Mutex<()>,
}

pub(super) struct ApiServiceRuntime {
    pub(super) corrosion: CorrosionClient,
    pub(super) cluster_id: ClusterId,
    pub(super) local_machine_id: MachineRowId,
    pub(super) listen_addr: SocketAddr,
    pub(super) corrosion_gossip_port: u16,
    pub(super) build: String,
    pub(super) mode: ApiRoleMode,
    pub(super) upgrade_store: PloyzdArtifactStore,
    pub(super) keeper_upgrade_socket_path: std::path::PathBuf,
    pub(super) upgrade_supervisor: MachineUpgradeSupervisor,
    pub(super) operations: Arc<super::operation_http::OperationRuntime>,
    pub(super) first_deploy: Option<super::first_deploy::FirstDeployDriver>,
    pub(super) container_runner:
        Option<Arc<crate::roles::api::execution::docker::runner::DockerManagedContainerRunner>>,
}

impl ApiService {
    pub(super) fn new(
        runtime: ApiServiceRuntime,
        join_door: Arc<JoinDoorRuntime>,
    ) -> (Arc<Self>, mpsc::UnboundedReceiver<LensCollection>) {
        let ApiServiceRuntime {
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
            corrosion_gossip_port,
            build,
            mode,
            upgrade_store,
            keeper_upgrade_socket_path,
            upgrade_supervisor,
            operations,
            first_deploy,
            container_runner,
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
        let service = Arc::new(Self {
            corrosion,
            cluster_id,
            local_machine_id,
            listen_addr,
            join_door,
            corrosion_gossip_port,
            build,
            mode,
            upgrade_store,
            keeper_upgrade_socket_path,
            upgrade_supervisor,
            operations,
            first_deploy,
            container_runner,
            lenses,
            lens_lifecycle: lifecycle_sender,
            founding_lock: Mutex::new(()),
        });
        (service, lifecycle_failures)
    }

    pub(super) async fn handle_join_door(
        &self,
        peer: SocketAddr,
        request: hyper::Request<hyper::body::Incoming>,
    ) -> Response<HttpBody> {
        super::join::handle_join(self, peer, request).await
    }

    pub(super) async fn handle(
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

        let is_status_request = matches!(
            parse_route(request.method(), request.uri().path()),
            Ok(V2Route::Status)
        );
        let principal = match resolve_peer_principal(
            &self.corrosion,
            &self.cluster_id,
            source_from_peer(peer, &request),
        )
        .await
        {
            Ok(principal) => principal,
            Err(PeerPrincipalError::EmptyAcceptedRoster { .. }) if is_status_request => {
                return super::diagnostics::status_response(self).await;
            }
            Err(PeerPrincipalError::Refusal(ApiRefusal::MissingCluster)) if is_status_request => {
                return super::diagnostics::status_response(self).await;
            }
            Err(error) => return refusal_response(error.into_refusal()),
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
            V2Route::Status => super::diagnostics::status_response(self).await,
            V2Route::Doctor => super::diagnostics::doctor_response(self).await,
            V2Route::TokenCreate
            | V2Route::TokenList
            | V2Route::TokenRevoke(_)
            | V2Route::MachineEndpointSet
            | V2Route::MachineRemove
            | V2Route::NamespaceCreate
            | V2Route::NamespaceRemove => {
                super::mutations::handle_mutation(self, route, principal, request).await
            }
            V2Route::PeerRemove | V2Route::ServiceRemove | V2Route::RouteRemove => {
                super::removals::handle_removal(self, route, request).await
            }
            V2Route::MachineUpgrade => super::upgrade::handle_machine_upgrade(self, request).await,
            V2Route::Operation(operation_id) => {
                super::operation_http::handle_lookup(self, operation_id).await
            }
            V2Route::OperationWatch(operation_id) => {
                super::operation_http::handle_watch(self, operation_id, &principal, shutdown).await
            }
            V2Route::FirstDeploy => {
                super::operation_http::handle_first_deploy(self, principal, request).await
            }
            V2Route::ServiceLogsTail(service_id) => {
                super::service_logs::handle_tail(self, service_id, request, shutdown).await
            }
            V2Route::ServiceLogsFollow(service_id) => {
                super::service_logs::handle_follow(self, service_id, request, shutdown).await
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

    pub(super) async fn shutdown(self) {
        let Some(lenses) = self.lenses.into_inner() else {
            return;
        };
        match Arc::try_unwrap(lenses) {
            Ok(lenses) => lenses.shutdown().await,
            Err(_) => tracing::warn!("API lenses retained a connection reference during shutdown"),
        }
    }

    pub(super) async fn shutdown_operations(&self) {
        let outcome = self.operations.shutdown().await;
        tracing::info!(?outcome, "operation task shutdown finished");
    }

    pub(super) async fn start_founding_lenses_and_observe_machine(&self) -> bool {
        let lenses = self.start_lenses().await;
        machines_lens_contains(lenses, &self.local_machine_id).await
    }

    pub(super) fn lenses(&self) -> Option<&Arc<ApiLenses>> {
        self.lenses.get()
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
