use crate::error::{Error, Result};
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.mesh_init("founder", "alpha")?;
    run.wait_mesh_ready_name("founder")?;
    run.machine_add("founder", "joiner")?;
    run.wait_all_machine_states("founder", &["joiner"], "disabled")?;
    run.wait_all_machine_states("founder", &["joiner"], "enabled")?;

    let remove_enabled = run.ssh_run_name("founder", "ployzd machine rm joiner")?;
    if remove_enabled.status.success() {
        return Err(Error::Message(
            "machine remove unexpectedly succeeded while joiner was enabled".into(),
        ));
    }
    if !remove_enabled.combined().contains("must be disabled") {
        return Err(Error::Message(format!(
            "expected enabled remove guard failure, got: {}",
            remove_enabled.combined()
        )));
    }

    run.machine_remove_force("founder", "joiner")?;
    run.wait_machine_absent_name("founder", "joiner")
}
