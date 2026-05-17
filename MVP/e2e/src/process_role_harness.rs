use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use mvp_bus::{BusSession, FactKeyPattern, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_projection::{
    DnsRecordProjection, FactSource, GatewayRouteProjection, ProjectionActorHandle,
    ProjectionActorStatus, ProjectionReport, SqliteProjectionStore,
};
use mvp_serving::{
    ServingActorHandle, ServingError, ServingFreshness, ServingSnapshotPaths, ServingStatus,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tokio::time::timeout;

use crate::bus_syntax::fact_pattern;
use crate::process_fact_source::{ProcessFactReadPolicy, ProcessFactSource};

const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);
const STALE_AFTER: Duration = Duration::from_secs(300);
const ROLE_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const ROLE_REQUEST_READ_TIMEOUT: Duration = Duration::from_millis(250);
const ROLE_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const ROLE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const ROLE_MAX_REQUEST_BYTES: usize = 64 * 1024;

pub(crate) fn run_role(args: Vec<String>) -> Result<(), String> {
    let [role, rest @ ..] = args.as_slice() else {
        return Err("usage: mvp-e2e role <serving-projection> ...".to_string());
    };
    match role.as_str() {
        "serving-projection" => {
            let config = ServingProjectionRoleConfig::parse(rest)?;
            runtime()?.block_on(run_serving_projection_role(config))
        }
        other => Err(format!("unknown process role '{other}'")),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create process role runtime: {error}"))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServingProjectionRoleConfig {
    root: PathBuf,
    socket: PathBuf,
}

impl ServingProjectionRoleConfig {
    pub(crate) fn new(root: impl Into<PathBuf>, socket: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            socket: socket.into(),
        }
    }

    fn parse(args: &[String]) -> Result<Self, String> {
        let mut root = None;
        let mut socket = None;
        let mut remaining = args;
        while let [flag, value, tail @ ..] = remaining {
            match flag.as_str() {
                "--root" => root = Some(PathBuf::from(value)),
                "--socket" => socket = Some(PathBuf::from(value)),
                other => return Err(format!("unknown serving-projection flag '{other}'")),
            }
            remaining = tail;
        }
        if !remaining.is_empty() {
            return Err(format!(
                "serving-projection arguments must be flag/value pairs, got {remaining:?}"
            ));
        }
        Ok(Self {
            root: root.ok_or_else(|| "serving-projection requires --root".to_string())?,
            socket: socket.ok_or_else(|| "serving-projection requires --socket".to_string())?,
        })
    }
}

pub(crate) async fn run_serving_projection_role(
    config: ServingProjectionRoleConfig,
) -> Result<(), String> {
    remove_stale_socket(&config.socket)?;
    if let Some(parent) = config.socket.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create socket dir '{}': {error}", parent.display()))?;
    }
    let listener = UnixListener::bind(&config.socket)
        .map_err(|error| format!("bind serving-projection socket: {error}"))?;
    let state = Arc::new(Mutex::new(ServingProjectionState::new(config.clone())?));
    loop {
        let (stream, _addr) = listener
            .accept()
            .await
            .map_err(|error| format!("accept serving-projection connection: {error}"))?;
        if handle_serving_projection_connection(stream, Arc::clone(&state)).await? {
            break;
        }
    }
    Ok(())
}

async fn handle_serving_projection_connection(
    mut stream: UnixStream,
    state: Arc<Mutex<ServingProjectionState>>,
) -> Result<bool, String> {
    let response = match read_request(&mut stream).await {
        Ok(request) => match serde_json::from_slice::<RoleRequest>(&request) {
            Ok(request) => match handle_role_request(request, state).await {
                Ok(success) => RoleResponse::Success(success),
                Err(error) => RoleResponse::Failure(error),
            },
            Err(error) => RoleResponse::Failure(RoleFailure::invalid_request(format!(
                "parse role request: {error}"
            ))),
        },
        Err(message) => RoleResponse::Failure(RoleFailure::invalid_request(message)),
    };
    let should_shutdown = matches!(response, RoleResponse::Success(RoleSuccess::Shutdown));
    let response = serde_json::to_vec(&response)
        .map_err(|error| format!("serialize role response: {error}"))?;
    write_with_timeout(&mut stream, &response, "role response").await?;
    shutdown_stream(&mut stream, "role response").await?;
    Ok(should_shutdown)
}

