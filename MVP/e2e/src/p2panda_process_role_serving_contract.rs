use std::fs;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use mvp_bus::{BusSession, FactKey, Grant, IslandId, PrincipalId, harness::InMemoryBus};
use mvp_p2panda_facts::{
    PandaFactAuthor, PandaFactStore, PandaSqliteOpenConfig, PandaTrustedAuthorKey,
};
use serde::Serialize;

use crate::assertions::assert_eq_named;
use crate::bus_syntax::fact_pattern;
use crate::metrics::{reset_dir, scenario_dir, write_json};
use crate::process_role_harness::{
    RoleRequest, ServingCommitInput, assert_serving_role_answer,
    cleanup_orphaned_children as cleanup_process_role_children, request_await_rebuild,
    request_begin_rebuild, request_project_once, request_reload, request_role,
    serving_commit_payload_for_input, serving_role_gateway_revision, spawn_process_role,
    wait_for_serving_role,
};

#[derive(Debug, Serialize)]
struct P2pandaProcessRoleServingReport {
    scenario: &'static str,
    serving_process_alive_after_update: bool,
    rebuild_query_probes: usize,
    baseline_gateway_revision: String,
    updated_gateway_revision: String,
    elapsed_ms: u128,
}

pub(crate) fn run() -> Result<(), String> {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| {
            format!("create tokio runtime for p2panda process-role serving: {error}")
        })?;
    runtime.block_on(run_async())
}

pub(crate) fn cleanup_orphaned_children() -> Result<(), String> {
    cleanup_process_role_children(&scenario_dir("p2panda-process-role-serving-contract"))
}

async fn run_async() -> Result<(), String> {
    let started = Instant::now();
    let root = scenario_dir("p2panda-process-role-serving-contract");
    reset_dir(&root)?;

    let island = IslandId::new("prod");
    let writer = PandaFactAuthor::new(PrincipalId::new("local-coordinator"));
    let store_path = root.join("p2panda-facts.sqlite");

    write_serving_commit(
        &store_path,
        &island,
        &writer,
        "serving-1",
        "fd00::1:8080",
        "fd00::1",
        1,
    )
    .await?;

    let serving_socket = root.join("serving.sock");
    let p2panda_path = store_path.display().to_string();
    let author_key = writer.author_key().as_hex();
    let args = [
        "--fact-source".to_string(),
        "p2panda-sqlite".to_string(),
        "--p2panda-path".to_string(),
        p2panda_path,
        "--p2panda-author".to_string(),
        writer.principal().as_str().to_string(),
        "--p2panda-author-key".to_string(),
        author_key,
    ];
    let arg_refs = args.iter().map(String::as_str).collect::<Vec<_>>();
    let mut serving = spawn_process_role(
        "p2panda-serving-projection",
        "serving-projection",
        &root,
        &serving_socket,
        &arg_refs,
    )?;
    wait_for_serving_role(&serving_socket).await?;

    request_project_once(&serving_socket).await?;
    let baseline_status = request_reload(&serving_socket).await?;
    assert_serving_role_answer(
        &serving_socket,
        "fd00::1:8080",
        "fd00::1",
        "p2panda process role",
    )
    .await?;
    let baseline_gateway_revision = serving_role_gateway_revision(&baseline_status)?;

    write_serving_commit(
        &store_path,
        &island,
        &writer,
        "serving-2",
        "fd00::2:8080",
        "fd00::2",
        2,
    )
    .await?;
    let serving_process_alive_after_update = serving.is_running()?;

    fs::remove_file(root.join("projections.sqlite")).map_err(|error| {
        format!("delete p2panda process-role projection sqlite during rebuild: {error}")
    })?;
    let token = request_begin_rebuild(&serving_socket).await?;
    assert_serving_role_answer(
        &serving_socket,
        "fd00::1:8080",
        "fd00::1",
        "p2panda process role",
    )
    .await?;
    request_await_rebuild(&serving_socket, token).await?;
    let updated_status = request_reload(&serving_socket).await?;
    assert_serving_role_answer(
        &serving_socket,
        "fd00::2:8080",
        "fd00::2",
        "p2panda process role",
    )
    .await?;
    let updated_gateway_revision = serving_role_gateway_revision(&updated_status)?;

    request_role(&serving_socket, &RoleRequest::Shutdown)
        .await
        .map_err(|error| format!("shutdown p2panda serving process: {error:?}"))?;
    serving.wait_for_exit().await?;

    let report = P2pandaProcessRoleServingReport {
        scenario: "p2panda-process-role-serving-contract",
        serving_process_alive_after_update,
        rebuild_query_probes: 1,
        baseline_gateway_revision,
        updated_gateway_revision,
        elapsed_ms: started.elapsed().as_millis(),
    };
    assert_eq_named(
        "p2panda serving alive after update",
        report.serving_process_alive_after_update,
        true,
    )?;
    let json = write_json(
        &root.join("p2panda-process-role-serving-contract-metrics.json"),
        &report,
    )?;
    println!("{json}");
    eprintln!("PASS p2panda-process-role-serving-contract");
    Ok(())
}

async fn write_serving_commit(
    store_path: &Path,
    island: &IslandId,
    author: &PandaFactAuthor,
    commit_id: &str,
    backend: &str,
    dns: &str,
    epoch: u64,
) -> Result<(), String> {
    let (bus, session) = p2panda_writer_bus(island, author.principal());
    let mut store = PandaFactStore::open_sqlite(
        Arc::new(bus),
        PandaSqliteOpenConfig::new(store_path, vec![island.clone()]).with_trusted_author_key(
            PandaTrustedAuthorKey::new(
                island.clone(),
                author.principal().clone(),
                author.author_key(),
            ),
        ),
    )
    .await
    .map_err(|error| format!("open p2panda serving writer store: {error}"))?;
    let input = ServingCommitInput::new(commit_id, backend, dns, epoch);
    let payload = serving_commit_payload_for_input(&input)?;
    let key = FactKey::parse(format!("/facts/serving/{commit_id}"))
        .map_err(|error| format!("parse p2panda serving fact key: {error}"))?;
    store
        .write_fact_payload(&session, author, key, payload.into())
        .await
        .map_err(|error| format!("write p2panda serving commit: {error}"))?;
    Ok(())
}

fn p2panda_writer_bus(island: &IslandId, principal: &PrincipalId) -> (InMemoryBus, BusSession) {
    let (bus, authority) = InMemoryBus::new_with_authority();
    let session = authority.grant_in(
        island.clone(),
        principal.clone(),
        Grant::empty()
            .with_fact_write(fact_pattern("/facts/serving/>").expect("fact pattern parses")),
    );
    authority.grant_in(
        island.clone(),
        PrincipalId::new("projection"),
        Grant::empty().with_fact_read(fact_pattern("/facts/>").expect("fact pattern parses")),
    );
    (bus, session)
}
