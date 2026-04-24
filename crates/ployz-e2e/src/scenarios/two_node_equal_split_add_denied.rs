use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    let founder_side = ["founder", "target1"];
    let peer_side = ["peer", "target2"];

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

    run.partition_groups(&founder_side, &peer_side)?;

    let founder_add = run.machine_add_command(&["target1"])?;
    let peer_add = run.machine_add_command(&["target2"])?;
    let outputs = run.ssh_run_concurrent(&[("founder", founder_add), ("peer", peer_add)])?;
    if outputs.iter().any(|output| output.status.success()) {
        return Err(Error::Message(
            "equal-split concurrent adds should fail without quorum".into(),
        ));
    }

    run.clear_partition_rules()?;
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
    run.wait_machine_rows(
        "peer",
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
    run.wait_mesh_absent_name("target1")?;
    run.wait_mesh_absent_name("target2")
}
