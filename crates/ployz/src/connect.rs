use std::{
    borrow::Cow,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::Arc,
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use hyper_util::rt::TokioIo;
use ployz_core::{
    CodecError, FramingError, MachineId, MachineTarget, RoutingMetadataError, RpcError,
    RpcErrorCode, apply_one_target,
};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    net::TcpStream,
    process::{Child, ChildStdin, ChildStdout, Command},
};
use tonic::transport::{Channel, Endpoint};

use crate::context::{
    Config, ConfigError, Connection, ConnectionError, ConnectionSource, ContextError,
    SelectedConnections, Transport, expand_home, select_connections,
};

mod relay;
pub(crate) use relay::{list_held, revoke_pairing as revoke_cloud_pairing};

pub use crate::cluster::{Client, MachineImagesObservation};
pub use ployz_relay::{DialCredential, HeldRegister, PairingCredential};

pub const DEFAULT_LOCAL_SOCKET: &str = "/run/ployz/ployz.sock";

pub(crate) const UNARY_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_millis(1500),
    Duration::from_secs(4),
];

pub(crate) const TARGET_RPC_TIMEOUT: Duration = Duration::from_secs(10);

pub(crate) fn stop_rpc_timeout(grace_period_seconds: Option<i32>) -> Option<Duration> {
    match grace_period_seconds {
        Some(seconds) if seconds < 0 => None,
        Some(seconds) => Some(TARGET_RPC_TIMEOUT + Duration::from_secs(seconds as u64)),
        None => None,
    }
}

pub trait ProxyStream: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> ProxyStream for T {}

pub type BoxProxyStream = Box<dyn ProxyStream>;

// TODO: a future client-side WireGuard connector must honour
// cancellation and try each visible Machine; Ployz keeps it excluded.
// Object-safe for `Arc<dyn Connector>` in Client; native async fn is not dyn-safe.
#[tonic::async_trait]
pub trait Connector: Send + Sync {
    async fn connect(&self, connection: &Connection) -> Result<Channel, ConnectError>;

    async fn dial_proxy(
        &self,
        connection: &Connection,
        network: &str,
        address: &str,
    ) -> Result<BoxProxyStream, ConnectError>;
}

#[derive(Clone, Debug)]
pub struct SystemConnector {
    ssh_program: PathBuf,
    ssh_timeout: Duration,
}

impl Default for SystemConnector {
    fn default() -> Self {
        Self::new("ssh")
    }
}

impl SystemConnector {
    pub fn new(ssh_program: impl Into<PathBuf>) -> Self {
        Self {
            ssh_program: ssh_program.into(),
            ssh_timeout: Duration::from_secs(5),
        }
    }

    /// Set the budget for SSH connection establishment, including the probe.
    #[must_use]
    pub fn with_ssh_timeout(mut self, timeout: Duration) -> Self {
        self.ssh_timeout = timeout;
        self
    }
}

#[tonic::async_trait]
impl Connector for SystemConnector {
    async fn connect(&self, connection: &Connection) -> Result<Channel, ConnectError> {
        match connection.transport() {
            Transport::Tcp(address) => connect_endpoint(format!("http://{address}")).await,
            Transport::Unix(path) => connect_endpoint(format!("unix:{}", path.display())).await,
            Transport::Ssh {
                destination,
                key_file,
            } => tokio::time::timeout(
                self.ssh_timeout,
                connect_ssh(
                    destination,
                    key_file.as_deref(),
                    &self.ssh_program,
                    self.ssh_timeout,
                ),
            )
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "SSH connection to {} timed out after {} seconds",
                        destination.target(),
                        self.ssh_timeout.as_secs()
                    ),
                )
            })?,
            Transport::Relay {
                url,
                credential,
                pairing,
            } => {
                let machine_id = connection
                    .machine_id()
                    .expect("Relay connections carry an entry Machine ID");
                relay::connect_channel(url, credential, pairing, machine_id).await
            }
        }
    }

    async fn dial_proxy(
        &self,
        connection: &Connection,
        network: &str,
        address: &str,
    ) -> Result<BoxProxyStream, ConnectError> {
        if network != "tcp" {
            return Err(ConnectError::UnsupportedNetwork(network.into()));
        }
        match connection.transport() {
            Transport::Tcp(_) => Err(ConnectError::ProxyUnsupported(connection.to_string())),
            Transport::Unix(_) => TcpStream::connect(address)
                .await
                .map(|stream| Box::new(stream) as BoxProxyStream)
                .map_err(ConnectError::from),
            Transport::Ssh {
                destination,
                key_file,
            } => {
                let mut args = ssh_base_args(
                    destination,
                    key_file.as_deref(),
                    control_path().as_deref(),
                    self.ssh_timeout,
                );
                args.extend(["-W".into(), address.into(), destination.target().into()]);
                spawn_ssh(&self.ssh_program, &args)
                    .map(|stream| Box::new(stream) as BoxProxyStream)
                    .map_err(ConnectError::from_ssh_spawn)
            }
            Transport::Relay { .. } => Err(ConnectError::ProxyUnsupported(connection.to_string())),
        }
    }
}