async fn read_request(stream: &mut UnixStream) -> Result<Vec<u8>, String> {
    timeout(
        ROLE_REQUEST_READ_TIMEOUT,
        read_bounded(stream, ROLE_MAX_REQUEST_BYTES),
    )
    .await
    .map_err(|_| "read role request timed out".to_string())?
}

async fn read_bounded(stream: &mut UnixStream, limit: usize) -> Result<Vec<u8>, String> {
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| format!("read role request: {error}"))?;
        if read == 0 {
            return Ok(request);
        }
        if request.len() + read > limit {
            return Err(format!("role request exceeds {limit} byte limit"));
        }
        request.extend_from_slice(&chunk[..read]);
    }
}

async fn write_with_timeout(
    stream: &mut UnixStream,
    bytes: &[u8],
    operation: &'static str,
) -> Result<(), String> {
    timeout(ROLE_WRITE_TIMEOUT, stream.write_all(bytes))
        .await
        .map_err(|_| format!("write {operation} timed out"))?
        .map_err(|error| format!("write {operation}: {error}"))
}

async fn shutdown_stream(stream: &mut UnixStream, operation: &'static str) -> Result<(), String> {
    timeout(ROLE_WRITE_TIMEOUT, stream.shutdown())
        .await
        .map_err(|_| format!("shutdown {operation} stream timed out"))?
        .map_err(|error| format!("shutdown {operation} stream: {error}"))
}

async fn handle_role_request(
    request: RoleRequest,
    state: Arc<Mutex<ServingProjectionState>>,
) -> Result<RoleSuccess, RoleFailure> {
    match request {
        RoleRequest::ProjectOnce => project_once(state).await,
        RoleRequest::Reload => reload_serving(state).await,
        RoleRequest::BeginRebuild => begin_rebuild(state).await,
        RoleRequest::AwaitRebuild { token } => await_rebuild(state, token).await,
        RoleRequest::QueryGateway { host } => query_gateway(state, host).await,
        RoleRequest::QueryDns { name, record_type } => query_dns(state, name, record_type).await,
        RoleRequest::Status => status(state).await,
        RoleRequest::Shutdown => {
            cancel_rebuild(&state).await;
            Ok(RoleSuccess::Shutdown)
        }
    }
}

async fn project_once(
    state: Arc<Mutex<ServingProjectionState>>,
) -> Result<RoleSuccess, RoleFailure> {
    let projection = {
        let state = state.lock().await;
        if state.rebuild.is_some() {
            return Err(RoleFailure::busy("rebuild already in progress"));
        }
        state.projection.clone()
    };
    let report = projection
        .project_once(PROJECT_TIMEOUT)
        .await
        .map_err(|error| RoleFailure::projection(error.to_string()))?;
    Ok(RoleSuccess::Projected(projected_summary(&report)))
}

async fn reload_serving(
    state: Arc<Mutex<ServingProjectionState>>,
) -> Result<RoleSuccess, RoleFailure> {
    let existing = {
        let state = state.lock().await;
        state.serving.clone()
    };
    let status = match existing {
        Some(serving) => serving
            .reload()
            .await
            .map_err(RoleFailure::from_serving_error)?,
        None => {
            let (expected_island, paths) = {
                let state = state.lock().await;
                (state.expected_island.clone(), state.snapshot_paths.clone())
            };
            let serving = ServingActorHandle::spawn(expected_island, paths, STALE_AFTER)
                .map_err(RoleFailure::from_serving_error)?;
            let status = serving
                .status()
                .await
                .map_err(RoleFailure::from_serving_error)?;
            let mut state = state.lock().await;
            state.serving = Some(serving);
            status
        }
    };
    Ok(RoleSuccess::Reloaded(role_serving_status(status)))
}

