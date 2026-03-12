use crate::error::Result;
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init_founder()?;
    run.wait_mesh_ready_default(run.node("founder")?)
}
