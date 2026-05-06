use crate::error::Result;
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;
    run.machine_add("founder", "joiner1")?;
    run.wait_mesh_ready_name("joiner1")?;
    run.machine_add("founder", "joiner2")?;
    run.wait_mesh_ready_name("joiner2")?;
    run.wait_machine_rows(
        "founder",
        &[
            MachineExpectation {
                id: "founder",
                lifecycle: "active",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "joiner1",
                lifecycle: "active",
                subnet: SubnetExpectation::Present,
            },
            MachineExpectation {
                id: "joiner2",
                lifecycle: "active",
                subnet: SubnetExpectation::Present,
            },
        ],
    )?;
    run.assert_doctor_storage("founder", true, &[("joiner1", true), ("joiner2", true)])?;
    run.assert_doctor_storage("joiner1", true, &[("founder", true), ("joiner2", true)])?;
    run.assert_doctor_storage("joiner2", true, &[("founder", true), ("joiner1", true)])?;
    run.assert_nats_asset_replicas("founder", 1)
}