async fn connect_endpoint(target: String) -> Result<Channel, ConnectError> {
    Endpoint::from_shared(target)?
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
        .map_err(ConnectError::from)
}

async fn connect_ssh(
    destination: &crate::context::SshDestination,
    key_file: Option<&Path>,
    program: &Path,
    timeout: Duration,
) -> Result<Channel, ConnectError> {
    let control_path = control_path();
    let mut probe_args = ssh_base_args(destination, key_file, control_path.as_deref(), timeout);
    probe_args.extend([destination.target().into(), "true".into()]);
    // TODO: cancelling drops this probe promptly, but a ControlMaster
    // created during OpenSSH establishment may outlive it until ControlPersist expires.
    let output = Command::new(program)
        .args(&probe_args)
        .stdin(Stdio::null())
        .kill_on_drop(true)
        .output()
        .await
        .map_err(ConnectError::from_ssh_spawn)?;
    if !output.status.success() {
        let mut detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        if detail.contains("Permission denied") {
            detail.push_str("; SSH authentication is noninteractive: unlock your key with ssh-add or configure credentials that do not require a prompt");
        }
        return Err(ConnectError::SshProbe {
            target: destination.target().to_owned(),
            status: output.status,
            detail,
        });
    }
    let args = ssh_args(destination, key_file, control_path.as_deref(), timeout);
    let program = program.to_owned();
    Endpoint::from_static("http://[::]:50051")
        .connect_timeout(timeout)
        .connect_with_connector(tower::service_fn(move |_| {
            let args = args.clone();
            let program = program.clone();
            async move { spawn_ssh(&program, &args).map(TokioIo::new) }
        }))
        .await
        .map_err(ConnectError::from)
}

fn ssh_args(
    destination: &crate::context::SshDestination,
    key_file: Option<&Path>,
    control_path: Option<&Path>,
    timeout: Duration,
) -> Vec<String> {
    let mut args = ssh_base_args(destination, key_file, control_path, timeout);
    args.extend([
        destination.target().into(),
        "ployzd".into(),
        "dial-stdio".into(),
    ]);
    args
}

fn ssh_base_args(
    destination: &crate::context::SshDestination,
    key_file: Option<&Path>,
    control_path: Option<&Path>,
    timeout: Duration,
) -> Vec<String> {
    let mut args = ssh_control_args(control_path);
    args.extend([
        "-o".into(),
        format!("ConnectTimeout={}", timeout.as_secs().max(1)),
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "StrictHostKeyChecking=accept-new".into(),
        "-T".into(),
    ]);
    if let Some(port) = destination.port() {
        args.extend(["-p".into(), port.to_string()]);
    }
    if let Some(path) = key_file {
        args.extend(["-i".into(), expand_home(path).display().to_string()]);
    }
    args
}

/// OpenSSH multiplexing options shared by provisioning and management connections.
pub(crate) fn ssh_control_args(control_path: Option<&Path>) -> Vec<String> {
    let mut args = Vec::new();
    if let Some(path) = control_path {
        args.extend([
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            format!("ControlPath={}", path.display()),
            "-o".into(),
            format!(
                "ControlPersist={}",
                std::env::var(crate::cli::env::SSH_CONTROL_PERSIST)
                    .unwrap_or_else(|_| "10m".into())
            ),
        ]);
    }
    args
}

