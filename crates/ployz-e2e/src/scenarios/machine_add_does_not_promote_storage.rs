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
    run.assert_doctor_roles(
        "founder",
        "storage_candidate",
        &[("joiner1", "mirror"), ("joiner2", "mirror")],
    )?;
    run.assert_doctor_roles(
        "joiner1",
        "mirror",
        &[("founder", "storage_candidate"), ("joiner2", "mirror")],
    )?;
    run.assert_doctor_roles(
        "joiner2",
        "mirror",
        &[("founder", "storage_candidate"), ("joiner1", "mirror")],
    )?;
    run.assert_nats_asset_replicas("founder", 1)
}
