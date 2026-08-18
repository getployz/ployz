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
    CodecError, FanoutFailure, FramingError, MachineId, MachineTarget, RoutingMetadataError,
    RpcError, RpcErrorCode, apply_one_target,
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

pub use crate::cluster::{Client, MachineImagesObservation};
pub use ployz_relay::DialCredential;

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

// TODO(UT-018, UT-019): a future client-side WireGuard connector must honour
// cancellation and try each visible Machine; this reconstruction keeps it excluded.
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
        }
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
            } => connect_ssh(destination, key_file.as_deref(), &self.ssh_program).await,
            Transport::Relay { url, credential } => {
                let machine_id = connection
                    .machine_id()
                    .expect("Relay connections carry an entry Machine ID");
                relay::connect_channel(url, credential, machine_id).await
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
                let mut args =
                    ssh_base_args(destination, key_file.as_deref(), control_path().as_deref());
                args.extend(["-W".into(), address.into(), destination.target().into()]);
                spawn_ssh(&self.ssh_program, &args)
                    .map(|stream| Box::new(stream) as BoxProxyStream)
                    .map_err(ConnectError::from)
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
) -> Result<Channel, ConnectError> {
    let control_path = control_path();
    let mut probe_args = ssh_base_args(destination, key_file, control_path.as_deref());
    probe_args.extend([destination.target().into(), "true".into()]);
    // TODO(UT-014): cancelling drops this probe promptly, but a ControlMaster
    // created during OpenSSH establishment may outlive it until ControlPersist expires.
    let status = Command::new(program)
        .args(&probe_args)
        .kill_on_drop(true)
        .status()
        .await?;
    if !status.success() {
        return Err(ConnectError::SshProbe {
            target: destination.target().to_owned(),
            status,
        });
    }
    let args = ssh_args(destination, key_file, control_path.as_deref());
    let program = program.to_owned();
    Endpoint::from_static("http://[::]:50051")
        .connect_timeout(Duration::from_secs(5))
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
) -> Vec<String> {
    let mut args = ssh_base_args(destination, key_file, control_path);
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
) -> Vec<String> {
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
                std::env::var("PLOYZ_SSH_CONTROL_PERSIST").unwrap_or_else(|_| "10m".into())
            ),
        ]);
    }
    args.extend([
        "-o".into(),
        "ConnectTimeout=5".into(),
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

fn control_path() -> Option<PathBuf> {
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
        | ConnectError::MissingMachineDetails
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

pub(crate) fn is_unary_retryable(error: &ConnectError) -> bool {
    error.is_retryable()
}

pub(crate) fn decode_fanout_failure(failure: FanoutFailure) -> RpcError {
    let code = match tonic::Code::from_i32(i32::try_from(failure.code).unwrap_or(i32::MAX)) {
        tonic::Code::InvalidArgument | tonic::Code::OutOfRange => RpcErrorCode::InvalidArgument,
        tonic::Code::NotFound => RpcErrorCode::NotFound,
        tonic::Code::AlreadyExists | tonic::Code::Aborted | tonic::Code::FailedPrecondition => {
            RpcErrorCode::Conflict
        }
        tonic::Code::Unimplemented => RpcErrorCode::Unsupported,
        tonic::Code::Unauthenticated => RpcErrorCode::Unauthenticated,
        tonic::Code::DeadlineExceeded
        | tonic::Code::ResourceExhausted
        | tonic::Code::Unavailable => RpcErrorCode::Unavailable,
        tonic::Code::Internal | tonic::Code::DataLoss | tonic::Code::Unknown => {
            RpcErrorCode::Internal
        }
        tonic::Code::Ok | tonic::Code::Cancelled | tonic::Code::PermissionDenied => {
            RpcErrorCode::Unknown(format!("grpc_{}", failure.code))
        }
    };
    RpcError {
        code,
        message: failure.message,
        details: failure.details.into(),
    }
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
/// Returns [`ConnectError::AllFailed`] after every connection is tried.
pub async fn connect_selected_with(
    selected: SelectedConnections,
    connector: Arc<dyn Connector>,
) -> Result<Client, ConnectError> {
    let mut last_error = None;
    for connection in &selected.connections {
        match connect_one(connection, &selected.source, &connector).await {
            Ok(client) => return Ok(client),
            Err(error) => last_error = Some(error),
        }
    }
    Err(ConnectError::AllFailed {
        source: selected.source,
        attempts: selected.connections.len(),
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
    let selected = resolve_connections(
        config_path,
        direct,
        context_override,
        Path::new(DEFAULT_LOCAL_SOCKET),
    )?;
    connect_selected_with(selected, Arc::new(SystemConnector::default())).await
}

/// Revoke the Cloud Pairing for this Dial tenant so Register fails afterwards.
///
/// # Errors
///
/// Returns [`ConnectError::InvalidDialCredential`] when the bearer is rejected.
pub(crate) async fn revoke_cloud_pairing(
    url: &str,
    credential: &DialCredential,
) -> Result<(), ConnectError> {
    relay::revoke_pairing(url, credential).await
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
    machine_id: MachineId,
) -> Result<Client, ConnectError> {
    let connector: Arc<dyn Connector> = Arc::new(SystemConnector::default());
    connect_one(
        &Connection::relay(url.as_ref(), credential, machine_id),
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
    #[error("connection attempt failed: SSH probe to {target} exited with {status}")]
    SshProbe {
        target: String,
        status: std::process::ExitStatus,
    },
    #[error("connection attempt failed: {0}")]
    Routing(#[from] RoutingMetadataError),
    #[error("connection attempt failed: {0}")]
    Join(#[from] tokio::task::JoinError),
    #[error("proxy dialing is unsupported over {0}")]
    ProxyUnsupported(String),
    #[error("proxy dialing does not support network {0:?}")]
    UnsupportedNetwork(String),
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Connection(#[from] ConnectionError),
    #[error(transparent)]
    Context(#[from] ContextError),
    #[error("could not inspect {path}: {source}")]
    Path { path: PathBuf, source: io::Error },
    #[error("all {attempts} connections from {source:?} failed")]
    AllFailed {
        source: ConnectionSource,
        attempts: usize,
        #[source]
        last: Option<Box<ConnectError>>,
    },
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
    pub(crate) fn is_retryable(&self) -> bool {
        match self {
            Self::Attempt(_)
            | Self::Io(_)
            | Self::Dial(_)
            | Self::SshProbe { .. }
            | Self::Join(_) => true,
            Self::Rpc(error) => error.is_retryable(),
            Self::Remote(_)
            | Self::InvalidDialCredential
            | Self::UnknownMachine
            | Self::MissingMachineDetails
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

    pub(crate) fn is_unreachable(&self) -> bool {
        matches!(
            self,
            Self::Attempt(_) | Self::Io(_) | Self::Dial(_) | Self::AllFailed { .. }
        ) || matches!(self, Self::Rpc(error) if error.is_unavailable())
    }
}

/// A gRPC transport failure with Display frozen to `tonic::Status`.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{display}")]
pub struct TransportError {
    code: tonic::Code,
    message: String,
    display: String,
    details: Value,
}

impl TransportError {
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
            display: status.to_string(),
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
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command as StdCommand, time::Duration};

    use super::*;
    use crate::context::SshDestination;
    use ployz_core::{ONE_TARGET_BINARY_HEADER, ONE_TARGET_HEADER};

    #[tokio::test]
    async fn target_timeout_becomes_a_typed_partial_failure() {
        let error = apply_timeout(
            Some(Duration::from_millis(1)),
            std::future::pending::<Result<(), ConnectError>>(),
        )
        .await
        .unwrap_err();

        assert_eq!(error.code, RpcErrorCode::Unavailable);
        assert_eq!(error.message, "target Machine RPC timed out");
    }

    #[test]
    fn non_ascii_machine_targets_use_binary_metadata() {
        let target = MachineTarget::parse("München edge").unwrap();
        let request = target_request(ployz_core::OpaquePayload::new(Vec::new()), Some(&target));

        assert!(request.metadata().get(ONE_TARGET_HEADER).is_none());
        assert_eq!(
            request
                .metadata()
                .get_bin(ONE_TARGET_BINARY_HEADER)
                .unwrap()
                .to_bytes()
                .unwrap(),
            target.as_str()
        );
    }

    #[test]
    fn system_ssh_command_delegates_identity_and_passphrase_handling() {
        let destination = SshDestination::parse("deploy@example.com:2222").unwrap();

        let args = ssh_args(&destination, Some(Path::new("/keys/deploy")), None);

        assert_eq!(
            args,
            [
                "-o",
                "ConnectTimeout=5",
                "-o",
                "StrictHostKeyChecking=accept-new",
                "-T",
                "-p",
                "2222",
                "-i",
                "/keys/deploy",
                "deploy@example.com",
                "ployzd",
                "dial-stdio",
            ]
        );
        assert!(!args.iter().any(|arg| arg.contains("id_*")));
        assert!(!args.iter().any(|arg| arg.contains("BatchMode")));
    }

    #[test]
    fn fanout_failures_preserve_status_code_and_details() {
        let error = decode_fanout_failure(FanoutFailure {
            code: tonic::Code::Internal as u32,
            message: "Docker failed".into(),
            details: vec![1, 2, 3],
        });

        assert_eq!(error.code, RpcErrorCode::Internal);
        assert_eq!(error.message, "Docker failed");
        assert_eq!(error.details, serde_json::json!([1, 2, 3]));
    }

    #[test]
    fn fanout_deadline_exceeded_still_maps_to_unavailable() {
        let error = decode_fanout_failure(FanoutFailure {
            code: tonic::Code::DeadlineExceeded as u32,
            message: "deadline".into(),
            details: Vec::new(),
        });

        assert_eq!(error.code, RpcErrorCode::Unavailable);
        assert_eq!(error.message, "deadline");
    }

    #[test]
    fn transport_predicates_follow_the_grpc_code() {
        #[rustfmt::skip]
        let cases = [
            (tonic::Code::Ok,                 false, false, false, RpcErrorCode::Internal),
            (tonic::Code::Cancelled,          false, false, false, RpcErrorCode::Internal),
            (tonic::Code::Unknown,            false, false, false, RpcErrorCode::Internal),
            (tonic::Code::InvalidArgument,    false, false, false, RpcErrorCode::InvalidArgument),
            (tonic::Code::DeadlineExceeded,   true,  false, false, RpcErrorCode::Unavailable),
            (tonic::Code::NotFound,           false, false, true,  RpcErrorCode::NotFound),
            (tonic::Code::AlreadyExists,      false, false, false, RpcErrorCode::Conflict),
            (tonic::Code::PermissionDenied,   false, false, false, RpcErrorCode::Internal),
            (tonic::Code::ResourceExhausted,  false, false, false, RpcErrorCode::Internal),
            (tonic::Code::FailedPrecondition, false, false, false, RpcErrorCode::Internal),
            (tonic::Code::Aborted,            false, false, false, RpcErrorCode::Conflict),
            (tonic::Code::OutOfRange,         false, false, false, RpcErrorCode::Internal),
            (tonic::Code::Unimplemented,      false, false, false, RpcErrorCode::Unsupported),
            (tonic::Code::Internal,           false, false, false, RpcErrorCode::Internal),
            (tonic::Code::Unavailable,        true,  true,  false, RpcErrorCode::Unavailable),
            (tonic::Code::DataLoss,           false, false, false, RpcErrorCode::Internal),
            (tonic::Code::Unauthenticated,    false, false, false, RpcErrorCode::Unauthenticated),
        ];

        for (code, retryable, unavailable, not_found, rpc) in cases {
            let error = TransportError::from(tonic::Status::new(code, "x"));
            assert_eq!(error.is_retryable(), retryable, "{code:?}");
            assert_eq!(error.is_unavailable(), unavailable, "{code:?}");
            assert_eq!(error.is_not_found(), not_found, "{code:?}");
            assert_eq!(error.to_rpc_error().code, rpc, "{code:?}");
        }
    }

    #[test]
    fn reached_target_cleanup_rejections_are_not_unreachable_fallbacks() {
        assert!(
            ConnectError::Rpc(TransportError::from(tonic::Status::unavailable(
                "route failed"
            )))
            .is_unreachable()
        );
        assert!(
            !ConnectError::Rpc(TransportError::from(tonic::Status::unimplemented(
                "older daemon"
            )))
            .is_unreachable()
        );
        assert!(
            !ConnectError::Remote(RpcError {
                code: RpcErrorCode::Unavailable,
                message: "Docker is unavailable".into(),
                details: serde_json::Value::Null,
            })
            .is_unreachable()
        );
    }

    #[test]
    fn deadline_exceeded_is_retryable_not_unreachable() {
        let error = ConnectError::Rpc(TransportError::from(tonic::Status::deadline_exceeded(
            "timed out",
        )));
        assert!(error.is_retryable());
        assert!(!error.is_unreachable());
    }

    #[tokio::test]
    async fn cancelling_ssh_establishment_returns_without_waiting_for_ssh() {
        let root = std::env::temp_dir().join(format!("ployz-ssh-cancel-{}", std::process::id()));
        let program = root.join("ssh");
        let _ = fs::remove_dir_all(&root);
        fs::create_dir_all(&root).unwrap();
        fs::write(&program, "#!/bin/sh\necho $$ > \"$0.pid\"\nexec sleep 30\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
        let connector = SystemConnector::new(&program);
        let connection = Connection::ssh(SshDestination::parse("user@example.com").unwrap());

        let result =
            tokio::time::timeout(Duration::from_millis(50), connector.connect(&connection)).await;
        assert!(result.is_err(), "{result:?}");
        let pid = fs::read_to_string(program.with_extension("pid")).unwrap();
        let deadline = tokio::time::Instant::now() + Duration::from_secs(1);
        while StdCommand::new("kill")
            .args(["-0", pid.trim()])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .is_ok_and(|status| status.success())
        {
            assert!(tokio::time::Instant::now() < deadline, "ssh was not killed");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        fs::remove_dir_all(root).unwrap();
    }
}
