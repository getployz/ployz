use std::env;
use std::fmt::{self, Display, Formatter};
use std::fs;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, ExitStatus, Stdio};
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use mvp_bus::{BusSession, FactKeyPattern, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_deploy::{DnsCommitId, GatewayCommitId, RouteCommitId, ServingCommitId, ServingCommitPlan};
use mvp_mesh::{WireGuardAppliedSnapshot, WireGuardSnapshotPaths, load_applied_snapshot};
use mvp_projection::{
    BackendEndpoint, DnsRecordFact, DnsRecordProjection, FactSource, GatewayRouteProjection,
    NodeId, ProjectionActorHandle, ProjectionActorStatus, ProjectionFactPayload, ProjectionReport,
    RouteId, ServingCommitFact, SqliteProjectionStore,
};
use mvp_serving::{
    DnsServerHandle, HttpGatewayHandle, ServingActorHandle, ServingError, ServingFreshness,
    ServingSnapshotPaths, ServingStatus, WireRoleMetrics, WireServingState, spawn_dns_server,
    spawn_http_gateway,
};
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UnixListener, UnixStream};
use tokio::process::{Child, Command as TokioCommand};
use tokio::sync::{Mutex, oneshot};
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

use crate::bus_syntax::fact_pattern;
use crate::process_fact_source::{ProcessFactReadPolicy, ProcessFactSource};

const PROJECT_TIMEOUT: Duration = Duration::from_secs(10);
const STALE_AFTER: Duration = Duration::from_secs(300);
const ROLE_CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const ROLE_REQUEST_READ_TIMEOUT: Duration = Duration::from_millis(250);
const ROLE_WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const ROLE_RESPONSE_TIMEOUT: Duration = Duration::from_secs(15);
const ROLE_MAX_REQUEST_BYTES: usize = 64 * 1024;
const READY_TIMEOUT: Duration = Duration::from_secs(3);
const CHILD_EXIT_TIMEOUT: Duration = Duration::from_secs(3);
const ROLE_REQUEST_PAUSE: Duration = Duration::from_millis(10);

pub(crate) fn run_role(args: Vec<String>) -> Result<(), String> {
    let [role, rest @ ..] = args.as_slice() else {
        return Err(
            "usage: mvp-e2e role <serving-projection|local-coordinator|remote-replication-injector> ..."
                .to_string(),
        );
    };
    match role.as_str() {
        "serving-projection" => {
            let config = ServingProjectionRoleConfig::parse(rest)?;
            runtime()?.block_on(run_serving_projection_role(config))
        }
        "local-coordinator" => {
            let config = LocalCoordinatorRoleConfig::parse(rest)?;
            runtime()?.block_on(run_local_coordinator_role(config))
        }
        "remote-replication-injector" => {
            let config = RemoteReplicationInjectorConfig::parse(rest)?;
            let response = run_remote_replication_injector(config);
            let json = serde_json::to_string(&response)
                .map_err(|error| format!("serialize injector response: {error}"))?;
            println!("{json}");
            Ok(())
        }
        "http-gateway" => {
            let config = WireRoleConfig::parse(rest, "http-gateway")?;
            runtime()?.block_on(run_http_gateway_role(config))
        }
        "dns-server" => {
            let config = WireRoleConfig::parse(rest, "dns-server")?;
            runtime()?.block_on(run_dns_server_role(config))
        }
        "mesh-data-plane" => {
            let config = MeshDataPlaneRoleConfig::parse(rest)?;
            runtime()?.block_on(run_mesh_data_plane_role(config))
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

pub(crate) async fn wait_for_serving_role(socket: &Path) -> Result<(), String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        match request_role(socket, &RoleRequest::Status).await {
            Ok(RoleSuccess::Status(_)) => return Ok(()),
            Ok(other) => return Err(format!("unexpected serving readiness response: {other:?}")),
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                sleep(ROLE_REQUEST_PAUSE).await;
            }
            Err(error) => return Err(format!("serving role did not become ready: {error:?}")),
        }
    }
}

pub(crate) async fn wait_for_coordinator(socket: &Path) -> Result<(), String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        match request_coordinator(socket, &CoordinatorRequest::Status).await {
            Ok(CoordinatorSuccess::Status(_)) => return Ok(()),
            Ok(other) => {
                return Err(format!(
                    "unexpected coordinator readiness response: {other:?}"
                ));
            }
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                sleep(ROLE_REQUEST_PAUSE).await;
            }
            Err(error) => return Err(format!("coordinator did not become ready: {error:?}")),
        }
    }
}

pub(crate) async fn wait_for_mesh_data_plane(socket: &Path) -> Result<(), String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        match request_mesh_role(socket, &MeshRoleRequest::Status).await {
            Ok(MeshRoleSuccess::Status(_)) => return Ok(()),
            Ok(other) => {
                return Err(format!(
                    "unexpected mesh data-plane readiness response: {other:?}"
                ));
            }
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                sleep(ROLE_REQUEST_PAUSE).await;
            }
            Err(error) => return Err(format!("mesh data-plane did not become ready: {error:?}")),
        }
    }
}

pub(crate) async fn commit_serving(
    socket: &Path,
    commit_id: &str,
    backend: &str,
    dns: &str,
    epoch: u64,
) -> Result<ServingCommitAck, String> {
    let response = request_coordinator(
        socket,
        &CoordinatorRequest::CommitServing(ServingCommitInput::new(commit_id, backend, dns, epoch)),
    )
    .await
    .map_err(|error| format!("commit serving through local coordinator: {error:?}"))?;
    match response {
        CoordinatorSuccess::Committed(ack) => Ok(ack),
        other => Err(format!("unexpected commit response: {other:?}")),
    }
}

pub(crate) async fn request_project_once(socket: &Path) -> Result<(), String> {
    match request_role(socket, &RoleRequest::ProjectOnce)
        .await
        .map_err(|error| format!("project role once: {error:?}"))?
    {
        RoleSuccess::Projected(_) => Ok(()),
        other => Err(format!("unexpected project response: {other:?}")),
    }
}

pub(crate) async fn request_reload(socket: &Path) -> Result<RoleServingStatus, String> {
    match request_role(socket, &RoleRequest::Reload)
        .await
        .map_err(|error| format!("reload serving role: {error:?}"))?
    {
        RoleSuccess::Reloaded(status) => Ok(status),
        other => Err(format!("unexpected reload response: {other:?}")),
    }
}

pub(crate) async fn request_begin_rebuild(socket: &Path) -> Result<u64, String> {
    match request_role(socket, &RoleRequest::BeginRebuild)
        .await
        .map_err(|error| format!("begin projection rebuild: {error:?}"))?
    {
        RoleSuccess::RebuildStarted { token } => Ok(token),
        other => Err(format!("unexpected begin rebuild response: {other:?}")),
    }
}

pub(crate) async fn request_await_rebuild(
    socket: &Path,
    token: u64,
) -> Result<ProjectedSummary, String> {
    match request_role(socket, &RoleRequest::AwaitRebuild { token })
        .await
        .map_err(|error| format!("await projection rebuild: {error:?}"))?
    {
        RoleSuccess::RebuildFinished {
            token: done,
            summary,
        } if done == token => Ok(summary),
        other => Err(format!("unexpected await rebuild response: {other:?}")),
    }
}

pub(crate) async fn request_status(socket: &Path) -> Result<RoleStatus, String> {
    match request_role(socket, &RoleRequest::Status)
        .await
        .map_err(|error| format!("request serving role status: {error:?}"))?
    {
        RoleSuccess::Status(status) => Ok(status),
        other => Err(format!("unexpected status response: {other:?}")),
    }
}

pub(crate) async fn assert_local_mutation_unavailable(
    socket: &Path,
    input: ServingCommitInput,
) -> Result<String, String> {
    let result = request_coordinator(socket, &CoordinatorRequest::CommitServing(input)).await;
    match result {
        Err(CoordinatorClientError::Transport(message)) => Ok(message),
        Err(CoordinatorClientError::Failure(failure)) => Ok(format!("{failure:?}")),
        Ok(success) => Err(format!(
            "local mutation unexpectedly succeeded after coordinator death: {success:?}"
        )),
    }
}

