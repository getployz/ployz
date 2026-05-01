mod bridge_forward_smoke;
mod deploy_smoke;
mod destroy_with_dead_peer;
mod machine_add_basic;
mod machine_add_does_not_promote_storage;
mod machine_drain_standby_activate_cycle;
mod mesh_restart_from_seed_cache;
mod offline_leaf_node_command_fails_loudly;
mod single_node_init;
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
        Scenario::MachineAddDoesNotPromoteStorage => machine_add_does_not_promote_storage::run(run),
        Scenario::MachineDrainStandbyActivateCycle => {
            machine_drain_standby_activate_cycle::run(run)
        }
        Scenario::MeshRestartFromSeedCache => mesh_restart_from_seed_cache::run(run),
        Scenario::OfflineLeafNodeCommandFailsLoudly => {
            offline_leaf_node_command_fails_loudly::run(run)
        }
        Scenario::DestroyWithDeadPeer => destroy_with_dead_peer::run(run),
        Scenario::WireguardReconnect => wireguard_reconnect::run(run),
        Scenario::DeploySmoke => deploy_smoke::run(run),
        Scenario::BridgeForwardSmoke => bridge_forward_smoke::run(run),
        Scenario::VolumeSmoke => volume_smoke::run(run),
        Scenario::ZfsTransferSmoke => zfs_transfer_smoke::run(run),
    }
}
