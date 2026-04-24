use crate::error::{Error, Result};
use crate::runner::{MachineExpectation, ScenarioRun, SubnetExpectation};
use crate::support::wait_until;
use std::time::Duration;

const BRIDGE_WAIT_TIMEOUT: Duration = Duration::from_secs(180);
const CORROSION_API_PORT: u16 = 51002;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.log_progress("mesh init founder");
    run.mesh_init("founder", "alpha")?;
    run.log_progress("wait founder mesh ready");
    run.wait_mesh_ready_name("founder")?;
    run.log_progress("wait founder enabled");
    run.wait_machine_rows(
        "founder",
        &[MachineExpectation {
            id: "founder",
            participation: "enabled",
            subnet: SubnetExpectation::Present,
        }],
    )?;
    run.log_progress("probe corrosion bridge forward");
    wait_for_bridge_health(run, "founder")?;
    run.log_progress("scenario complete");
    Ok(())
}

fn wait_for_bridge_health(run: &ScenarioRun, node_name: &str) -> Result<()> {
    let command = format!(
        "sh -lc 'curl --http2-prior-knowledge -fsS \"http://127.0.0.1:{CORROSION_API_PORT}/v1/health?gaps=0\" | grep -q \"\\\"response\\\"\" && \
         curl --http2-prior-knowledge -fsS \"http://127.0.0.1:{CORROSION_API_PORT}/v1/health?gaps=0\" | grep -q \"\\\"members\\\"\"'"
    );
    let mut last_output = String::new();

    wait_until(BRIDGE_WAIT_TIMEOUT, || {
        let output = run.ssh_run_name(node_name, &command)?;
        last_output = output.combined();
        Ok(output.status.success())
    })
    .map_err(|error| {
        Error::Message(format!(
            "bridge forward on {node_name} did not return corrosion health over 127.0.0.1:{CORROSION_API_PORT}: {error}\nlast output:\n{last_output}"
        ))
    })
}