pub(crate) async fn run_remote_injector(
    root: &Path,
    author: &str,
    commit_id: &str,
    backend: &str,
    dns: &str,
    epoch: u64,
) -> Result<ServingCommitAck, String> {
    let mut command = TokioCommand::new(current_exe()?);
    command
        .arg("role")
        .arg("remote-replication-injector")
        .arg("--root")
        .arg(root)
        .arg("--author")
        .arg(author)
        .arg("--commit-id")
        .arg(commit_id)
        .arg("--backend")
        .arg(backend)
        .arg("--dns")
        .arg(dns)
        .arg("--epoch")
        .arg(epoch.to_string())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
    let child = command
        .spawn()
        .map_err(|error| format!("spawn remote injector: {error}"))?;
    let pid_file = register_child_pid(root, "remote-replication-injector", &child)?;
    let output_result = timeout(CHILD_EXIT_TIMEOUT, child.wait_with_output()).await;
    unregister_child_pid(&pid_file)?;
    let output = output_result
        .map_err(|_| "remote injector timed out".to_string())?
        .map_err(|error| format!("run remote injector: {error}"))?;
    if !output.status.success() {
        return Err(format!(
            "remote injector exited with {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr)
        ));
    }
    let response =
        serde_json::from_slice::<ReplicationInjectorResponse>(&output.stdout).map_err(|error| {
            format!(
                "parse remote injector response: {error}; stdout={}",
                String::from_utf8_lossy(&output.stdout)
            )
        })?;
    match response {
        ReplicationInjectorResponse::Success(ack) => Ok(ack),
        ReplicationInjectorResponse::Failure(ReplicationInjectorFailure { kind, message }) => {
            Err(format!("remote injector failed with {kind:?}: {message}"))
        }
    }
}

pub(crate) fn spawn_process_role(
    name: &'static str,
    role: &'static str,
    root: &Path,
    socket: &Path,
    extra_args: &[&str],
) -> Result<RunningChild, String> {
    let mut command = TokioCommand::new(current_exe()?);
    command
        .arg("role")
        .arg(role)
        .arg("--root")
        .arg(root)
        .arg("--socket")
        .arg(socket)
        .kill_on_drop(true);
    for arg in extra_args {
        command.arg(arg);
    }
    let child = command
        .spawn()
        .map_err(|error| format!("spawn {name} role: {error}"))?;
    let pid_file = register_child_pid(root, name, &child)?;
    Ok(RunningChild {
        name,
        child,
        pid_file: Some(pid_file),
    })
}

pub(crate) fn spawn_process_role_owned(
    name: &'static str,
    role: &'static str,
    root: &Path,
    socket: &Path,
    extra_args: &[String],
) -> Result<RunningChild, String> {
    let mut command = TokioCommand::new(current_exe()?);
    command
        .arg("role")
        .arg(role)
        .arg("--root")
        .arg(root)
        .arg("--socket")
        .arg(socket)
        .kill_on_drop(true);
    for arg in extra_args {
        command.arg(arg);
    }
    let child = command
        .spawn()
        .map_err(|error| format!("spawn {name} role: {error}"))?;
    let pid_file = register_child_pid(root, name, &child)?;
    Ok(RunningChild {
        name,
        child,
        pid_file: Some(pid_file),
    })
}

pub(crate) fn cleanup_orphaned_children(root: &Path) -> Result<(), String> {
    let dir = child_pid_dir(root);
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("read child pid dir '{}': {error}", dir.display())),
    };

    let mut records = Vec::new();
    let mut errors = Vec::new();
    for entry in entries {
        let path = match entry {
            Ok(entry) => entry.path(),
            Err(error) => {
                errors.push(format!("read child pid entry: {error}"));
                continue;
            }
        };
        if path.extension().and_then(|extension| extension.to_str()) != Some("pid") {
            continue;
        }
        match read_child_pid_record(&path) {
            Ok(Some(record)) => match process_matches_record(&record) {
                Ok(true) => records.push((path, record)),
                Ok(false) => record_cleanup_error(&mut errors, unregister_child_pid(&path)),
                Err(error) => errors.push(error),
            },
            Ok(None) => record_cleanup_error(&mut errors, unregister_child_pid(&path)),
            Err(error) => errors.push(error),
        }
    }

    for (_, record) in &records {
        if let Err(error) = send_signal(record.pid, "-TERM") {
            match process_matches_record(record) {
                Ok(true) => errors.push(error),
                Ok(false) => {}
                Err(error) => errors.push(error),
            }
        }
    }
    thread::sleep(Duration::from_millis(100));
    for (path, record) in records {
        match process_matches_record(&record) {
            Ok(true) => {
                if let Err(error) = send_signal(record.pid, "-KILL") {
                    match process_matches_record(&record) {
                        Ok(true) => errors.push(error),
                        Ok(false) => {}
                        Err(error) => errors.push(error),
                    }
                }
            }
            Ok(false) => {}
            Err(error) => errors.push(error),
        }
        record_cleanup_error(&mut errors, unregister_child_pid(&path));
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(format!("cleanup orphaned children: {}", errors.join("; ")))
    }
}

fn record_cleanup_error(errors: &mut Vec<String>, result: Result<(), String>) {
    if let Err(error) = result {
        errors.push(error);
    }
}

fn current_exe() -> Result<PathBuf, String> {
    env::current_exe().map_err(|error| format!("resolve current mvp-e2e binary: {error}"))
}

fn child_pid_dir(root: &Path) -> PathBuf {
    root.join("child-pids")
}

fn register_child_pid(root: &Path, name: &str, child: &Child) -> Result<PathBuf, String> {
    let pid = child
        .id()
        .ok_or_else(|| format!("{name} child did not expose a process id"))?;
    let dir = child_pid_dir(root);
    fs::create_dir_all(&dir)
        .map_err(|error| format!("create child pid dir '{}': {error}", dir.display()))?;
    let path = dir.join(format!("{}-{}.pid", pid, name));
    let record = ChildPidRecord {
        pid,
        name: name.to_string(),
        exe: child_exe_for_record(pid)?.display().to_string(),
    };
    let bytes = serde_json::to_vec(&record)
        .map_err(|error| format!("serialize child pid record: {error}"))?;
    fs::write(&path, bytes)
        .map_err(|error| format!("write child pid '{}': {error}", path.display()))?;
    Ok(path)
}

fn unregister_child_pid(path: &Path) -> Result<(), String> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(format!("remove child pid '{}': {error}", path.display())),
    }
}

#[cfg(target_os = "linux")]
fn child_exe_for_record(pid: u32) -> Result<PathBuf, String> {
    fs::read_link(format!("/proc/{pid}/exe"))
        .map_err(|error| format!("read /proc/{pid}/exe for child pid record: {error}"))
}

#[cfg(not(target_os = "linux"))]
fn child_exe_for_record(_pid: u32) -> Result<PathBuf, String> {
    current_exe()
}

fn read_child_pid_record(path: &Path) -> Result<Option<ChildPidRecord>, String> {
    let contents =
        fs::read(path).map_err(|error| format!("read child pid '{}': {error}", path.display()))?;
    let record = serde_json::from_slice::<ChildPidRecord>(&contents);
    match record {
        Ok(record) => Ok(Some(record)),
        Err(_) => Ok(None),
    }
}

#[cfg(target_os = "linux")]
fn process_matches_record(record: &ChildPidRecord) -> Result<bool, String> {
    let exe = match fs::read_link(format!("/proc/{}/exe", record.pid)) {
        Ok(exe) => exe,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(error) => return Err(format!("read /proc/{}/exe: {error}", record.pid)),
    };
    Ok(exe.as_path() == Path::new(&record.exe))
}

#[cfg(not(target_os = "linux"))]
fn process_matches_record(_record: &ChildPidRecord) -> Result<bool, String> {
    Ok(false)
}

