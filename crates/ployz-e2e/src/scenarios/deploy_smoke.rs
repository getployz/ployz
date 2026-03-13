use crate::error::Result;
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;
    run.ssh_expect_ok_name(
        "founder",
        "ployzd deploy service web --namespace default --image nginx:1.27-alpine",
    )?;
    run.wait_service_container_name("founder", "default", "web")
}
