use crate::error::Result;
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;
    run.machine_add("founder", "joiner")?;
    run.wait_all_machine_states("founder", &["joiner"], "disabled")?;
    run.wait_all_machine_states("founder", &["joiner"], "enabled")?;
    run.machine_add("founder", "replacement")?;
    run.wait_all_machine_states("founder", &["replacement"], "disabled")?;
    run.wait_all_machine_states("founder", &["replacement"], "enabled")?;
    run.machine_drain("founder", "joiner")?;
    run.machine_remove_force("founder", "joiner")?;
    run.wait_machine_absent_name("founder", "joiner")?;
    run.wait_machine_state_name("founder", "replacement", "enabled")
}