async fn begin_rebuild(
    state: Arc<Mutex<ServingProjectionState>>,
) -> Result<RoleSuccess, RoleFailure> {
    let token = {
        let mut state = state.lock().await;
        if state.rebuild.is_some() {
            return Err(RoleFailure::busy("rebuild already in progress"));
        }
        let token = state.next_rebuild_token;
        state.next_rebuild_token += 1;
        let (release_tx, release_rx) = oneshot::channel();
        let projection = state.fresh_projection();
        state.projection = projection.clone();
        let handle = tokio::spawn(async move {
            if release_rx.await.is_err() {
                return Err(RoleFailure::cancelled("rebuild cancelled before release"));
            }
            projection
                .project_once(PROJECT_TIMEOUT)
                .await
                .map(|report| projected_summary(&report))
                .map_err(|error| RoleFailure::projection(error.to_string()))
        });
        state.rebuild = Some(RebuildState::Running {
            token,
            release: release_tx,
            handle,
        });
        token
    };
    Ok(RoleSuccess::RebuildStarted { token })
}

async fn await_rebuild(
    state: Arc<Mutex<ServingProjectionState>>,
    token: u64,
) -> Result<RoleSuccess, RoleFailure> {
    let rebuild = {
        let mut state = state.lock().await;
        match state.rebuild.take() {
            Some(RebuildState::Running {
                token: running,
                release,
                handle,
            }) if running == token => (release, handle),
            Some(RebuildState::Completing { token: running }) if running == token => {
                state.rebuild = Some(RebuildState::Completing { token: running });
                return Err(RoleFailure::busy(format!(
                    "rebuild token {token} is already completing"
                )));
            }
            Some(other) => {
                state.rebuild = Some(other);
                return Err(RoleFailure::invalid_request(format!(
                    "no running rebuild token {token}"
                )));
            }
            None => {
                return Err(RoleFailure::invalid_request(format!(
                    "no running rebuild token {token}"
                )));
            }
        }
    };
    let (release, handle) = rebuild;
    {
        let mut state = state.lock().await;
        state.rebuild = Some(RebuildState::Completing { token });
    }
    let _ = release.send(());
    let result = handle
        .await
        .map_err(|error| RoleFailure::internal(format!("rebuild worker join: {error}")))
        .and_then(|result| result);
    {
        let mut state = state.lock().await;
        if matches!(state.rebuild, Some(RebuildState::Completing { token: running }) if running == token)
        {
            state.rebuild = None;
        }
    }
    let summary = result?;
    Ok(RoleSuccess::RebuildFinished { token, summary })
}

async fn cancel_rebuild(state: &Arc<Mutex<ServingProjectionState>>) {
    let rebuild = {
        let mut state = state.lock().await;
        state.rebuild.take()
    };
    if let Some(RebuildState::Running {
        release, handle, ..
    }) = rebuild
    {
        drop(release);
        handle.abort();
    }
}

async fn query_gateway(
    state: Arc<Mutex<ServingProjectionState>>,
    host: String,
) -> Result<RoleSuccess, RoleFailure> {
    let serving = serving_handle(&state).await?;
    let route = serving
        .gateway_route_for_host(host)
        .await
        .map_err(RoleFailure::from_serving_error)?;
    Ok(RoleSuccess::GatewayRoute { route })
}

async fn query_dns(
    state: Arc<Mutex<ServingProjectionState>>,
    name: String,
    record_type: String,
) -> Result<RoleSuccess, RoleFailure> {
    let serving = serving_handle(&state).await?;
    let records = serving
        .dns_records(name, record_type)
        .await
        .map_err(RoleFailure::from_serving_error)?;
    Ok(RoleSuccess::DnsRecords { records })
}

async fn status(state: Arc<Mutex<ServingProjectionState>>) -> Result<RoleSuccess, RoleFailure> {
    let (serving, projection, rebuild) = {
        let state = state.lock().await;
        (
            state.serving.clone(),
            state.projection.clone(),
            state.rebuild.as_ref().map(RebuildState::token),
        )
    };
    let serving = match serving {
        Some(serving) => role_serving_status(
            serving
                .status()
                .await
                .map_err(RoleFailure::from_serving_error)?,
        ),
        None => RoleServingStatus::Unavailable {
            kind: "missing_snapshot".to_string(),
            message: "serving snapshots have not been loaded".to_string(),
        },
    };
    let projection = projection
        .status()
        .await
        .map_err(|error| RoleFailure::projection(error.to_string()))?;
    Ok(RoleSuccess::Status(RoleStatus {
        serving,
        projection: role_projection_status(projection),
        rebuild_in_progress: rebuild,
        mutation: RoleMutationStatus::UnavailableInThisRole,
    }))
}