/// Select the existing runtime or SSH directory for shared control sockets.
pub(crate) fn control_path() -> Option<PathBuf> {
    if let Some(directory) = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from)
        && directory.is_dir()
    {
        return Some(directory.join("ployz_ssh_%C.sock"));
    }
    let directory = std::env::var_os("HOME").map(PathBuf::from)?.join(".ssh");
    directory
        .is_dir()
        .then(|| directory.join("ployz_ssh_%C.sock"))
}

fn spawn_ssh(program: &Path, args: &[String]) -> io::Result<SshIo> {
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn()?;
    let reader = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("ssh stdout was not piped"))?;
    let writer = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("ssh stdin was not piped"))?;
    Ok(SshIo {
        reader,
        writer,
        _child: child,
    })
}

struct SshIo {
    reader: ChildStdout,
    writer: ChildStdin,
    _child: Child,
}

impl AsyncRead for SshIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.reader).poll_read(context, buffer)
    }
}

impl AsyncWrite for SshIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        Pin::new(&mut self.writer).poll_write(context, buffer)
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.writer).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.writer).poll_shutdown(context)
    }
}

pub(crate) fn rpc_error(error: ConnectError) -> RpcError {
    match error {
        ConnectError::Remote(error) => error,
        ConnectError::Rpc(error) => error.to_rpc_error(),
        ConnectError::InvalidDialCredential => RpcError {
            code: RpcErrorCode::Unauthenticated,
            message: ConnectError::InvalidDialCredential.to_string(),
            details: Value::Null,
        },
        ConnectError::UnknownMachine => RpcError {
            code: RpcErrorCode::NotFound,
            message: ConnectError::UnknownMachine.to_string(),
            details: Value::Null,
        },
        error @ (ConnectError::Attempt(_)
        | ConnectError::Io(_)
        | ConnectError::Dial(_)
        | ConnectError::Relay(_)
        | ConnectError::MissingMachineDetails
        | ConnectError::SshClientMissing(_)
        | ConnectError::SshProbe { .. }
        | ConnectError::Routing(_)
        | ConnectError::Join(_)
        | ConnectError::ProxyUnsupported(_)
        | ConnectError::UnsupportedNetwork(_)
        | ConnectError::Config(_)
        | ConnectError::Connection(_)
        | ConnectError::Context(_)
        | ConnectError::Path { .. }
        | ConnectError::AllFailed { .. }
        | ConnectError::Codec(_)
        | ConnectError::Framing(_)
        | ConnectError::Value(_)) => RpcError {
            code: RpcErrorCode::Internal,
            message: error.to_string(),
            details: Value::Null,
        },
    }
}

impl From<ConnectError> for RpcError {
    fn from(error: ConnectError) -> Self {
        rpc_error(error)
    }
}

pub(crate) async fn apply_timeout<T>(
    timeout: Option<Duration>,
    future: impl Future<Output = Result<T, ConnectError>>,
) -> Result<T, RpcError> {
    let result = match timeout {
        Some(timeout) => match tokio::time::timeout(timeout, future).await {
            Ok(result) => result,
            Err(_) => {
                return Err(RpcError {
                    code: RpcErrorCode::Unavailable,
                    message: "target Machine RPC timed out".into(),
                    details: Value::Null,
                });
            }
        },
        None => future.await,
    };
    result.map_err(rpc_error)
}

pub(crate) fn target_request<T>(payload: T, target: Option<&MachineTarget>) -> tonic::Request<T> {
    let mut request = tonic::Request::new(payload);
    if let Some(target) = target {
        apply_one_target(request.metadata_mut(), target);
    }
    request
}

