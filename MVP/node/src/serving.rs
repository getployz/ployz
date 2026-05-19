use std::fmt::{self, Display, Formatter};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use mvp_serving::{
    DnsServerHandle, GatewayHandle, GatewayOptions, ServingFailure, ServingFreshness,
    ServingSnapshotPaths, ServingStatus, WireRoleMetrics, WireServingState, spawn_dns_server,
    spawn_gateway,
};
use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::Mutex;
use tokio::time::timeout;

use crate::error::{NodeError, NodeResult};
use crate::state::load_node;

const CONTROL_REQUEST_TIMEOUT: Duration = Duration::from_secs(5);
const CONTROL_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_CONTROL_REQUEST_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServingRoleKind {
    Gateway,
    Dns,
}

impl Display for ServingRoleKind {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Gateway => f.write_str("gateway"),
            Self::Dns => f.write_str("dns"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServingRoleOptions {
    pub state_dir: PathBuf,
    pub listen: SocketAddr,
    pub control_socket: PathBuf,
    pub stale_after: Duration,
}

impl ServingRoleOptions {
    #[must_use]
    pub fn new(
        state_dir: impl Into<PathBuf>,
        listen: SocketAddr,
        control_socket: impl Into<PathBuf>,
    ) -> Self {
        Self {
            state_dir: state_dir.into(),
            listen,
            control_socket: control_socket.into(),
            stale_after: Duration::from_secs(30),
        }
    }

    #[must_use]
    pub fn with_stale_after(mut self, stale_after: Duration) -> Self {
        self.stale_after = stale_after;
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ServingRoleRequest {
    Readiness,
    Status,
    Reload,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", content = "data", rename_all = "snake_case")]
pub enum ServingRoleResponse {
    Success(ServingRoleSuccess),
    Failure(ServingRoleFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "event", rename_all = "snake_case")]
pub enum ServingRoleSuccess {
    Status(Box<ServingRoleProcessStatus>),
    Reloaded(Box<ServingRoleProcessStatus>),
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServingRoleProcessStatus {
    pub kind: ServingRoleKind,
    pub listen_addr: SocketAddr,
    pub serving: ServingRoleServingStatus,
    pub metrics: WireRoleMetrics,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServingRoleServingStatus {
    pub loaded_revisions: ServingRoleRevisions,
    pub loaded_at_ms: u64,
    pub snapshot_age_ms: u64,
    pub freshness: ServingFreshness,
    pub acme_http01_challenge_count: usize,
    pub reload_attempts: u64,
    pub last_reload_attempt_at_ms: Option<u64>,
    pub last_reload_success_at_ms: Option<u64>,
    pub last_failure: Option<ServingFailure>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServingRoleRevisions {
    pub generation: String,
    pub gateway: String,
    pub dns: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ServingRoleFailure {
    pub kind: ServingRoleFailureKind,
    pub message: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServingRoleFailureKind {
    InvalidRequest,
    ServingUnavailable,
    WireUnavailable,
}

impl ServingRoleFailure {
    fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            kind: ServingRoleFailureKind::InvalidRequest,
            message: message.into(),
        }
    }

    fn serving_unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: ServingRoleFailureKind::ServingUnavailable,
            message: message.into(),
        }
    }

    fn wire_unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: ServingRoleFailureKind::WireUnavailable,
            message: message.into(),
        }
    }
}

enum ServingWireHandle {
    Gateway(GatewayHandle),
    Dns(DnsServerHandle),
}

impl ServingWireHandle {
    fn listen_addr(&self) -> SocketAddr {
        match self {
            Self::Gateway(handle) => handle.listen_addr(),
            Self::Dns(handle) => handle.listen_addr(),
        }
    }

    fn metrics(&self) -> WireRoleMetrics {
        match self {
            Self::Gateway(handle) => handle.metrics(),
            Self::Dns(handle) => handle.metrics(),
        }
    }

    async fn shutdown(self) -> Result<(), String> {
        match self {
            Self::Gateway(handle) => handle.shutdown().await.map_err(|error| error.to_string()),
            Self::Dns(handle) => handle.shutdown().await.map_err(|error| error.to_string()),
        }
    }
}

struct ServingRoleState {
    kind: ServingRoleKind,
    wire: WireServingState,
    server: Option<ServingWireHandle>,
}

pub async fn run_gateway_role(options: ServingRoleOptions) -> NodeResult<()> {
    run_serving_role(ServingRoleKind::Gateway, options).await
}

pub async fn run_dns_role(options: ServingRoleOptions) -> NodeResult<()> {
    run_serving_role(ServingRoleKind::Dns, options).await
}

async fn run_serving_role(kind: ServingRoleKind, options: ServingRoleOptions) -> NodeResult<()> {
    remove_stale_socket(options.control_socket.as_path())?;
    if let Some(parent) = options
        .control_socket
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        ensure_private_control_parent(parent)?;
    }
    let listener = UnixListener::bind(&options.control_socket).map_err(|source| {
        NodeError::ServingControlSocket {
            path: options.control_socket.clone(),
            operation: "bind",
            source,
        }
    })?;
    set_control_socket_permissions(options.control_socket.as_path())?;
    let node = load_node(options.state_dir)?;
    let snapshots = ServingSnapshotPaths::new(
        node.paths().gateway_snapshot.clone(),
        node.paths().dns_snapshot.clone(),
    );
    let serving =
        mvp_serving::ServingActorHandle::spawn(node.island(), snapshots, options.stale_after)
            .map_err(|source| NodeError::Serving { source })?;
    let wire = WireServingState::new(serving);
    let server = match kind {
        ServingRoleKind::Gateway => ServingWireHandle::Gateway(
            spawn_gateway(GatewayOptions::new(options.listen), wire.clone())
                .await
                .map_err(|source| NodeError::HttpGateway { source })?,
        ),
        ServingRoleKind::Dns => ServingWireHandle::Dns(
            spawn_dns_server(options.listen, wire.clone())
                .await
                .map_err(|source| NodeError::DnsServer { source })?,
        ),
    };
    let state = Mutex::new(ServingRoleState {
        kind,
        wire,
        server: Some(server),
    });

    loop {
        let (stream, _addr) =
            listener
                .accept()
                .await
                .map_err(|source| NodeError::ServingControlSocket {
                    path: options.control_socket.clone(),
                    operation: "accept",
                    source,
                })?;
        if handle_control_connection(stream, &state).await? {
            break;
        }
    }
    Ok(())
}

async fn handle_control_connection(
    mut stream: UnixStream,
    state: &Mutex<ServingRoleState>,
) -> NodeResult<bool> {
    let request = match read_control_request(&mut stream).await {
        Ok(bytes) => match serde_json::from_slice::<ServingRoleRequest>(&bytes) {
            Ok(request) => request,
            Err(source) => {
                let response = ServingRoleResponse::Failure(ServingRoleFailure::invalid_request(
                    source.to_string(),
                ));
                write_control_response(&mut stream, &response).await?;
                return Ok(false);
            }
        },
        Err(failure) => {
            let response = ServingRoleResponse::Failure(failure);
            write_control_response(&mut stream, &response).await?;
            return Ok(false);
        }
    };
    let response = handle_control_request(request, state).await;
    let should_shutdown = matches!(
        response,
        ServingRoleResponse::Success(ServingRoleSuccess::Shutdown)
    );
    write_control_response(&mut stream, &response).await?;
    Ok(should_shutdown)
}

async fn handle_control_request(
    request: ServingRoleRequest,
    state: &Mutex<ServingRoleState>,
) -> ServingRoleResponse {
    match request {
        ServingRoleRequest::Readiness | ServingRoleRequest::Status => {
            status_response(state, ServingRoleStatusKind::Status).await
        }
        ServingRoleRequest::Reload => {
            let wire = {
                let state = state.lock().await;
                state.wire.clone()
            };
            match wire.reload().await {
                Ok(_) => status_response(state, ServingRoleStatusKind::Reloaded).await,
                Err(error) => ServingRoleResponse::Failure(
                    ServingRoleFailure::serving_unavailable(error.to_string()),
                ),
            }
        }
        ServingRoleRequest::Shutdown => {
            let server = {
                let mut state = state.lock().await;
                state.server.take()
            };
            match server {
                Some(server) => match server.shutdown().await {
                    Ok(()) => ServingRoleResponse::Success(ServingRoleSuccess::Shutdown),
                    Err(message) => {
                        ServingRoleResponse::Failure(ServingRoleFailure::wire_unavailable(message))
                    }
                },
                None => ServingRoleResponse::Success(ServingRoleSuccess::Shutdown),
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ServingRoleStatusKind {
    Status,
    Reloaded,
}

async fn status_response(
    state: &Mutex<ServingRoleState>,
    status_kind: ServingRoleStatusKind,
) -> ServingRoleResponse {
    let (kind, wire, listen_addr, metrics) = {
        let state = state.lock().await;
        let Some(server) = &state.server else {
            return ServingRoleResponse::Failure(ServingRoleFailure::wire_unavailable(
                "serving role is shut down",
            ));
        };
        (
            state.kind,
            state.wire.clone(),
            server.listen_addr(),
            server.metrics(),
        )
    };
    let status = match wire.status().await {
        Ok(status) => status,
        Err(error) => {
            return ServingRoleResponse::Failure(ServingRoleFailure::serving_unavailable(
                error.to_string(),
            ));
        }
    };
    let status = ServingRoleProcessStatus {
        kind,
        listen_addr,
        serving: role_serving_status(status),
        metrics,
    };
    match status_kind {
        ServingRoleStatusKind::Status => {
            ServingRoleResponse::Success(ServingRoleSuccess::Status(Box::new(status)))
        }
        ServingRoleStatusKind::Reloaded => {
            ServingRoleResponse::Success(ServingRoleSuccess::Reloaded(Box::new(status)))
        }
    }
}

fn role_serving_status(status: ServingStatus) -> ServingRoleServingStatus {
    ServingRoleServingStatus {
        loaded_revisions: ServingRoleRevisions {
            generation: status.loaded_revisions.generation,
            gateway: status.loaded_revisions.gateway,
            dns: status.loaded_revisions.dns,
        },
        loaded_at_ms: system_time_ms(status.loaded_at),
        snapshot_age_ms: duration_ms(status.snapshot_age),
        freshness: status.freshness,
        acme_http01_challenge_count: status.acme_http01_challenge_count,
        reload_attempts: status.reload_attempts,
        last_reload_attempt_at_ms: status.last_reload_attempt_at.map(system_time_ms),
        last_reload_success_at_ms: status.last_reload_success_at.map(system_time_ms),
        last_failure: status.last_failure,
    }
}

async fn read_control_request(stream: &mut UnixStream) -> Result<Vec<u8>, ServingRoleFailure> {
    timeout(CONTROL_REQUEST_TIMEOUT, read_bounded(stream))
        .await
        .map_err(|_| ServingRoleFailure::invalid_request("read control request timed out"))?
}

async fn read_bounded(stream: &mut UnixStream) -> Result<Vec<u8>, ServingRoleFailure> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| ServingRoleFailure::invalid_request(error.to_string()))?;
        if read == 0 {
            return Ok(request);
        }
        if request.len() + read > MAX_CONTROL_REQUEST_BYTES {
            return Err(ServingRoleFailure::invalid_request(format!(
                "control request exceeds {MAX_CONTROL_REQUEST_BYTES} bytes"
            )));
        }
        request.extend_from_slice(&chunk[..read]);
    }
}

async fn write_control_response(
    stream: &mut UnixStream,
    response: &ServingRoleResponse,
) -> NodeResult<()> {
    let bytes = serde_json::to_vec(response)
        .map_err(|source| NodeError::EncodeServingRoleResponse { source })?;
    timeout(CONTROL_WRITE_TIMEOUT, stream.write_all(&bytes))
        .await
        .map_err(|_| NodeError::ServingControlSocket {
            path: PathBuf::from("<accepted>"),
            operation: "write response timeout",
            source: std::io::Error::new(std::io::ErrorKind::TimedOut, "write response timed out"),
        })?
        .map_err(|source| NodeError::ServingControlSocket {
            path: PathBuf::from("<accepted>"),
            operation: "write response",
            source,
        })?;
    timeout(CONTROL_WRITE_TIMEOUT, stream.shutdown())
        .await
        .map_err(|_| NodeError::ServingControlSocket {
            path: PathBuf::from("<accepted>"),
            operation: "shutdown response timeout",
            source: std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "shutdown response timed out",
            ),
        })?
        .map_err(|source| NodeError::ServingControlSocket {
            path: PathBuf::from("<accepted>"),
            operation: "shutdown response",
            source,
        })
}

fn remove_stale_socket(path: &Path) -> NodeResult<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(source) => Err(NodeError::ServingControlSocket {
            path: path.to_path_buf(),
            operation: "remove stale",
            source,
        }),
    }
}

#[cfg(unix)]
fn ensure_private_control_parent(parent: &Path) -> NodeResult<()> {
    use std::os::unix::fs::PermissionsExt;

    match std::fs::metadata(parent) {
        Ok(_metadata) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(parent).map_err(|source| NodeError::ServingControlSocket {
                path: parent.to_path_buf(),
                operation: "create parent directory",
                source,
            })?;
            let mut permissions = std::fs::metadata(parent)
                .map_err(|source| NodeError::ServingControlSocket {
                    path: parent.to_path_buf(),
                    operation: "read parent permissions",
                    source,
                })?
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(parent, permissions).map_err(|source| {
                NodeError::ServingControlSocket {
                    path: parent.to_path_buf(),
                    operation: "set parent permissions",
                    source,
                }
            })?;
        }
        Err(source) => {
            return Err(NodeError::ServingControlSocket {
                path: parent.to_path_buf(),
                operation: "read parent permissions",
                source,
            });
        }
    }
    Ok(())
}

#[cfg(not(unix))]
fn ensure_private_control_parent(parent: &Path) -> NodeResult<()> {
    std::fs::create_dir_all(parent).map_err(|source| NodeError::ServingControlSocket {
        path: parent.to_path_buf(),
        operation: "create parent directory",
        source,
    })
}

#[cfg(unix)]
fn set_control_socket_permissions(path: &Path) -> NodeResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)
        .map_err(|source| NodeError::ServingControlSocket {
            path: path.to_path_buf(),
            operation: "read socket permissions",
            source,
        })?
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions).map_err(|source| NodeError::ServingControlSocket {
        path: path.to_path_buf(),
        operation: "set socket permissions",
        source,
    })
}

#[cfg(not(unix))]
fn set_control_socket_permissions(_path: &Path) -> NodeResult<()> {
    Ok(())
}

fn duration_ms(duration: Duration) -> u64 {
    let millis = duration.as_millis();
    if millis > u128::from(u64::MAX) {
        return u64::MAX;
    }
    millis as u64
}

fn system_time_ms(time: SystemTime) -> u64 {
    let millis = time
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    if millis > u128::from(u64::MAX) {
        return u64::MAX;
    }
    millis as u64
}
