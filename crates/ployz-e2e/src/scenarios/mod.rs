mod deploy_http_acme_gateway_smoke;
mod docker_bridge_forward_smoke;
mod mesh_bootstrap_join_smoke;
mod migrate_service_real_smoke;
mod node_restart_adopts_data_plane;
mod wireguard_partition_reconnect;
mod zfs_support;

use crate::cli::Scenario;
use crate::error::Result;
use crate::runner::ScenarioRun;

pub(crate) fn run(run: &ScenarioRun) -> Result<()> {
    match run.scenario() {
        Scenario::MeshBootstrapJoinSmoke => mesh_bootstrap_join_smoke::run(run),
        Scenario::NodeRestartAdoptsDataPlane => node_restart_adopts_data_plane::run(run),
        Scenario::WireguardPartitionReconnect => wireguard_partition_reconnect::run(run),
        Scenario::DeployHttpAcmeGatewaySmoke => deploy_http_acme_gateway_smoke::run(run),
        Scenario::DockerBridgeForwardSmoke => docker_bridge_forward_smoke::run(run),
        Scenario::MigrateServiceRealSmoke => migrate_service_real_smoke::run(run),
    }
}
