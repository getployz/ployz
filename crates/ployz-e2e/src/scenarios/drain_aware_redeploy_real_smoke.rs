use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};
use crate::support::parse_daemon_json_response;
use serde_json::Value;

use super::zfs_support::{
    assert_real_zfs_dataset, deploy_volume_manifest, wait_for_container_bind,
    wait_for_no_service_container, wait_for_volume_value, write_volume_manifest, zfs_context,
};

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;
    run.machine_add("founder", "peer")?;
    run.wait_machine_rows(
        "founder",
        &[
            MachineExpectation {
                id: "founder",
                lifecycle: "active",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "peer",
                lifecycle: "active",
                subnet: SubnetExpectation::Present,
            },
        ],
    )?;
    run.wait_mesh_ready_name("peer")?;

    run.log_progress("verify real zfs configured");
    let source_zfs = zfs_context(run, "founder")?;
    let target_zfs = zfs_context(run, "peer")?;
    if source_zfs.mode != "real" || target_zfs.mode != "real" {
        return Err(Error::Message(format!(
            "drain-aware redeploy smoke requires real zfs, got founder={} peer={}",
            source_zfs.mode, target_zfs.mode
        )));
    }

    run.log_progress("deploy managed volume before drain");
    deploy_volume_manifest(run, "founder", "v1")?;
    wait_for_volume_value(run, "founder", &source_zfs.volume_source(), "v1")?;
    wait_for_container_bind(run, "founder", &source_zfs.volume_source())?;
    assert_real_zfs_dataset(run, "founder", &source_zfs)?;

    run.log_progress("mutate source volume before drain-aware redeploy");
    run.ssh_expect_ok_name(
        "founder",
        &format!("printf 'v2\\n' >{}/value", source_zfs.volume_source()),
    )?;
    wait_for_volume_value(run, "founder", &source_zfs.volume_source(), "v2")?;

    run.log_progress("mark founder draining");
    run.ssh_expect_ok_name("founder", "ployzd machine drain founder")?;
    run.wait_machine_rows(
        "founder",
        &[
            MachineExpectation {
                id: "founder",
                lifecycle: "draining",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "peer",
                lifecycle: "active",
                subnet: SubnetExpectation::Present,
            },
        ],
    )?;

    run.log_progress("preview drain-aware redeploy");
    write_volume_manifest(run, "founder", "v1")?;
    let output = run.ssh_expect_ok_name(
        "founder",
        "ployzd --json deploy preview -f /tmp/ployz-volume-smoke.json",
    )?;
    assert_response_moved_volume(&output.stdout, "deploy preview")?;

    run.log_progress("apply drain-aware redeploy");
    let output = run.ssh_expect_ok_name(
        "founder",
        "ployzd --json deploy -f /tmp/ployz-volume-smoke.json",
    )?;
    assert_apply_response_moved_volume(&output.stdout)?;

    run.log_progress("verify service and volume moved to peer");
    wait_for_volume_value(run, "peer", &target_zfs.volume_source(), "v2")?;
    wait_for_container_bind(run, "peer", &target_zfs.volume_source())?;
    assert_real_zfs_dataset(run, "peer", &target_zfs)?;
    wait_for_no_service_container(run, "founder")?;

    run.log_progress("reapply after drain-aware move");
    let output = run.ssh_expect_ok_name(
        "founder",
        "ployzd --json deploy -f /tmp/ployz-volume-smoke.json",
    )?;
    assert_apply_response_without_volume_move(&output.stdout)?;
    wait_for_volume_value(run, "peer", &target_zfs.volume_source(), "v2")?;
    wait_for_container_bind(run, "peer", &target_zfs.volume_source())?;
    wait_for_no_service_container(run, "founder")?;

    Ok(())
}

fn assert_apply_response_moved_volume(output: &str) -> Result<()> {
    let response = parse_daemon_json_response(output)?;
    if !response.ok {
        return Err(Error::Message(format!(
            "deploy apply failed [{}]: {}",
            response.code, response.message
        )));
    }
    let apply: Value = serde_json::from_str(&response.message).map_err(|error| {
        Error::Message(format!(
            "failed to parse deploy apply result message: {error}"
        ))
    })?;
    let Some(preview) = apply.get("preview") else {
        return Err(Error::Message(format!(
            "deploy apply response did not include preview: {}",
            response.message
        )));
    };
    assert_volume_move(preview, "deploy apply")
}

fn assert_response_moved_volume(output: &str, context: &str) -> Result<()> {
    let response = parse_daemon_json_response(output)?;
    if !response.ok {
        return Err(Error::Message(format!(
            "{context} failed [{}]: {}",
            response.code, response.message
        )));
    }
    let preview: Value = serde_json::from_str(&response.message)
        .map_err(|error| Error::Message(format!("failed to parse {context} message: {error}")))?;
    assert_volume_move(&preview, context)
}

fn assert_apply_response_without_volume_move(output: &str) -> Result<()> {
    let response = parse_daemon_json_response(output)?;
    if !response.ok {
        return Err(Error::Message(format!(
            "post-move deploy apply failed [{}]: {}",
            response.code, response.message
        )));
    }
    let apply: Value = serde_json::from_str(&response.message).map_err(|error| {
        Error::Message(format!(
            "failed to parse post-move deploy apply result message: {error}"
        ))
    })?;
    let Some(volume_moves) = apply
        .get("preview")
        .and_then(|preview| preview.get("volume_moves"))
        .and_then(Value::as_array)
    else {
        return Err(Error::Message(format!(
            "post-move deploy apply response did not include preview.volume_moves: {}",
            response.message
        )));
    };
    if volume_moves.is_empty() {
        return Ok(());
    }
    Err(Error::Message(format!(
        "post-move redeploy unexpectedly planned volume moves: {}",
        response.message
    )))
}

fn assert_volume_move(preview: &Value, context: &str) -> Result<()> {
    let Some(volume_moves) = preview.get("volume_moves").and_then(Value::as_array) else {
        return Err(Error::Message(format!(
            "{context} did not include preview.volume_moves: {preview}"
        )));
    };
    if volume_moves.iter().any(|move_plan| {
        move_plan.get("volume").and_then(Value::as_str) == Some("data")
            && move_plan.get("from_machine").and_then(Value::as_str) == Some("founder")
            && move_plan.get("to_machine").and_then(Value::as_str) == Some("peer")
    }) {
        return Ok(());
    }
    Err(Error::Message(format!(
        "{context} did not record default/data moving founder -> peer: {preview}"
    )))
}