/// Walk every connection and keep the first whose daemon answers.
///
/// A tunnel or lazy channel is not enough. Confirmation is one-shot so a down
/// daemon cannot stall the walk or consume unary retries.
///
/// # Errors
///
/// Returns [`ConnectError::SshClientMissing`] when the local ssh client cannot
/// be spawned; further connections cannot succeed. Returns
/// [`ConnectError::AllFailed`] after every connection is tried.
pub async fn connect_selected_with(
    selected: SelectedConnections,
    connector: Arc<dyn Connector>,
) -> Result<Client, ConnectError> {
    let mut last_error = None;
    let mut setup_retryable = false;
    for connection in &selected.connections {
        match connect_one(connection, &selected.source, &connector).await {
            Ok(client) => return Ok(client),
            Err(error) if matches!(error, ConnectError::SshClientMissing(_)) => {
                return Err(error);
            }
            Err(error) => {
                setup_retryable |= error.is_setup_retryable();
                last_error = Some(error);
            }
        }
    }
    Err(ConnectError::AllFailed {
        source: selected.source,
        attempts: selected.connections.len(),
        setup_retryable,
        last: last_error.map(Box::new),
    })
}

async fn connect_one(
    connection: &Connection,
    source: &ConnectionSource,
    connector: &Arc<dyn Connector>,
) -> Result<Client, ConnectError> {
    let channel = connector.connect(connection).await?;
    let client = Client::new(
        channel,
        connection.clone(),
        source.clone(),
        connector.clone(),
    );
    client.confirm_entry().await?;
    Ok(client)
}

pub fn resolve_connections(
    config_path: &Path,
    direct: Option<&str>,
    context_override: Option<&str>,
    local_socket: &Path,
) -> Result<SelectedConnections, ConnectError> {
    if let Some(direct) = direct {
        return select_connections(
            Some(direct.parse()?),
            None,
            context_override,
            false,
            local_socket,
        )
        .map_err(ConnectError::Context);
    }
    match Config::load(config_path) {
        Ok(config) => {
            return select_connections(None, Some(&config), context_override, false, local_socket)
                .map_err(ConnectError::Context);
        }
        Err(ConfigError::Read { source, .. }) if source.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(ConnectError::Config(error)),
    }
    let socket_available = local_socket
        .try_exists()
        .map_err(|source| ConnectError::Path {
            path: local_socket.to_owned(),
            source,
        })?;
    select_connections(None, None, context_override, socket_available, local_socket)
        .map_err(ConnectError::Context)
}

pub async fn connect(
    config_path: &Path,
    direct: Option<&str>,
    context_override: Option<&str>,
) -> Result<Client, ConnectError> {
    connect_with_ssh_timeout(
        config_path,
        direct,
        context_override,
        Duration::from_secs(5),
    )
    .await
}

/// Select a management connection with the supplied SSH setup budget.
///
/// # Errors
///
/// Returns configuration or connection errors when no selected entry is reachable.
pub(crate) async fn connect_with_ssh_timeout(
    config_path: &Path,
    direct: Option<&str>,
    context_override: Option<&str>,
    ssh_timeout: Duration,
) -> Result<Client, ConnectError> {
    let selected = resolve_connections(
        config_path,
        direct,
        context_override,
        Path::new(DEFAULT_LOCAL_SOCKET),
    )?;
    connect_selected_with(
        selected,
        Arc::new(SystemConnector::default().with_ssh_timeout(ssh_timeout)),
    )
    .await
}

/// Open a Machine RPC channel through Cloud Relay.
///
/// Succeeds only after Relay Dial and Machine Attach produce a usable RPC
/// channel. Does not mint Attach credentials, perform Cloud Pairing, or choose
/// an entry Machine.
///
/// # Errors
///
/// Returns [`ConnectError::InvalidDialCredential`] when the bearer is rejected,
/// [`ConnectError::UnknownMachine`] when the Machine ID is not registered, or
/// another [`ConnectError`] when the Relay or inner RPC channel fails.
pub async fn connect_relay(
    url: impl AsRef<str>,
    credential: DialCredential,
    pairing: PairingCredential,
    machine_id: MachineId,
) -> Result<Client, ConnectError> {
    let connector: Arc<dyn Connector> = Arc::new(SystemConnector::default());
    connect_one(
        &Connection::relay(
            ployz_core::RelayEndpoint::parse(url.as_ref())?,
            credential,
            pairing,
            machine_id,
        ),
        &ConnectionSource::Direct,
        &connector,
    )
    .await
}

