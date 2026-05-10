mod deploy_http_acme_gateway_smoke;
mod docker_bridge_forward_smoke;
mod drain_aware_redeploy_real_smoke;
mod image_push_existing_image;
mod mesh_bootstrap_join_smoke;
mod migrate_service_real_smoke;
mod node_restart_adopts_data_plane;
mod volume_clone_branch_real_smoke;
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
        Scenario::ImagePushExistingImage => image_push_existing_image::run(run),
        Scenario::DrainAwareRedeployRealSmoke => drain_aware_redeploy_real_smoke::run(run),
        Scenario::MigrateServiceRealSmoke => migrate_service_real_smoke::run(run),
        Scenario::VolumeCloneBranchRealSmoke => volume_clone_branch_real_smoke::run(run),
    }
}