async fn serving_handle(
    state: &Arc<Mutex<ServingProjectionState>>,
) -> Result<ServingActorHandle, RoleFailure> {
    let serving = {
        let state = state.lock().await;
        state.serving.clone()
    };
    serving
        .ok_or_else(|| RoleFailure::serving_unavailable("serving snapshots have not been loaded"))
}

struct ServingProjectionState {
    expected_island: IslandId,
    projection_session: BusSession,
    source: Arc<ProcessFactSource>,
    root: PathBuf,
    snapshot_paths: ServingSnapshotPaths,
    pattern: FactKeyPattern,
    projection: ProjectionActorHandle,
    serving: Option<ServingActorHandle>,
    rebuild: Option<RebuildState>,
    next_rebuild_token: u64,
}

impl ServingProjectionState {
    fn new(config: ServingProjectionRoleConfig) -> Result<Self, String> {
        let expected_island = IslandId::new("prod");
        let projection_principal = PrincipalId::new("projection");
        let pattern = fact_pattern("/facts/>")?;
        let source = Arc::new(ProcessFactSource::new(
            config.root.join("facts"),
            ProcessFactReadPolicy::allow(
                expected_island.clone(),
                projection_principal.clone(),
                vec![pattern.clone()],
            ),
        )?);
        let projection_session =
            identity_session(expected_island.clone(), projection_principal.clone());
        let snapshot_paths = ServingSnapshotPaths::new(
            config.root.join("gateway.snapshot"),
            config.root.join("dns.snapshot"),
        );
        let projection = spawn_projection(
            Arc::clone(&source),
            expected_island.clone(),
            projection_session.clone(),
            pattern.clone(),
            config.root.as_path(),
            &snapshot_paths,
        );
        Ok(Self {
            expected_island,
            projection_session,
            source,
            root: config.root,
            snapshot_paths,
            pattern,
            projection,
            serving: None,
            rebuild: None,
            next_rebuild_token: 1,
        })
    }

    fn fresh_projection(&self) -> ProjectionActorHandle {
        spawn_projection(
            Arc::clone(&self.source),
            self.expected_island.clone(),
            self.projection_session.clone(),
            self.pattern.clone(),
            self.root.as_path(),
            &self.snapshot_paths,
        )
    }
}

enum RebuildState {
    Running {
        token: u64,
        release: oneshot::Sender<()>,
        handle: JoinHandle<Result<ProjectedSummary, RoleFailure>>,
    },
    Completing {
        token: u64,
    },
}

impl RebuildState {
    fn token(&self) -> u64 {
        match self {
            Self::Running { token, .. } | Self::Completing { token } => *token,
        }
    }
}

fn identity_session(island: IslandId, principal: PrincipalId) -> BusSession {
    let (_bus, authority) = InMemoryBus::new_with_authority();
    authority.grant_in(island, principal, Grant::empty())
}

fn spawn_projection(
    source: Arc<ProcessFactSource>,
    island: IslandId,
    session: BusSession,
    pattern: FactKeyPattern,
    root: &Path,
    snapshot_paths: &ServingSnapshotPaths,
) -> ProjectionActorHandle {
    let source: Arc<dyn FactSource> = source;
    ProjectionActorHandle::spawn(
        source,
        island,
        session,
        pattern,
        SqliteProjectionStore::new(root.join("projections.sqlite")),
        snapshot_paths.gateway.clone(),
        snapshot_paths.dns.clone(),
    )
}

