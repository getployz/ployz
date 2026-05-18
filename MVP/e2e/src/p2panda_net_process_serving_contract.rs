use std::time::{Duration, Instant};
use std::{fs, path::Path};

use mvp_bus::PrincipalId;
use mvp_p2panda_facts::PandaFactAuthor;
use serde::Serialize;
use tokio::time::sleep;

use crate::assertions::assert_eq_named;
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::process_role_harness::{
    P2pandaNetRoleStatus, P2pandaNetScriptedPublisherProcess, RoleRequest, RoleStatus,
    ServingCommitInput, assert_local_mutation_unavailable, assert_serving_role_answer,
    cleanup_orphaned_children as cleanup_process_role_children, request_await_rebuild,
    request_begin_rebuild, request_reload, request_role, request_status,
    serving_role_gateway_revision, spawn_p2panda_net_scripted_publisher_process,
    spawn_process_role, wait_for_serving_role,
};

const SCENARIO: &str = "p2panda-net-process-serving-contract";

#[derive(Debug, Serialize)]
struct P2pandaNetProcessServingReport {
    scenario: &'static str,
    remote_updates_imported: usize,
    rejected_operations: usize,
    rejected_operation_kind: String,
    stream_idle_refreshes_after_rejection: usize,
    replayed_operations_skipped_after_rejection: u64,
    local_mutation_failure: String,
    serving_process_alive_after_update: bool,
    baseline_gateway_revision: String,
    updated_gateway_revision: String,
    restarted_gateway_revision: String,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("create tokio runtime for p2panda-net serving: {error}"))?;
    runtime.block_on(run_async())
}

pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
    cleanup_process_role_children(&scenario_dir(SCENARIO))
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir(SCENARIO);
    reset_dir(&root)?;

    let network = repeated_hex("31");
    let topic = repeated_hex("32");
    let receiver_seed = repeated_hex("33");
    let baseline_publisher_seed = repeated_hex("34");
    let author_seed = repeated_hex("41");
    let remote_author_seed = repeated_hex("42");
    let author = PandaFactAuthor::from_private_key_hex(
        PrincipalId::new("p2panda-net-scripted-scheduler"),
        &author_seed,
    )
    .map_err(|error| format!("create p2panda-net author from seed: {error}"))?;
    let remote_author = PandaFactAuthor::from_private_key_hex(
        PrincipalId::new("p2panda-net-remote-scheduler"),
        &remote_author_seed,
    )
    .map_err(|error| format!("create remote p2panda-net author from seed: {error}"))?;

    let serving_socket = root.join("serving.sock");
    let store_path = root.join("p2panda-net-serving.sqlite");
    let trusted_author = format!(
        "{}:{}",
        author.principal().as_str(),
        author.author_key().as_hex()
    );
    let remote_trusted_author = format!(
        "{}:{}",
        remote_author.principal().as_str(),
        remote_author.author_key().as_hex()
    );
    let trusted_authors = vec![trusted_author.clone(), remote_trusted_author.clone()];
    let receiver_args = p2panda_net_receiver_args(
        &store_path,
        &network,
        &topic,
        &receiver_seed,
        &trusted_authors,
    );
    let receiver_arg_refs = receiver_args.iter().map(String::as_str).collect::<Vec<_>>();
    let mut serving = spawn_process_role(
        "p2panda-net-serving-projection",
        "p2panda-net-serving-projection",
        &root,
        &serving_socket,
        &receiver_arg_refs,
    )?;
    wait_for_serving_role(&serving_socket).await?;
    let receiver_ticket =
        wait_for_p2panda_net_status(&serving_socket, |status| status.p2panda_net().is_some())
            .await?
            .p2panda_net()
            .ok_or_else(|| "p2panda-net status missing after readiness".to_string())?
            .node_ticket
            .clone();

    let mut baseline_publisher =
        spawn_p2panda_net_scripted_publisher_process(P2pandaNetScriptedPublisherProcess {
            root: &root,
            network: &network,
            topic: &topic,
            seed: &baseline_publisher_seed,
            bootstrap: &receiver_ticket,
            author: remote_author.principal().as_str(),
            author_seed: &remote_author_seed,
            first: ServingCommitInput::new("serving-1", "fd00::1:8080", "fd00::1", 1),
            second: Some(ServingCommitInput::new(
                "serving-2",
                "fd00::2:8080",
                "fd00::2",
                2,
            )),
            second_delay_ms: 4_000,
            publish_rejected: true,
        })?;

    let baseline_status = wait_for_p2panda_net_status(&serving_socket, |status| {
        p2panda_reloaded_revision(status, "serving-1")
    })
    .await?;
    let baseline_serving = request_reload(&serving_socket).await?;
    assert_serving_role_answer(
        &serving_socket,
        "fd00::1:8080",
        "fd00::1",
        "p2panda-net process role baseline",
    )
    .await?;
    let baseline_gateway_revision = serving_role_gateway_revision(&baseline_serving)?;
    let local_mutation_failure = assert_local_mutation_unavailable(
        &root.join("missing-coordinator.sock"),
        ServingCommitInput::new("serving-local", "fd00::9:8080", "fd00::9", 9),
    )
    .await?;

    let updated_status = wait_for_p2panda_net_status(&serving_socket, |status| {
        p2panda_reloaded_revision(status, "serving-2")
    })
    .await?;
    let updated_serving = request_reload(&serving_socket).await?;
    assert_serving_role_answer(
        &serving_socket,
        "fd00::2:8080",
        "fd00::2",
        "p2panda-net process role update",
    )
    .await?;
    let updated_gateway_revision = serving_role_gateway_revision(&updated_serving)?;
    let serving_process_alive_after_update = serving.is_running()?;

    let updated_refreshes = p2panda_status(&updated_status)?.stream_idle_refreshes;
    let after_rejection_status = wait_for_p2panda_net_status(&serving_socket, |status| {
        status.p2panda_net().is_some_and(|p2panda| {
            p2panda.stream_idle_refreshes > updated_refreshes && p2panda.rejected == 1
        })
    })
    .await?;
    assert_serving_role_answer(
        &serving_socket,
        "fd00::2:8080",
        "fd00::2",
        "p2panda-net process role after rejected import",
    )
    .await?;

    if baseline_publisher.is_running()? {
        baseline_publisher.kill_and_wait().await?;
    } else {
        let publisher_status = baseline_publisher.wait_for_exit().await?;
        if !publisher_status.success() {
            return Err(format!(
                "baseline p2panda-net scripted publisher exited with {publisher_status}"
            ));
        }
    }

    fs::remove_file(root.join("projections.sqlite"))
        .map_err(|error| format!("delete p2panda-net projection sqlite: {error}"))?;
    let token = request_begin_rebuild(&serving_socket).await?;
    assert_serving_role_answer(
        &serving_socket,
        "fd00::2:8080",
        "fd00::2",
        "p2panda-net process role during rebuild",
    )
    .await?;
    request_await_rebuild(&serving_socket, token).await?;
    request_reload(&serving_socket).await?;
    assert_serving_role_answer(
        &serving_socket,
        "fd00::2:8080",
        "fd00::2",
        "p2panda-net process role after rebuild",
    )
    .await?;

    request_role(&serving_socket, &RoleRequest::Shutdown)
        .await
        .map_err(|error| format!("shutdown p2panda-net serving process: {error:?}"))?;
    serving.wait_for_exit().await?;

    let restarted_socket = root.join("serving-restarted.sock");
    let receiver_args = p2panda_net_receiver_args(
        &store_path,
        &network,
        &topic,
        &receiver_seed,
        &trusted_authors,
    );
    let receiver_arg_refs = receiver_args.iter().map(String::as_str).collect::<Vec<_>>();
    let mut restarted = spawn_process_role(
        "p2panda-net-serving-projection-restarted",
        "p2panda-net-serving-projection",
        &root,
        &restarted_socket,
        &receiver_arg_refs,
    )?;
    wait_for_serving_role(&restarted_socket).await?;
    request_reload(&restarted_socket).await?;
    assert_serving_role_answer(
        &restarted_socket,
        "fd00::2:8080",
        "fd00::2",
        "p2panda-net process role after restart",
    )
    .await?;
    let restarted_gateway_revision =
        serving_role_gateway_revision(&request_reload(&restarted_socket).await?)?;
    request_role(&restarted_socket, &RoleRequest::Shutdown)
        .await
        .map_err(|error| format!("shutdown restarted p2panda-net serving process: {error:?}"))?;
    restarted.wait_for_exit().await?;

    let baseline_p2panda = p2panda_status(&baseline_status)?;
    let updated_p2panda = p2panda_status(&updated_status)?;
    let after_rejection_p2panda = p2panda_status(&after_rejection_status)?;
    let rejected_operation_kind = after_rejection_p2panda
        .last_failure
        .clone()
        .ok_or_else(|| "p2panda-net rejected import did not record a failure kind".to_string())?;
    if !rejected_operation_kind.contains("UntrustedAuthor") {
        return Err(format!(
            "p2panda-net rejected import recorded the wrong kind: {rejected_operation_kind}"
        ));
    }
    let report = P2pandaNetProcessServingReport {
        scenario: SCENARIO,
        remote_updates_imported: updated_p2panda.imported,
        rejected_operations: after_rejection_p2panda.rejected,
        rejected_operation_kind,
        stream_idle_refreshes_after_rejection: after_rejection_p2panda.stream_idle_refreshes,
        replayed_operations_skipped_after_rejection: after_rejection_p2panda
            .replayed_operations_skipped,
        local_mutation_failure,
        serving_process_alive_after_update,
        baseline_gateway_revision,
        updated_gateway_revision,
        restarted_gateway_revision,
        elapsed_ms: started.elapsed().as_millis(),
    };
    assert_eq_named("baseline imported count", baseline_p2panda.imported, 1)?;
    assert_eq_named("updated imported count", updated_p2panda.imported, 2)?;
    assert_eq_named(
        "rejected operation count",
        after_rejection_p2panda.rejected,
        1,
    )?;
    assert_eq_named(
        "rejected replay skipped count",
        after_rejection_p2panda.replayed_operations_skipped,
        0,
    )?;
    assert_eq_named(
        "p2panda-net serving alive after update",
        report.serving_process_alive_after_update,
        true,
    )?;
    let json = write_json(&root.join(format!("{SCENARIO}-metrics.json")), &report)?;
    println!("{json}");
    eprintln!("PASS {SCENARIO}");
    Ok(())
}

