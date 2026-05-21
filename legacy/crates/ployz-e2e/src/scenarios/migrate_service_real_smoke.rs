use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};
use crate::support::parse_daemon_json_response;
use serde_json::Value;

use super::zfs_support::{
    assert_real_zfs_dataset, deploy_volume_manifest, wait_for_container_bind,
    wait_for_no_service_container, wait_for_volume_value, zfs_context,
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
            "migrate service smoke requires real zfs, got founder={} peer={}",
            source_zfs.mode, target_zfs.mode
        )));
    }

    run.log_progress("deploy managed volume before migrate");
    deploy_volume_manifest(run, "founder", "v1")?;
    wait_for_volume_value(run, "founder", &source_zfs.volume_source(), "v1")?;
    wait_for_container_bind(run, "founder", &source_zfs.volume_source())?;
    assert_real_zfs_dataset(run, "founder", &source_zfs)?;

    run.log_progress("mutate source volume before migrate");
    run.ssh_expect_ok_name(
        "founder",
        &format!("printf 'v2\\n' >{}/value", source_zfs.volume_source()),
    )?;
    wait_for_volume_value(run, "founder", &source_zfs.volume_source(), "v2")?;

    run.log_progress("migrate service to peer");
    let output = run.ssh_expect_ok_name(
        "founder",
        "ployzd --json migrate apply default/db --to peer",
    )?;
    assert_migrate_apply_moved_volume(&output.stdout)?;

    run.log_progress("verify service and volume moved to peer");
    wait_for_volume_value(run, "peer", &target_zfs.volume_source(), "v2")?;
    wait_for_container_bind(run, "peer", &target_zfs.volume_source())?;
    assert_real_zfs_dataset(run, "peer", &target_zfs)?;
    wait_for_no_service_container(run, "founder")?;

    run.log_progress("verify migrate command is idempotence-hostile");
    let output = run.ssh_run_name(
        "founder",
        "ployzd --json migrate apply default/db --to peer",
    )?;
    if output.status.success() {
        return Err(Error::Message(
            "second migrate to the current owner unexpectedly succeeded".into(),
        ));
    }
    let response = parse_daemon_json_response(&output.stdout)?;
    if response.code != "MIGRATE_RENDER_FAILED" {
        return Err(Error::Message(format!(
            "second migrate failed with unexpected code '{}': {}",
            response.code, response.message
        )));
    }
    if !response
        .message
        .contains("volume 'data' is already on target machine 'peer'")
    {
        return Err(Error::Message(format!(
            "second migrate failed for the wrong precondition: {}",
            response.message
        )));
    }
    wait_for_volume_value(run, "peer", &target_zfs.volume_source(), "v2")?;
    wait_for_container_bind(run, "peer", &target_zfs.volume_source())?;

    Ok(())
}

fn assert_migrate_apply_moved_volume(output: &str) -> Result<()> {
    let response = parse_daemon_json_response(output)?;
    if !response.ok {
        return Err(Error::Message(format!(
            "migrate apply failed [{}]: {}",
            response.code, response.message
        )));
    }
    let apply: Value = serde_json::from_str(&response.message).map_err(|error| {
        Error::Message(format!(
            "failed to parse migrate apply result message: {error}"
        ))
    })?;
    let Some(volume_moves) = apply
        .get("preview")
        .and_then(|preview| preview.get("volume_moves"))
        .and_then(Value::as_array)
    else {
        return Err(Error::Message(format!(
            "migrate apply response did not include preview.volume_moves: {}",
            response.message
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
        "migrate apply response did not record default/data moving founder -> peer: {}",
        response.message
    )))
}
