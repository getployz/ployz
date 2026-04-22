use crate::error::Result;
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;

    run.machine_add("founder", "peer")?;
    run.wait_machine_rows(
        "founder",
        &[
            MachineExpectation {
                id: "founder",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "peer",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
        ],
    )?;
    run.wait_mesh_ready_name("peer")?;

    run.machine_disable("founder", "peer")?;
    run.wait_machine_rows(
        "founder",
        &[
            MachineExpectation {
                id: "founder",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "peer",
                participation: "disabled",
                subnet: SubnetExpectation::Absent,
            },
        ],
    )?;
    run.wait_mesh_standby_name("peer")?;

    run.machine_enable("founder", "peer")?;
    run.wait_machine_rows(
        "founder",
        &[
            MachineExpectation {
                id: "founder",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "peer",
                participation: "enabled",
                subnet: SubnetExpectation::Present,
            },
        ],
    )?;
    run.wait_mesh_ready_name("peer")
}
