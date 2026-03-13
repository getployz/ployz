use crate::error::Result;
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;
    run.machine_add("founder", "joiner")?;
    run.wait_all_machine_states("founder", &["joiner"], "disabled")?;
    run.wait_all_machine_states("founder", &["joiner"], "enabled")
}
