use std::env;
use std::fs;
use std::net::{Ipv4Addr, SocketAddr, SocketAddrV4};
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use hickory_proto::op::{Message, Query, ResponseCode};
use hickory_proto::rr::{Name, RData, RecordType};
use serde::Serialize;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream, UdpSocket};
use tokio::process::{Child, Command as TokioCommand};
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout};

use crate::assertions::assert_eq_named;
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::process_role_harness::{
    CoordinatorClientError, CoordinatorRequest, CoordinatorSuccess, ReplicationInjectorFailure,
    ReplicationInjectorResponse, RoleRequest, RoleServingStatus, RoleSuccess, ServingCommitAck,
    ServingCommitInput, WireProcessStatus, WireRoleClientError, WireRoleRequest, WireRoleSuccess,
    request_coordinator, request_role, request_wire_role,
};

const READY_TIMEOUT: Duration = Duration::from_secs(3);
const CHILD_EXIT_TIMEOUT: Duration = Duration::from_secs(3);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
const ROLE_REQUEST_PAUSE: Duration = Duration::from_millis(10);
const OUTAGE_PROBES: usize = 3;

#[derive(Debug, Serialize)]
struct WireServingReport {
    scenario: &'static str,
    coordinator_killed: bool,
    http_outage_success_count: usize,
    dns_outage_success_count: usize,
    rebuild_probe_success_count: usize,
    local_mutation_failure_after_death: String,
    http_reload_latency_us: u128,
    dns_reload_latency_us: u128,
    projection_rebuild_us: u128,
    http_restart_us: u128,
    dns_restart_us: u128,
    stale_snapshot_age_us: u64,
    http_request_count: u64,
    dns_request_count: u64,
    malformed_dns_count: u64,
    backend_failure_count: u64,
    http_latency_samples_us: Vec<u64>,
    dns_latency_samples_us: Vec<u64>,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create tokio runtime for wire serving: {error}"))?;
    runtime.block_on(run_async())
}

pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
    let dir = child_pid_dir(&scenario_dir("wire-serving-contract"));
    let entries = match fs::read_dir(&dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(format!("read child pid dir '{}': {error}", dir.display())),
    };

    let mut pids = Vec::new();
    for entry in entries {
        let path = entry
            .map_err(|error| format!("read child pid entry: {error}"))?
            .path();
        if path.extension().and_then(|extension| extension.to_str()) != Some("pid") {
            continue;
        }
        let contents = fs::read_to_string(&path)
            .map_err(|error| format!("read child pid '{}': {error}", path.display()))?;
        let pid = contents
            .trim()
            .parse::<u32>()
            .map_err(|error| format!("parse child pid '{}': {error}", path.display()))?;
        pids.push(pid);
    }

    for pid in &pids {
        send_signal(*pid, "-TERM")?;
    }
    thread::sleep(Duration::from_millis(100));
    for pid in pids {
        send_signal(pid, "-KILL")?;
    }
    Ok(())
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("wire-serving-contract");
    reset_dir(&root)?;

    let serving_socket = root.join("serving.sock");
    let coordinator_socket = root.join("coordinator.sock");
    let http_socket = root.join("http-gateway.sock");
    let dns_socket = root.join("dns-server.sock");

    let backend_1 = TestBackend::spawn("backend-1").await?;
    let backend_2 = TestBackend::spawn("backend-2").await?;

    let mut serving = spawn_process_role(
        "serving-projection",
        "serving-projection",
        &root,
        &serving_socket,
        &[],
    )?;
    wait_for_serving_role(&serving_socket).await?;

    let mut coordinator = spawn_process_role(
        "local-coordinator",
        "local-coordinator",
        &root,
        &coordinator_socket,
        &[],
    )?;
    wait_for_coordinator(&coordinator_socket).await?;

    commit_serving(
        &coordinator_socket,
        "serving-1",
        &backend_1.addr().to_string(),
        "fd00::1",
        1,
    )
    .await?;
    request_project_once(&serving_socket).await?;
    request_reload_serving(&serving_socket).await?;

    let mut http = spawn_process_role(
        "http-gateway",
        "http-gateway",
        &root,
        &http_socket,
        &["--listen", "127.0.0.1:0"],
    )?;
    let mut dns = spawn_process_role(
        "dns-server",
        "dns-server",
        &root,
        &dns_socket,
        &["--listen", "127.0.0.1:0"],
    )?;
    let http_addr = wait_for_wire_role(&http_socket).await?;
    let dns_addr = wait_for_wire_role(&dns_socket).await?;
    assert_http_answer(http_addr, "backend-1").await?;
    assert_dns_answer(dns_addr, "fd00::1").await?;

    let coordinator_killed = coordinator.kill_and_wait().await?;

    let mut http_outage_success_count = 0;
    let mut dns_outage_success_count = 0;
    for _ in 0..OUTAGE_PROBES {
        assert_http_answer(http_addr, "backend-1").await?;
        http_outage_success_count += 1;
        assert_dns_answer(dns_addr, "fd00::1").await?;
        dns_outage_success_count += 1;
    }
    let local_mutation_failure_after_death =
        assert_local_mutation_unavailable(&coordinator_socket).await?;

    run_remote_injector(
        &root,
        "remote-scheduler",
        "serving-2",
        &backend_2.addr().to_string(),
        "fd00::2",
        2,
    )
    .await?;
    request_project_once(&serving_socket).await?;
    request_reload_serving(&serving_socket).await?;
    let http_reload_latency_us = reload_wire_role(&http_socket).await?;
    let dns_reload_latency_us = reload_wire_role(&dns_socket).await?;
    assert_http_answer(http_addr, "backend-2").await?;
    assert_dns_answer(dns_addr, "fd00::2").await?;

    fs::write(root.join("dns.snapshot"), b"not json")
        .map_err(|error| format!("corrupt dns snapshot: {error}"))?;
    assert_wire_reload_fails(&http_socket).await?;
    assert_wire_reload_fails(&dns_socket).await?;
    assert_http_answer(http_addr, "backend-2").await?;
    assert_dns_answer(dns_addr, "fd00::2").await?;

    request_project_once(&serving_socket).await?;
    reload_wire_role(&http_socket).await?;
    reload_wire_role(&dns_socket).await?;

    fs::remove_file(root.join("projections.sqlite"))
        .map_err(|error| format!("delete projection sqlite during wire serving: {error}"))?;
    let rebuild_started = Instant::now();
    let token = begin_rebuild(&serving_socket).await?;
    assert_http_answer(http_addr, "backend-2").await?;
    assert_dns_answer(dns_addr, "fd00::2").await?;
    let rebuild_probe_success_count = 2;
    await_rebuild(&serving_socket, token).await?;
    reload_wire_role(&http_socket).await?;
    reload_wire_role(&dns_socket).await?;
    let projection_rebuild_us = rebuild_started.elapsed().as_micros();

    request_wire_role(&http_socket, &WireRoleRequest::Shutdown)
        .await
        .map_err(|error| format!("shutdown http gateway before restart: {error:?}"))?;
    http.wait_for_exit().await?;
    let http_restart_started = Instant::now();
    let restarted_http_socket = root.join("http-gateway-restarted.sock");
    let mut restarted_http = spawn_process_role(
        "http-gateway-restarted",
        "http-gateway",
        &root,
        &restarted_http_socket,
        &["--listen", "127.0.0.1:0"],
    )?;
    let restarted_http_addr = wait_for_wire_role(&restarted_http_socket).await?;
    assert_http_answer(restarted_http_addr, "backend-2").await?;
    let http_restart_us = http_restart_started.elapsed().as_micros();

    request_wire_role(&dns_socket, &WireRoleRequest::Shutdown)
        .await
        .map_err(|error| format!("shutdown dns server before restart: {error:?}"))?;
    dns.wait_for_exit().await?;
    let dns_restart_started = Instant::now();
    let restarted_dns_socket = root.join("dns-server-restarted.sock");
    let mut restarted_dns = spawn_process_role(
        "dns-server-restarted",
        "dns-server",
        &root,
        &restarted_dns_socket,
        &["--listen", "127.0.0.1:0"],
    )?;
    let restarted_dns_addr = wait_for_wire_role(&restarted_dns_socket).await?;
    assert_dns_answer(restarted_dns_addr, "fd00::2").await?;
    let dns_restart_us = dns_restart_started.elapsed().as_micros();

    send_malformed_dns(restarted_dns_addr).await?;
    assert_dns_answer(restarted_dns_addr, "fd00::2").await?;

    let final_http_status = request_wire_status(&restarted_http_socket).await?;
    let final_dns_status = request_wire_status(&restarted_dns_socket).await?;
    let stale_snapshot_age_us = snapshot_age_us(&final_http_status)?;

    request_wire_role(&restarted_http_socket, &WireRoleRequest::Shutdown)
        .await
        .map_err(|error| format!("shutdown restarted http gateway: {error:?}"))?;
    restarted_http.wait_for_exit().await?;
    request_wire_role(&restarted_dns_socket, &WireRoleRequest::Shutdown)
        .await
        .map_err(|error| format!("shutdown restarted dns server: {error:?}"))?;
    restarted_dns.wait_for_exit().await?;
    request_role(&serving_socket, &RoleRequest::Shutdown)
        .await
        .map_err(|error| format!("shutdown serving projection: {error:?}"))?;
    serving.wait_for_exit().await?;
    backend_1.shutdown().await;
    backend_2.shutdown().await;

    let report = WireServingReport {
        scenario: "wire-serving-contract",
        coordinator_killed,
        http_outage_success_count,
        dns_outage_success_count,
        rebuild_probe_success_count,
        local_mutation_failure_after_death,
        http_reload_latency_us,
        dns_reload_latency_us,
        projection_rebuild_us,
        http_restart_us,
        dns_restart_us,
        stale_snapshot_age_us,
        http_request_count: final_http_status.metrics.request_count,
        dns_request_count: final_dns_status.metrics.request_count,
        malformed_dns_count: final_dns_status.metrics.malformed_dns_count,
        backend_failure_count: final_http_status.metrics.backend_failure_count,
        http_latency_samples_us: final_http_status.metrics.latency_samples_us,
        dns_latency_samples_us: final_dns_status.metrics.latency_samples_us,
        elapsed_ms: started.elapsed().as_millis(),
    };
    assert_eq_named("coordinator killed", report.coordinator_killed, true)?;
    assert_eq_named(
        "http outage success count",
        report.http_outage_success_count,
        OUTAGE_PROBES,
    )?;
    assert_eq_named(
        "dns outage success count",
        report.dns_outage_success_count,
        OUTAGE_PROBES,
    )?;
    assert_eq_named(
        "rebuild probe success count",
        report.rebuild_probe_success_count,
        2,
    )?;
    assert_eq_named("malformed dns count", report.malformed_dns_count, 1)?;
    if report.http_latency_samples_us.is_empty() || report.dns_latency_samples_us.is_empty() {
        return Err("wire latency samples must be present".to_string());
    }

    let json = write_json(&root.join("wire-serving-contract-metrics.json"), &report)?;
    println!("{json}");
    eprintln!("PASS wire-serving-contract");
    Ok(())
}

