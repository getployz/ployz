mod deploy_smoke;
mod machine_add_basic;
mod quorum_subnet_coordination;
mod single_node_init;
mod wireguard_reconnect;

use crate::cli::Scenario;
use crate::error::Result;
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    match run.scenario() {
        Scenario::SingleNodeInit => single_node_init::run(run),
        Scenario::MachineAddBasic => machine_add_basic::run(run),
        Scenario::QuorumSubnetCoordination => quorum_subnet_coordination::run(run),
        Scenario::WireguardReconnect => wireguard_reconnect::run(run),
        Scenario::DeploySmoke => deploy_smoke::run(run),
    }
}
