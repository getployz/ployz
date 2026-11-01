mod build_grant;
pub mod diagnostics;
mod management;

pub use build_grant::{GrantRegistry, open_grant_registry};
pub use management::{ManagementRelay, management_proof};
use management::{connect_management, dial_tunnel};

use std::{
    borrow::Cow,
    collections::BTreeMap,
    future::Future,
    io,
    path::{Path, PathBuf},
    pin::Pin,
    process::Stdio,
    sync::Arc,
    task::{Context as TaskContext, Poll},
    time::Duration,
};

use futures_util::{StreamExt, stream::FuturesUnordered};
use hyper_util::rt::TokioIo;
use ployz_core::{
    CodecError, FramingError, MachineId, MachineTarget, ResolvedServiceSpec, Retry,
    RoutingMetadataError, RpcError, RpcErrorCode, SETTLE_INSPECT_GRACE, apply_one_target,
    settle_deadline,
};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncReadExt as _, AsyncWrite, ReadBuf},
    net::{TcpStream, UnixStream},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
    sync::oneshot,
    time::{Instant, Sleep},
};
use tonic::transport::{Channel, Endpoint};

use crate::context::{
    Config, ConfigError, Connection, ConnectionError, ConnectionSource, ContextError,
    SelectedConnections, Transport, expand_home, select_connections,
};

pub use crate::cluster::{Client, MachineImagesObservation, Remover};

pub const DEFAULT_LOCAL_SOCKET: &str = "/run/ployz/ployz.sock";

pub(crate) const UNARY_RETRY_DELAYS: [Duration; 3] = [
    Duration::from_millis(500),
    Duration::from_millis(1500),
    Duration::from_secs(4),
];

pub(crate) const TARGET_RPC_TIMEOUT: Duration = Duration::from_secs(10);

/// How long a targeted read waits before the target counts as silent.
pub(crate) fn target_read_timeout() -> Duration {
    crate::cli::env::seconds(crate::cli::env::TARGET_READ_TIMEOUT_SECONDS)
        .unwrap_or(TARGET_RPC_TIMEOUT)
}

/// While a call is open, the client pings the entry daemon after this long without a
/// read, and fails an open call within interval + timeout of its last read: an entry
/// that lost power or its network ends a Deploy instead of hanging it.
const KEEP_ALIVE_INTERVAL: Duration = Duration::from_secs(10);
/// A ping or its answer queues behind up to [`ployz_core::RPC_STREAM_WINDOW_BYTES`] of
/// unread data per call, so this also sets the slowest link a call survives: about
/// 17 KB/s.
const KEEP_ALIVE_TIMEOUT: Duration = Duration::from_secs(60);

/// Bounds how long a connect waits for the entry daemon to confirm itself.
///
/// A socket-activated daemon accepts connects before it serves; this bound is
/// what keeps a starting daemon from hanging the CLI.
pub(crate) const CONNECT_CONFIRM_TIMEOUT: Duration = Duration::from_secs(5);

/// The deadline of a stop RPC that waits behind `queued - 1` other stops on its
/// Machine: a Machine stops its containers one at a time, each taking up to its grace.
pub(crate) fn stop_rpc_timeout(grace_period_seconds: Option<u16>, queued: u32) -> Option<Duration> {
    grace_period_seconds
        .map(|seconds| TARGET_RPC_TIMEOUT + Duration::from_secs(u64::from(seconds)) * queued.max(1))
}

/// The deadline of an In RPC: the owner settles the copy's health before it
/// writes In, so the ask waits out the copy's settle deadline, plus the grace its
/// last inspect may run past it, on top of the RPC.
pub(crate) fn in_rpc_timeout(spec: &ResolvedServiceSpec) -> Duration {
    TARGET_RPC_TIMEOUT + settle_deadline(spec) + SETTLE_INSPECT_GRACE
}

pub trait ProxyStream: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> ProxyStream for T {}

pub type BoxProxyStream = Box<dyn ProxyStream>;

// Object-safe for `Arc<dyn Connector>` in Client; native async fn is not dyn-safe.
#[tonic::async_trait]
pub trait Connector: Send + Sync {
    async fn connect(&self, connection: &Connection) -> Result<Channel, ConnectError>;

    /// [`Self::connect`] for a dial started beside another still in flight. Either
    /// may lose and be dropped, so this one must leave nothing behind when it is.
    async fn connect_beside(&self, connection: &Connection) -> Result<Channel, ConnectError> {
        self.connect(connection).await
    }

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
    relay: ManagementRelay,
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
            ssh_timeout: Duration::from_secs(20),
            relay: ManagementRelay::default(),
        }
    }

    /// Dial the management transport through `relay` instead of the compiled default.
    #[must_use]
    pub fn with_management_relay(mut self, relay: ManagementRelay) -> Self {
        self.relay = relay;
        self
    }

    /// Set the budget for SSH connection establishment, including the probe.
    #[must_use]
    pub fn with_ssh_timeout(mut self, timeout: Duration) -> Self {
        self.ssh_timeout = timeout;
        self
    }
}

impl SystemConnector {
    async fn dial(
        &self,
        connection: &Connection,
        master: SshMaster,
    ) -> Result<Channel, ConnectError> {
        match connection.transport() {
            Transport::Management(capability) => tokio::time::timeout(
                Duration::from_secs(15),
                connect_management(capability, &self.relay),
            )
            .await
            .map_err(|_| {
                io::Error::new(io::ErrorKind::TimedOut, "management connection timed out")
            })?,
            Transport::Tcp(address) => connect_endpoint(format!("http://{address}")).await,
            Transport::Unix(path) => connect_local(path).await,
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
                    master,
                ),
            )
            .await
            .map_err(|_| {
                io::Error::new(
                    io::ErrorKind::TimedOut,
                    format!(
                        "SSH connection to {} timed out after {} seconds; a slow or distant Server may need --ssh-timeout 60",
                        destination.target(),
                        self.ssh_timeout.as_secs()
                    ),
                )
            })?,
        }
    }
}

#[tonic::async_trait]
impl Connector for SystemConnector {
    async fn connect(&self, connection: &Connection) -> Result<Channel, ConnectError> {
        self.dial(connection, SshMaster::Share).await
    }

