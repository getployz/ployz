use std::{
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
    CodecError, ContractDescription, CreateVolumeRequest, DockerVolume, DockerVolumeId,
    MachineFailure, MachineId, MachineObservation, MachineRpcClient, MachineSelector,
    MachineSuccess, PartialResult, RpcError, RpcErrorCode, RpcRequest, RpcRequestBody, RpcResponse,
    RpcResponseBody,
};
use serde_json::{Value, json};
use thiserror::Error;
use tokio::{
    io::{AsyncRead, AsyncWrite, ReadBuf},
    process::{Child, ChildStdin, ChildStdout, Command},
};
use tonic::transport::{Channel, Endpoint};

use crate::context::{
    Config, ConfigError, Connection, ConnectionError, ConnectionSource, ContextError,
    SelectedConnections, Transport, expand_home, select_connections,
};

pub const DEFAULT_LOCAL_SOCKET: &str = "/run/ployz/ployz.sock";

pub trait ProxyStream: AsyncRead + AsyncWrite + Send + Unpin {}

impl<T: AsyncRead + AsyncWrite + Send + Unpin> ProxyStream for T {}

pub type BoxProxyStream = Box<dyn ProxyStream>;

// TODO: add the separately planned client-side WireGuard connector here.
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
        }
    }

    async fn dial_proxy(
        &self,
        connection: &Connection,
        network: &str,
        address: &str,
    ) -> Result<BoxProxyStream, ConnectError> {
        match connection.transport() {
            Transport::Tcp(_) | Transport::Unix(_) => {
                Err(ConnectError::ProxyUnsupported(connection.to_string()))
            }
            Transport::Ssh {
                destination,
                key_file,
            } if network == "tcp" => {
                let mut args =
                    ssh_base_args(destination, key_file.as_deref(), control_path().as_deref());
                args.extend(["-W".into(), address.into(), destination.target().into()]);
                spawn_ssh(&self.ssh_program, &args)
                    .map(|stream| Box::new(stream) as BoxProxyStream)
                    .map_err(|error| ConnectError::Attempt(error.to_string()))
            }
            Transport::Ssh { .. } => Err(ConnectError::UnsupportedNetwork(network.into())),
        }
    }
}