async fn wait_for_p2panda_net_status(
    socket: &std::path::Path,
    predicate: impl Fn(&RoleStatus) -> bool,
) -> Result<RoleStatus, String> {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        let status = request_status(socket).await?;
        if predicate(&status) {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "p2panda-net status did not reach target: {status:?}"
            ));
        }
        sleep(Duration::from_millis(25)).await;
    }
}

fn p2panda_status(status: &RoleStatus) -> Result<&P2pandaNetRoleStatus, String> {
    status
        .p2panda_net()
        .ok_or_else(|| "status missing p2panda-net details".to_string())
}

fn p2panda_reloaded_revision(status: &RoleStatus, revision: &str) -> bool {
    status
        .p2panda_net()
        .and_then(|p2panda| p2panda.last_reload.as_ref())
        .and_then(|serving| serving_role_gateway_revision(serving).ok())
        .is_some_and(|gateway_revision| gateway_revision.contains(revision))
}

fn repeated_hex(byte: &str) -> String {
    byte.repeat(32)
}

fn p2panda_net_receiver_args(
    store_path: &Path,
    network: &str,
    topic: &str,
    receiver_seed: &str,
    trusted_authors: &[String],
) -> Vec<String> {
    let mut args = vec![
        "--p2panda-path".to_string(),
        store_path.display().to_string(),
        "--p2panda-network".to_string(),
        network.to_string(),
        "--p2panda-topic".to_string(),
        topic.to_string(),
        "--p2panda-seed".to_string(),
        receiver_seed.to_string(),
        "--p2panda-replica".to_string(),
        "p2panda-net-serving-replica".to_string(),
    ];
    for trusted_author in trusted_authors {
        args.push("--p2panda-trusted-author".to_string());
        args.push(trusted_author.clone());
    }
    args
}
