use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;

    run.machine_add_many("founder", &["peer1", "peer2"])?;
    let three_active = [
        MachineExpectation {
            id: "founder",
            lifecycle: "active",
            subnet: SubnetExpectation::Present,
        },
        MachineExpectation {
            id: "peer1",
            lifecycle: "active",
            subnet: SubnetExpectation::Present,
        },
        MachineExpectation {
            id: "peer2",
            lifecycle: "active",
            subnet: SubnetExpectation::Present,
        },
    ];
    run.wait_machine_rows("founder", &three_active)?;
    run.wait_machine_rows("peer1", &three_active)?;
    run.wait_mesh_ready_name("peer1")?;
    run.wait_mesh_ready_name("peer2")?;

    run.partition_groups(&["founder", "peer1"], &["peer2"])?;

    let destroy = run.ssh_run_name("founder", "ployzd mesh destroy alpha")?;
    if destroy.status.success() {
        return Err(Error::Message(
            "mesh destroy should fail while peer2 is registered but unreachable".into(),
        ));
    }
    let combined = destroy.combined();
    if !combined.contains("peer2") || !combined.contains("confirm teardown") {
        return Err(Error::Message(format!(
            "mesh destroy failure did not identify unreachable peer2: {combined}"
        )));
    }

    run.wait_mesh_ready_name("founder")?;
    run.wait_mesh_ready_name("peer1")?;

    run.machine_rm("founder", "peer2", true)?;
    let remaining = [
        MachineExpectation {
            id: "founder",
            lifecycle: "active",
            subnet: SubnetExpectation::Present,
        },
        MachineExpectation {
            id: "peer1",
            lifecycle: "active",
            subnet: SubnetExpectation::Present,
        },
    ];
    run.wait_machine_rows("founder", &remaining)?;
    run.wait_machine_rows("peer1", &remaining)?;

    run.mesh_destroy("founder", "alpha")?;
    run.wait_mesh_absent_name("founder")?;
    run.wait_mesh_absent_name("peer1")?;
    run.ssh_expect_ok_name("founder", "test ! -d /var/lib/ployz/networks/alpha")?;
    run.ssh_expect_ok_name("peer1", "test ! -d /var/lib/ployz/networks/alpha")?;
    Ok(())
}
