use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;
    run.machine_add("founder", "peer")?;

    let active = [
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
    ];
    run.wait_machine_rows("founder", &active)?;
    run.wait_machine_rows("peer", &active)?;
    run.wait_mesh_ready_name("peer")?;

    run.partition_groups(&["founder"], &["peer"])?;

    let remove = run.ssh_run_name("founder", "ployzd machine rm peer")?;
    if remove.status.success() {
        return Err(Error::Message(
            "machine rm should fail when the target cannot answer its NATS command".into(),
        ));
    }
    let combined = remove.combined();
    if !combined.contains("MACHINE_REMOVE_PEER_UNREACHABLE")
        || !combined.contains("peer")
        || !combined.contains("--force")
    {
        return Err(Error::Message(format!(
            "machine rm failure did not name the unreachable peer and explicit recovery: {combined}"
        )));
    }

    run.wait_machine_rows("founder", &active)?;
    run.machine_rm("founder", "peer", true)?;
    run.wait_machine_rows(
        "founder",
        &[MachineExpectation {
            id: "founder",
            lifecycle: "active",
            subnet: SubnetExpectation::Present,
        }],
    )?;
    Ok(())
}