fn projected_summary(report: &ProjectionReport) -> ProjectedSummary {
    ProjectedSummary {
        duration_us: duration_to_us(report.duration),
        gateway_route_count: report
            .state
            .gateway
            .as_ref()
            .map_or(0, |gateway| gateway.routes.len()),
        dns_record_count: report.state.dns.as_ref().map_or(0, |dns| dns.records.len()),
    }
}

fn duration_to_us(duration: Duration) -> u64 {
    let micros = duration.as_micros();
    if micros > u128::from(u64::MAX) {
        return u64::MAX;
    }
    micros as u64
}

fn role_serving_status(status: ServingStatus) -> RoleServingStatus {
    RoleServingStatus::Available {
        gateway_revision: status.loaded_revisions.gateway,
        dns_revision: status.loaded_revisions.dns,
        freshness: status.freshness,
        reload_attempts: status.reload_attempts,
        last_failure: status.last_failure.map(|failure| failure.to_string()),
    }
}

fn role_projection_status(status: ProjectionActorStatus) -> RoleProjectionStatus {
    RoleProjectionStatus {
        hints_seen: status.hints_seen,
        last_success_gateway_routes: status
            .last_success
            .as_ref()
            .map(|success| success.gateway_route_count),
        last_success_dns_records: status
            .last_success
            .as_ref()
            .map(|success| success.dns_record_count),
        last_failure: status.last_failure.map(|failure| failure.message),
    }
}