    async fn connect_beside(&self, connection: &Connection) -> Result<Channel, ConnectError> {
        self.dial(connection, SshMaster::ReuseOnly).await
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
            Transport::Management(capability) => tokio::time::timeout(
                Duration::from_secs(15),
                dial_tunnel(capability, &self.relay, address),
            )
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::TimedOut, "tunnel open timed out"))?,
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
                    SshMaster::Share,
                    self.ssh_timeout,
                );
                args.extend(["-W".into(), address.into(), destination.target().into()]);
                spawn_child(&self.ssh_program, &args)
                    .map(|stream| Box::new(stream) as BoxProxyStream)
                    .map_err(ConnectError::from_ssh_spawn)
            }
        }
    }
}

/// The one stream a [`connect_stream`] channel runs on has closed, so no call on
/// that channel reaches a daemon again; only a redial does.
#[derive(Debug, Error)]
#[error("connection to the daemon closed")]
struct SessionClosed;

const SESSION_CLOSED_REASON: &str = "session_closed";

/// The call failed on a channel whose one stream had closed: resending it on
/// that channel can never reach a daemon.
pub(crate) fn on_closed_session(error: &RpcError) -> bool {
    error.details.get("reason").and_then(Value::as_str) == Some(SESSION_CLOSED_REASON)
}

#[cfg(test)]
pub(crate) fn closed_session_error() -> RpcError {
    TransportError {
        session_closed: true,
        ..TransportError::from(tonic::Status::unavailable("transport error"))
    }
    .to_rpc_error()
}

async fn connect_stream(
    stream: impl AsyncRead + AsyncWrite + Unpin + Send + 'static,
    timeout: Duration,
) -> Result<Channel, ConnectError> {
    let mut stream = Some(stream);
    Endpoint::from_static("http://[::]:50051")
        .connect_timeout(timeout)
        .http2_keep_alive_interval(KEEP_ALIVE_INTERVAL)
        .keep_alive_timeout(KEEP_ALIVE_TIMEOUT)
        .initial_stream_window_size(ployz_core::RPC_STREAM_WINDOW_BYTES)
        .initial_connection_window_size(ployz_core::RPC_CONNECTION_WINDOW_BYTES)
        .connect_with_connector(tower::service_fn(move |_| {
            // A redial must pass shared Machine confirmation before another RPC.
            // Let Client decide whether the failed operation may be retried.
            std::future::ready(
                stream
                    .take()
                    .map(TokioIo::new)
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, SessionClosed)),
            )
        }))
        .await
        .map_err(ConnectError::from)
}

async fn connect_endpoint(target: String) -> Result<Channel, ConnectError> {
    Endpoint::from_shared(target)?
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
        .map_err(ConnectError::from)
}

async fn connect_local(path: &Path) -> Result<Channel, ConnectError> {
    let path = path.to_owned();
    Endpoint::from_static("http://[::]:50051")
        .connect_timeout(Duration::from_secs(5))
        .connect_with_connector(tower::service_fn(move |_| {
            let path = path.clone();
            async move {
                let run_dir = path.parent().map(Path::to_owned);
                match UnixStream::connect(&path).await {
                    Ok(stream) => Ok(TokioIo::new(LocalDaemon { stream, run_dir })),
                    Err(error) => Err(
                        match run_dir.as_deref().and_then(ployz_core::startup_refusal) {
                            Some(refusal) => {
                                io::Error::new(error.kind(), format!("{error}; {refusal}"))
                            }
                            None => error,
                        },
                    ),
                }
            }
        }))
        .await
        .map_err(ConnectError::from)
}

/// A local daemon's socket. A start that refuses closes the connection that
/// started it unanswered, so the stream's end names the refusal it left.
struct LocalDaemon {
    stream: UnixStream,
    run_dir: Option<PathBuf>,
}

impl LocalDaemon {
    fn ended<T>(&self, ended: io::Result<T>) -> io::Result<T> {
        match self
            .run_dir
            .as_deref()
            .and_then(ployz_core::startup_refusal)
        {
            Some(refusal) => Err(io::Error::new(io::ErrorKind::BrokenPipe, refusal)),
            None => ended,
        }
    }
}

impl AsyncRead for LocalDaemon {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        match std::task::ready!(Pin::new(&mut self.stream).poll_read(context, buffer)) {
            Ok(()) if buffer.filled().len() == before && buffer.remaining() > 0 => {
                Poll::Ready(self.ended(Ok(())))
            }
            Err(error) => Poll::Ready(self.ended(Err(error))),
            read => Poll::Ready(read),
        }
    }
}

impl AsyncWrite for LocalDaemon {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        match std::task::ready!(Pin::new(&mut self.stream).poll_write(context, buffer)) {
            Err(error) => Poll::Ready(self.ended(Err(error))),
            written => Poll::Ready(written),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_flush(context)
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.stream).poll_shutdown(context)
    }
}

async fn connect_ssh(
    destination: &crate::context::SshDestination,
    key_file: Option<&Path>,
    program: &Path,
    timeout: Duration,
    master: SshMaster,
) -> Result<Channel, ConnectError> {
    let control_path = control_path();
    let mut probe_args = ssh_base_args(
        destination,
        key_file,
        control_path.as_deref(),
        master,
        timeout,
    );
    probe_args.extend([destination.target().into(), "true".into()]);
    let output = Command::new(program)
        .args(&probe_args)
        .stdin(Stdio::null())
        .process_group(0)
        .kill_on_drop(true)
        .output()
        .await
        .map_err(ConnectError::from_ssh_spawn)?;
    if !output.status.success() {
        let detail = String::from_utf8_lossy(&output.stderr).trim().to_owned();
        return Err(ConnectError::SshProbe {
            target: destination.target().to_owned(),
            status: output.status,
            failure: SshFailure::of(&detail),
            detail,
        });
    }
    let args = ssh_args(
        destination,
        key_file,
        control_path.as_deref(),
        master,
        timeout,
    );
    connect_stream(spawn_session(program, &args)?, timeout).await
}

fn ssh_args(
    destination: &crate::context::SshDestination,
    key_file: Option<&Path>,
    control_path: Option<&Path>,
    master: SshMaster,
    timeout: Duration,
) -> Vec<String> {
    let mut args = ssh_base_args(destination, key_file, control_path, master, timeout);
    args.extend([
        destination.target().into(),
        "ployzd".into(),
        "dial-stdio".into(),
    ]);
    args
}

