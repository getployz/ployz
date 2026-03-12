use crate::error::Result;
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init_founder()?;
    let founder = run.node("founder")?;
    run.wait_mesh_ready_default(founder)?;
    run.ssh_expect_ok(
        founder,
        "ployzd deploy service web --namespace default --image nginx:1.27-alpine",
    )?;
    run.wait_service_container(founder, "default", "web")
}
