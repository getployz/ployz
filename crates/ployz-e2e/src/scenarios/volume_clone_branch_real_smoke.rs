use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};
use crate::support::{DaemonJsonPayload, parse_daemon_json_response};
use serde_json::Value;

use super::zfs_support::{
    assert_real_zfs_dataset, assert_real_zfs_dataset_in_namespace, deploy_volume_manifest,
    wait_for_container_bind, wait_for_container_bind_in_namespace, wait_for_volume_value,
    wait_for_volume_value_in_namespace, write_volume_manifest, zfs_context,
};

const BRANCH_NAMESPACE: &str = "pr-39";

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;
    run.wait_machine_rows(
        "founder",
        &[MachineExpectation {
            id: "founder",
            lifecycle: "active",
            subnet: SubnetExpectation::Present,
        }],
    )?;

    run.log_progress("verify real zfs configured");
    let zfs = zfs_context(run, "founder")?;
    if zfs.mode != "real" {
        return Err(Error::Message(format!(
            "volume clone branch smoke requires real zfs, got founder={}",
            zfs.mode
        )));
    }

    run.log_progress("deploy source namespace volume");
    deploy_volume_manifest(run, "founder", "v1")?;
    wait_for_volume_value(run, "founder", &zfs.volume_source(), "v1")?;
    wait_for_container_bind(run, "founder", &zfs.volume_source())?;
    assert_real_zfs_dataset(run, "founder", &zfs)?;

    run.log_progress("mutate source before branch clone");
    run.ssh_expect_ok_name(
        "founder",
        &format!("printf 'v2\\n' >{}/value", zfs.volume_source()),
    )?;
    wait_for_volume_value(run, "founder", &zfs.volume_source(), "v2")?;

    run.log_progress("preview branch command clone");
    let output = run.ssh_expect_ok_name(
        "founder",
        "ployzd --json branch preview default pr-39 --volume data=branch",
    )?;
    assert_volume_clone(&output.stdout, "deploy preview")?;

    run.log_progress("prepare branch command clone");
    let output = run.ssh_expect_ok_name(
        "founder",
        "ployzd --json branch prepare default pr-39 --volume data=branch",
    )?;
    let prepared_deploy_id =
        prepared_deploy_id_with_volume_clone(&output.stdout, "branch prepare")?;

    run.log_progress("apply prepared branch command clone");
    let output = run.ssh_expect_ok_name(
        "founder",
        &format!("ployzd --json branch apply-prepared {prepared_deploy_id}"),
    )?;
    assert_volume_clone(&output.stdout, "branch apply-prepared")?;

    run.log_progress("verify cloned namespace has source snapshot data");
    wait_for_volume_value_in_namespace(run, "founder", &zfs, BRANCH_NAMESPACE, "data", "v2")?;
    wait_for_container_bind_in_namespace(
        run,
        "founder",
        BRANCH_NAMESPACE,
        "db",
        &zfs.volume_source_for(BRANCH_NAMESPACE, "data"),
    )?;
    assert_real_zfs_dataset_in_namespace(run, "founder", &zfs, BRANCH_NAMESPACE, "data")?;

    run.log_progress("verify source and branch diverge after clone");
    run.ssh_expect_ok_name(
        "founder",
        &format!("printf 'v3\\n' >{}/value", zfs.volume_source()),
    )?;
    wait_for_volume_value(run, "founder", &zfs.volume_source(), "v3")?;
    wait_for_volume_value_in_namespace(run, "founder", &zfs, BRANCH_NAMESPACE, "data", "v2")?;

    run.log_progress("prepare branch command clone reapply");
    let output = run.ssh_expect_ok_name(
        "founder",
        "ployzd --json branch prepare default pr-39 --volume data=branch",
    )?;
    let prepared_deploy_id = extract_prepared_deploy_id(&output.stdout, "branch reapply prepare")?;

    run.log_progress("reapply prepared branch command clone");
    let output = run.ssh_expect_ok_name(
        "founder",
        &format!("ployzd --json branch apply-prepared {prepared_deploy_id}"),
    )?;
    assert_no_volume_clone(&output.stdout, "branch clone reapply")?;
    wait_for_volume_value_in_namespace(run, "founder", &zfs, BRANCH_NAMESPACE, "data", "v2")?;

    run.log_progress("prepare branch command before source drift");
    let output = run.ssh_expect_ok_name(
        "founder",
        "ployzd --json branch prepare default pr-drift --volume data=branch",
    )?;
    let prepared_deploy_id =
        prepared_deploy_id_with_volume_clone(&output.stdout, "source drift branch prepare")?;

    run.log_progress("mutate source service after branch prepare");
    write_volume_manifest(run, "founder", "v4")?;
    run.ssh_expect_ok_name("founder", "ployzd deploy -f /tmp/ployz-volume-smoke.json")?;

    run.log_progress("verify prepared branch apply rejects source drift");
    let output = run.ssh_run_name(
        "founder",
        &format!("ployzd --json branch apply-prepared {prepared_deploy_id}"),
    )?;
    if output.status.success() {
        return Err(Error::Message(
            "branch apply-prepared unexpectedly succeeded after source drift".into(),
        ));
    }
    assert_apply_prepared_rejects_source_drift(&output.stdout, "source drift apply-prepared")?;

    Ok(())
}