pub(crate) fn ssh_base_args(
    destination: &crate::context::SshDestination,
    key_file: Option<&Path>,
    control_path: Option<&Path>,
    master: SshMaster,
    timeout: Duration,
) -> Vec<String> {
    let mut args = ssh_control_args(control_path, master);
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

/// How an SSH dial uses the master at its control path.
#[derive(Clone, Copy, Debug)]
pub(crate) enum SshMaster {
    /// Reuse a running master, or start one that serves later commands for 10 minutes.
    Share,
    /// Reuse a running master but start none. A master forks away from the dial, so
    /// one started by a dial that is then dropped would outlive it.
    ReuseOnly,
}

/// OpenSSH multiplexing options shared by provisioning and management connections.
pub(crate) fn ssh_control_args(control_path: Option<&Path>, master: SshMaster) -> Vec<String> {
    let Some(path) = control_path else {
        return Vec::new();
    };
    let control_path = format!("ControlPath={}", path.display());
    match master {
        SshMaster::Share => vec![
            "-o".into(),
            "ControlMaster=auto".into(),
            "-o".into(),
            control_path,
            "-o".into(),
            "ControlPersist=10m".into(),
        ],
        SshMaster::ReuseOnly => vec![
            "-o".into(),
            "ControlMaster=no".into(),
            "-o".into(),
            control_path,
        ],
    }
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

/// A child stream whose end passes straight through, as an `ssh -W` forward's
/// must: that ssh lives on after the target closes, until our side closes.
fn spawn_child(program: &Path, args: &[String]) -> io::Result<ChildIo> {
    let (reader, writer, child) = spawn_piped(program, args, Stdio::null())?;
    Ok(ChildIo {
        reader,
        writer: Some(writer),
        exit: Some(ChildExit::Ended { child, said: None }),
    })
}

/// A child stream that ends, when the child exits unsuccessfully, with the
/// last line it wrote to stderr: a remote `ployzd dial-stdio` says there why.
fn spawn_session(program: &Path, args: &[String]) -> io::Result<ChildIo> {
    let (reader, writer, mut child) = spawn_piped(program, args, Stdio::piped())?;
    let stderr = child
        .stderr
        .take()
        .ok_or_else(|| io::Error::other("child stderr was not piped"))?;
    Ok(ChildIo {
        reader,
        writer: Some(writer),
        exit: Some(ChildExit::Running {
            child,
            stderr: drain_stderr(stderr),
        }),
    })
}

fn spawn_piped(
    program: &Path,
    args: &[String],
    stderr: Stdio,
) -> io::Result<(ChildStdout, ChildStdin, Child)> {
    // A terminal Ctrl-C signals the whole foreground process group. The transport
    // stays out of it so a cancelled command can still clean up over it.
    let mut child = Command::new(program)
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(stderr)
        .process_group(0)
        .kill_on_drop(true)
        .spawn()?;
    let reader = child
        .stdout
        .take()
        .ok_or_else(|| io::Error::other("child stdout was not piped"))?;
    let writer = child
        .stdin
        .take()
        .ok_or_else(|| io::Error::other("child stdin was not piped"))?;
    Ok((reader, writer, child))
}

/// How long a stream that ended waits for its child to exit and say why.
const CHILD_EXIT_GRACE: Duration = Duration::from_secs(1);
const CHILD_STDERR_KEPT: usize = 4096;

/// Reads `stderr` to its end, so the child never blocks on a full pipe, and
/// sends the last [`CHILD_STDERR_KEPT`] bytes.
fn drain_stderr(mut stderr: ChildStderr) -> oneshot::Receiver<Vec<u8>> {
    let (sender, receiver) = oneshot::channel();
    tokio::spawn(async move {
        let (mut kept, mut chunk) = (Vec::new(), [0; 1024]);
        while let Ok(read @ 1..) = stderr.read(&mut chunk).await {
            kept.extend(chunk.iter().take(read));
            kept.drain(..kept.len().saturating_sub(CHILD_STDERR_KEPT));
        }
        let _ = sender.send(kept);
    });
    receiver
}

type Ending = Pin<Box<dyn Future<Output = (Child, Option<String>)> + Send>>;

/// The SSH child behind a [`ChildIo`], and once its stream ended, why. A remote
/// `ployzd dial-stdio` that fails, as when the daemon refused to start, says
/// why on stderr and exits non-zero.
enum ChildExit {
    Running {
        child: Child,
        stderr: oneshot::Receiver<Vec<u8>>,
    },
    Ending(Ending),
    Ended {
        child: Child,
        said: Option<String>,
    },
}

/// The last line `child` wrote to stderr, when it exits unsuccessfully within
/// [`CHILD_EXIT_GRACE`]. A clean exit's stderr holds only SSH's own notes.
async fn last_words(
    mut child: Child,
    stderr: oneshot::Receiver<Vec<u8>>,
) -> (Child, Option<String>) {
    let failed = matches!(
        tokio::time::timeout(CHILD_EXIT_GRACE, child.wait()).await,
        Ok(Ok(status)) if !status.success()
    );
    let said = match failed {
        true => tokio::time::timeout(CHILD_EXIT_GRACE, stderr)
            .await
            .ok()
            .and_then(Result::ok)
            .and_then(|kept| {
                String::from_utf8_lossy(&kept)
                    .lines()
                    .map(str::trim)
                    .rfind(|line| !line.is_empty())
                    .map(str::to_owned)
            }),
        false => None,
    };
    (child, said)
}

struct ChildIo {
    reader: ChildStdout,
    writer: Option<ChildStdin>,
    exit: Option<ChildExit>,
}

impl ChildIo {
    /// `ended`, or once the child exits, the error it gave for ending the stream.
    fn poll_ended<T>(
        &mut self,
        context: &mut TaskContext<'_>,
        ended: io::Result<T>,
    ) -> Poll<io::Result<T>> {
        loop {
            match self.exit.take() {
                Some(ChildExit::Running { child, stderr }) => {
                    self.exit = Some(ChildExit::Ending(Box::pin(last_words(child, stderr))));
                }
                Some(ChildExit::Ending(mut ending)) => match ending.as_mut().poll(context) {
                    Poll::Ready((child, said)) => {
                        self.exit = Some(ChildExit::Ended { child, said })
                    }
                    Poll::Pending => {
                        self.exit = Some(ChildExit::Ending(ending));
                        return Poll::Pending;
                    }
                },
                Some(ChildExit::Ended { child, said }) => {
                    let error = said
                        .as_ref()
                        .map(|said| io::Error::new(io::ErrorKind::BrokenPipe, said.clone()));
                    self.exit = Some(ChildExit::Ended { child, said });
                    return Poll::Ready(error.map_or(ended, Err));
                }
                None => return Poll::Ready(ended),
            }
        }
    }

    #[cfg(test)]
    fn child(&mut self) -> &mut Child {
        match self.exit.as_mut() {
            Some(ChildExit::Running { child, .. } | ChildExit::Ended { child, .. }) => child,
            _ => panic!("the child is held while the stream ends"),
        }
    }
}

impl AsyncRead for ChildIo {
    fn poll_read(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let before = buffer.filled().len();
        match std::task::ready!(Pin::new(&mut self.reader).poll_read(context, buffer)) {
            Ok(()) if buffer.filled().len() == before && buffer.remaining() > 0 => {
                self.poll_ended(context, Ok(()))
            }
            Err(error) => self.poll_ended(context, Err(error)),
            read => Poll::Ready(read),
        }
    }
}

impl AsyncWrite for ChildIo {
    fn poll_write(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
        buffer: &[u8],
    ) -> Poll<io::Result<usize>> {
        let written = match self.writer.as_mut() {
            Some(writer) => std::task::ready!(Pin::new(writer).poll_write(context, buffer)),
            None => return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into())),
        };
        match written {
            Err(error) => self.poll_ended(context, Err(error)),
            written => Poll::Ready(written),
        }
    }

    fn poll_flush(mut self: Pin<&mut Self>, context: &mut TaskContext<'_>) -> Poll<io::Result<()>> {
        match self.writer.as_mut() {
            Some(writer) => Pin::new(writer).poll_flush(context),
            None => Poll::Ready(Ok(())),
        }
    }

    fn poll_shutdown(
        mut self: Pin<&mut Self>,
        context: &mut TaskContext<'_>,
    ) -> Poll<io::Result<()>> {
        if let Some(writer) = self.writer.as_mut() {
            std::task::ready!(Pin::new(writer).poll_shutdown(context))?;
        }
        // Pipe shutdown alone does not deliver EOF; close the owned write descriptor.
        self.writer.take();
        Poll::Ready(Ok(()))
    }
}