fn remove_stale_socket(path: &Path) -> Result<(), String> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove stale socket '{}': {error}", path.display())),
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub(crate) enum RoleRequest {
    ProjectOnce,
    Reload,
    BeginRebuild,
    AwaitRebuild { token: u64 },
    QueryGateway { host: String },
    QueryDns { name: String, record_type: String },
    Status,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum RoleResponse {
    Success(RoleSuccess),
    Failure(RoleFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum RoleSuccess {
    Projected(ProjectedSummary),
    Reloaded(RoleServingStatus),
    RebuildStarted {
        token: u64,
    },
    RebuildFinished {
        token: u64,
        summary: ProjectedSummary,
    },
    GatewayRoute {
        route: Option<GatewayRouteProjection>,
    },
    DnsRecords {
        records: Vec<DnsRecordProjection>,
    },
    Status(RoleStatus),
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ProjectedSummary {
    pub(crate) duration_us: u64,
    pub(crate) gateway_route_count: usize,
    pub(crate) dns_record_count: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RoleStatus {
    pub(crate) serving: RoleServingStatus,
    pub(crate) projection: RoleProjectionStatus,
    pub(crate) rebuild_in_progress: Option<u64>,
    pub(crate) mutation: RoleMutationStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub(crate) enum RoleServingStatus {
    Unavailable {
        kind: String,
        message: String,
    },
    Available {
        gateway_revision: String,
        dns_revision: String,
        freshness: ServingFreshness,
        reload_attempts: u64,
        last_failure: Option<String>,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RoleProjectionStatus {
    pub(crate) hints_seen: u64,
    pub(crate) last_success_gateway_routes: Option<usize>,
    pub(crate) last_success_dns_records: Option<usize>,
    pub(crate) last_failure: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RoleMutationStatus {
    UnavailableInThisRole,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct RoleFailure {
    pub(crate) kind: RoleFailureKind,
    pub(crate) message: String,
}

impl RoleFailure {
    fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            kind: RoleFailureKind::InvalidRequest,
            message: message.into(),
        }
    }

    fn busy(message: impl Into<String>) -> Self {
        Self {
            kind: RoleFailureKind::Busy,
            message: message.into(),
        }
    }

    fn projection(message: impl Into<String>) -> Self {
        Self {
            kind: RoleFailureKind::Projection,
            message: message.into(),
        }
    }

    fn serving_unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: RoleFailureKind::ServingUnavailable,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: RoleFailureKind::Internal,
            message: message.into(),
        }
    }

    fn cancelled(message: impl Into<String>) -> Self {
        Self {
            kind: RoleFailureKind::Cancelled,
            message: message.into(),
        }
    }

    fn from_serving_error(error: ServingError) -> Self {
        match error {
            ServingError::SnapshotLoad { failure } => Self {
                kind: RoleFailureKind::ServingUnavailable,
                message: failure.to_string(),
            },
            ServingError::ActorUnavailable { operation, reason } => Self {
                kind: RoleFailureKind::Internal,
                message: format!("{operation}: {reason}"),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum RoleFailureKind {
    InvalidRequest,
    Busy,
    Projection,
    ServingUnavailable,
    Internal,
    Cancelled,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RoleClientError {
    Transport(String),
    Failure(RoleFailure),
}

pub(crate) async fn request_role(
    socket: &Path,
    request: &RoleRequest,
) -> Result<RoleSuccess, RoleClientError> {
    let mut stream = timeout(ROLE_CONNECT_TIMEOUT, UnixStream::connect(socket))
        .await
        .map_err(|_| RoleClientError::Transport("connect role socket timed out".to_string()))?
        .map_err(|error| {
            RoleClientError::Transport(format!(
                "connect role socket '{}': {error}",
                socket.display()
            ))
        })?;
    let bytes = serde_json::to_vec(request)
        .map_err(|error| RoleClientError::Transport(format!("serialize request: {error}")))?;
    write_with_timeout(&mut stream, &bytes, "role request")
        .await
        .map_err(RoleClientError::Transport)?;
    shutdown_stream(&mut stream, "role request")
        .await
        .map_err(RoleClientError::Transport)?;
    let mut response = Vec::new();
    timeout(ROLE_RESPONSE_TIMEOUT, stream.read_to_end(&mut response))
        .await
        .map_err(|_| RoleClientError::Transport("read role response timed out".to_string()))?
        .map_err(|error| RoleClientError::Transport(format!("read role response: {error}")))?;
    match serde_json::from_slice::<RoleResponse>(&response)
        .map_err(|error| RoleClientError::Transport(format!("parse role response: {error}")))?
    {
        RoleResponse::Success(success) => Ok(success),
        RoleResponse::Failure(failure) => Err(RoleClientError::Failure(failure)),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

    use mvp_bus::Payload;
    use mvp_deploy::{
        DnsCommitId, GatewayCommitId, RouteCommitId, ServingCommitId, ServingCommitPlan,
    };
    use mvp_projection::{BackendEndpoint, DnsRecordFact, NodeId, ProjectionFactPayload, RouteId};
    use tokio::task::JoinHandle;
    use tokio::time::{Duration, sleep, timeout};

    use super::*;

    static TEST_ID: AtomicU64 = AtomicU64::new(0);

    fn root(test_name: &str) -> PathBuf {
        let id = TEST_ID.fetch_add(1, Ordering::SeqCst);
        let root = std::env::temp_dir().join(format!(
            "ployz-mvp-role-harness-{test_name}-{}-{id}",
            std::process::id()
        ));
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).expect("create root");
        root
    }

    fn spawn_role(root: &Path) -> (PathBuf, JoinHandle<Result<(), String>>) {
        let socket = root.join("serving.sock");
        let config = ServingProjectionRoleConfig::new(root, &socket);
        let handle = tokio::spawn(run_serving_projection_role(config));
        (socket, handle)
    }

    async fn wait_ready(socket: &Path) -> Result<(), String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match request_role(socket, &RoleRequest::Status).await {
                Ok(RoleSuccess::Status(_)) => return Ok(()),
                Ok(other) => return Err(format!("unexpected ready response: {other:?}")),
                Err(error) if std::time::Instant::now() < deadline => {
                    let _ = error;
                    sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(format!("role did not become ready: {error:?}")),
            }
        }
    }

    fn fact_source(root: &Path) -> ProcessFactSource {
        ProcessFactSource::new(
            root.join("facts"),
            ProcessFactReadPolicy::allow(
                IslandId::new("prod"),
                PrincipalId::new("projection"),
                vec![fact_pattern("/facts/>").expect("fact pattern")],
            ),
        )
        .expect("process fact source")
    }

    fn serving_commit(id: &str, backend: &str, dns: &str, epoch: u64) -> ServingCommitPlan {
        ServingCommitPlan {
            serving_commit_id: ServingCommitId::new(id),
            route_commit_id: RouteCommitId::new(format!("{id}-route")),
            gateway_commit_id: GatewayCommitId::new(format!("{id}-gateway")),
            dns_commit_id: DnsCommitId::new(format!("{id}-dns")),
            route_id: RouteId::new("web-http"),
            hostnames: vec!["web.example.test".to_string()],
            active_backends: vec![BackendEndpoint {
                node_id: NodeId::new("node-web"),
                address: backend.to_string(),
            }],
            old_backends_to_drain: Vec::new(),
            dns_records: vec![DnsRecordFact {
                name: "web.example.test".to_string(),
                record_type: "AAAA".to_string(),
                value: dns.to_string(),
                ttl_seconds: 30,
            }],
            epoch,
        }
    }

    fn write_serving_fact(root: &Path, commit: &ServingCommitPlan) {
        let payload = ProjectionFactPayload::ServingCommit(mvp_projection::ServingCommitFact {
            serving_commit_id: commit.serving_commit_id.to_string(),
            route_commit_id: commit.route_commit_id.to_string(),
            gateway_commit_id: commit.gateway_commit_id.to_string(),
            dns_commit_id: commit.dns_commit_id.to_string(),
            route_id: commit.route_id.clone(),
            hostnames: commit.hostnames.clone(),
            backends: commit.active_backends.clone(),
            old_backends_to_drain: commit.old_backends_to_drain.clone(),
            dns_records: commit.dns_records.clone(),
            epoch: commit.epoch,
        })
        .to_fact_bytes()
        .expect("payload serializes");
        fact_source(root)
            .write_payload(
                &IslandId::new("prod"),
                &PrincipalId::new("coordinator"),
                mvp_bus::FactKey::parse(format!("/facts/serving/{}", commit.serving_commit_id))
                    .expect("fact key"),
                Payload::from(payload),
            )
            .expect("write serving fact");
    }

    async fn shutdown_role(
        socket: &Path,
        handle: JoinHandle<Result<(), String>>,
    ) -> Result<(), String> {
        request_role(socket, &RoleRequest::Shutdown)
            .await
            .map_err(|error| format!("shutdown request: {error:?}"))?;
        timeout(Duration::from_secs(2), handle)
            .await
            .map_err(|_| "role did not exit after shutdown".to_string())?
            .map_err(|error| format!("join role after shutdown: {error}"))?
    }

    #[tokio::test]
    async fn role_reports_serving_unavailable_until_snapshots_exist() {
        let root = root("unavailable");
        let (socket, handle) = spawn_role(&root);
        wait_ready(&socket).await.expect("role ready");

        let status = request_role(&socket, &RoleRequest::Status)
            .await
            .expect("status");
        let RoleSuccess::Status(status) = status else {
            panic!("expected status");
        };
        assert!(matches!(
            status.serving,
            RoleServingStatus::Unavailable { .. }
        ));
        assert_eq!(status.mutation, RoleMutationStatus::UnavailableInThisRole);
        let error = request_role(
            &socket,
            &RoleRequest::QueryGateway {
                host: "web.example.test".to_string(),
            },
        )
        .await
        .expect_err("query before snapshots should fail");
        assert!(matches!(
            error,
            RoleClientError::Failure(RoleFailure {
                kind: RoleFailureKind::ServingUnavailable,
                ..
            })
        ));

        shutdown_role(&socket, handle).await.expect("shutdown role");
    }

    #[tokio::test]
    async fn role_projects_reloads_and_answers_gateway_and_dns() {
        let root = root("query");
        write_serving_fact(
            &root,
            &serving_commit("serving-1", "fd00::1:8080", "fd00::1", 1),
        );
        let (socket, handle) = spawn_role(&root);
        wait_ready(&socket).await.expect("role ready");

        request_role(&socket, &RoleRequest::ProjectOnce)
            .await
            .expect("project once");
        request_role(&socket, &RoleRequest::Reload)
            .await
            .expect("reload");
        let route = request_role(
            &socket,
            &RoleRequest::QueryGateway {
                host: "WEB.EXAMPLE.TEST".to_string(),
            },
        )
        .await
        .expect("query gateway");
        let RoleSuccess::GatewayRoute { route: Some(route) } = route else {
            panic!("expected gateway route");
        };
        assert_eq!(route.backends[0].address, "fd00::1:8080");

        let records = request_role(
            &socket,
            &RoleRequest::QueryDns {
                name: "web.example.test".to_string(),
                record_type: "aaaa".to_string(),
            },
        )
        .await
        .expect("query dns");
        let RoleSuccess::DnsRecords { records } = records else {
            panic!("expected dns records");
        };
        assert_eq!(records[0].value, "fd00::1");

        shutdown_role(&socket, handle).await.expect("shutdown role");
    }

    #[tokio::test]
    async fn begin_rebuild_keeps_last_good_queries_available() {
        let root = root("rebuild");
        write_serving_fact(
            &root,
            &serving_commit("serving-1", "fd00::1:8080", "fd00::1", 1),
        );
        let (socket, handle) = spawn_role(&root);
        wait_ready(&socket).await.expect("role ready");
        request_role(&socket, &RoleRequest::ProjectOnce)
            .await
            .expect("project once");
        request_role(&socket, &RoleRequest::Reload)
            .await
            .expect("reload");
        fs::remove_file(root.join("projections.sqlite")).expect("remove sqlite");

        let started = request_role(&socket, &RoleRequest::BeginRebuild)
            .await
            .expect("begin rebuild");
        let RoleSuccess::RebuildStarted { token } = started else {
            panic!("expected rebuild token");
        };
        let status = request_role(&socket, &RoleRequest::Status)
            .await
            .expect("status");
        let RoleSuccess::Status(status) = status else {
            panic!("expected status");
        };
        assert_eq!(status.rebuild_in_progress, Some(token));

        let route = request_role(
            &socket,
            &RoleRequest::QueryGateway {
                host: "web.example.test".to_string(),
            },
        )
        .await
        .expect("query during rebuild");
        assert!(matches!(
            route,
            RoleSuccess::GatewayRoute { route: Some(_) }
        ));

        request_role(&socket, &RoleRequest::AwaitRebuild { token })
            .await
            .expect("await rebuild");
        let status = request_role(&socket, &RoleRequest::Status)
            .await
            .expect("status");
        let RoleSuccess::Status(status) = status else {
            panic!("expected status");
        };
        assert_eq!(status.rebuild_in_progress, None);

        shutdown_role(&socket, handle).await.expect("shutdown role");
    }

    #[tokio::test]
    async fn stalled_client_does_not_block_later_requests() {
        let root = root("stalled-client");
        let (socket, handle) = spawn_role(&root);
        wait_ready(&socket).await.expect("role ready");
        let _stalled = UnixStream::connect(&socket)
            .await
            .expect("connect stalled client");
        sleep(ROLE_REQUEST_READ_TIMEOUT + Duration::from_millis(100)).await;

        let status = request_role(&socket, &RoleRequest::Status)
            .await
            .expect("status after stalled client");
        assert!(matches!(status, RoleSuccess::Status(_)));

        shutdown_role(&socket, handle).await.expect("shutdown role");
    }

    #[tokio::test]
    async fn shutdown_after_begin_rebuild_cancels_pending_rebuild() {
        let root = root("shutdown-rebuild");
        write_serving_fact(
            &root,
            &serving_commit("serving-1", "fd00::1:8080", "fd00::1", 1),
        );
        let (socket, handle) = spawn_role(&root);
        wait_ready(&socket).await.expect("role ready");

        request_role(&socket, &RoleRequest::BeginRebuild)
            .await
            .expect("begin rebuild");

        shutdown_role(&socket, handle).await.expect("shutdown role");
    }

    #[tokio::test]
    async fn shutdown_exits_role() {
        let root = root("shutdown");
        let (socket, handle) = spawn_role(&root);
        wait_ready(&socket).await.expect("role ready");

        request_role(&socket, &RoleRequest::Shutdown)
            .await
            .expect("shutdown");
        let result = timeout(Duration::from_secs(2), handle)
            .await
            .expect("role exits")
            .expect("join role");

        assert!(result.is_ok());
    }
}