fn prepared_deploy_id_with_volume_clone(output: &str, context: &str) -> Result<String> {
    let response = parse_daemon_json_response(output)?;
    if !response.ok {
        return Err(Error::Message(format!(
            "{context} failed [{}]: {}",
            response.code, response.message
        )));
    }
    let Some(DaemonJsonPayload::DeployPrepare(payload)) = response.payload else {
        return Err(Error::Message(format!(
            "{context} did not include deploy prepare payload"
        )));
    };
    assert_volume_clone_preview(&payload.prepared.preview, context)?;
    Ok(payload.prepared.prepared_deploy_id)
}

fn extract_prepared_deploy_id(output: &str, context: &str) -> Result<String> {
    let response = parse_daemon_json_response(output)?;
    if !response.ok {
        return Err(Error::Message(format!(
            "{context} failed [{}]: {}",
            response.code, response.message
        )));
    }
    let Some(DaemonJsonPayload::DeployPrepare(payload)) = response.payload else {
        return Err(Error::Message(format!(
            "{context} did not include deploy prepare payload"
        )));
    };
    Ok(payload.prepared.prepared_deploy_id)
}

fn assert_volume_clone(output: &str, context: &str) -> Result<()> {
    let preview = deploy_preview(output, context)?;
    assert_volume_clone_preview(&preview, context)
}

fn assert_volume_clone_preview(preview: &Value, context: &str) -> Result<()> {
    assert_service_branch(preview, context)?;
    let Some(volume_clones) = preview.get("volume_clones").and_then(Value::as_array) else {
        return Err(Error::Message(format!(
            "{context} did not include preview.volume_clones: {preview}"
        )));
    };
    if volume_clones.iter().any(|clone_plan| {
        clone_plan.get("volume").and_then(Value::as_str) == Some("data")
            && clone_plan.get("source_namespace").and_then(Value::as_str) == Some("default")
            && clone_plan.get("source_volume").and_then(Value::as_str) == Some("data")
            && clone_plan.get("source_machine").and_then(Value::as_str) == Some("founder")
            && clone_plan.get("target_machine").and_then(Value::as_str) == Some("founder")
    }) {
        return Ok(());
    }
    Err(Error::Message(format!(
        "{context} did not plan default/data clone on founder: {preview}"
    )))
}

fn assert_service_branch(preview: &Value, context: &str) -> Result<()> {
    let Some(service_sources) = preview
        .get("service_branch_sources")
        .and_then(Value::as_array)
    else {
        return Err(Error::Message(format!(
            "{context} did not include preview.service_branch_sources: {preview}"
        )));
    };
    if service_sources.iter().any(|source| {
        source.get("service").and_then(Value::as_str) == Some("db")
            && source.get("source_namespace").and_then(Value::as_str) == Some("default")
            && source.get("source_service").and_then(Value::as_str) == Some("db")
            && source
                .get("source_revision_hash")
                .and_then(Value::as_str)
                .is_some_and(|hash| !hash.is_empty())
    }) {
        return Ok(());
    }
    Err(Error::Message(format!(
        "{context} did not plan default/db service branch: {preview}"
    )))
}

fn assert_no_volume_clone(output: &str, context: &str) -> Result<()> {
    let preview = deploy_preview(output, context)?;
    let Some(volume_clones) = preview.get("volume_clones").and_then(Value::as_array) else {
        return Ok(());
    };
    if volume_clones.is_empty() {
        return Ok(());
    }
    Err(Error::Message(format!(
        "{context} unexpectedly planned volume clones: {preview}"
    )))
}

fn assert_apply_prepared_rejects_source_drift(output: &str, context: &str) -> Result<()> {
    let response = parse_daemon_json_response(output)?;
    if response.ok {
        return Err(Error::Message(format!(
            "{context} unexpectedly succeeded: {}",
            response.message
        )));
    }
    match response.code.as_str() {
        "BRANCH_SOURCE_REVISION_CHANGED" | "DEPLOY_BASELINE_CHANGED" => Ok(()),
        code => Err(Error::Message(format!(
            "{context} failed with unexpected code '{code}': {}",
            response.message
        ))),
    }
}

fn deploy_preview(output: &str, context: &str) -> Result<Value> {
    let response = parse_daemon_json_response(output)?;
    if !response.ok {
        return Err(Error::Message(format!(
            "{context} failed [{}]: {}",
            response.code, response.message
        )));
    }
    let body: Value = serde_json::from_str(&response.message)
        .map_err(|error| Error::Message(format!("failed to parse {context} message: {error}")))?;
    Ok(body.get("preview").cloned().unwrap_or(body))
}