/// Name the capability and target on a bare `Unsupported` for a catalogued route.
#[must_use]
pub(crate) fn name_refusal(mut error: RpcError, path: &str, target: &MachineTarget) -> RpcError {
    if error.code == RpcErrorCode::Unsupported
        && error.details.is_null()
        && let Some(details) = refusal_details(path, target)
    {
        error.details = details;
    }
    error
}

fn refusal_details(path: &str, target: &MachineTarget) -> Option<Value> {
    ployz_core::capability_of(path)
        .map(|capability| json!({ "capability": capability, "target": target.as_str() }))
}

pub(crate) fn rpc_error(error: ConnectError) -> RpcError {
    match error {
        ConnectError::AllFailed {
            attempts: 1,
            last: Some(error),
            ..
        } if matches!(
            *error,
            ConnectError::ClientRefused | ConnectError::ClientCleared | ConnectError::OlderMachine
        ) =>
        {
            rpc_error(*error)
        }
        ConnectError::ClientCleared => RpcError {
            code: RpcErrorCode::Unauthenticated,
            message: "Machine confirmed this Management Client was cleared".into(),
            details: json!({ "management_client": "cleared" }),
            cause: Vec::new(),
        },
        ConnectError::Remote(error) => error,
        ConnectError::Rpc(error) => error.to_rpc_error(),
        error @ ConnectError::OlderMachine => RpcError {
            code: RpcErrorCode::Unsupported,
            message: error.to_string(),
            details: json!({ "management_protocol": "older" }),
            cause: Vec::new(),
        },
        error @ (ConnectError::IdentityMismatch { .. }
        | ConnectError::ClientRefused
        | ConnectError::TunnelNeedsFull) => RpcError {
            code: RpcErrorCode::Unauthenticated,
            message: error.to_string(),
            details: Value::Null,
            cause: Vec::new(),
        },
        error @ ConnectError::Codec(CodecError::DecodeJson(_)) => RpcError {
            details: json!({ "answer": UNREADABLE_ANSWER }),
            ..crate::ui::rpc_error(RpcErrorCode::Internal, &error)
        },
        error @ (ConnectError::Attempt(_)
        | ConnectError::Exhausted(_)
        | ConnectError::EntryNotReady
        | ConnectError::Io(_)
        | ConnectError::Dial(_)
        | ConnectError::MissingMachineDetails
        | ConnectError::SshClientMissing
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
        | ConnectError::Value(_)) => crate::ui::rpc_error(RpcErrorCode::Internal, &error),
    }
}

const UNREADABLE_ANSWER: &str = "unreadable";

/// A daemon answered in a form this ployz can't decode, as a newer release's
/// answer can be: updating ployz here fixes it, not the network.
pub(crate) fn unreadable_answer(error: &RpcError) -> bool {
    error.details.get("answer").and_then(Value::as_str) == Some(UNREADABLE_ANSWER)
}

impl From<ConnectError> for RpcError {
    fn from(error: ConnectError) -> Self {
        rpc_error(error)
    }
}

pub(crate) async fn apply_timeout<T>(
    timeout: Option<Duration>,
    future: impl Future<Output = Result<T, ConnectError>>,
) -> Result<T, ConnectError> {
    match timeout {
        Some(timeout) => tokio::time::timeout(timeout, future)
            .await
            .unwrap_or_else(|_| {
                Err(ConnectError::Rpc(TransportError::budget_expired(
                    "target Machine RPC timed out",
                )))
            }),
        None => future.await,
    }
}

pub(crate) fn target_request<T>(payload: T, target: Option<&MachineTarget>) -> tonic::Request<T> {
    let mut request = tonic::Request::new(payload);
    if let Some(target) = target {
        apply_one_target(request.metadata_mut(), target);
    }
    request
}

/// How long a dial over `transport` may go unconfirmed before the next connection
/// is dialled beside it. A down Server holds its dial for the whole connect bound,
/// 15 s through Cloud and the SSH timeout over SSH. A daemon that answers confirms
/// well inside 1 s, but a cold SSH dial may take seconds while healthy, and racing
/// it costs a second login.
fn entry_stagger(transport: &Transport) -> Duration {
    match transport {
        Transport::Ssh { .. } => Duration::from_secs(5),
        Transport::Management(_) | Transport::Tcp(_) | Transport::Unix(_) => Duration::from_secs(1),
    }
}

/// Walk every connection and keep the first whose daemon answers. Each connection
/// starts when the one before it fails or stays silent for its [`entry_stagger`].
///
/// A tunnel or lazy channel is not enough. Confirmation is one-shot so a down
/// daemon cannot stall the walk or consume unary retries.
///
/// # Errors
///
/// Returns [`ConnectError::SshClientMissing`] when the local ssh client cannot
/// be spawned, only SSH connections remain and no earlier daemon answered. Returns
/// [`ConnectError::AllFailed`] after every connection is tried.
pub async fn connect_selected_with(
    selected: SelectedConnections,
    connector: Arc<dyn Connector>,
) -> Result<Client, ConnectError> {
    connect_selected(selected, connector, true).await
}

