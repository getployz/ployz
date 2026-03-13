mod deploy_smoke;
mod dual_controller_add;
mod machine_add_basic;
mod machine_remove_guard;
mod replace_machine;
mod single_node_init;

use crate::cli::Scenario;
use crate::error::Result;
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    match run.scenario() {
        Scenario::SingleNodeInit => single_node_init::run(run),
        Scenario::MachineAddBasic => machine_add_basic::run(run),
        Scenario::MachineRemoveGuard => machine_remove_guard::run(run),
        Scenario::ReplaceMachine => replace_machine::run(run),
        Scenario::DualControllerAdd => dual_controller_add::run(run),
        Scenario::DeploySmoke => deploy_smoke::run(run),
    }
}
