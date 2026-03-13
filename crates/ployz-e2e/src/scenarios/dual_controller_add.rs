use crate::error::Result;
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;

    run.machine_add("founder", "peer")?;
    run.wait_for_settled_machine_states("founder", &[("founder", "enabled"), ("peer", "enabled")])?;

    let founder_add = run.machine_add_command(&["joiner1", "joiner2", "joiner3"])?;
    let peer_add = run.machine_add_command(&["joiner4", "joiner5", "joiner6"])?;
    run.ssh_expect_ok_concurrent(&[("founder", founder_add), ("peer", peer_add)])?;

    run.wait_for_settled_machine_states(
        "founder",
        &[
            ("founder", "enabled"),
            ("peer", "enabled"),
            ("joiner1", "enabled"),
            ("joiner2", "enabled"),
            ("joiner3", "enabled"),
            ("joiner4", "enabled"),
            ("joiner5", "enabled"),
            ("joiner6", "enabled"),
        ],
    )?;
    run.assert_unique_machine_subnets("founder")?;
    Ok(())
}