/// [`connect_selected_with`] without the race: each connection starts only once the one
/// before it fails, for a caller that acts on the entry itself and so must reach the
/// first one that answers, however slowly.
///
/// # Errors
///
/// As [`connect_selected_with`].
pub(crate) async fn connect_selected_in_order(
    selected: SelectedConnections,
    connector: Arc<dyn Connector>,
) -> Result<Client, ConnectError> {
    connect_selected(selected, connector, false).await
}

async fn connect_selected(
    selected: SelectedConnections,
    connector: Arc<dyn Connector>,
    race: bool,
) -> Result<Client, ConnectError> {
    let mut failures = BTreeMap::new();
    {
        let (source, connector) = (&selected.source, &connector);
        let mut queued = selected.connections.iter().enumerate();
        let mut dialing = FuturesUnordered::new();
        let stagger = tokio::time::sleep(Duration::ZERO);
        tokio::pin!(stagger);
        // Dials the next connection, if any, and says whether more are queued.
        let mut dial_next = |dialing: &mut FuturesUnordered<_>, stagger: Pin<&mut Sleep>| {
            if let Some((index, connection)) = queued.next() {
                let beside = !dialing.is_empty();
                stagger.reset(Instant::now() + entry_stagger(connection.transport()));
                dialing.push(async move {
                    let outcome = connect_one(connection, source, connector, beside).await;
                    (index, connection, outcome)
                });
            }
            queued.len() > 0
        };
        let mut more = dial_next(&mut dialing, stagger.as_mut());
        while !dialing.is_empty() {
            tokio::select! {
                Some((index, connection, outcome)) = dialing.next() => {
                    let error = match outcome {
                        Ok(client) => return Ok(client),
                        Err(error) => error,
                    };
                    failures.insert(index, (connection, error));
                    more = dial_next(&mut dialing, stagger.as_mut());
                }
                () = &mut stagger, if race && more => {
                    more = dial_next(&mut dialing, stagger.as_mut());
                }
            }
        }
    }
    let mut last_error = None;
    let mut server = None;
    let mut relayed = false;
    let mut retry = Retry::Never;
    let mut reached = false;
    for (index, (connection, error)) in failures {
        if matches!(error, ConnectError::SshClientMissing)
            && !reached
            && selected
                .connections
                .iter()
                .skip(index + 1)
                .all(|next| matches!(next.transport(), Transport::Ssh { .. }))
        {
            return Err(error);
        }
        if error.retry() != Retry::Never {
            retry = Retry::AfterRestart;
        }
        reached |= !error.unreached();
        last_error = Some(error);
        server = connection.server();
        relayed = matches!(connection.transport(), Transport::Management(_));
    }
    Err(ConnectError::AllFailed {
        selection: selected.source,
        attempts: selected.connections.len(),
        server,
        relayed,
        retry,
        reached,
        last: last_error.map(Box::new),
    })
}

/// Dial and confirm one entry, `beside` another dial still in flight or alone.
pub(crate) async fn connect_one(
    connection: &Connection,
    source: &ConnectionSource,
    connector: &Arc<dyn Connector>,
    beside: bool,
) -> Result<Client, ConnectError> {
    let channel = if beside {
        connector.connect_beside(connection)
    } else {
        connector.connect(connection)
    }
    .await?;
    let mut client = Client::new(
        channel,
        connection.clone(),
        source.clone(),
        connector.clone(),
    );
    let mut stage = diagnostics::StageGuard::begin(diagnostics::Stage::EntryDescribeContract);
    let confirmed = client.confirm_entry().await;
    stage.finish(&confirmed);
    confirmed.map_err(|error| naming_local_refusal(connection, error))?;
    Ok(client)
}

