use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    let majority_side = ["founder", "peer1", "target1"];
    let minority_side = ["peer2", "target2"];

    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;

    run.machine_add("founder", "peer1")?;
    run.wait_mesh_ready_name("peer1")?;
    run.machine_add("founder", "peer2")?;
    run.wait_mesh_ready_name("peer2")?;
    run.wait_machine_rows(
        "founder",
        &[
            MachineExpectation {
                id: "founder",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "peer1",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "peer2",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
        ],
    )?;

    run.partition_groups(&majority_side, &minority_side)?;

    let majority_add = run.machine_add_command(&["target1"])?;
    let minority_add = run.machine_add_command(&["target2"])?;
    let outputs = run.ssh_run_concurrent(&[("founder", majority_add), ("peer2", minority_add)])?;
    let [majority, minority] = outputs.as_slice() else {
        return Err(Error::Message("expected two concurrent add outputs".into()));
    };
    if !majority.status.success() {
        return Err(Error::Message(format!(
            "majority-side add should succeed:\n{}",
            majority.combined()
        )));
    }
    if minority.status.success() {
        return Err(Error::Message(
            "minority-side add should fail without quorum".into(),
        ));
    }

    run.clear_partition_rules()?;
    run.wait_machine_rows(
        "founder",
        &[
            MachineExpectation {
                id: "founder",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "peer1",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "peer2",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "target1",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
        ],
    )?;
    run.assert_unique_machine_subnets("founder")?;
    run.wait_mesh_ready_name("target1")?;
    run.wait_mesh_absent_name("target2")
}
