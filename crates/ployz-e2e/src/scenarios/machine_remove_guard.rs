use crate::error::{Error, Result};
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.setup_founder_and_joiner()?;
    let founder = run.node("founder")?;

    let remove_enabled = run.ssh_run(founder, "ployzd machine rm joiner")?;
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

    run.ssh_expect_ok(founder, "ployzd machine drain joiner")?;
    let remove_draining = run.ssh_run(founder, "ployzd machine rm joiner")?;
    if remove_draining.status.success() {
        return Err(Error::Message(
            "machine remove unexpectedly succeeded while joiner was draining".into(),
        ));
    }
    if !remove_draining.combined().contains("must be disabled") {
        return Err(Error::Message(format!(
            "expected draining remove guard failure, got: {}",
            remove_draining.combined()
        )));
    }

    run.ssh_expect_ok(founder, "ployzd machine rm joiner --force")?;
    run.wait_machine_absent_default(founder, "joiner")
}
