use crate::error::Result;
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    run.setup_founder_and_joiner()?;
    let founder = run.node("founder")?;
    let replacement = run.node("replacement")?;
    let add_replacement = format!(
        "ployzd machine add --identity /e2e-keys/id_ed25519 root@{}",
        replacement.outer_ip
    );
    run.ssh_expect_ok(founder, &add_replacement)?;
    run.wait_machine_state_default(founder, "replacement", "disabled")?;
    run.wait_machine_state_default(founder, "replacement", "enabled")?;
    run.ssh_expect_ok(founder, "ployzd machine drain joiner")?;
    run.ssh_expect_ok(founder, "ployzd machine rm joiner --force")?;
    run.wait_machine_absent_default(founder, "joiner")?;
    run.wait_machine_state_default(founder, "replacement", "enabled")
}
