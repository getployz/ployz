use crate::error::{Error, Result};
use ployz_api::{MachineListPayload, NodeStatusPayload, PeerHealthPayload};
use ployz_sdk::{DaemonClient, TcpTransport};
use ployz_types::model::Phase;
use std::convert::TryFrom;
use std::net::TcpListener;
use std::net::{Ipv4Addr, SocketAddr};
use std::process::{Command, ExitStatus};
use std::thread;
use std::time::{Duration, Instant};

const RPC_CONNECT_TIMEOUT: Duration = Duration::from_millis(500);
const RPC_REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const WAIT_NODE_PHASE_RPC_GRACE: Duration = Duration::from_secs(1);
const POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug)]
pub(crate) struct CommandOutput {
    pub(crate) status: ExitStatus,
    pub(crate) stdout: String,
    pub(crate) stderr: String,
}

impl Default for CommandOutput {
    fn default() -> Self {
        Self {
            status: success_status(),
            stdout: String::new(),
            stderr: String::new(),
        }
    }
}

pub(crate) fn daemon_machine_list(rpc_port: u16) -> Result<MachineListPayload> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Io(format!("build machine list runtime: {error}")))?;
    let client = daemon_client(rpc_port);
    runtime
        .block_on(async { client.machine_list().await })
        .map_err(|error| {
            Error::Io(format!(
                "load machine list via tcp rpc port {rpc_port}: {error}"
            ))
        })
}

pub(crate) fn daemon_node_status(rpc_port: u16) -> Result<NodeStatusPayload> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Io(format!("build node status runtime: {error}")))?;
    let client = daemon_client(rpc_port);
    runtime
        .block_on(async { client.node_status().await })
        .map_err(|error| {
            Error::Io(format!(
                "probe node status via tcp rpc port {rpc_port}: {error}"
            ))
        })
}

pub(crate) fn daemon_wait_node_phase(
    rpc_port: u16,
    phase: Phase,
    timeout: Duration,
) -> Result<NodeStatusPayload> {
    let timeout_ms = u64::try_from(timeout.as_millis())
        .map_err(|error| Error::Io(format!("convert wait timeout to ms: {error}")))?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Io(format!("build wait node phase runtime: {error}")))?;
    let client = daemon_client_with_timeouts(
        rpc_port,
        RPC_CONNECT_TIMEOUT,
        timeout + WAIT_NODE_PHASE_RPC_GRACE,
    );
    runtime
        .block_on(async { client.wait_node_phase(phase, timeout_ms).await })
        .map_err(|error| {
            Error::Io(format!(
                "wait for node phase '{phase}' via tcp rpc port {rpc_port}: {error}"
            ))
        })
}

pub(crate) fn daemon_peer_health(rpc_port: u16, machine_id: &str) -> Result<PeerHealthPayload> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| Error::Io(format!("build peer health runtime: {error}")))?;
    let client = daemon_client(rpc_port);
    runtime
        .block_on(async { client.peer_health(machine_id).await })
        .map_err(|error| {
            Error::Io(format!(
                "load peer health for '{machine_id}' via tcp rpc port {rpc_port}: {error}"
            ))
        })
}

pub(crate) fn docker_outer<const N: usize>(args: [&str; N]) -> Result<CommandOutput> {
    run_command_expect_ok("docker", &args)
}

pub(crate) fn docker_outer_raw<const N: usize>(args: [&str; N]) -> Result<CommandOutput> {
    run_command("docker", &args)
}

pub(crate) fn run_command(program: &str, args: &[&str]) -> Result<CommandOutput> {
    let output = Command::new(program)
        .args(args)
        .output()
        .map_err(|error| Error::Io(format!("spawn {program}: {error}")))?;
    let stdout = String::from_utf8_lossy(&output.stdout).to_string();
    let stderr = String::from_utf8_lossy(&output.stderr).to_string();
    Ok(CommandOutput {
        status: output.status,
        stdout,
        stderr,
    })
}

pub(crate) fn run_command_expect_ok(program: &str, args: &[&str]) -> Result<CommandOutput> {
    let output = run_command(program, args)?;
    if output.status.success() {
        return Ok(output);
    }
    Err(Error::CommandFailed {
        command: format!("{program} {}", args.join(" ")),
        stdout: output.stdout,
        stderr: output.stderr,
    })
}

pub(crate) fn wait_until<F>(timeout: Duration, mut predicate: F) -> Result<()>
where
    F: FnMut() -> Result<bool>,
{
    let deadline = Instant::now() + timeout;
    loop {
        if predicate()? {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(Error::Message(format!(
                "timed out after {}s",
                timeout.as_secs()
            )));
        }
        thread::sleep(POLL_INTERVAL);
    }
}

pub(crate) fn pick_free_port() -> Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))
        .map_err(|error| Error::Io(format!("bind free port probe: {error}")))?;
    let port = listener
        .local_addr()
        .map_err(|error| Error::Io(format!("read free port probe address: {error}")))?
        .port();
    drop(listener);
    Ok(port)
}

fn daemon_client(rpc_port: u16) -> DaemonClient<TcpTransport> {
    daemon_client_with_timeouts(rpc_port, RPC_CONNECT_TIMEOUT, RPC_REQUEST_TIMEOUT)
}

fn daemon_client_with_timeouts(
    rpc_port: u16,
    connect_timeout: Duration,
    request_timeout: Duration,
) -> DaemonClient<TcpTransport> {
    let addr = SocketAddr::from((Ipv4Addr::LOCALHOST, rpc_port));
    DaemonClient::new(TcpTransport::new(addr).with_timeouts(connect_timeout, request_timeout))
}

#[cfg(unix)]
fn success_status() -> ExitStatus {
    use std::os::unix::process::ExitStatusExt;
    ExitStatusExt::from_raw(0)
}

#[cfg(windows)]
fn success_status() -> ExitStatus {
    use std::os::windows::process::ExitStatusExt;
    ExitStatusExt::from_raw(0)
}
