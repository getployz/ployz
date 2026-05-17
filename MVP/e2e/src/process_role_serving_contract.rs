use std::env;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command as StdCommand, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tokio::process::{Child, Command as TokioCommand};
use tokio::time::{sleep, timeout};

use crate::assertions::assert_eq_named;
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::process_role_harness::{
    CoordinatorClientError, CoordinatorRequest, CoordinatorSuccess, ReplicationInjectorFailure,
    ReplicationInjectorResponse, RoleMutationStatus, RoleRequest, RoleServingStatus, RoleStatus,
    RoleSuccess, ServingCommitAck, ServingCommitInput, request_coordinator, request_role,
};

const READY_TIMEOUT: Duration = Duration::from_secs(3);
const CHILD_EXIT_TIMEOUT: Duration = Duration::from_secs(3);
const ROLE_REQUEST_PAUSE: Duration = Duration::from_millis(10);
const OUTAGE_PROBES: usize = 3;

#[derive(Debug, Serialize)]
struct ProcessRoleServingReport {
    scenario: &'static str,
    coordinator_killed: bool,
    serving_process_alive_after_kill: bool,
    coordinator_outage_query_probes: usize,
    rebuild_query_probes: usize,
    local_mutation_failure_after_death: String,
    commit_to_reload_us: u128,
    remote_commit_to_reload_us: u128,
    projection_rebuild_us: u128,
    serving_restart_us: u128,
    stale_snapshot_age_us: u64,
    baseline_gateway_revision: String,
    updated_gateway_revision: String,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create tokio runtime for process-role serving: {error}"))?;
    runtime.block_on(run_async())
}

pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
    let dir = child_pid_dir(&scenario_dir("process-role-serving-contract"));
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
    let root = scenario_dir("process-role-serving-contract");
    reset_dir(&root)?;

    let serving_socket = root.join("serving.sock");
    let coordinator_socket = root.join("coordinator.sock");

    let mut serving = spawn_process_role(
        "serving-projection",
        "serving-projection",
        &root,
        &serving_socket,
    )?;
    wait_for_serving_role(&serving_socket).await?;

    let mut coordinator = spawn_process_role(
        "local-coordinator",
        "local-coordinator",
        &root,
        &coordinator_socket,
    )?;
    wait_for_coordinator(&coordinator_socket).await?;

    let commit_started = Instant::now();
    let baseline_ack = commit_serving(
        &coordinator_socket,
        "serving-1",
        "fd00::1:8080",
        "fd00::1",
        1,
    )
    .await?;
    assert_eq_named(
        "baseline commit author",
        baseline_ack.author.as_str(),
        "local-coordinator",
    )?;
    request_project_once(&serving_socket).await?;
    let baseline_status = request_reload(&serving_socket).await?;
    assert_role_answer(&serving_socket, "fd00::1:8080", "fd00::1").await?;
    let commit_to_reload_us = commit_started.elapsed().as_micros();
    let baseline_gateway_revision = gateway_revision(&baseline_status)?;

    let coordinator_killed = coordinator.kill_and_wait().await?;
    let serving_process_alive_after_kill = serving.is_running()?;

    let mut coordinator_outage_query_probes = 0;
    for _ in 0..OUTAGE_PROBES {
        assert_role_answer(&serving_socket, "fd00::1:8080", "fd00::1").await?;
        coordinator_outage_query_probes += 1;
    }
    let role_status_after_kill = request_status(&serving_socket).await?;
    assert_eq_named(
        "serving role mutation status",
        role_status_after_kill.mutation,
        RoleMutationStatus::UnavailableInThisRole,
    )?;

    let local_mutation_failure_after_death =
        assert_local_mutation_unavailable(&coordinator_socket).await?;

    let remote_started = Instant::now();
    let remote_ack = run_remote_injector(
        &root,
        "remote-scheduler",
        "serving-2",
        "fd00::2:8080",
        "fd00::2",
        2,
    )
    .await?;
    assert_eq_named(
        "remote injected author",
        remote_ack.author.as_str(),
        "remote-scheduler",
    )?;
    request_project_once(&serving_socket).await?;
    let updated_status = request_reload(&serving_socket).await?;
    assert_role_answer(&serving_socket, "fd00::2:8080", "fd00::2").await?;
    let remote_commit_to_reload_us = remote_started.elapsed().as_micros();
    let updated_gateway_revision = gateway_revision(&updated_status)?;

    fs::remove_file(root.join("projections.sqlite")).map_err(|error| {
        format!("delete projection sqlite during process-role serving: {error}")
    })?;
    let rebuild_started = Instant::now();
    let token = begin_rebuild(&serving_socket).await?;
    let rebuilding_status = request_status(&serving_socket).await?;
    assert_eq_named(
        "visible rebuild token",
        rebuilding_status.rebuild_in_progress,
        Some(token),
    )?;
    assert_role_answer(&serving_socket, "fd00::2:8080", "fd00::2").await?;
    let rebuild_query_probes = 1;
    await_rebuild(&serving_socket, token).await?;
    request_reload(&serving_socket).await?;
    assert_role_answer(&serving_socket, "fd00::2:8080", "fd00::2").await?;
    let projection_rebuild_us = rebuild_started.elapsed().as_micros();

    request_role(&serving_socket, &RoleRequest::Shutdown)
        .await
        .map_err(|error| format!("shutdown serving process before restart: {error:?}"))?;
    serving.wait_for_exit().await?;

    let restart_started = Instant::now();
    let restarted_socket = root.join("serving-restarted.sock");
    let mut restarted_serving = spawn_process_role(
        "serving-projection-restart",
        "serving-projection",
        &root,
        &restarted_socket,
    )?;
    wait_for_serving_role(&restarted_socket).await?;
    request_reload(&restarted_socket).await?;
    assert_role_answer(&restarted_socket, "fd00::2:8080", "fd00::2").await?;
    let serving_restart_us = restart_started.elapsed().as_micros();
    let final_status = request_status(&restarted_socket).await?;
    let stale_snapshot_age_us = snapshot_age_us(&final_status)?;

    request_role(&restarted_socket, &RoleRequest::Shutdown)
        .await
        .map_err(|error| format!("shutdown restarted serving process: {error:?}"))?;
    restarted_serving.wait_for_exit().await?;

    let report = ProcessRoleServingReport {
        scenario: "process-role-serving-contract",
        coordinator_killed,
        serving_process_alive_after_kill,
        coordinator_outage_query_probes,
        rebuild_query_probes,
        local_mutation_failure_after_death,
        commit_to_reload_us,
        remote_commit_to_reload_us,
        projection_rebuild_us,
        serving_restart_us,
        stale_snapshot_age_us,
        baseline_gateway_revision,
        updated_gateway_revision,
        elapsed_ms: started.elapsed().as_millis(),
    };
    assert_eq_named("coordinator killed", report.coordinator_killed, true)?;
    assert_eq_named(
        "serving process alive after coordinator kill",
        report.serving_process_alive_after_kill,
        true,
    )?;
    assert_eq_named(
        "outage query probes",
        report.coordinator_outage_query_probes,
        OUTAGE_PROBES,
    )?;
    assert_eq_named("rebuild query probes", report.rebuild_query_probes, 1)?;

    let json = write_json(
        &root.join("process-role-serving-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS process-role-serving-contract");
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

async fn request_reload(socket: &Path) -> Result<RoleServingStatus, String> {
    match request_role(socket, &RoleRequest::Reload)
        .await
        .map_err(|error| format!("reload serving role: {error:?}"))?
    {
        RoleSuccess::Reloaded(status) => Ok(status),
        other => Err(format!("unexpected reload response: {other:?}")),
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

async fn request_status(socket: &Path) -> Result<RoleStatus, String> {
    match request_role(socket, &RoleRequest::Status)
        .await
        .map_err(|error| format!("request serving role status: {error:?}"))?
    {
        RoleSuccess::Status(status) => Ok(status),
        other => Err(format!("unexpected status response: {other:?}")),
    }
}

async fn assert_role_answer(socket: &Path, backend: &str, dns: &str) -> Result<(), String> {
    let route = match request_role(
        socket,
        &RoleRequest::QueryGateway {
            host: "WEB.EXAMPLE.TEST".to_string(),
        },
    )
    .await
    .map_err(|error| format!("query gateway through serving role: {error:?}"))?
    {
        RoleSuccess::GatewayRoute { route: Some(route) } => route,
        other => return Err(format!("unexpected gateway response: {other:?}")),
    };
    let [actual_backend] = route.backends.as_slice() else {
        return Err(format!(
            "expected exactly one gateway backend, got {:?}",
            route.backends
        ));
    };
    assert_eq_named(
        "process role gateway backend",
        actual_backend.address.as_str(),
        backend,
    )?;

    let records = match request_role(
        socket,
        &RoleRequest::QueryDns {
            name: "web.example.test".to_string(),
            record_type: "aaaa".to_string(),
        },
    )
    .await
    .map_err(|error| format!("query dns through serving role: {error:?}"))?
    {
        RoleSuccess::DnsRecords { records } => records,
        other => return Err(format!("unexpected dns response: {other:?}")),
    };
    let [record] = records.as_slice() else {
        return Err(format!("expected exactly one dns record, got {records:?}"));
    };
    assert_eq_named("process role dns value", record.value.as_str(), dns)
}

async fn assert_local_mutation_unavailable(socket: &Path) -> Result<String, String> {
    let result = request_coordinator(
        socket,
        &CoordinatorRequest::CommitServing(ServingCommitInput::new(
            "serving-after-kill",
            "fd00::dead:8080",
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

fn gateway_revision(status: &RoleServingStatus) -> Result<String, String> {
    match status {
        RoleServingStatus::Available {
            gateway_revision, ..
        } => Ok(gateway_revision.clone()),
        other => Err(format!("serving role was not available: {other:?}")),
    }
}

fn snapshot_age_us(status: &RoleStatus) -> Result<u64, String> {
    match &status.serving {
        RoleServingStatus::Available {
            snapshot_age_us, ..
        } => Ok(*snapshot_age_us),
        other => Err(format!("serving role was not available: {other:?}")),
    }
}

fn spawn_process_role(
    name: &'static str,
    role: &'static str,
    root: &Path,
    socket: &Path,
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

struct RunningChild {
    name: &'static str,
    child: Child,
    pid_file: Option<PathBuf>,
}

impl RunningChild {
    fn is_running(&mut self) -> Result<bool, String> {
        self.child
            .try_wait()
            .map(|status| status.is_none())
            .map_err(|error| format!("poll {} child: {error}", self.name))
    }

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
