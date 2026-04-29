mod bridge_forward_smoke;
mod deploy_smoke;
mod destroy_with_dead_peer;
mod machine_add_basic;
mod machine_drain_standby_activate_cycle;
mod mesh_restart_from_seed_cache;
mod single_node_init;
mod three_node_majority_add_succeeds;
mod two_node_equal_split_add_denied;
mod volume_smoke;
mod wireguard_reconnect;
mod zfs_support;
mod zfs_transfer_smoke;

use crate::cli::Scenario;
use crate::error::Result;
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    match run.scenario() {
        Scenario::SingleNodeInit => single_node_init::run(run),
        Scenario::MachineAddBasic => machine_add_basic::run(run),
        Scenario::MachineDrainStandbyActivateCycle => {
            machine_drain_standby_activate_cycle::run(run)
        }
        Scenario::MeshRestartFromSeedCache => mesh_restart_from_seed_cache::run(run),
        Scenario::TwoNodeEqualSplitAddDenied => two_node_equal_split_add_denied::run(run),
        Scenario::ThreeNodeMajorityAddSucceeds => three_node_majority_add_succeeds::run(run),
        Scenario::DestroyWithDeadPeer => destroy_with_dead_peer::run(run),
        Scenario::WireguardReconnect => wireguard_reconnect::run(run),
        Scenario::DeploySmoke => deploy_smoke::run(run),
        Scenario::BridgeForwardSmoke => bridge_forward_smoke::run(run),
        Scenario::VolumeSmoke => volume_smoke::run(run),
        Scenario::ZfsTransferSmoke => zfs_transfer_smoke::run(run),
    }
}