async fn wait_for_serving_role(socket: &Path) -> Result<(), String> {
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

async fn wait_for_coordinator(socket: &Path) -> Result<(), String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        match request_coordinator(socket, &CoordinatorRequest::Status).await {
            Ok(CoordinatorSuccess::Status(_)) => return Ok(()),
            Ok(other) => return Err(format!("unexpected coordinator ready response: {other:?}")),
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                sleep(ROLE_REQUEST_PAUSE).await;
            }
            Err(error) => return Err(format!("coordinator did not become ready: {error:?}")),
        }
    }
}

async fn wait_for_wire_role(socket: &Path) -> Result<SocketAddr, String> {
    let deadline = Instant::now() + READY_TIMEOUT;
    loop {
        match request_wire_status(socket).await {
            Ok(status) => return parse_listen_addr(&status),
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                sleep(ROLE_REQUEST_PAUSE).await;
            }
            Err(error) => return Err(format!("wire role did not become ready: {error:?}")),
        }
    }
}

async fn request_wire_status(socket: &Path) -> Result<WireProcessStatus, String> {
    match request_wire_role(socket, &WireRoleRequest::Status)
        .await
        .map_err(|error| format!("request wire role status: {error:?}"))?
    {
        WireRoleSuccess::Status(status) => Ok(status),
        other => Err(format!("unexpected wire status response: {other:?}")),
    }
}

async fn reload_wire_role(socket: &Path) -> Result<u128, String> {
    let started = Instant::now();
    match request_wire_role(socket, &WireRoleRequest::Reload)
        .await
        .map_err(|error| format!("reload wire role: {error:?}"))?
    {
        WireRoleSuccess::Status(_) => Ok(started.elapsed().as_micros()),
        other => Err(format!("unexpected wire reload response: {other:?}")),
    }
}

async fn assert_wire_reload_fails(socket: &Path) -> Result<(), String> {
    match request_wire_role(socket, &WireRoleRequest::Reload).await {
        Err(WireRoleClientError::Failure(_)) => Ok(()),
        Err(WireRoleClientError::Transport(message)) => Err(format!(
            "wire reload transport failed unexpectedly: {message}"
        )),
        Ok(success) => Err(format!("wire reload unexpectedly succeeded: {success:?}")),
    }
}