async fn connect_endpoint(target: String) -> Result<Channel, ConnectError> {
    Endpoint::from_shared(target)
        .map_err(|error| ConnectError::Attempt(error.to_string()))?
        .connect_timeout(Duration::from_secs(5))
        .connect()
        .await
        .map_err(|error| ConnectError::Attempt(error.to_string()))
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
        .await
        .map_err(|error| ConnectError::Attempt(error.to_string()))?;
    if !status.success() {
        return Err(ConnectError::Attempt(format!(
            "SSH probe to {} exited with {status}",
            destination.target()
        )));
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
        .map_err(|error| ConnectError::Attempt(error.to_string()))
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

#[derive(Clone)]
pub struct Client {
    pub(crate) rpc: MachineRpcClient<Channel>,
    connection: Connection,
    source: ConnectionSource,
    connector: Arc<dyn Connector>,
}

impl Client {
    #[must_use]
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    #[must_use]
    pub fn connection_source(&self) -> &ConnectionSource {
        &self.source
    }

    pub async fn describe_contract(&mut self) -> Result<ContractDescription, ConnectError> {
        self.request(RpcRequest::describe_contract(), None)
            .await?
            .decode_contract_description()
            .cloned()
            .map_err(ConnectError::Codec)
    }

    pub async fn list_machines(&mut self) -> Result<Vec<MachineObservation>, ConnectError> {
        self.request(RpcRequest::list_machines(), None)
            .await?
            .decode_machine_list()
            .map(<[_]>::to_vec)
            .map_err(ConnectError::Codec)
    }

    pub async fn create_volume(
        &mut self,
        machine_id: &MachineId,
        request: CreateVolumeRequest,
    ) -> Result<DockerVolume, ConnectError> {
        let target = MachineSelector::from(machine_id);
        self.request(RpcRequest::create_volume(request), Some(&target))
            .await?
            .decode_volume_created()
            .cloned()
            .map_err(ConnectError::Codec)
    }

    pub async fn list_volumes(
        &mut self,
        machines: &[MachineObservation],
    ) -> PartialResult<Vec<DockerVolume>, RpcError> {
        // UT-028: keep every target's success or typed failure instead of warning and omitting it.
        let mut requests = tokio::task::JoinSet::new();
        for (index, machine) in machines.iter().enumerate() {
            let machine_id = machine.machine.id.clone();
            let mut client = self.clone();
            requests.spawn(async move {
                let target = MachineSelector::from(&machine_id);
                let outcome = client
                    .request(RpcRequest::list_volumes(), Some(&target))
                    .await
                    .and_then(|response| {
                        response
                            .decode_volume_list()
                            .map(<[_]>::to_vec)
                            .map_err(ConnectError::Codec)
                    });
                (index, machine_id, outcome)
            });
        }
        let mut outcomes = Vec::with_capacity(machines.len());
        while let Some(outcome) = requests.join_next().await {
            outcomes.push(outcome.expect("Volume listing task does not panic"));
        }
        outcomes.sort_by_key(|(index, _, _)| *index);
        let mut result = PartialResult {
            successes: Vec::new(),
            failures: Vec::new(),
            omissions: Vec::new(),
        };
        for (_, machine_id, outcome) in outcomes {
            match outcome {
                Ok(volumes) => result.successes.push(MachineSuccess {
                    machine_id,
                    value: volumes,
                }),
                Err(error) => result.failures.push(MachineFailure {
                    machine_id,
                    error: rpc_error(error),
                }),
            }
        }
        result
    }

    pub async fn inspect_volume(
        &mut self,
        id: &DockerVolumeId,
    ) -> Result<DockerVolume, ConnectError> {
        let target = MachineSelector::from(&id.machine_id);
        self.request(RpcRequest::inspect_volume(id.name.clone()), Some(&target))
            .await?
            .decode_volume_details()
            .cloned()
            .map_err(ConnectError::Codec)
    }

    pub async fn remove_volume(
        &mut self,
        id: DockerVolumeId,
        force: bool,
    ) -> Result<(), ConnectError> {
        let target = MachineSelector::from(&id.machine_id);
        self.request(RpcRequest::remove_volume(id.name, force), Some(&target))
            .await?
            .decode_volume_removed()
            .map_err(ConnectError::Codec)
    }

    pub async fn request(
        &mut self,
        request: RpcRequest,
        target: Option<&MachineSelector>,
    ) -> Result<RpcResponse, ConnectError> {
        let payload = target_request(request.encode()?, target);
        macro_rules! dispatch {
            (
                unary { $($variant:ident: ($method:ident, $route:literal, $request:ty, $command:literal),)+ }
                server_streaming { $($stream_variant:ident: ($stream_method:ident, $stream_route:literal, $stream_request:ty, $stream_command:literal),)+ }
            ) => {
                match request.body {
                    $(RpcRequestBody::$variant(_) => self.rpc.$method(payload).await?,)+
                    $(RpcRequestBody::$stream_variant(_) => return Err(ConnectError::StreamingRequest($stream_command)),)+
                }
            };
        }
        let payload = ployz_core::rpc_catalog!(dispatch);
        let response = payload.into_inner().decode_response()?;
        if let RpcResponseBody::Error(error) = &response.body {
            return Err(ConnectError::Remote(error.clone()));
        }
        Ok(response)
    }

    pub async fn dial_proxy(
        &self,
        network: &str,
        address: &str,
    ) -> Result<BoxProxyStream, ConnectError> {
        self.connector
            .dial_proxy(&self.connection, network, address)
            .await
    }
}

pub(crate) fn rpc_error(error: ConnectError) -> RpcError {
    match error {
        ConnectError::Remote(error) => error,
        ConnectError::Rpc(status) => RpcError {
            code: match status.code() {
                tonic::Code::InvalidArgument => RpcErrorCode::InvalidArgument,
                tonic::Code::NotFound => RpcErrorCode::NotFound,
                tonic::Code::AlreadyExists | tonic::Code::Aborted => RpcErrorCode::Conflict,
                tonic::Code::Unavailable | tonic::Code::DeadlineExceeded => {
                    RpcErrorCode::Unavailable
                }
                tonic::Code::Unimplemented => RpcErrorCode::Unsupported,
                tonic::Code::Ok
                | tonic::Code::Cancelled
                | tonic::Code::Unknown
                | tonic::Code::PermissionDenied
                | tonic::Code::ResourceExhausted
                | tonic::Code::FailedPrecondition
                | tonic::Code::OutOfRange
                | tonic::Code::Internal
                | tonic::Code::DataLoss
                | tonic::Code::Unauthenticated => RpcErrorCode::Internal,
            },
            message: status.message().into(),
            details: if status.details().is_empty() {
                Value::Null
            } else {
                json!({ "grpc_details": String::from_utf8_lossy(status.details()) })
            },
        },
        error @ (ConnectError::Attempt(_)
        | ConnectError::ProxyUnsupported(_)
        | ConnectError::UnsupportedNetwork(_)
        | ConnectError::Config(_)
        | ConnectError::Connection(_)
        | ConnectError::Context(_)
        | ConnectError::Path { .. }
        | ConnectError::AllFailed { .. }
        | ConnectError::Codec(_)
        | ConnectError::StreamingRequest(_)) => RpcError {
            code: RpcErrorCode::Internal,
            message: error.to_string(),
            details: Value::Null,
        },
    }
}

fn target_request(
    payload: ployz_core::OpaquePayload,
    target: Option<&MachineSelector>,
) -> tonic::Request<ployz_core::OpaquePayload> {
    let mut request = tonic::Request::new(payload);
    if let Some(target) = target {
        if target
            .as_str()
            .bytes()
            .all(|byte| (b'!'..=b'~').contains(&byte))
        {
            request.metadata_mut().insert(
                "machine",
                target.as_str().parse().expect("visible ASCII metadata"),
            );
        } else {
            request.metadata_mut().insert_bin(
                "machine-bin",
                tonic::metadata::MetadataValue::from_bytes(target.as_str().as_bytes()),
            );
        }
    }
    request
}

pub async fn connect_selected_with(
    selected: SelectedConnections,
    connector: Arc<dyn Connector>,
) -> Result<Client, ConnectError> {
    let mut last_error = None;
    for connection in &selected.connections {
        match connector.connect(connection).await {
            Ok(channel) => {
                return Ok(Client {
                    rpc: MachineRpcClient::new(channel),
                    connection: connection.clone(),
                    source: selected.source.clone(),
                    connector,
                });
            }
            Err(error) => last_error = Some(error),
        }
    }
    Err(ConnectError::AllFailed {
        source: selected.source,
        attempts: selected.connections.len(),
        last: last_error.map(Box::new),
    })
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

#[derive(Debug, Error)]
pub enum ConnectError {
    #[error("connection attempt failed: {0}")]
    Attempt(String),
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
    Rpc(Box<tonic::Status>),
    #[error("Machine RPC payload failed: {0}")]
    Codec(#[from] CodecError),
    #[error("Machine RPC returned: {}", .0.message)]
    Remote(RpcError),
    #[error("streaming RPC {0:?} must use the streaming client")]
    StreamingRequest(&'static str),
}

impl From<tonic::Status> for ConnectError {
    fn from(status: tonic::Status) -> Self {
        Self::Rpc(Box::new(status))
    }
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, process::Command as StdCommand, time::Duration};

    use super::*;
    use crate::context::SshDestination;

    #[test]
    fn non_ascii_machine_targets_use_binary_metadata() {
        let target = MachineSelector::parse("München edge").unwrap();
        let request = target_request(ployz_core::OpaquePayload::new(Vec::new()), Some(&target));

        assert!(request.metadata().get("machine").is_none());
        assert_eq!(
            request
                .metadata()
                .get_bin("machine-bin")
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
