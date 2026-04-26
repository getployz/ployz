use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};
use crate::support::{
    DaemonJsonPayload, VolumeZfsTransferInfo, parse_daemon_json_response, wait_until,
};
use std::thread;
use std::time::{Duration, Instant};

use super::zfs_support::{
    assert_real_zfs_dataset, deploy_volume_manifest, wait_for_container_bind,
    wait_for_volume_value, zfs_context,
};

const TRANSFER_WAIT_TIMEOUT: Duration = Duration::from_secs(240);
const TRANSFER_POLL: Duration = Duration::from_millis(100);
const SNAPSHOT_PRESEED: &str = "measure-preseed";
const SNAPSHOT_WARM: &str = "measure-warm";
const SNAPSHOT_FINAL: &str = "measure-final";

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

    let source_zfs = zfs_context(run, "founder")?;
    let target_zfs = zfs_context(run, "peer")?;
    if source_zfs.mode != "real" || target_zfs.mode != "real" {
        return Err(Error::Message(format!(
            "zfs cutover measurement requires real mode, got founder={} peer={}",
            source_zfs.mode, target_zfs.mode
        )));
    }

    run.log_progress("deploy managed volume before cutover measurement");
    deploy_volume_manifest(run, "founder", "preseed")?;
    wait_for_volume_value(run, "founder", &source_zfs.volume_source(), "preseed")?;
    wait_for_container_bind(run, "founder", &source_zfs.volume_source())?;
    assert_real_zfs_dataset(run, "founder", &source_zfs)?;

    let preseed = timed_transfer(run, SNAPSHOT_PRESEED, None)?;
    print_measure("full_send", preseed.elapsed, preseed.bytes_transferred);
    assert_target_value(run, &target_zfs, SNAPSHOT_PRESEED, "preseed")?;

    run.log_progress("write warm dirty set");
    write_source_value_and_dirty_file(run, &source_zfs.volume_source(), "warm", 16)?;
    let warm = timed_transfer(run, SNAPSHOT_WARM, Some(SNAPSHOT_PRESEED))?;
    print_measure(
        "warm_incremental_send",
        warm.elapsed,
        warm.bytes_transferred,
    );
    assert_target_value(run, &target_zfs, SNAPSHOT_WARM, "warm")?;

    run.log_progress("write final dirty set");
    write_source_value_and_dirty_file(run, &source_zfs.volume_source(), "final", 1)?;

    run.log_progress("measure final stop-to-target-read cutover");
    let downtime_start = Instant::now();
    stop_source_workload(run)?;
    let final_transfer = timed_transfer(run, SNAPSHOT_FINAL, Some(SNAPSHOT_WARM))?;
    assert_target_value(run, &target_zfs, SNAPSHOT_FINAL, "final")?;
    let target_start = time_target_container_read(run, &target_zfs.volume_source(), "final")?;
    let downtime = downtime_start.elapsed();

    print_measure(
        "final_incremental_send",
        final_transfer.elapsed,
        final_transfer.bytes_transferred,
    );
    print_measure("target_container_read", target_start, None);
    print_measure("stop_to_target_container_read", downtime, None);

    Ok(())
}

#[derive(Debug)]
struct TimedTransfer {
    elapsed: Duration,
    bytes_transferred: Option<u64>,
}

fn timed_transfer(
    run: &ScenarioRun,
    snapshot: &str,
    from_snapshot: Option<&str>,
) -> Result<TimedTransfer> {
    let started = Instant::now();
    let transfer = start_transfer(run, snapshot, from_snapshot)?;
    let completed = wait_for_transfer_success(run, &transfer.id)?;
    Ok(TimedTransfer {
        elapsed: started.elapsed(),
        bytes_transferred: completed.bytes_transferred,
    })
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
    let deadline = Instant::now() + TRANSFER_WAIT_TIMEOUT;
    loop {
        let output = run.ssh_run_name(
            "founder",
            &format!("ployzd --json volume zfs transfer get {transfer_id}"),
        )?;
        if output.status.success() {
            let transfer = transfer_from_output(&output.stdout)?;
            match transfer.status.as_str() {
                "succeeded" if transfer.stage == "complete" => return Ok(transfer),
                "succeeded" => {
                    return Err(Error::Message(format!(
                        "zfs transfer {transfer_id} succeeded at unexpected stage '{}'",
                        transfer.stage
                    )));
                }
                "failed" | "interrupted" => {
                    return Err(Error::Message(format!(
                        "zfs transfer {transfer_id} ended with status={} stage={} last_error={}",
                        transfer.status,
                        transfer.stage,
                        transfer.last_error.as_deref().unwrap_or("none")
                    )));
                }
                "running" => {}
                other => {
                    return Err(Error::Message(format!(
                        "zfs transfer {transfer_id} reported unexpected status '{other}'"
                    )));
                }
            }
        }
        if Instant::now() >= deadline {
            return Err(Error::Message(format!(
                "timed out waiting for zfs transfer {transfer_id}"
            )));
        }
        thread::sleep(TRANSFER_POLL);
    }
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

fn write_source_value_and_dirty_file(
    run: &ScenarioRun,
    volume_source: &str,
    value: &str,
    mib: u32,
) -> Result<()> {
    run.ssh_expect_ok_name(
        "founder",
        &format!(
            "printf '{value}\\n' >{volume_source}/value; \
             dd if=/dev/zero of={volume_source}/dirty.bin bs=1M count={mib} conv=fsync status=none"
        ),
    )?;
    wait_for_volume_value(run, "founder", volume_source, value)
}

fn stop_source_workload(run: &ScenarioRun) -> Result<()> {
    run.ssh_expect_ok_name(
        "founder",
        "docker rm -f $(docker ps -q --filter label=dev.ployz.namespace=default --filter label=dev.ployz.service=db) >/dev/null",
    )?;
    Ok(())
}

fn assert_target_value(
    run: &ScenarioRun,
    target_zfs: &super::zfs_support::ZfsContext,
    snapshot: &str,
    value: &str,
) -> Result<()> {
    let target_source = target_zfs.volume_source();
    let target_dataset = target_zfs.volume_dataset();
    let command = format!(
        "test \"$(cat {target_source}/value 2>/dev/null)\" = \"{value}\"; \
         zfs list -H -t snapshot {target_dataset}@{snapshot} >/dev/null"
    );
    wait_until(TRANSFER_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name("peer", &command)?;
        Ok(output.status.success())
    })
}

fn time_target_container_read(
    run: &ScenarioRun,
    target_source: &str,
    expected: &str,
) -> Result<Duration> {
    let started = Instant::now();
    run.ssh_expect_ok_name(
        "peer",
        &format!(
            "docker run --rm -v {target_source}:/data:ro ployz-e2e-preload/http-smoke:latest \
             sh -c 'test \"$(cat /data/value)\" = \"{expected}\"'"
        ),
    )?;
    Ok(started.elapsed())
}

fn print_measure(name: &str, duration: Duration, bytes: Option<u64>) {
    match bytes {
        Some(bytes) => eprintln!("MEASURE {name} ms={} bytes={bytes}", duration.as_millis()),
        None => eprintln!("MEASURE {name} ms={}", duration.as_millis()),
    }
}
