use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use mvp_p2panda_facts::SharedPandaFactStore;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::oneshot;
use tokio::task::LocalSet;
use tokio::time::timeout;

use super::daemon_control_protocol::{
    DaemonControlRequest, daemon_deploy_json, daemon_failure_json, daemon_status_json,
    parse_daemon_control_request,
};
use crate::deploy::deploy_product_service_with_context;
use crate::error::{NodeError, NodeResult};
use crate::state::LoadedNodeState;

const DAEMON_CONTROL_READ_TIMEOUT: Duration = Duration::from_secs(5);
const DAEMON_CONTROL_WRITE_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_DAEMON_CONTROL_REQUEST_BYTES: usize = 16 * 1024;

pub(super) struct DaemonControlRuntime {
    state: Arc<DaemonControlSharedState>,
    shutdown: oneshot::Sender<()>,
    task: std::thread::JoinHandle<NodeResult<()>>,
}

impl DaemonControlRuntime {
    pub(super) fn record_import_progress(&self, batches: u64, operations: u64) {
        self.state
            .imported_batches
            .store(batches, Ordering::Relaxed);
        self.state
            .imported_operations
            .store(operations, Ordering::Relaxed);
    }

    pub(super) async fn shutdown(self) -> NodeResult<()> {
        let _ = self.shutdown.send(());
        match self.task.join() {
            Ok(result) => result,
            Err(error) => Err(NodeError::DaemonControlTask {
                message: format!("{error:?}"),
            }),
        }
    }
}

struct DaemonControlSharedState {
    imported_batches: AtomicU64,
    imported_operations: AtomicU64,
    node_agent_handlers: usize,
}

pub(super) struct DaemonControlTaskOptions {
    pub(super) socket_path: PathBuf,
    pub(super) state: LoadedNodeState,
    pub(super) product_bus: mvp_bus::BusActorHandle,
    pub(super) operator_session: mvp_bus::BusSession,
    pub(super) facts: SharedPandaFactStore,
    pub(super) fact_session: mvp_bus::BusSession,
    pub(super) node_agent_handlers: usize,
}

pub(super) fn start_daemon_control_task(
    options: DaemonControlTaskOptions,
) -> NodeResult<DaemonControlRuntime> {
    let listener = open_daemon_control_socket(&options.socket_path)?;
    let shared_state = Arc::new(DaemonControlSharedState {
        imported_batches: AtomicU64::new(0),
        imported_operations: AtomicU64::new(0),
        node_agent_handlers: options.node_agent_handlers,
    });
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let task_state = Arc::clone(&shared_state);
    let task = std::thread::spawn(move || {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .enable_time()
            .build()
            .map_err(|source| NodeError::Runtime { source })?;
        let local = LocalSet::new();
        local.block_on(
            &runtime,
            run_daemon_control_task(listener, shutdown_rx, task_state, options),
        )
    });
    Ok(DaemonControlRuntime {
        state: shared_state,
        shutdown: shutdown_tx,
        task,
    })
}

fn open_daemon_control_socket(path: &Path) -> NodeResult<std::os::unix::net::UnixListener> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        ensure_private_control_parent(parent)?;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
        Err(source) => {
            return Err(NodeError::DaemonControlSocket {
                path: path.to_path_buf(),
                operation: "remove stale socket",
                source,
            });
        }
    }
    let listener = std::os::unix::net::UnixListener::bind(path).map_err(|source| {
        NodeError::DaemonControlSocket {
            path: path.to_path_buf(),
            operation: "bind",
            source,
        }
    })?;
    set_control_socket_permissions(path)?;
    listener
        .set_nonblocking(true)
        .map_err(|source| NodeError::DaemonControlSocket {
            path: path.to_path_buf(),
            operation: "set nonblocking",
            source,
        })?;
    Ok(listener)
}