#[derive(Debug, Error)]
pub enum ConnectError {
    #[error("invalid Dial Credential")]
    InvalidDialCredential,
    #[error("unknown Machine ID")]
    UnknownMachine,
    #[error("connection attempt failed: {0}")]
    Attempt(Cow<'static, str>),
    #[error("connection attempt failed: {0}")]
    Io(#[from] io::Error),
    #[error("connection attempt failed: {0}")]
    Dial(#[from] tonic::transport::Error),
    #[error("connection attempt failed: inspect response omitted Machine details")]
    MissingMachineDetails,
    #[error("local ssh client not found; install an ssh client")]
    SshClientMissing(#[source] io::Error),
    #[error("connection attempt failed: SSH probe to {target} exited with {status}: {detail}")]
    SshProbe {
        target: String,
        status: std::process::ExitStatus,
        detail: String,
    },
    #[error("connection attempt failed: {0}")]
    Routing(#[from] RoutingMetadataError),
    #[error("connection attempt failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("proxy dialing is unsupported over {0}")]
    ProxyUnsupported(String),
    #[error("proxy dialing does not support network {0}")]
    UnsupportedNetwork(String),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error(transparent)]
    Context(#[from] ContextError),
    #[error("could not inspect {path}: {source}")]
    Path { path: PathBuf, source: io::Error },
    #[error("all {attempts} connections from {source} failed: {}", last.as_ref().map_or_else(|| "no connection available".to_owned(), ToString::to_string))]
    AllFailed {
        source: ConnectionSource,
        attempts: usize,
        setup_retryable: bool,
        #[source]
        last: Option<Box<ConnectError>>,
    },
    #[error(transparent)]
    Relay(ployz_relay::ClientError),
    #[error("Machine RPC failed: {0}")]
    Rpc(TransportError),
    #[error("Machine RPC payload failed: {0}")]
    Codec(#[from] CodecError),
    #[error("Machine RPC returned: {}", .0.message)]
    Remote(RpcError),
    #[error("Machine RPC framing failed: {0}")]
    Framing(#[from] FramingError),
    #[error("Machine RPC identity failed: {0}")]
    Value(#[from] ployz_core::ValueError),
}

impl From<tonic::Status> for ConnectError {
    fn from(status: tonic::Status) -> Self {
        Self::Rpc(TransportError::from(status))
    }
}

impl ConnectError {
    fn from_ssh_spawn(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::NotFound {
            Self::SshClientMissing(error)
        } else {
            Self::Io(error)
        }
    }

    pub(crate) fn is_retryable(&self) -> bool {
        match self {
            Self::Attempt(_)
            | Self::Io(_)
            | Self::Dial(_)
            | Self::SshProbe { .. }
            | Self::Join(_) => true,
            Self::Relay(error) => error
                .status()
                .is_none_or(|status| matches!(status.as_u16(), 408 | 429 | 500 | 502 | 503 | 504)),
            Self::Rpc(error) => error.is_retryable(),
            Self::Remote(_)
            | Self::InvalidDialCredential
            | Self::UnknownMachine
            | Self::MissingMachineDetails
            | Self::SshClientMissing(_)
            | Self::Routing(_)
            | Self::ProxyUnsupported(_)
            | Self::UnsupportedNetwork(_)
            | Self::Config(_)
            | Self::Connection(_)
            | Self::Context(_)
            | Self::Path { .. }
            | Self::AllFailed { .. }
            | Self::Codec(_)
            | Self::Framing(_)
            | Self::Value(_) => false,
        }
    }

    /// Setup retries must not spend a minute retrying missing credentials or keys.
    #[expect(
        clippy::wildcard_enum_match_arm,
        reason = "setup overrides SSH, IO and aggregate errors; other variants use the exhaustive transport classifier"
    )]
    pub(crate) fn is_setup_retryable(&self) -> bool {
        match self {
            Self::AllFailed {
                setup_retryable, ..
            } => *setup_retryable,
            Self::SshProbe { detail, .. } => [
                "Connection timed out",
                "Operation timed out",
                "Connection refused",
                "No route to host",
                "Network is unreachable",
                "Connection reset",
                "Connection closed",
                "Temporary failure in name resolution",
            ]
            .iter()
            .any(|message| detail.contains(message)),
            Self::Io(error) => {
                crate::setup_retry::temporary_dns(error)
                    || matches!(
                        error.kind(),
                        io::ErrorKind::ConnectionRefused
                            | io::ErrorKind::ConnectionReset
                            | io::ErrorKind::ConnectionAborted
                            | io::ErrorKind::TimedOut
                            | io::ErrorKind::NotConnected
                            | io::ErrorKind::UnexpectedEof
                            | io::ErrorKind::NetworkUnreachable
                            | io::ErrorKind::HostUnreachable
                    )
            }
            _ => self.is_retryable(),
        }
    }

    pub(crate) fn is_unreachable(&self) -> bool {
        matches!(
            self,
            Self::Attempt(_) | Self::Io(_) | Self::Dial(_) | Self::AllFailed { .. }
        ) || matches!(self, Self::Rpc(error) if error.is_unavailable())
            || matches!(self, Self::Relay(_) if self.is_retryable())
    }
}

/// A gRPC transport failure. Display is the status message, not Status Debug.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct TransportError {
    code: tonic::Code,
    message: String,
    details: Value,
}

impl TransportError {
    pub(crate) fn from_stream_status(status: tonic::Status) -> Self {
        // Remote statuses cross the wire without a source. Tonic attaches one
        // only when the client stream itself fails.
        let interrupted =
            status.code() == tonic::Code::Cancelled || std::error::Error::source(&status).is_some();
        let mut error = Self::from(status);
        if interrupted {
            error.code = tonic::Code::Unavailable;
        }
        error
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    #[must_use]
    pub fn is_retryable(&self) -> bool {
        matches!(
            self.code,
            tonic::Code::Unavailable | tonic::Code::DeadlineExceeded
        )
    }

    #[must_use]
    pub fn is_unavailable(&self) -> bool {
        self.code == tonic::Code::Unavailable
    }

    #[must_use]
    pub fn is_not_found(&self) -> bool {
        self.code == tonic::Code::NotFound
    }

    #[must_use]
    pub fn to_rpc_error(&self) -> RpcError {
        RpcError {
            code: rpc_error_code(self.code),
            message: self.message.clone(),
            details: self.details.clone(),
        }
    }
}

impl From<tonic::Status> for TransportError {
    fn from(status: tonic::Status) -> Self {
        Self {
            code: status.code(),
            message: status.message().to_owned(),
            details: if status.details().is_empty() {
                Value::Null
            } else {
                json!({ "grpc_details": String::from_utf8_lossy(status.details()) })
            },
        }
    }
}

fn rpc_error_code(code: tonic::Code) -> RpcErrorCode {
    match code {
        tonic::Code::InvalidArgument => RpcErrorCode::InvalidArgument,
        tonic::Code::NotFound => RpcErrorCode::NotFound,
        tonic::Code::AlreadyExists | tonic::Code::Aborted => RpcErrorCode::Conflict,
        tonic::Code::Unavailable | tonic::Code::DeadlineExceeded => RpcErrorCode::Unavailable,
        tonic::Code::Unimplemented => RpcErrorCode::Unsupported,
        tonic::Code::Unauthenticated => RpcErrorCode::Unauthenticated,
        tonic::Code::Ok
        | tonic::Code::Cancelled
        | tonic::Code::Unknown
        | tonic::Code::PermissionDenied
        | tonic::Code::ResourceExhausted
        | tonic::Code::FailedPrecondition
        | tonic::Code::OutOfRange
        | tonic::Code::Internal
        | tonic::Code::DataLoss => RpcErrorCode::Internal,
    }
}

#[cfg(test)]
#[path = "connect/tests.rs"]
mod tests;

#[cfg(test)]
pub(crate) mod test_support;