/// A refusing start can stop a local socket while the first call is still being
/// written. h2 then fails the call with its own broken pipe, dropping the words
/// [`LocalDaemon`] attached, so read the refusal here as well (#3715).
fn naming_local_refusal(connection: &Connection, error: ConnectError) -> ConnectError {
    let Transport::Unix(path) = connection.transport() else {
        return error;
    };
    if error.startup_refused() {
        return error;
    }
    let ConnectError::Rpc(transport) = error else {
        return error;
    };
    if !transport.local {
        return ConnectError::Rpc(transport);
    }
    match path.parent().and_then(ployz_core::startup_refusal) {
        Some(refusal) => ConnectError::Rpc(transport.caused_by(refusal)),
        None => ConnectError::Rpc(transport),
    }
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
    if let Some(name) = context_override {
        return Err(ConnectError::Context(ContextError::ContextNotFound {
            name: name.to_owned(),
            path: config_path.to_owned(),
        }));
    }
    // A socket this user can't even see is not one it can use: fall through
    // to Cloud, whose sign-in hint is the useful one.
    let socket_available = match local_socket.try_exists() {
        Ok(exists) => exists,
        Err(source) if source.kind() == io::ErrorKind::PermissionDenied => false,
        Err(source) => {
            return Err(ConnectError::Path {
                path: local_socket.to_owned(),
                source,
            });
        }
    };
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

#[derive(Debug, Error)]
pub enum ConnectError {
    #[error("entry Machine identity mismatch: expected {expected}, received {actual}")]
    IdentityMismatch {
        expected: MachineId,
        actual: MachineId,
    },
    #[error("connection attempt failed: {0}")]
    Attempt(Cow<'static, str>),
    #[error(transparent)]
    Exhausted(Box<dyn std::error::Error + Send + Sync>),
    /// Not retried: a starting daemon costs one confirm timeout, not one per retry.
    #[error(
        "the Server's daemon accepted the connection but didn't answer within {:?}; it may still be starting",
        CONNECT_CONFIRM_TIMEOUT
    )]
    EntryNotReady,
    #[error("Couldn't open a connection to the Server.")]
    Io(#[from] io::Error),
    #[error("Couldn't open a connection to the Server.")]
    Dial(#[from] tonic::transport::Error),
    #[error("connection attempt failed: inspect response omitted Machine details")]
    MissingMachineDetails,
    #[error("Machine refused this Management Capability")]
    ClientRefused,
    #[error("Machine confirmed this Management Client was cleared")]
    ClientCleared,
    #[error("tunnels need Full access")]
    TunnelNeedsFull,
    /// The Machine speaks the management protocol of Ployz 0.2 or 0.3.0-beta.6 and earlier, which
    /// this release does not; only its own upgrade reaches it, over SSH.
    #[error(
        "This Server runs Ployz 0.2 or a 0.3.0 beta up to beta.6, which this release can't reach through the relay. \
         Upgrade it over SSH: ployz server upgrade stable SERVER --connect ssh://USER@HOST. \
         Docs: ployz docs servers/manage-servers.md"
    )]
    OlderMachine,
    #[error("local ssh client not found; install an ssh client")]
    SshClientMissing,
    #[error("connection attempt failed: SSH probe to {target} exited with {status}: {detail}{}", failure.hint())]
    SshProbe {
        target: String,
        status: std::process::ExitStatus,
        failure: SshFailure,
        detail: String,
    },
    #[error("Machine RPC routing metadata is invalid.")]
    Routing(#[from] RoutingMetadataError),
    #[error("The connection task failed.")]
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
    #[error("Could not inspect {path}.")]
    Path { path: PathBuf, source: io::Error },
    #[error("{}", all_failed(*attempts, selection, *reached, server.as_deref(), *relayed, last.as_deref()))]
    AllFailed {
        selection: ConnectionSource,
        attempts: usize,
        /// The Server the last connection tried enters, by name or else Machine ID.
        server: Option<String>,
        /// The last connection tried went through Ployz Cloud's relay.
        relayed: bool,
        /// [`Retry::AfterRestart`] when any connection may still come up.
        retry: Retry,
        /// A daemon answered at least one connection, even if `last` did not reach one.
        reached: bool,
        #[source]
        last: Option<Box<ConnectError>>,
    },
    #[error("Ployz's call to your Server's daemon failed.")]
    Rpc(#[source] TransportError),
    #[error("Machine RPC payload failed.")]
    Codec(#[from] CodecError),
    /// The daemon's own error, kept as the source so the causes it sent stay in the chain.
    #[error("{0}")]
    Remote(#[source] RpcError),
    #[error("Machine RPC framing failed.")]
    Framing(#[from] FramingError),
    #[error("Machine RPC identity failed.")]
    Value(#[from] ployz_core::ValueError),
}

/// That a Server gave no answer, and what to check. Through Ployz Cloud's relay, the
/// Server's network is as likely the cause as its daemon.
fn isnt_answering(relayed: bool) -> &'static str {
    if relayed {
        "isn't answering through Ployz Cloud. Check that it is online, its Ployz daemon is \
         running, and its outgoing https isn't filtered, then retry."
    } else {
        "isn't answering. Check that its Ployz daemon is running, then retry."
    }
}

/// `server`, by name or Machine ID, gave no answer to a connection this process made.
pub(crate) fn not_answering(server: &str, relayed: bool) -> String {
    format!("Server {server} {}", isnt_answering(relayed))
}

/// Say a daemon answered when one did: a refusal is not an unreachable Server. Name the
/// Server tried last where the connection says which it is, and how `last` failed there.
fn all_failed(
    attempts: usize,
    selection: &ConnectionSource,
    reached: bool,
    server: Option<&str>,
    relayed: bool,
    last: Option<&ConnectError>,
) -> String {
    match (attempts, reached, server) {
        (1, false, Some(server)) => not_answering(server, relayed),
        (1, false, None) => "Ployz couldn't reach your Server's daemon.".into(),
        (1, true, Some(server)) => {
            format!("Server {server}'s daemon answered, but the connection was refused.")
        }
        (1, true, None) => "Your Server's daemon answered, but the connection was refused.".into(),
        (_, false, Some(server)) => format!(
            "Ployz couldn't reach a Server's daemon through any of the {attempts} connections from {selection}. \
             Server {server}, tried last, {}",
            isnt_answering(relayed)
        ),
        (_, false, None) => format!(
            "Ployz couldn't reach a Server's daemon through any of the {attempts} connections from {selection}."
        ),
        (_, true, Some(server)) => {
            let how = if last.is_some_and(|last| !last.unreached()) {
                "refused the connection."
            } else {
                isnt_answering(relayed)
            };
            format!(
                "A Server's daemon answered, but the {attempts} connections from {selection} all failed. \
                 Server {server}, tried last, {how}"
            )
        }
        (_, true, None) => format!(
            "A Server's daemon answered, but the {attempts} connections from {selection} all failed."
        ),
    }
}

impl From<tonic::Status> for ConnectError {
    fn from(status: tonic::Status) -> Self {
        Self::Rpc(TransportError::from(status))
    }
}

impl ConnectError {
    fn from_ssh_spawn(error: io::Error) -> Self {
        if error.kind() == io::ErrorKind::NotFound {
            Self::SshClientMissing
        } else {
            Self::Io(error)
        }
    }

    pub(crate) fn retry(&self) -> Retry {
        match self {
            Self::Attempt(_) | Self::Exhausted(_) | Self::Dial(_) | Self::Join(_) => Retry::Now,
            Self::Io(error) if transient_io(error) => Retry::Now,
            Self::SshProbe {
                failure: SshFailure::Network,
                ..
            } => Retry::Now,
            Self::Rpc(error) => match rpc_error_code(error.code).retry() {
                // A restarting daemon drops the connection without answering.
                Retry::Never if error.unanswered => Retry::AfterRestart,
                retry @ (Retry::Now | Retry::AfterRestart | Retry::Never) => retry,
            },
            // A socket-activated daemon accepts before it serves.
            Self::EntryNotReady => Retry::AfterRestart,
            Self::AllFailed { retry, .. } => *retry,
            Self::Io(_)
            | Self::SshProbe { .. }
            | Self::Remote(_)
            | Self::IdentityMismatch { .. }
            | Self::ClientRefused
            | Self::OlderMachine
            | Self::ClientCleared
            | Self::TunnelNeedsFull
            | Self::MissingMachineDetails
            | Self::SshClientMissing
            | Self::Routing(_)
            | Self::ProxyUnsupported(_)
            | Self::UnsupportedNetwork(_)
            | Self::Config(_)
            | Self::Connection(_)
            | Self::Context(_)
            | Self::Path { .. }
            | Self::Codec(_)
            | Self::Framing(_)
            | Self::Value(_) => Retry::Never,
        }
    }

    /// Repeatable reads retry until the daemon answers; an answer is final.
    /// They must not spend a minute retrying missing credentials or keys.
    pub(crate) fn is_setup_retryable(&self) -> bool {
        self.retry() != Retry::Never
    }

    /// [`Self::is_setup_retryable`] for a wait on one Server's daemon to start: its
    /// startup refusal ends the wait, since only an operator's safe recovery clears
    /// it (#3703). Not part of [`Self::retry`], because an upgrade's compensation
    /// restores the previous release and clears a refusal on its own.
    pub(crate) fn is_start_retryable(&self) -> bool {
        self.is_setup_retryable() && !self.startup_refused()
    }

    fn startup_refused(&self) -> bool {
        if let Self::AllFailed { attempts, last, .. } = self {
            // Another connection may still come up.
            return *attempts == 1 && last.as_deref().is_some_and(Self::startup_refused);
        }
        std::iter::once(self.to_string())
            .chain(ployz_core::error_chain::causes(self))
            .any(|line| line.contains(ployz_core::STARTUP_REFUSED))
    }

    pub(crate) fn is_unreachable(&self) -> bool {
        matches!(
            self,
            Self::Attempt(_)
                | Self::Exhausted(_)
                | Self::EntryNotReady
                | Self::Io(_)
                | Self::Dial(_)
                | Self::AllFailed { .. }
        ) || matches!(self, Self::Rpc(error) if error.is_unavailable())
    }

    /// No daemon answered: selection, dialing or the transport failed first. A daemon's
    /// own refusal, identity or protocol is an answer, and so is anything unclassified.
    pub(crate) fn unreached(&self) -> bool {
        match self {
            Self::Attempt(_)
            | Self::Exhausted(_)
            | Self::EntryNotReady
            | Self::Io(_)
            | Self::Dial(_)
            | Self::SshClientMissing
            | Self::SshProbe { .. }
            | Self::ProxyUnsupported(_)
            | Self::UnsupportedNetwork(_)
            | Self::Config(_)
            | Self::Connection(_)
            | Self::Context(_)
            | Self::Path { .. } => true,
            Self::Rpc(error) => error.local || error.budget_expired,
            Self::AllFailed { reached, .. } => !reached,
            Self::IdentityMismatch { .. }
            | Self::MissingMachineDetails
            | Self::ClientRefused
            | Self::OlderMachine
            | Self::ClientCleared
            | Self::TunnelNeedsFull
            | Self::Routing(_)
            | Self::Join(_)
            | Self::Codec(_)
            | Self::Remote(_)
            | Self::Framing(_)
            | Self::Value(_) => false,
        }
    }

    /// The entry's router made this status for a target that didn't answer, and it came back
    /// over a working channel: the entry itself answered.
    pub(crate) fn routed_by_entry(&self) -> bool {
        matches!(self, Self::Rpc(error)
            if error.routed_unanswered && !error.local && !error.session_closed)
    }

    /// The read's whole budget ran out without a reply, or the entry's router could not
    /// get one from the target. A status a daemon sent, or a failure reaching the entry,
    /// says nothing about whether the target lives.
    pub(crate) fn target_unanswered(&self) -> bool {
        matches!(self, Self::Rpc(error) if error.budget_expired || error.routed_unanswered)
    }

    /// A daemon sent this status itself, so the target was not silent. A call on
    /// a closed session never left this client, so nothing replied to it.
    pub(crate) fn target_replied(&self) -> bool {
        matches!(self, Self::Rpc(error) if !error.session_closed) && !self.unreplied()
    }

    /// The call went out and no status came back: the client's connection dropped, its
    /// deadline elapsed, or the entry's router heard nothing from the target. Whether the
    /// target acted on it is unknown.
    pub(crate) fn unreplied(&self) -> bool {
        matches!(self, Self::Rpc(error)
            if error.unanswered || error.budget_expired || error.routed_unanswered)
    }

    /// The entry's router said its path to the target drops large packets.
    pub(crate) fn large_packets_dropped(&self) -> bool {
        matches!(self, Self::Rpc(error)
            if error.details.get("reason").and_then(Value::as_str)
                == Some(ployz_core::rpc::LARGE_PACKETS_DROPPED_REASON))
    }

    /// The entry's router could not dial the target, so the target never got the call.
    /// An entry Machine too old to say so leaves this false.
    pub(crate) fn target_unreached(&self) -> bool {
        matches!(self, Self::Rpc(error) if error.routed_unreached)
    }
}

/// A target that replied on an earlier attempt was not silent: its reply stands in for a
/// later silence. The caller stores only failures where `target_replied` holds.
pub(crate) fn keep_target_reply(
    reply: &mut Option<ConnectError>,
    error: ConnectError,
) -> ConnectError {
    match reply.take() {
        Some(earlier) if error.target_unanswered() => earlier,
        kept => {
            *reply = kept;
            error
        }
    }
}

/// A call that went out and lost its answer may have been done, and a later attempt that
/// never reached the target says nothing about it, so the earlier loss stands. A target
/// that stayed silent is stronger news than an earlier drop, and a failure that no retry
/// fixes, such as a refused SSH key, is what the user has to act on.
pub(crate) fn keep_lost_call(lost: Option<ConnectError>, error: ConnectError) -> ConnectError {
    match lost {
        Some(lost)
            if error.unreached()
                && !error.target_unanswered()
                && error.retry() != ployz_core::Retry::Never =>
        {
            lost
        }
        _ => error,
    }
}

/// A gRPC transport failure. Display is the status message, not Status Debug.
#[derive(Clone, Debug, Eq, Error, PartialEq)]
#[error("{message}")]
pub struct TransportError {
    code: tonic::Code,
    message: String,
    details: Value,
    /// Worth retrying as a silence: the client produced this status, or it is Cancelled.
    unanswered: bool,
    /// The client's connection or stream failed: no status crossed the wire.
    local: bool,
    /// The client's own deadline for the whole call elapsed.
    budget_expired: bool,
    /// The entry's router marked this status: the target did not answer it.
    routed_unanswered: bool,
    /// The entry's router marked this status: its dial to the target failed, so the
    /// target never got the call. Never set on a local failure, whose metadata may be
    /// headers that came back before the stream broke.
    routed_unreached: bool,
    /// The channel's one stream closed: resending on it can never succeed, so
    /// [`TransportError::to_rpc_error`] marks it for [`on_closed_session`].
    session_closed: bool,
    /// The status carried details, which ployzd attaches and tonic or a proxy never does.
    detailed: bool,
    /// Why the client's connection or stream failed, below tonic's own words.
    #[source]
    cause: Option<crate::failure::JoinedChain>,
}

impl TransportError {
    /// The target's daemon sent this status itself, with its own message and the details
    /// ployzd attaches. A status tonic or a proxy made up carries no details.
    pub(crate) fn answered(&self) -> bool {
        !(self.unanswered
            || self.local
            || self.budget_expired
            || self.routed_unanswered
            || self.routed_unreached
            || self.session_closed
            || self.message.is_empty()
            || !self.detailed)
    }

    /// The call's path to its target went away: the client's connection or stream broke, the
    /// call was cancelled, its session closed, or the entry marked the target silent or unreached.
    /// A status tonic or the daemon made, such as a decode failure, is not one.
    pub(crate) fn connection_lost(&self) -> bool {
        self.unanswered || self.session_closed || self.routed_unanswered || self.routed_unreached
    }

    pub(crate) fn budget_expired(message: &str) -> Self {
        Self {
            budget_expired: true,
            ..Self::from(tonic::Status::deadline_exceeded(message))
        }
    }

    pub(crate) fn from_stream_status(status: tonic::Status) -> Self {
        let mut error = Self::from(status);
        if error.unanswered {
            error.code = tonic::Code::Unavailable;
        }
        error
    }

    pub(crate) fn name_refusal(mut self, path: &str, target: &MachineTarget) -> Self {
        if self.code == tonic::Code::Unimplemented
            && self.details.is_null()
            && let Some(details) = refusal_details(path, target)
        {
            self.details = details;
        }
        self
    }

    /// This error with `line` as its deepest cause.
    fn caused_by(mut self, line: String) -> Self {
        let mut lines: Vec<String> = self
            .cause
            .as_ref()
            .map(|cause| {
                std::iter::once(cause.to_string())
                    .chain(ployz_core::error_chain::causes(cause))
                    .collect()
            })
            .unwrap_or_default();
        lines.push(line);
        self.cause = crate::failure::JoinedChain::of(lines);
        self
    }

    pub(crate) fn addressed_as(mut self, target: &str) -> Self {
        if let Some(details) = self
            .details
            .as_object_mut()
            .filter(|details| details.contains_key("capability"))
        {
            details.insert("target".into(), target.into());
        }
        self
    }

    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
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
            details: if self.session_closed {
                json!({ "reason": SESSION_CLOSED_REASON })
            } else {
                self.details.clone()
            },
            cause: ployz_core::error_chain::causes(self),
        }
    }
}

impl From<tonic::Status> for TransportError {
    fn from(status: tonic::Status) -> Self {
        // Remote statuses cross the wire without a source. Tonic attaches one
        // only when the client connection or stream itself fails.
        let source = std::error::Error::source(&status);
        let local = source.is_some();
        let session_closed = std::iter::successors(source, |error| error.source()).any(|error| {
            error
                .downcast_ref::<io::Error>()
                .and_then(io::Error::get_ref)
                .is_some_and(|inner| inner.is::<SessionClosed>())
        });
        // A call on a closed session never left this client.
        let unanswered = (status.code() == tonic::Code::Cancelled || local) && !session_closed;
        let sent = ployz_core::rpc::status_causes(status.details());
        let cause = match source {
            Some(source) => {
                let mut lines: Vec<String> = std::iter::once(source.to_string())
                    .chain(crate::ui::causes(source))
                    .skip_while(|line| line == status.message())
                    .collect();
                lines.dedup();
                crate::failure::JoinedChain::of(lines)
            }
            None => sent.clone().and_then(crate::failure::JoinedChain::of),
        };
        Self {
            unanswered,
            local,
            budget_expired: false,
            routed_unanswered: status
                .metadata()
                .contains_key(ployz_core::rpc::TARGET_UNANSWERED_KEY),
            routed_unreached: !local
                && status
                    .metadata()
                    .contains_key(ployz_core::rpc::TARGET_UNREACHED_KEY),
            session_closed,
            detailed: !status.details().is_empty(),
            cause,
            code: status.code(),
            message: status.message().to_owned(),
            details: if status.details().is_empty() {
                Value::Null
            } else {
                ployz_core::rpc::status_details(status.details()).unwrap_or_else(
                    || json!({ "grpc_details": String::from_utf8_lossy(status.details()) }),
                )
            },
        }
    }
}

fn transient_io(error: &io::Error) -> bool {
    crate::setup_retry::temporary_dns(error)
        || matches!(
            error.kind(),
            io::ErrorKind::ConnectionRefused
                | io::ErrorKind::ConnectionReset
                | io::ErrorKind::ConnectionAborted
                | io::ErrorKind::BrokenPipe
                | io::ErrorKind::NotFound
                | io::ErrorKind::TimedOut
                | io::ErrorKind::NotConnected
                | io::ErrorKind::UnexpectedEof
                | io::ErrorKind::NetworkUnreachable
                | io::ErrorKind::HostUnreachable
                | io::ErrorKind::WouldBlock
                | io::ErrorKind::Interrupted
        )
}

/// What an SSH probe's stderr says went wrong. Exit status 255 covers every
/// ssh failure, so its text is the only channel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SshFailure {
    Network,
    Auth,
    Other,
}

impl SshFailure {
    fn of(detail: &str) -> Self {
        const NETWORK: [&str; 8] = [
            "Connection timed out",
            "Operation timed out",
            "Connection refused",
            "No route to host",
            "Network is unreachable",
            "Connection reset",
            "Connection closed",
            "Temporary failure in name resolution",
        ];
        if detail.contains("Permission denied") {
            Self::Auth
        } else if NETWORK.iter().any(|message| detail.contains(message)) {
            Self::Network
        } else {
            Self::Other
        }
    }

    fn hint(self) -> &'static str {
        match self {
            Self::Auth => {
                "; SSH authentication is noninteractive: unlock your key with ssh-add or configure credentials that do not require a prompt"
            }
            Self::Network | Self::Other => "",
        }
    }
}

/// The code of a refusal on the caller's access, such as a Server refusing a scope.
pub(crate) const FORBIDDEN: &str = "forbidden";

fn rpc_error_code(code: tonic::Code) -> RpcErrorCode {
    match code {
        tonic::Code::InvalidArgument => RpcErrorCode::InvalidArgument,
        tonic::Code::NotFound => RpcErrorCode::NotFound,
        tonic::Code::AlreadyExists | tonic::Code::Aborted => RpcErrorCode::Conflict,
        tonic::Code::Unavailable | tonic::Code::DeadlineExceeded => RpcErrorCode::Unavailable,
        tonic::Code::Unimplemented => RpcErrorCode::Unsupported,
        tonic::Code::Unauthenticated => RpcErrorCode::Unauthenticated,
        // The spelling Cloud's refusals already use; a reader that doesn't know it keeps it.
        tonic::Code::PermissionDenied => RpcErrorCode::from(FORBIDDEN),
        tonic::Code::Ok
        | tonic::Code::Cancelled
        | tonic::Code::Unknown
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
