use std::fs;
use std::path::Path;
use std::time::Instant;

use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::process_role_harness::{
    RoleMutationStatus, RoleRequest, RoleServingStatus, RoleStatus, RoleSuccess,
    ServingCommitInput, assert_local_mutation_unavailable,
    cleanup_orphaned_children as cleanup_process_role_children, commit_serving,
    request_await_rebuild, request_begin_rebuild, request_project_once, request_reload,
    request_role, request_status, run_remote_injector, spawn_process_role, wait_for_coordinator,
    wait_for_serving_role,
};

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
    cleanup_process_role_children(&scenario_dir("process-role-serving-contract"))
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

    let local_mutation_failure_after_death = assert_local_mutation_unavailable(
        &coordinator_socket,
        ServingCommitInput::new("serving-after-kill", "fd00::dead:8080", "fd00::dead", 99),
    )
    .await?;

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
    let token = request_begin_rebuild(&serving_socket).await?;
    let rebuilding_status = request_status(&serving_socket).await?;
    assert_eq_named(
        "visible rebuild token",
        rebuilding_status.rebuild_in_progress,
        Some(token),
    )?;
    assert_role_answer(&serving_socket, "fd00::2:8080", "fd00::2").await?;
    let rebuild_query_probes = 1;
    request_await_rebuild(&serving_socket, token).await?;
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
        &[],
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