#[cfg(unix)]
fn ensure_private_control_parent(parent: &Path) -> NodeResult<()> {
    use std::os::unix::fs::PermissionsExt;

    match std::fs::metadata(parent) {
        Ok(_metadata) => {}
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            std::fs::create_dir_all(parent).map_err(|source| NodeError::DaemonControlSocket {
                path: parent.to_path_buf(),
                operation: "create parent",
                source,
            })?;
            let mut permissions = std::fs::metadata(parent)
                .map_err(|source| NodeError::DaemonControlSocket {
                    path: parent.to_path_buf(),
                    operation: "read parent permissions",
                    source,
                })?
                .permissions();
            permissions.set_mode(0o700);
            std::fs::set_permissions(parent, permissions).map_err(|source| {
                NodeError::DaemonControlSocket {
                    path: parent.to_path_buf(),
                    operation: "set parent permissions",
                    source,
                }
            })?;
        }
        Err(source) => {
            return Err(NodeError::DaemonControlSocket {
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
    std::fs::create_dir_all(parent).map_err(|source| NodeError::DaemonControlSocket {
        path: parent.to_path_buf(),
        operation: "create parent",
        source,
    })
}

#[cfg(unix)]
fn set_control_socket_permissions(path: &Path) -> NodeResult<()> {
    use std::os::unix::fs::PermissionsExt;

    let mut permissions = std::fs::metadata(path)
        .map_err(|source| NodeError::DaemonControlSocket {
            path: path.to_path_buf(),
            operation: "read socket permissions",
            source,
        })?
        .permissions();
    permissions.set_mode(0o600);
    std::fs::set_permissions(path, permissions).map_err(|source| NodeError::DaemonControlSocket {
        path: path.to_path_buf(),
        operation: "set socket permissions",
        source,
    })
}

#[cfg(not(unix))]
fn set_control_socket_permissions(_path: &Path) -> NodeResult<()> {
    Ok(())
}

async fn run_daemon_control_task(
    listener: std::os::unix::net::UnixListener,
    mut shutdown: oneshot::Receiver<()>,
    shared_state: Arc<DaemonControlSharedState>,
    options: DaemonControlTaskOptions,
) -> NodeResult<()> {
    let listener =
        UnixListener::from_std(listener).map_err(|source| NodeError::DaemonControlSocket {
            path: options.socket_path.clone(),
            operation: "convert to async listener",
            source,
        })?;
    loop {
        tokio::select! {
            biased;
            _ = &mut shutdown => return Ok(()),
            accepted = listener.accept() => {
                let (stream, _addr) = accepted.map_err(|source| NodeError::DaemonControlSocket {
                    path: options.socket_path.clone(),
                    operation: "accept",
                    source,
                })?;
                tokio::task::spawn_local(handle_daemon_control_connection(
                    stream,
                    options.socket_path.clone(),
                    options.state.clone(),
                    options.product_bus.clone(),
                    options.operator_session.clone(),
                    options.facts.clone(),
                    options.fact_session.clone(),
                    Arc::clone(&shared_state),
                ));
            }
        }
    }
}

async fn handle_daemon_control_connection(
    mut stream: UnixStream,
    socket_path: PathBuf,
    state: LoadedNodeState,
    product_bus: mvp_bus::BusActorHandle,
    operator_session: mvp_bus::BusSession,
    facts: SharedPandaFactStore,
    fact_session: mvp_bus::BusSession,
    shared_state: Arc<DaemonControlSharedState>,
) {
    let response = match read_daemon_control_request(&mut stream).await {
        Ok(bytes) => match parse_daemon_control_request(&bytes) {
            Some(DaemonControlRequest::Status) => daemon_status_json(
                state.node_id_str(),
                shared_state.imported_batches.load(Ordering::Relaxed),
                shared_state.imported_operations.load(Ordering::Relaxed),
                shared_state.node_agent_handlers,
            ),
            Some(DaemonControlRequest::Deploy(request)) => {
                match deploy_product_service_with_context(
                    request.into_options(state.paths().state_dir.clone()),
                    product_bus,
                    operator_session,
                    facts,
                    fact_session,
                    &state,
                )
                .await
                {
                    Ok(report) => daemon_deploy_json(report),
                    Err(error) => daemon_failure_json(error.to_string()),
                }
            }
            None => daemon_failure_json("invalid daemon control request"),
        },
        Err(error) => daemon_failure_json(error),
    };
    let response = match response {
        Ok(response) => response,
        Err(error) => format!(r#"{{"status":"failed","error":"{}"}}"#, error),
    };
    let _ = write_daemon_control_response(&mut stream, socket_path, response.into_bytes()).await;
}

async fn read_daemon_control_request(stream: &mut UnixStream) -> Result<Vec<u8>, String> {
    timeout(
        DAEMON_CONTROL_READ_TIMEOUT,
        read_daemon_control_request_bounded(stream),
    )
    .await
    .map_err(|_| "read daemon control request timed out".to_string())?
}

async fn read_daemon_control_request_bounded(stream: &mut UnixStream) -> Result<Vec<u8>, String> {
    let mut request = Vec::new();
    let mut chunk = [0_u8; 1024];
    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| error.to_string())?;
        if read == 0 {
            return Ok(request);
        }
        if request.len() + read > MAX_DAEMON_CONTROL_REQUEST_BYTES {
            return Err(format!(
                "daemon control request exceeds {MAX_DAEMON_CONTROL_REQUEST_BYTES} bytes"
            ));
        }
        request.extend_from_slice(&chunk[..read]);
    }
}

async fn write_daemon_control_response(
    stream: &mut UnixStream,
    socket_path: PathBuf,
    response: Vec<u8>,
) -> NodeResult<()> {
    timeout(DAEMON_CONTROL_WRITE_TIMEOUT, stream.write_all(&response))
        .await
        .map_err(|_| NodeError::DaemonControlSocket {
            path: socket_path.clone(),
            operation: "write response timeout",
            source: std::io::Error::new(std::io::ErrorKind::TimedOut, "write response timed out"),
        })?
        .map_err(|source| NodeError::DaemonControlSocket {
            path: socket_path.clone(),
            operation: "write response",
            source,
        })?;
    timeout(DAEMON_CONTROL_WRITE_TIMEOUT, stream.write_all(b"\n"))
        .await
        .map_err(|_| NodeError::DaemonControlSocket {
            path: socket_path.clone(),
            operation: "write response newline timeout",
            source: std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "write response newline timed out",
            ),
        })?
        .map_err(|source| NodeError::DaemonControlSocket {
            path: socket_path.clone(),
            operation: "write response newline",
            source,
        })?;
    timeout(DAEMON_CONTROL_WRITE_TIMEOUT, stream.shutdown())
        .await
        .map_err(|_| NodeError::DaemonControlSocket {
            path: socket_path.clone(),
            operation: "shutdown response timeout",
            source: std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "shutdown response timed out",
            ),
        })?
        .map_err(|source| NodeError::DaemonControlSocket {
            path: socket_path,
            operation: "shutdown response",
            source,
        })
}
