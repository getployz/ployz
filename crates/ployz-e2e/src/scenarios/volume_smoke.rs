use crate::error::Result;
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};

use super::zfs_support::{
    assert_zfs_mode_state, deploy_volume_manifest, wait_for_container_bind, wait_for_volume_value,
    zfs_context,
};

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

    run.log_progress("verify zfs configured");
    let zfs = zfs_context(run, "founder")?;

    run.log_progress("deploy managed volume manifest v1");
    deploy_volume_manifest(run, "founder", "v1")?;
    wait_for_volume_value(run, "founder", &zfs.volume_source(), "v1")?;
    wait_for_container_bind(run, "founder", &zfs.volume_source())?;
    assert_zfs_mode_state(run, "founder", &zfs)?;

    run.log_progress("deploy managed volume manifest v2");
    deploy_volume_manifest(run, "founder", "v2")?;
    wait_for_volume_value(run, "founder", &zfs.volume_source(), "v2")?;
    wait_for_container_bind(run, "founder", &zfs.volume_source())?;
    assert_zfs_mode_state(run, "founder", &zfs)?;

    Ok(())
}
