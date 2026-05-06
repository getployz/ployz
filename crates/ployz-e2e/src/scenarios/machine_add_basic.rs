use crate::error::Result;
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;
    run.machine_add("founder", "joiner")?;
    run.wait_machine_rows(
        "founder",
        &[
            MachineExpectation {
                id: "founder",
                lifecycle: "active",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "joiner",
                lifecycle: "active",
                subnet: SubnetExpectation::Present,
            },
        ],
    )?;
    run.wait_mesh_ready_name("joiner")?;
    run.assert_doctor_storage("founder", true, &[("joiner", true)])?;
    run.assert_doctor_storage("joiner", true, &[("founder", true)])?;
    Ok(())
}