fn parse_listen_addr(status: &WireProcessStatus) -> Result<SocketAddr, String> {
    status
        .listen_addr
        .parse::<SocketAddr>()
        .map_err(|error| format!("parse wire listen addr '{}': {error}", status.listen_addr))
}

async fn commit_serving(
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

async fn request_project_once(socket: &Path) -> Result<(), String> {
    match request_role(socket, &RoleRequest::ProjectOnce)
        .await
        .map_err(|error| format!("project role once: {error:?}"))?
    {
        RoleSuccess::Projected(_) => Ok(()),
        other => Err(format!("unexpected project response: {other:?}")),
    }
}

async fn request_reload_serving(socket: &Path) -> Result<(), String> {
    match request_role(socket, &RoleRequest::Reload)
        .await
        .map_err(|error| format!("reload serving projection: {error:?}"))?
    {
        RoleSuccess::Reloaded(_) => Ok(()),
        other => Err(format!("unexpected serving reload response: {other:?}")),
    }
}

async fn begin_rebuild(socket: &Path) -> Result<u64, String> {
    match request_role(socket, &RoleRequest::BeginRebuild)
        .await
        .map_err(|error| format!("begin projection rebuild: {error:?}"))?
    {
        RoleSuccess::RebuildStarted { token } => Ok(token),
        other => Err(format!("unexpected begin rebuild response: {other:?}")),
    }
}

async fn await_rebuild(socket: &Path, token: u64) -> Result<(), String> {
    match request_role(socket, &RoleRequest::AwaitRebuild { token })
        .await
        .map_err(|error| format!("await projection rebuild: {error:?}"))?
    {
        RoleSuccess::RebuildFinished { token: done, .. } if done == token => Ok(()),
        other => Err(format!("unexpected await rebuild response: {other:?}")),
    }
}

async fn assert_local_mutation_unavailable(socket: &Path) -> Result<String, String> {
    let result = request_coordinator(
        socket,
        &CoordinatorRequest::CommitServing(ServingCommitInput::new(
            "wire-serving-after-kill",
            "127.0.0.1:9",
            "fd00::dead",
            99,
        )),
    )
    .await;
    match result {
        Err(CoordinatorClientError::Transport(message)) => Ok(message),
        Err(CoordinatorClientError::Failure(failure)) => Ok(format!("{failure:?}")),
        Ok(success) => Err(format!(
            "local mutation unexpectedly succeeded after coordinator death: {success:?}"
        )),
    }
}

async fn run_remote_injector(
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

async fn assert_http_answer(addr: SocketAddr, backend_id: &str) -> Result<(), String> {
    let response = http_get(addr, "web.example.test", "/health").await?;
    if !response.starts_with("HTTP/1.1 200 OK") || !response.contains(backend_id) {
        return Err(format!(
            "unexpected HTTP response for {backend_id}: {response}"
        ));
    }
    Ok(())
}

async fn assert_dns_answer(addr: SocketAddr, expected: &str) -> Result<(), String> {
    let response = dns_query(addr, "web.example.test.", RecordType::AAAA).await?;
    if response.metadata.response_code != ResponseCode::NoError {
        return Err(format!(
            "unexpected DNS response code: {:?}",
            response.metadata.response_code
        ));
    }
    let [answer] = response.answers.as_slice() else {
        return Err(format!(
            "expected one DNS answer, got {:?}",
            response.answers
        ));
    };
    match &answer.data {
        RData::AAAA(addr) if addr.to_string() == expected => Ok(()),
        other => Err(format!("unexpected DNS answer: {other:?}")),
    }
}

async fn http_get(addr: SocketAddr, host: &str, path: &str) -> Result<String, String> {
    let mut stream = timeout(REQUEST_TIMEOUT, TcpStream::connect(addr))
        .await
        .map_err(|_| format!("connect HTTP gateway {addr} timed out"))?
        .map_err(|error| format!("connect HTTP gateway {addr}: {error}"))?;
    let request = format!("GET {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n\r\n");
    timeout(REQUEST_TIMEOUT, stream.write_all(request.as_bytes()))
        .await
        .map_err(|_| "write HTTP request timed out".to_string())?
        .map_err(|error| format!("write HTTP request: {error}"))?;
    let mut response = Vec::new();
    timeout(REQUEST_TIMEOUT, stream.read_to_end(&mut response))
        .await
        .map_err(|_| "read HTTP response timed out".to_string())?
        .map_err(|error| format!("read HTTP response: {error}"))?;
    String::from_utf8(response).map_err(|error| format!("HTTP response was not utf8: {error}"))
}

async fn dns_query(
    addr: SocketAddr,
    name: &str,
    record_type: RecordType,
) -> Result<Message, String> {
    let socket = UdpSocket::bind(loopback_any())
        .await
        .map_err(|error| format!("bind DNS client: {error}"))?;
    let mut query = Message::query();
    query.add_query(Query::query(
        Name::from_ascii(name).map_err(|error| format!("parse DNS name '{name}': {error}"))?,
        record_type,
    ));
    let bytes = query
        .to_vec()
        .map_err(|error| format!("encode DNS query: {error}"))?;
    socket
        .send_to(&bytes, addr)
        .await
        .map_err(|error| format!("send DNS query: {error}"))?;
    let mut response = [0_u8; 2048];
    let len = timeout(REQUEST_TIMEOUT, socket.recv(&mut response))
        .await
        .map_err(|_| "DNS response timed out".to_string())?
        .map_err(|error| format!("receive DNS response: {error}"))?;
    Message::from_vec(&response[..len]).map_err(|error| format!("decode DNS response: {error}"))
}

async fn send_malformed_dns(addr: SocketAddr) -> Result<(), String> {
    let socket = UdpSocket::bind(loopback_any())
        .await
        .map_err(|error| format!("bind malformed DNS client: {error}"))?;
    socket
        .send_to(b"not dns", addr)
        .await
        .map_err(|error| format!("send malformed DNS packet: {error}"))?;
    sleep(Duration::from_millis(20)).await;
    Ok(())
}

fn snapshot_age_us(status: &WireProcessStatus) -> Result<u64, String> {
    match &status.serving {
        RoleServingStatus::Available {
            snapshot_age_us, ..
        } => Ok(*snapshot_age_us),
        other => Err(format!("wire role serving state unavailable: {other:?}")),
    }
}

fn spawn_process_role(
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
    fs::write(&path, format!("{pid}\n"))
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

fn send_signal(pid: u32, signal: &str) -> Result<(), String> {
    let _status = StdCommand::new("kill")
        .arg(signal)
        .arg(pid.to_string())
        .status()
        .map_err(|error| format!("run kill {signal} {pid}: {error}"))?;
    Ok(())
}

fn loopback_any() -> SocketAddr {
    SocketAddr::V4(SocketAddrV4::new(Ipv4Addr::LOCALHOST, 0))
}

struct RunningChild {
    name: &'static str,
    child: Child,
    pid_file: Option<PathBuf>,
}

impl RunningChild {
    async fn kill_and_wait(&mut self) -> Result<bool, String> {
        self.child
            .start_kill()
            .map_err(|error| format!("kill {} child: {error}", self.name))?;
        self.wait_for_exit().await.map(|status| !status.success())
    }

    async fn wait_for_exit(&mut self) -> Result<ExitStatus, String> {
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
        let _ = self.child.start_kill();
    }
}

struct TestBackend {
    addr: SocketAddr,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

impl TestBackend {
    async fn spawn(id: &'static str) -> Result<Self, String> {
        let listener = TcpListener::bind(loopback_any())
            .await
            .map_err(|error| format!("bind test backend: {error}"))?;
        let addr = listener
            .local_addr()
            .map_err(|error| format!("read test backend addr: {error}"))?;
        let (shutdown, mut shutdown_rx) = oneshot::channel();
        let task = tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut shutdown_rx => return,
                    accepted = listener.accept() => {
                        let Ok((mut stream, _)) = accepted else {
                            return;
                        };
                        tokio::spawn(async move {
                            let mut request = Vec::new();
                            let mut chunk = [0_u8; 1024];
                            loop {
                                let Ok(read) = stream.read(&mut chunk).await else {
                                    return;
                                };
                                if read == 0 {
                                    return;
                                }
                                request.extend_from_slice(&chunk[..read]);
                                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                                    break;
                                }
                            }
                            let request_line = String::from_utf8_lossy(&request)
                                .lines()
                                .next()
                                .unwrap_or("GET / HTTP/1.1")
                                .to_string();
                            let path = request_line.split_whitespace().nth(1).unwrap_or("/");
                            let body = format!("{id} {path}\n");
                            let response = format!(
                                "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                                body.len(),
                                body
                            );
                            let _ = stream.write_all(response.as_bytes()).await;
                        });
                    }
                }
            }
        });
        Ok(Self {
            addr,
            shutdown,
            task,
        })
    }

    fn addr(&self) -> SocketAddr {
        self.addr
    }

    async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let _ = self.task.await;
    }
}