fn send_signal(pid: u32, signal: &str) -> Result<(), String> {
    let status = StdCommand::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .status()
        .map_err(|error| format!("run kill {signal} {pid}: {error}"))?;
    if status.success() {
        return Ok(());
    }
    Err(format!("kill {signal} {pid} exited with {status}"))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct ChildPidRecord {
    pid: u32,
    name: String,
    exe: String,
}

pub(crate) struct RunningChild {
    name: &'static str,
    child: Child,
    pid_file: Option<PathBuf>,
}

impl RunningChild {
    pub(crate) fn is_running(&mut self) -> Result<bool, String> {
        self.child
            .try_wait()
            .map(|status| status.is_none())
            .map_err(|error| format!("poll {} child: {error}", self.name))
    }

    pub(crate) async fn kill_and_wait(&mut self) -> Result<bool, String> {
        self.child
            .start_kill()
            .map_err(|error| format!("kill {} child: {error}", self.name))?;
        self.wait_for_exit().await.map(|status| !status.success())
    }

    pub(crate) async fn wait_for_exit(&mut self) -> Result<ExitStatus, String> {
        let status = timeout(CHILD_EXIT_TIMEOUT, self.child.wait())
            .await
            .map_err(|_| format!("{} child did not exit before deadline", self.name))?
            .map_err(|error| format!("wait for {} child: {error}", self.name))?;
        if let Some(pid_file) = self.pid_file.take() {
            unregister_child_pid(&pid_file)?;
        }
        Ok(status)
    }
}

impl Drop for RunningChild {
    fn drop(&mut self) {
        match self.child.try_wait() {
            Ok(Some(_)) => {
                if let Some(pid_file) = self.pid_file.take() {
                    let _ = unregister_child_pid(&pid_file);
                }
            }
            Ok(None) | Err(_) => {
                let _ = self.child.start_kill();
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ServingProjectionRoleConfig {
    root: PathBuf,
    socket: PathBuf,
}

impl ServingProjectionRoleConfig {
    #[cfg(test)]
    pub(crate) fn new(root: impl Into<PathBuf>, socket: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            socket: socket.into(),
        }
    }

    fn parse(args: &[String]) -> Result<Self, String> {
        let (root, socket) = parse_root_socket_flags(args, "serving-projection")?;
        Ok(Self { root, socket })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LocalCoordinatorRoleConfig {
    root: PathBuf,
    socket: PathBuf,
}

impl LocalCoordinatorRoleConfig {
    #[cfg(test)]
    pub(crate) fn new(root: impl Into<PathBuf>, socket: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            socket: socket.into(),
        }
    }

    fn parse(args: &[String]) -> Result<Self, String> {
        let (root, socket) = parse_root_socket_flags(args, "local-coordinator")?;
        Ok(Self { root, socket })
    }
}

fn parse_root_socket_flags(
    args: &[String],
    role: &'static str,
) -> Result<(PathBuf, PathBuf), String> {
    let mut root = None;
    let mut socket = None;
    let mut remaining = args;
    while let [flag, value, tail @ ..] = remaining {
        match flag.as_str() {
            "--root" => root = Some(PathBuf::from(value)),
            "--socket" => socket = Some(PathBuf::from(value)),
            other => return Err(format!("unknown {role} flag '{other}'")),
        }
        remaining = tail;
    }
    if !remaining.is_empty() {
        return Err(format!(
            "{role} arguments must be flag/value pairs, got {remaining:?}"
        ));
    }
    Ok((
        root.ok_or_else(|| format!("{role} requires --root"))?,
        socket.ok_or_else(|| format!("{role} requires --socket"))?,
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RemoteReplicationInjectorConfig {
    root: PathBuf,
    author: String,
    input: ServingCommitInput,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WireRoleConfig {
    root: PathBuf,
    socket: PathBuf,
    listen: SocketAddr,
}

impl WireRoleConfig {
    fn parse(args: &[String], role: &'static str) -> Result<Self, String> {
        let mut root = None;
        let mut socket = None;
        let mut listen = None;
        let mut remaining = args;
        while let [flag, value, tail @ ..] = remaining {
            match flag.as_str() {
                "--root" => root = Some(PathBuf::from(value)),
                "--socket" => socket = Some(PathBuf::from(value)),
                "--listen" => {
                    listen =
                        Some(value.parse::<SocketAddr>().map_err(|error| {
                            format!("{role} --listen must be SocketAddr: {error}")
                        })?)
                }
                other => return Err(format!("unknown {role} flag '{other}'")),
            }
            remaining = tail;
        }
        if !remaining.is_empty() {
            return Err(format!(
                "{role} arguments must be flag/value pairs, got {remaining:?}"
            ));
        }
        Ok(Self {
            root: root.ok_or_else(|| format!("{role} requires --root"))?,
            socket: socket.ok_or_else(|| format!("{role} requires --socket"))?,
            listen: listen.ok_or_else(|| format!("{role} requires --listen"))?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MeshDataPlaneRoleConfig {
    socket: PathBuf,
    listen: SocketAddr,
    snapshot: PathBuf,
    island: IslandId,
}

impl MeshDataPlaneRoleConfig {
    fn parse(args: &[String]) -> Result<Self, String> {
        let role = "mesh-data-plane";
        let mut socket = None;
        let mut listen = None;
        let mut snapshot = None;
        let mut island = None;
        let mut remaining = args;
        while let [flag, value, tail @ ..] = remaining {
            match flag.as_str() {
                "--root" => {
                    let _ = value;
                }
                "--socket" => socket = Some(PathBuf::from(value)),
                "--listen" => {
                    listen =
                        Some(value.parse::<SocketAddr>().map_err(|error| {
                            format!("{role} --listen must be SocketAddr: {error}")
                        })?)
                }
                "--snapshot" => snapshot = Some(PathBuf::from(value)),
                "--island" => island = Some(IslandId::new(value.clone())),
                other => return Err(format!("unknown {role} flag '{other}'")),
            }
            remaining = tail;
        }
        if !remaining.is_empty() {
            return Err(format!(
                "{role} arguments must be flag/value pairs, got {remaining:?}"
            ));
        }
        Ok(Self {
            socket: socket.ok_or_else(|| format!("{role} requires --socket"))?,
            listen: listen.ok_or_else(|| format!("{role} requires --listen"))?,
            snapshot: snapshot.ok_or_else(|| format!("{role} requires --snapshot"))?,
            island: island.ok_or_else(|| format!("{role} requires --island"))?,
        })
    }
}

impl RemoteReplicationInjectorConfig {
    #[cfg(test)]
    pub(crate) fn new(
        root: impl Into<PathBuf>,
        author: impl Into<String>,
        commit_id: impl Into<String>,
        backend_address: impl Into<String>,
        dns_value: impl Into<String>,
        epoch: u64,
    ) -> Self {
        Self {
            root: root.into(),
            author: author.into(),
            input: ServingCommitInput::new(commit_id, backend_address, dns_value, epoch),
        }
    }

    fn parse(args: &[String]) -> Result<Self, String> {
        parse_remote_replication_injector_flags(args)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ServingCommitInput {
    commit_id: String,
    backend_address: String,
    dns_value: String,
    epoch: u64,
}

impl ServingCommitInput {
    pub(crate) fn new(
        commit_id: impl Into<String>,
        backend_address: impl Into<String>,
        dns_value: impl Into<String>,
        epoch: u64,
    ) -> Self {
        Self {
            commit_id: commit_id.into(),
            backend_address: backend_address.into(),
            dns_value: dns_value.into(),
            epoch,
        }
    }
}

fn parse_remote_replication_injector_flags(
    args: &[String],
) -> Result<RemoteReplicationInjectorConfig, String> {
    let role = "remote-replication-injector";
    let mut root = None;
    let mut author_value = None;
    let mut commit_id = None;
    let mut backend_address = None;
    let mut dns_value = None;
    let mut epoch = None;
    let mut remaining = args;
    while let [flag, value, tail @ ..] = remaining {
        match flag.as_str() {
            "--root" => root = Some(PathBuf::from(value)),
            "--author" => author_value = Some(value.clone()),
            "--commit-id" => commit_id = Some(value.clone()),
            "--backend" => backend_address = Some(value.clone()),
            "--dns" => dns_value = Some(value.clone()),
            "--epoch" => {
                epoch = Some(
                    value
                        .parse::<u64>()
                        .map_err(|error| format!("{role} --epoch must be u64: {error}"))?,
                )
            }
            other => return Err(format!("unknown {role} flag '{other}'")),
        }
        remaining = tail;
    }
    if !remaining.is_empty() {
        return Err(format!(
            "{role} arguments must be flag/value pairs, got {remaining:?}"
        ));
    }
    if author_value
        .as_ref()
        .is_none_or(|value| value.trim().is_empty())
    {
        return Err(format!("{role} requires --author"));
    }
    Ok(RemoteReplicationInjectorConfig {
        root: root.ok_or_else(|| format!("{role} requires --root"))?,
        author: author_value.ok_or_else(|| format!("{role} requires --author"))?,
        input: ServingCommitInput::new(
            commit_id.ok_or_else(|| format!("{role} requires --commit-id"))?,
            backend_address.ok_or_else(|| format!("{role} requires --backend"))?,
            dns_value.ok_or_else(|| format!("{role} requires --dns"))?,
            epoch.ok_or_else(|| format!("{role} requires --epoch"))?,
        ),
    })
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

pub(crate) async fn run_local_coordinator_role(
    config: LocalCoordinatorRoleConfig,
) -> Result<(), String> {
    let writer = ServingFactWriter::new(config.root.as_path())
        .map_err(|error| format!("prepare local-coordinator fact writer: {error}"))?;
    remove_stale_socket(&config.socket)?;
    if let Some(parent) = config.socket.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("create socket dir '{}': {error}", parent.display()))?;
    }
    let listener = UnixListener::bind(&config.socket)
        .map_err(|error| format!("bind local-coordinator socket: {error}"))?;
    loop {
        let (stream, _addr) = listener
            .accept()
            .await
            .map_err(|error| format!("accept local-coordinator connection: {error}"))?;
        if handle_local_coordinator_connection(stream, writer.clone()).await? {
            break;
        }
    }
    Ok(())
}

pub(crate) async fn run_http_gateway_role(config: WireRoleConfig) -> Result<(), String> {
    remove_stale_socket(&config.socket)?;
    if let Some(parent) = config.socket.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "create http-gateway socket dir '{}': {error}",
                parent.display()
            )
        })?;
    }
    let listener = UnixListener::bind(&config.socket)
        .map_err(|error| format!("bind http-gateway control socket: {error}"))?;
    let state = spawn_wire_serving_state(&config)?;
    let gateway = spawn_http_gateway(config.listen, state.clone())
        .await
        .map_err(|error| format!("start http gateway: {error}"))?;
    let state = Arc::new(Mutex::new(HttpGatewayRoleState {
        state,
        gateway: Some(gateway),
    }));
    loop {
        let (stream, _addr) = listener
            .accept()
            .await
            .map_err(|error| format!("accept http-gateway control connection: {error}"))?;
        if handle_http_gateway_connection(stream, Arc::clone(&state)).await? {
            break;
        }
    }
    Ok(())
}

pub(crate) async fn run_dns_server_role(config: WireRoleConfig) -> Result<(), String> {
    remove_stale_socket(&config.socket)?;
    if let Some(parent) = config.socket.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "create dns-server socket dir '{}': {error}",
                parent.display()
            )
        })?;
    }
    let listener = UnixListener::bind(&config.socket)
        .map_err(|error| format!("bind dns-server control socket: {error}"))?;
    let state = spawn_wire_serving_state(&config)?;
    let dns = spawn_dns_server(config.listen, state.clone())
        .await
        .map_err(|error| format!("start dns server: {error}"))?;
    let state = Arc::new(Mutex::new(DnsServerRoleState {
        state,
        dns: Some(dns),
    }));
    loop {
        let (stream, _addr) = listener
            .accept()
            .await
            .map_err(|error| format!("accept dns-server control connection: {error}"))?;
        if handle_dns_server_connection(stream, Arc::clone(&state)).await? {
            break;
        }
    }
    Ok(())
}

pub(crate) async fn run_mesh_data_plane_role(
    config: MeshDataPlaneRoleConfig,
) -> Result<(), String> {
    remove_stale_socket(&config.socket)?;
    if let Some(parent) = config.socket.parent() {
        std::fs::create_dir_all(parent).map_err(|error| {
            format!(
                "create mesh-data-plane socket dir '{}': {error}",
                parent.display()
            )
        })?;
    }
    let control = UnixListener::bind(&config.socket)
        .map_err(|error| format!("bind mesh-data-plane control socket: {error}"))?;
    let paths = WireGuardSnapshotPaths {
        applied: config.snapshot.clone(),
    };
    let snapshot = load_applied_snapshot(&paths, &config.island)
        .map_err(|error| format!("load mesh data-plane snapshot: {error}"))?;
    let listener = TcpListener::bind(config.listen)
        .await
        .map_err(|error| format!("bind mesh data-plane tcp listener: {error}"))?;
    let listen = listener
        .local_addr()
        .map_err(|error| format!("read mesh data-plane listener addr: {error}"))?;
    let echo_task = tokio::spawn(run_mesh_echo_listener(listener));
    let state = Arc::new(Mutex::new(MeshDataPlaneState {
        paths,
        island: config.island,
        snapshot,
        listen,
        last_reload_failure: None,
    }));

    loop {
        let (stream, _addr) = control
            .accept()
            .await
            .map_err(|error| format!("accept mesh-data-plane control connection: {error}"))?;
        if handle_mesh_data_plane_connection(stream, Arc::clone(&state)).await? {
            break;
        }
    }
    echo_task.abort();
    Ok(())
}

async fn run_mesh_echo_listener(listener: TcpListener) -> Result<(), String> {
    loop {
        let (mut stream, _addr) = listener
            .accept()
            .await
            .map_err(|error| format!("accept mesh data-plane tcp connection: {error}"))?;
        tokio::spawn(async move {
            let mut body = Vec::new();
            if stream.read_to_end(&mut body).await.is_ok() {
                let _ = stream.write_all(&body).await;
                let _ = stream.shutdown().await;
            }
        });
    }
}

async fn handle_local_coordinator_connection(
    mut stream: UnixStream,
    writer: ServingFactWriter,
) -> Result<bool, String> {
    let response = match read_request(&mut stream, "coordinator request").await {
        Ok(request) => match serde_json::from_slice::<CoordinatorRequest>(&request) {
            Ok(request) => match handle_coordinator_request(request, writer).await {
                Ok(success) => CoordinatorResponse::Success(success),
                Err(error) => CoordinatorResponse::Failure(error),
            },
            Err(error) => CoordinatorResponse::Failure(CoordinatorFailure::invalid_request(
                format!("parse coordinator request: {error}"),
            )),
        },
        Err(message) => CoordinatorResponse::Failure(CoordinatorFailure::invalid_request(message)),
    };
    let should_shutdown = matches!(
        response,
        CoordinatorResponse::Success(CoordinatorSuccess::Shutdown)
    );
    let response = serde_json::to_vec(&response)
        .map_err(|error| format!("serialize coordinator response: {error}"))?;
    if write_response(&mut stream, &response, "coordinator response")
        .await
        .is_err()
    {
        return Ok(should_shutdown);
    }
    Ok(should_shutdown)
}

async fn handle_coordinator_request(
    request: CoordinatorRequest,
    writer: ServingFactWriter,
) -> Result<CoordinatorSuccess, CoordinatorFailure> {
    match request {
        CoordinatorRequest::CommitServing(input) => {
            let ack = tokio::task::spawn_blocking(move || {
                writer.write(ServingCommitWriteOrigin::LocalCoordinator, &input)
            })
            .await
            .map_err(|error| {
                CoordinatorFailure::internal(format!("join serving commit writer: {error}"))
            })?
            .map_err(CoordinatorFailure::from)?;
            Ok(CoordinatorSuccess::Committed(ack))
        }
        CoordinatorRequest::Status => Ok(CoordinatorSuccess::Status(CoordinatorStatus {
            mutation: LocalMutationStatus::Available,
        })),
        CoordinatorRequest::Shutdown => Ok(CoordinatorSuccess::Shutdown),
    }
}

async fn handle_mesh_role_request(
    request: MeshRoleRequest,
    state: Arc<Mutex<MeshDataPlaneState>>,
) -> Result<MeshRoleSuccess, MeshRoleFailure> {
    match request {
        MeshRoleRequest::Send {
            target_node_id,
            target_addr,
            payload,
        } => {
            let allowed = {
                let state = state.lock().await;
                state.snapshot.has_peer(&target_node_id)
            };
            if !allowed {
                return Err(MeshRoleFailure::peer_not_applied(&target_node_id));
            }
            let payload = send_mesh_tcp(target_addr, payload).await?;
            Ok(MeshRoleSuccess::Sent { payload })
        }
        MeshRoleRequest::Reload => {
            let mut state = state.lock().await;
            match load_applied_snapshot(&state.paths, &state.island) {
                Ok(snapshot) => {
                    state.snapshot = snapshot;
                    state.last_reload_failure = None;
                    Ok(MeshRoleSuccess::Reloaded(state.status()))
                }
                Err(error) => {
                    state.last_reload_failure = Some(error.to_string());
                    Err(MeshRoleFailure::snapshot(error.to_string()))
                }
            }
        }
        MeshRoleRequest::Status => Ok(MeshRoleSuccess::Status(state.lock().await.status())),
        MeshRoleRequest::Shutdown => Ok(MeshRoleSuccess::Shutdown),
    }
}

async fn send_mesh_tcp(
    target_addr: SocketAddr,
    payload: Vec<u8>,
) -> Result<Vec<u8>, MeshRoleFailure> {
    let mut stream = timeout(ROLE_CONNECT_TIMEOUT, TcpStream::connect(target_addr))
        .await
        .map_err(|_| MeshRoleFailure::io("connect target timed out"))?
        .map_err(|error| MeshRoleFailure::io(format!("connect target {target_addr}: {error}")))?;
    timeout(ROLE_WRITE_TIMEOUT, stream.write_all(&payload))
        .await
        .map_err(|_| MeshRoleFailure::io("write target timed out"))?
        .map_err(|error| MeshRoleFailure::io(format!("write target: {error}")))?;
    timeout(ROLE_WRITE_TIMEOUT, stream.shutdown())
        .await
        .map_err(|_| MeshRoleFailure::io("finish target write timed out"))?
        .map_err(|error| MeshRoleFailure::io(format!("finish target write: {error}")))?;
    let mut response = Vec::new();
    timeout(ROLE_RESPONSE_TIMEOUT, stream.read_to_end(&mut response))
        .await
        .map_err(|_| MeshRoleFailure::io("read target response timed out"))?
        .map_err(|error| MeshRoleFailure::io(format!("read target response: {error}")))?;
    Ok(response)
}

impl MeshDataPlaneState {
    fn status(&self) -> MeshRoleStatus {
        MeshRoleStatus {
            listen: self.listen,
            revision: self.snapshot.revision,
            peer_count: self.snapshot.peers.len(),
            last_reload_failure: self.last_reload_failure.clone(),
        }
    }
}

pub(crate) fn run_remote_replication_injector(
    args: RemoteReplicationInjectorConfig,
) -> ReplicationInjectorResponse {
    let RemoteReplicationInjectorConfig {
        root,
        author,
        input,
    } = args;
    let result = ServingFactWriter::new(root.as_path()).and_then(|writer| {
        writer.write(
            ServingCommitWriteOrigin::RemoteReplication { author },
            &input,
        )
    });
    match result {
        Ok(ack) => ReplicationInjectorResponse::Success(ack),
        Err(error) => ReplicationInjectorResponse::Failure(error.into()),
    }
}

async fn handle_serving_projection_connection(
    mut stream: UnixStream,
    state: Arc<Mutex<ServingProjectionState>>,
) -> Result<bool, String> {
    let response = match read_request(&mut stream, "serving role request").await {
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
    if write_response(&mut stream, &response, "role response")
        .await
        .is_err()
    {
        return Ok(should_shutdown);
    }
    Ok(should_shutdown)
}

async fn handle_http_gateway_connection(
    mut stream: UnixStream,
    state: Arc<Mutex<HttpGatewayRoleState>>,
) -> Result<bool, String> {
    let response = match read_request(&mut stream, "http-gateway request").await {
        Ok(request) => match serde_json::from_slice::<WireRoleRequest>(&request) {
            Ok(request) => match handle_http_gateway_request(request, state).await {
                Ok(success) => WireRoleResponse::Success(success),
                Err(error) => WireRoleResponse::Failure(error),
            },
            Err(error) => WireRoleResponse::Failure(WireRoleFailure::invalid_request(format!(
                "parse http-gateway request: {error}"
            ))),
        },
        Err(message) => WireRoleResponse::Failure(WireRoleFailure::invalid_request(message)),
    };
    let should_shutdown = matches!(
        response,
        WireRoleResponse::Success(WireRoleSuccess::Shutdown)
    );
    let response = serde_json::to_vec(&response)
        .map_err(|error| format!("serialize http-gateway response: {error}"))?;
    if write_response(&mut stream, &response, "http-gateway response")
        .await
        .is_err()
    {
        return Ok(should_shutdown);
    }
    Ok(should_shutdown)
}

async fn handle_dns_server_connection(
    mut stream: UnixStream,
    state: Arc<Mutex<DnsServerRoleState>>,
) -> Result<bool, String> {
    let response = match read_request(&mut stream, "dns-server request").await {
        Ok(request) => match serde_json::from_slice::<WireRoleRequest>(&request) {
            Ok(request) => match handle_dns_server_request(request, state).await {
                Ok(success) => WireRoleResponse::Success(success),
                Err(error) => WireRoleResponse::Failure(error),
            },
            Err(error) => WireRoleResponse::Failure(WireRoleFailure::invalid_request(format!(
                "parse dns-server request: {error}"
            ))),
        },
        Err(message) => WireRoleResponse::Failure(WireRoleFailure::invalid_request(message)),
    };
    let should_shutdown = matches!(
        response,
        WireRoleResponse::Success(WireRoleSuccess::Shutdown)
    );
    let response = serde_json::to_vec(&response)
        .map_err(|error| format!("serialize dns-server response: {error}"))?;
    if write_response(&mut stream, &response, "dns-server response")
        .await
        .is_err()
    {
        return Ok(should_shutdown);
    }
    Ok(should_shutdown)
}

async fn handle_mesh_data_plane_connection(
    mut stream: UnixStream,
    state: Arc<Mutex<MeshDataPlaneState>>,
) -> Result<bool, String> {
    let response = match read_request(&mut stream, "mesh-data-plane request").await {
        Ok(request) => match serde_json::from_slice::<MeshRoleRequest>(&request) {
            Ok(request) => match handle_mesh_role_request(request, state).await {
                Ok(success) => MeshRoleResponse::Success(success),
                Err(error) => MeshRoleResponse::Failure(error),
            },
            Err(error) => MeshRoleResponse::Failure(MeshRoleFailure::invalid_request(format!(
                "parse mesh-data-plane request: {error}"
            ))),
        },
        Err(message) => MeshRoleResponse::Failure(MeshRoleFailure::invalid_request(message)),
    };
    let should_shutdown = matches!(
        response,
        MeshRoleResponse::Success(MeshRoleSuccess::Shutdown)
    );
    let response = serde_json::to_vec(&response)
        .map_err(|error| format!("serialize mesh-data-plane response: {error}"))?;
    if write_response(&mut stream, &response, "mesh-data-plane response")
        .await
        .is_err()
    {
        return Ok(should_shutdown);
    }
    Ok(should_shutdown)
}

async fn read_request(stream: &mut UnixStream, operation: &str) -> Result<Vec<u8>, String> {
    timeout(
        ROLE_REQUEST_READ_TIMEOUT,
        read_bounded(stream, ROLE_MAX_REQUEST_BYTES, operation),
    )
    .await
    .map_err(|_| format!("read {operation} timed out"))?
}

async fn read_bounded(
    stream: &mut UnixStream,
    limit: usize,
    operation: &str,
) -> Result<Vec<u8>, String> {
    let mut request = Vec::new();
    let mut chunk = [0u8; 4096];
    loop {
        let read = stream
            .read(&mut chunk)
            .await
            .map_err(|error| format!("read {operation}: {error}"))?;
        if read == 0 {
            return Ok(request);
        }
        if request.len() + read > limit {
            return Err(format!("{operation} exceeds {limit} byte limit"));
        }
        request.extend_from_slice(&chunk[..read]);
    }
}

async fn write_with_timeout(
    stream: &mut UnixStream,
    bytes: &[u8],
    operation: &str,
) -> Result<(), String> {
    timeout(ROLE_WRITE_TIMEOUT, stream.write_all(bytes))
        .await
        .map_err(|_| format!("write {operation} timed out"))?
        .map_err(|error| format!("write {operation}: {error}"))
}

async fn shutdown_stream(stream: &mut UnixStream, operation: &str) -> Result<(), String> {
    timeout(ROLE_WRITE_TIMEOUT, stream.shutdown())
        .await
        .map_err(|_| format!("shutdown {operation} stream timed out"))?
        .map_err(|error| format!("shutdown {operation} stream: {error}"))
}

async fn write_response(
    stream: &mut UnixStream,
    response: &[u8],
    operation: &str,
) -> Result<(), String> {
    write_with_timeout(stream, response, operation).await?;
    shutdown_stream(stream, operation).await
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

async fn handle_http_gateway_request(
    request: WireRoleRequest,
    state: Arc<Mutex<HttpGatewayRoleState>>,
) -> Result<WireRoleSuccess, WireRoleFailure> {
    match request {
        WireRoleRequest::Status | WireRoleRequest::Readiness => http_gateway_status(state).await,
        WireRoleRequest::Reload => {
            let wire = {
                let state = state.lock().await;
                state.state.clone()
            };
            wire.reload()
                .await
                .map_err(WireRoleFailure::from_serving_error)?;
            http_gateway_status(state).await
        }
        WireRoleRequest::Shutdown => {
            let gateway = {
                let mut state = state.lock().await;
                state.gateway.take()
            };
            if let Some(gateway) = gateway {
                gateway
                    .shutdown()
                    .await
                    .map_err(|error| WireRoleFailure::internal(error.to_string()))?;
            }
            Ok(WireRoleSuccess::Shutdown)
        }
    }
}

async fn handle_dns_server_request(
    request: WireRoleRequest,
    state: Arc<Mutex<DnsServerRoleState>>,
) -> Result<WireRoleSuccess, WireRoleFailure> {
    match request {
        WireRoleRequest::Status | WireRoleRequest::Readiness => dns_server_status(state).await,
        WireRoleRequest::Reload => {
            let wire = {
                let state = state.lock().await;
                state.state.clone()
            };
            wire.reload()
                .await
                .map_err(WireRoleFailure::from_serving_error)?;
            dns_server_status(state).await
        }
        WireRoleRequest::Shutdown => {
            let dns = {
                let mut state = state.lock().await;
                state.dns.take()
            };
            if let Some(dns) = dns {
                dns.shutdown()
                    .await
                    .map_err(|error| WireRoleFailure::internal(error.to_string()))?;
            }
            Ok(WireRoleSuccess::Shutdown)
        }
    }
}

async fn http_gateway_status(
    state: Arc<Mutex<HttpGatewayRoleState>>,
) -> Result<WireRoleSuccess, WireRoleFailure> {
    let (wire, listen_addr, metrics) = {
        let state = state.lock().await;
        let gateway = state
            .gateway
            .as_ref()
            .ok_or_else(|| WireRoleFailure::unavailable("http gateway is shut down"))?;
        (
            state.state.clone(),
            gateway.listen_addr(),
            gateway.metrics(),
        )
    };
    wire_role_status(WireRoleKind::HttpGateway, wire, listen_addr, metrics).await
}

async fn dns_server_status(
    state: Arc<Mutex<DnsServerRoleState>>,
) -> Result<WireRoleSuccess, WireRoleFailure> {
    let (wire, listen_addr, metrics) = {
        let state = state.lock().await;
        let dns = state
            .dns
            .as_ref()
            .ok_or_else(|| WireRoleFailure::unavailable("dns server is shut down"))?;
        (state.state.clone(), dns.listen_addr(), dns.metrics())
    };
    wire_role_status(WireRoleKind::DnsServer, wire, listen_addr, metrics).await
}

async fn wire_role_status(
    kind: WireRoleKind,
    wire: WireServingState,
    listen_addr: SocketAddr,
    metrics: WireRoleMetrics,
) -> Result<WireRoleSuccess, WireRoleFailure> {
    let serving = wire
        .status()
        .await
        .map_err(WireRoleFailure::from_serving_error)?;
    Ok(WireRoleSuccess::Status(WireProcessStatus {
        kind,
        serving: role_serving_status(serving),
        listen_addr,
        metrics,
    }))
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

struct HttpGatewayRoleState {
    state: WireServingState,
    gateway: Option<HttpGatewayHandle>,
}

struct DnsServerRoleState {
    state: WireServingState,
    dns: Option<DnsServerHandle>,
}

fn spawn_wire_serving_state(config: &WireRoleConfig) -> Result<WireServingState, String> {
    let expected_island = IslandId::new("prod");
    let paths = ServingSnapshotPaths::new(
        config.root.join("gateway.snapshot"),
        config.root.join("dns.snapshot"),
    );
    let serving = ServingActorHandle::spawn(expected_island, paths, STALE_AFTER)
        .map_err(|error| format!("load wire serving snapshots: {error}"))?;
    Ok(WireServingState::new(serving))
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum ServingCommitWriteOrigin {
    LocalCoordinator,
    RemoteReplication { author: String },
}

impl ServingCommitWriteOrigin {
    fn author(&self) -> PrincipalId {
        match self {
            Self::LocalCoordinator => PrincipalId::new("local-coordinator"),
            Self::RemoteReplication { author } => PrincipalId::new(author.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ServingCommitWriteError {
    Validation(String),
    Storage(String),
}

impl ServingCommitWriteError {
    fn validation(message: impl Into<String>) -> Self {
        Self::Validation(message.into())
    }

    fn storage(message: impl Into<String>) -> Self {
        Self::Storage(message.into())
    }
}

impl Display for ServingCommitWriteError {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation(message) | Self::Storage(message) => f.write_str(message),
        }
    }
}

#[derive(Clone)]
struct ServingFactWriter {
    source: Arc<ProcessFactSource>,
    island: IslandId,
}

impl ServingFactWriter {
    fn new(root: &Path) -> Result<Self, ServingCommitWriteError> {
        let island = IslandId::new("prod");
        let source = ProcessFactSource::new(
            root.join("facts"),
            ProcessFactReadPolicy::allow(
                island.clone(),
                PrincipalId::new("projection"),
                vec![fact_pattern("/facts/>").map_err(ServingCommitWriteError::storage)?],
            ),
        )
        .map_err(ServingCommitWriteError::storage)?;
        Ok(Self {
            source: Arc::new(source),
            island,
        })
    }

    fn write(
        &self,
        origin: ServingCommitWriteOrigin,
        input: &ServingCommitInput,
    ) -> Result<ServingCommitAck, ServingCommitWriteError> {
        let plan = serving_commit_plan(input)?;
        let payload = serving_commit_payload(&plan)?;
        let key = mvp_bus::FactKey::parse(format!("/facts/serving/{}", plan.serving_commit_id))
            .map_err(|error| {
                ServingCommitWriteError::validation(format!("parse serving commit key: {error}"))
            })?;
        let author = origin.author();
        let written = self
            .source
            .write_payload(&self.island, &author, key, mvp_bus::Payload::from(payload))
            .map_err(ServingCommitWriteError::storage)?;
        Ok(ServingCommitAck {
            key: written.key.as_str().to_string(),
            content_hash: written.content_hash.as_str().to_string(),
            author: author.as_str().to_string(),
            origin,
        })
    }
}

fn serving_commit_payload(plan: &ServingCommitPlan) -> Result<Vec<u8>, ServingCommitWriteError> {
    ProjectionFactPayload::ServingCommit(serving_commit_fact(plan))
        .to_fact_bytes()
        .map_err(|error| {
            ServingCommitWriteError::storage(format!("serialize serving commit fact: {error}"))
        })
}

fn serving_commit_fact(plan: &ServingCommitPlan) -> ServingCommitFact {
    ServingCommitFact {
        serving_commit_id: plan.serving_commit_id.to_string(),
        route_commit_id: plan.route_commit_id.to_string(),
        gateway_commit_id: plan.gateway_commit_id.to_string(),
        dns_commit_id: plan.dns_commit_id.to_string(),
        route_id: plan.route_id.clone(),
        hostnames: plan.hostnames.clone(),
        backends: plan.active_backends.clone(),
        old_backends_to_drain: plan.old_backends_to_drain.clone(),
        dns_records: plan.dns_records.clone(),
        epoch: plan.epoch,
    }
}

fn serving_commit_plan(
    input: &ServingCommitInput,
) -> Result<ServingCommitPlan, ServingCommitWriteError> {
    if input.commit_id.trim().is_empty() {
        return Err(ServingCommitWriteError::validation(
            "serving commit id may not be empty",
        ));
    }
    if input.backend_address.trim().is_empty() {
        return Err(ServingCommitWriteError::validation(
            "serving backend address may not be empty",
        ));
    }
    if input.dns_value.trim().is_empty() {
        return Err(ServingCommitWriteError::validation(
            "serving dns value may not be empty",
        ));
    }
    Ok(ServingCommitPlan {
        serving_commit_id: ServingCommitId::new(input.commit_id.clone()),
        route_commit_id: RouteCommitId::new(format!("{}-route", input.commit_id)),
        gateway_commit_id: GatewayCommitId::new(format!("{}-gateway", input.commit_id)),
        dns_commit_id: DnsCommitId::new(format!("{}-dns", input.commit_id)),
        route_id: RouteId::new("web-http"),
        hostnames: vec!["web.example.test".to_string()],
        active_backends: vec![BackendEndpoint {
            node_id: NodeId::new("node-web"),
            address: input.backend_address.clone(),
        }],
        old_backends_to_drain: Vec::new(),
        dns_records: vec![DnsRecordFact {
            name: "web.example.test".to_string(),
            record_type: "AAAA".to_string(),
            value: input.dns_value.clone(),
            ttl_seconds: 30,
        }],
        epoch: input.epoch,
    })
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
        snapshot_age_us: duration_to_us(status.snapshot_age),
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
pub(crate) enum CoordinatorRequest {
    CommitServing(ServingCommitInput),
    Status,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum CoordinatorResponse {
    Success(CoordinatorSuccess),
    Failure(CoordinatorFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum CoordinatorSuccess {
    Committed(ServingCommitAck),
    Status(CoordinatorStatus),
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ServingCommitAck {
    pub(crate) key: String,
    pub(crate) content_hash: String,
    pub(crate) author: String,
    pub(crate) origin: ServingCommitWriteOrigin,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum ReplicationInjectorResponse {
    Success(ServingCommitAck),
    Failure(ReplicationInjectorFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct ReplicationInjectorFailure {
    pub(crate) kind: ReplicationInjectorFailureKind,
    pub(crate) message: String,
}

impl From<ServingCommitWriteError> for ReplicationInjectorFailure {
    fn from(error: ServingCommitWriteError) -> Self {
        match error {
            ServingCommitWriteError::Validation(message) => Self {
                kind: ReplicationInjectorFailureKind::InvalidRequest,
                message,
            },
            ServingCommitWriteError::Storage(message) => Self {
                kind: ReplicationInjectorFailureKind::Storage,
                message,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum ReplicationInjectorFailureKind {
    InvalidRequest,
    Storage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CoordinatorStatus {
    pub(crate) mutation: LocalMutationStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum LocalMutationStatus {
    Available,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub(crate) enum WireRoleRequest {
    Readiness,
    Reload,
    Status,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum WireRoleResponse {
    Success(WireRoleSuccess),
    Failure(WireRoleFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "success", rename_all = "snake_case")]
pub(crate) enum WireRoleSuccess {
    Status(WireProcessStatus),
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WireProcessStatus {
    pub(crate) kind: WireRoleKind,
    pub(crate) serving: RoleServingStatus,
    pub(crate) listen_addr: SocketAddr,
    pub(crate) metrics: WireRoleMetrics,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WireRoleKind {
    HttpGateway,
    DnsServer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct WireRoleFailure {
    pub(crate) kind: WireRoleFailureKind,
    pub(crate) message: String,
}

impl WireRoleFailure {
    fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            kind: WireRoleFailureKind::InvalidRequest,
            message: message.into(),
        }
    }

    fn unavailable(message: impl Into<String>) -> Self {
        Self {
            kind: WireRoleFailureKind::Unavailable,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: WireRoleFailureKind::Internal,
            message: message.into(),
        }
    }

    fn from_serving_error(error: ServingError) -> Self {
        match error {
            ServingError::SnapshotLoad { failure } => Self {
                kind: WireRoleFailureKind::Unavailable,
                message: failure.to_string(),
            },
            ServingError::ActorUnavailable { operation, reason } => Self {
                kind: WireRoleFailureKind::Internal,
                message: format!("{operation}: {reason}"),
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum WireRoleFailureKind {
    InvalidRequest,
    Unavailable,
    Internal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct CoordinatorFailure {
    pub(crate) kind: CoordinatorFailureKind,
    pub(crate) message: String,
}

impl CoordinatorFailure {
    fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            kind: CoordinatorFailureKind::InvalidRequest,
            message: message.into(),
        }
    }

    fn storage(message: impl Into<String>) -> Self {
        Self {
            kind: CoordinatorFailureKind::Storage,
            message: message.into(),
        }
    }

    fn internal(message: impl Into<String>) -> Self {
        Self {
            kind: CoordinatorFailureKind::Internal,
            message: message.into(),
        }
    }
}

impl From<ServingCommitWriteError> for CoordinatorFailure {
    fn from(error: ServingCommitWriteError) -> Self {
        match error {
            ServingCommitWriteError::Validation(message) => Self::invalid_request(message),
            ServingCommitWriteError::Storage(message) => Self::storage(message),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum CoordinatorFailureKind {
    InvalidRequest,
    Storage,
    Internal,
}

#[derive(Debug, Clone)]
struct MeshDataPlaneState {
    paths: WireGuardSnapshotPaths,
    island: IslandId,
    snapshot: WireGuardAppliedSnapshot,
    listen: SocketAddr,
    last_reload_failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "command", rename_all = "snake_case")]
pub(crate) enum MeshRoleRequest {
    Send {
        target_node_id: NodeId,
        target_addr: SocketAddr,
        payload: Vec<u8>,
    },
    Reload,
    Status,
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "snake_case")]
pub(crate) enum MeshRoleResponse {
    Success(MeshRoleSuccess),
    Failure(MeshRoleFailure),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub(crate) enum MeshRoleSuccess {
    Sent { payload: Vec<u8> },
    Reloaded(MeshRoleStatus),
    Status(MeshRoleStatus),
    Shutdown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MeshRoleStatus {
    pub(crate) listen: SocketAddr,
    pub(crate) revision: u64,
    pub(crate) peer_count: usize,
    pub(crate) last_reload_failure: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct MeshRoleFailure {
    pub(crate) kind: MeshRoleFailureKind,
    pub(crate) message: String,
}

impl MeshRoleFailure {
    fn invalid_request(message: impl Into<String>) -> Self {
        Self {
            kind: MeshRoleFailureKind::InvalidRequest,
            message: message.into(),
        }
    }

    fn peer_not_applied(node_id: &NodeId) -> Self {
        Self {
            kind: MeshRoleFailureKind::PeerNotApplied,
            message: format!(
                "peer {} is not in last-applied mesh snapshot",
                node_id.as_str()
            ),
        }
    }

    fn io(message: impl Into<String>) -> Self {
        Self {
            kind: MeshRoleFailureKind::Io,
            message: message.into(),
        }
    }

    fn snapshot(message: impl Into<String>) -> Self {
        Self {
            kind: MeshRoleFailureKind::Snapshot,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(crate) enum MeshRoleFailureKind {
    InvalidRequest,
    PeerNotApplied,
    Io,
    Snapshot,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum MeshRoleClientError {
    Transport(String),
    Failure(MeshRoleFailure),
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
        snapshot_age_us: u64,
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum CoordinatorClientError {
    Transport(String),
    Failure(CoordinatorFailure),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WireRoleClientError {
    Transport(String),
    Failure(WireRoleFailure),
}

pub(crate) async fn request_role(
    socket: &Path,
    request: &RoleRequest,
) -> Result<RoleSuccess, RoleClientError> {
    match request_json::<_, RoleResponse>(socket, request, "serving role").await {
        Ok(RoleResponse::Success(success)) => Ok(success),
        Ok(RoleResponse::Failure(failure)) => Err(RoleClientError::Failure(failure)),
        Err(error) => Err(RoleClientError::Transport(error)),
    }
}

pub(crate) async fn request_wire_role(
    socket: &Path,
    request: &WireRoleRequest,
) -> Result<WireRoleSuccess, WireRoleClientError> {
    match request_json::<_, WireRoleResponse>(socket, request, "wire role").await {
        Ok(WireRoleResponse::Success(success)) => Ok(success),
        Ok(WireRoleResponse::Failure(failure)) => Err(WireRoleClientError::Failure(failure)),
        Err(error) => Err(WireRoleClientError::Transport(error)),
    }
}

pub(crate) async fn request_mesh_role(
    socket: &Path,
    request: &MeshRoleRequest,
) -> Result<MeshRoleSuccess, MeshRoleClientError> {
    match request_json::<_, MeshRoleResponse>(socket, request, "mesh-data-plane").await {
        Ok(MeshRoleResponse::Success(success)) => Ok(success),
        Ok(MeshRoleResponse::Failure(failure)) => Err(MeshRoleClientError::Failure(failure)),
        Err(error) => Err(MeshRoleClientError::Transport(error)),
    }
}

pub(crate) async fn request_coordinator(
    socket: &Path,
    request: &CoordinatorRequest,
) -> Result<CoordinatorSuccess, CoordinatorClientError> {
    match request_json::<_, CoordinatorResponse>(socket, request, "local coordinator").await {
        Ok(CoordinatorResponse::Success(success)) => Ok(success),
        Ok(CoordinatorResponse::Failure(failure)) => Err(CoordinatorClientError::Failure(failure)),
        Err(error) => Err(CoordinatorClientError::Transport(error)),
    }
}

async fn request_json<TRequest, TResponse>(
    socket: &Path,
    request: &TRequest,
    endpoint: &'static str,
) -> Result<TResponse, String>
where
    TRequest: Serialize,
    TResponse: DeserializeOwned,
{
    let mut stream = timeout(ROLE_CONNECT_TIMEOUT, UnixStream::connect(socket))
        .await
        .map_err(|_| format!("connect {endpoint} socket timed out"))?
        .map_err(|error| format!("connect {endpoint} socket '{}': {error}", socket.display()))?;
    let bytes = serde_json::to_vec(request)
        .map_err(|error| format!("serialize {endpoint} request: {error}"))?;
    let request_operation = format!("{endpoint} request");
    write_with_timeout(&mut stream, &bytes, &request_operation)
        .await
        .map_err(|error| format!("send {endpoint} request: {error}"))?;
    shutdown_stream(&mut stream, &request_operation)
        .await
        .map_err(|error| format!("finish {endpoint} request: {error}"))?;
    let mut response = Vec::new();
    timeout(ROLE_RESPONSE_TIMEOUT, stream.read_to_end(&mut response))
        .await
        .map_err(|_| format!("read {endpoint} response timed out"))?
        .map_err(|error| format!("read {endpoint} response: {error}"))?;
    serde_json::from_slice::<TResponse>(&response)
        .map_err(|error| format!("parse {endpoint} response: {error}"))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::atomic::{AtomicU64, Ordering};

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

    fn spawn_coordinator(root: &Path) -> (PathBuf, JoinHandle<Result<(), String>>) {
        let socket = root.join("coordinator.sock");
        let config = LocalCoordinatorRoleConfig::new(root, &socket);
        let handle = tokio::spawn(run_local_coordinator_role(config));
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

    fn write_serving_fact(root: &Path, id: &str, backend: &str, dns: &str, epoch: u64) {
        let input = ServingCommitInput::new(id, backend, dns, epoch);
        ServingFactWriter::new(root)
            .expect("serving fact writer")
            .write(ServingCommitWriteOrigin::LocalCoordinator, &input)
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

    async fn wait_coordinator_ready(socket: &Path) -> Result<(), String> {
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        loop {
            match request_coordinator(socket, &CoordinatorRequest::Status).await {
                Ok(CoordinatorSuccess::Status(_)) => return Ok(()),
                Ok(other) => return Err(format!("unexpected coordinator response: {other:?}")),
                Err(error) if std::time::Instant::now() < deadline => {
                    let _ = error;
                    sleep(Duration::from_millis(10)).await;
                }
                Err(error) => return Err(format!("coordinator did not become ready: {error:?}")),
            }
        }
    }

    async fn shutdown_coordinator(
        socket: &Path,
        handle: JoinHandle<Result<(), String>>,
    ) -> Result<(), String> {
        request_coordinator(socket, &CoordinatorRequest::Shutdown)
            .await
            .map_err(|error| format!("coordinator shutdown request: {error:?}"))?;
        timeout(Duration::from_secs(2), handle)
            .await
            .map_err(|_| "coordinator did not exit after shutdown".to_string())?
            .map_err(|error| format!("join coordinator after shutdown: {error}"))?
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
        write_serving_fact(&root, "serving-1", "fd00::1:8080", "fd00::1", 1);
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
        write_serving_fact(&root, "serving-1", "fd00::1:8080", "fd00::1", 1);
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
        write_serving_fact(&root, "serving-1", "fd00::1:8080", "fd00::1", 1);
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

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn cleanup_orphaned_children_kills_registered_child_and_removes_pid_file() {
        let root = root("cleanup-orphan");
        let child = TokioCommand::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep child");
        let pid_file = register_child_pid(&root, "sleep", &child).expect("register child");
        assert!(pid_file.exists());

        cleanup_orphaned_children(&root).expect("cleanup child");
        let status = timeout(Duration::from_secs(2), child.wait_with_output())
            .await
            .expect("child exits after cleanup")
            .expect("wait child");

        assert!(!status.status.success());
        assert!(!pid_file.exists());
    }

    #[cfg(target_os = "linux")]
    #[tokio::test]
    async fn dropping_running_child_keeps_pid_file_for_cleanup() {
        let root = root("drop-child");
        let child = TokioCommand::new("sleep")
            .arg("30")
            .spawn()
            .expect("spawn sleep child");
        let pid_file = register_child_pid(&root, "sleep", &child).expect("register child");
        let running = RunningChild {
            name: "sleep",
            child,
            pid_file: Some(pid_file.clone()),
        };

        drop(running);

        assert!(pid_file.exists());
        cleanup_orphaned_children(&root).expect("cleanup dropped child");
        assert!(!pid_file.exists());
    }

    #[tokio::test]
    async fn local_coordinator_commits_serving_fact_and_shuts_down() {
        let root = root("coordinator-commit");
        let (socket, handle) = spawn_coordinator(&root);
        wait_coordinator_ready(&socket)
            .await
            .expect("coordinator ready");

        let committed = request_coordinator(
            &socket,
            &CoordinatorRequest::CommitServing(ServingCommitInput::new(
                "serving-1",
                "fd00::1:8080",
                "fd00::1",
                1,
            )),
        )
        .await
        .expect("commit serving");
        let CoordinatorSuccess::Committed(ack) = committed else {
            panic!("expected commit ack");
        };
        assert_eq!(ack.key, "/facts/serving/serving-1");
        assert_eq!(ack.author, "local-coordinator");
        assert_eq!(ack.origin, ServingCommitWriteOrigin::LocalCoordinator);

        let candidates = fact_source(&root)
            .list_candidates(
                &IslandId::new("prod"),
                &fact_pattern("/facts/serving/>").expect("pattern"),
                &identity_session(IslandId::new("prod"), PrincipalId::new("projection")),
            )
            .expect("list candidates");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].author().as_str(), "local-coordinator");

        shutdown_coordinator(&socket, handle)
            .await
            .expect("shutdown coordinator");
    }

    #[tokio::test]
    async fn local_coordinator_rejects_invalid_commit_without_fact() {
        let root = root("coordinator-invalid");
        let (socket, handle) = spawn_coordinator(&root);
        wait_coordinator_ready(&socket)
            .await
            .expect("coordinator ready");

        let error = request_coordinator(
            &socket,
            &CoordinatorRequest::CommitServing(ServingCommitInput::new(
                String::new(),
                "fd00::1:8080",
                "fd00::1",
                1,
            )),
        )
        .await
        .expect_err("invalid commit should fail");
        assert!(matches!(
            error,
            CoordinatorClientError::Failure(CoordinatorFailure {
                kind: CoordinatorFailureKind::InvalidRequest,
                ..
            })
        ));
        let candidates = fact_source(&root)
            .list_candidates(
                &IslandId::new("prod"),
                &fact_pattern("/facts/serving/>").expect("pattern"),
                &identity_session(IslandId::new("prod"), PrincipalId::new("projection")),
            )
            .expect("list candidates");
        assert!(candidates.is_empty());

        shutdown_coordinator(&socket, handle)
            .await
            .expect("shutdown coordinator");
    }

    #[tokio::test]
    async fn dropped_coordinator_client_does_not_stop_coordinator() {
        let root = root("coordinator-dropped-client");
        let (socket, handle) = spawn_coordinator(&root);
        wait_coordinator_ready(&socket)
            .await
            .expect("coordinator ready");

        let mut stream = UnixStream::connect(&socket)
            .await
            .expect("connect dropped client");
        let request = serde_json::to_vec(&CoordinatorRequest::Status).expect("serialize status");
        stream.write_all(&request).await.expect("write status");
        stream.shutdown().await.expect("finish status request");
        drop(stream);

        let status = request_coordinator(&socket, &CoordinatorRequest::Status)
            .await
            .expect("coordinator still accepts status");
        assert!(matches!(status, CoordinatorSuccess::Status(_)));

        shutdown_coordinator(&socket, handle)
            .await
            .expect("shutdown coordinator");
    }

    #[tokio::test]
    async fn local_coordinator_reports_storage_errors_separately() {
        let root = root("coordinator-storage");
        let writer = ServingFactWriter::new(&root).expect("serving fact writer");
        let payloads = root.join("facts").join("payloads");
        fs::remove_dir_all(&payloads).expect("remove payload dir");
        fs::write(&payloads, b"file blocks payload directory").expect("write blocking file");

        let error = handle_coordinator_request(
            CoordinatorRequest::CommitServing(ServingCommitInput::new(
                "serving-1",
                "fd00::1:8080",
                "fd00::1",
                1,
            )),
            writer,
        )
        .await
        .expect_err("storage error should fail");

        assert!(matches!(
            error,
            CoordinatorFailure {
                kind: CoordinatorFailureKind::Storage,
                ..
            }
        ));
    }

    #[test]
    fn remote_replication_injector_writes_already_authorized_fact() {
        let root = root("remote-injector");
        let response = run_remote_replication_injector(RemoteReplicationInjectorConfig::new(
            &root,
            "remote-scheduler",
            "serving-2",
            "fd00::2:8080",
            "fd00::2",
            2,
        ));
        let ReplicationInjectorResponse::Success(ack) = response else {
            panic!("expected injector success");
        };

        assert_eq!(ack.key, "/facts/serving/serving-2");
        assert_eq!(ack.author, "remote-scheduler");
        assert_eq!(
            ack.origin,
            ServingCommitWriteOrigin::RemoteReplication {
                author: "remote-scheduler".to_string()
            }
        );
        let candidates = fact_source(&root)
            .list_candidates(
                &IslandId::new("prod"),
                &fact_pattern("/facts/serving/>").expect("pattern"),
                &identity_session(IslandId::new("prod"), PrincipalId::new("projection")),
            )
            .expect("list candidates");
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].author().as_str(), "remote-scheduler");
    }

    #[test]
    fn remote_replication_injector_returns_typed_failures() {
        let root = root("remote-injector-failure");
        let response = run_remote_replication_injector(RemoteReplicationInjectorConfig::new(
            &root,
            "remote-scheduler",
            String::new(),
            "fd00::2:8080",
            "fd00::2",
            2,
        ));

        assert!(matches!(
            response,
            ReplicationInjectorResponse::Failure(ReplicationInjectorFailure {
                kind: ReplicationInjectorFailureKind::InvalidRequest,
                ..
            })
        ));
        let candidates = fact_source(&root)
            .list_candidates(
                &IslandId::new("prod"),
                &fact_pattern("/facts/serving/>").expect("pattern"),
                &identity_session(IslandId::new("prod"), PrincipalId::new("projection")),
            )
            .expect("list candidates");
        assert!(candidates.is_empty());
    }
}
