use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};
use crate::support::{
    DaemonJsonPayload, VolumeZfsTransferInfo, parse_daemon_json_response, wait_until,
};
use std::time::Duration;

use super::zfs_support::{
    assert_real_zfs_dataset, deploy_volume_manifest, wait_for_container_bind,
    wait_for_volume_value, zfs_context,
};

const TRANSFER_WAIT_TIMEOUT: Duration = Duration::from_secs(240);
const SNAPSHOT_V1: &str = "smoke-v1";
const SNAPSHOT_V2: &str = "smoke-v2";

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
            "zfs transfer smoke requires real mode, got founder={} peer={}",
            source_zfs.mode, target_zfs.mode
        )));
    }

    run.log_progress("deploy managed volume before transfer");
    deploy_volume_manifest(run, "founder", "v1")?;
    wait_for_volume_value(run, "founder", &source_zfs.volume_source(), "v1")?;
    wait_for_container_bind(run, "founder", &source_zfs.volume_source())?;
    assert_real_zfs_dataset(run, "founder", &source_zfs)?;

    run.log_progress("start full zfs transfer");
    let full = start_transfer(run, SNAPSHOT_V1, None)?;
    wait_for_transfer_success(run, &full.id)?;
    assert_snapshot_replicated(run, &source_zfs, &target_zfs, SNAPSHOT_V1, "v1")?;

    run.log_progress("mutate source volume for incremental transfer");
    run.ssh_expect_ok_name(
        "founder",
        &format!("printf 'v2\\n' >{}/value", source_zfs.volume_source()),
    )?;
    wait_for_volume_value(run, "founder", &source_zfs.volume_source(), "v2")?;

    run.log_progress("start incremental zfs transfer");
    let incremental = start_transfer(run, SNAPSHOT_V2, Some(SNAPSHOT_V1))?;
    let completed = wait_for_transfer_success(run, &incremental.id)?;
    assert_snapshot_replicated(run, &source_zfs, &target_zfs, SNAPSHOT_V2, "v2")?;
    if completed.bytes_transferred.unwrap_or(0) == 0 {
        return Err(Error::Message(format!(
            "incremental transfer '{}' reported no bytes transferred",
            completed.id
        )));
    }

    Ok(())
}

fn start_transfer(
    run: &ScenarioRun,
    snapshot: &str,
    from_snapshot: Option<&str>,
) -> Result<VolumeZfsTransferInfo> {
    let from = from_snapshot
        .map(|name| format!(" --from-snapshot {name}"))
        .unwrap_or_default();
    let output = run.ssh_expect_ok_name(
        "founder",
        &format!(
            "ployzd --json volume zfs send default/data --snapshot {snapshot}{from} --to peer"
        ),
    )?;
    transfer_from_output(&output.stdout)
}

fn wait_for_transfer_success(
    run: &ScenarioRun,
    transfer_id: &str,
) -> Result<VolumeZfsTransferInfo> {
    let mut last: Option<VolumeZfsTransferInfo> = None;
    wait_until(TRANSFER_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(
            "founder",
            &format!("ployzd --json volume zfs transfer get {transfer_id}"),
        )?;
        if !output.status.success() {
            return Ok(false);
        }
        let transfer = transfer_from_output(&output.stdout)?;
        match transfer.status.as_str() {
            "succeeded" => {
                last = Some(transfer);
                Ok(true)
            }
            "failed" | "interrupted" => Err(Error::Message(format!(
                "zfs transfer {transfer_id} ended with status={} stage={} last_error={}",
                transfer.status,
                transfer.stage,
                transfer.last_error.as_deref().unwrap_or("none")
            ))),
            "running" => {
                last = Some(transfer);
                Ok(false)
            }
            other => Err(Error::Message(format!(
                "zfs transfer {transfer_id} reported unexpected status '{other}'"
            ))),
        }
    })?;
    let Some(transfer) = last else {
        return Err(Error::Message(format!(
            "zfs transfer {transfer_id} completed without a final record"
        )));
    };
    if transfer.stage != "complete" {
        return Err(Error::Message(format!(
            "zfs transfer {transfer_id} succeeded at unexpected stage '{}'",
            transfer.stage
        )));
    }
    Ok(transfer)
}

fn transfer_from_output(output: &str) -> Result<VolumeZfsTransferInfo> {
    let response = parse_daemon_json_response(output)?;
    let Some(DaemonJsonPayload::VolumeZfsTransfer(payload)) = response.payload else {
        return Err(Error::Message(format!(
            "expected volume zfs transfer payload, got: {output}"
        )));
    };
    Ok(payload.transfer)
}

fn assert_snapshot_replicated(
    run: &ScenarioRun,
    source_zfs: &super::zfs_support::ZfsContext,
    target_zfs: &super::zfs_support::ZfsContext,
    snapshot: &str,
    value: &str,
) -> Result<()> {
    let source_dataset = source_zfs.volume_dataset();
    let target_dataset = target_zfs.volume_dataset();
    let target_source = target_zfs.volume_source();
    let source_guid_command = format!("zfs get -H -o value guid {source_dataset}@{snapshot}");
    let target_command = format!(
        "test \"$(cat {target_source}/value 2>/dev/null)\" = \"{value}\"; \
         zfs get -H -o value guid {target_dataset}@{snapshot}"
    );
    wait_until(TRANSFER_WAIT_TIMEOUT, || {
        let source = run.ssh_run_name("founder", &source_guid_command)?;
        if !source.status.success() {
            return Ok(false);
        }
        let target = run.ssh_run_name("peer", &target_command)?;
        if !target.status.success() {
            return Ok(false);
        }
        Ok(source.stdout.trim() == target.stdout.trim())
    })
    .map_err(|error| {
        Error::Message(format!(
            "snapshot {snapshot} did not replicate value '{value}' to peer: {error}"
        ))
    })
}
