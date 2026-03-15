mod deploy_smoke;
mod machine_add_basic;
mod single_node_init;
mod split_brain_concurrent_add_subnet_heal;
mod wireguard_reconnect;

use crate::cli::Scenario;
use crate::error::Result;
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    match run.scenario() {
        Scenario::SingleNodeInit => single_node_init::run(run),
        Scenario::MachineAddBasic => machine_add_basic::run(run),
        Scenario::SplitBrainConcurrentAddSubnetHeal => {
            split_brain_concurrent_add_subnet_heal::run(run)
        }
        Scenario::WireguardReconnect => wireguard_reconnect::run(run),
        Scenario::DeploySmoke => deploy_smoke::run(run),
    }
}
