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
    run.wait_mesh_ready_name("peer")?;
    run.assert_doctor_storage(
        "founder",
        true,
        "authority:auth-default",
        &[("peer", true, "candidate")],
    )?;
    run.assert_doctor_storage(
        "peer",
        true,
        "candidate",
        &[("founder", true, "authority:auth-default")],
    )?;
    run.ssh_expect_ok_name(
        "peer",
        "sh -lc '! grep -q \"cluster {\" /var/lib/ployz/networks/alpha/nats/nats.conf'",
    )?;
    run.ssh_expect_ok_name(
        "founder",
        "sh -lc 'grep -q \"jetstream\" /var/lib/ployz/networks/alpha/nats/nats.conf'",
    )?;
    Ok(())
}
