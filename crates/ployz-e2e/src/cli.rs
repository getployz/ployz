use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;

const DEFAULT_IMAGE: &str = "ployz-e2e-node:test";

#[derive(Debug, Parser)]
#[command(
    name = "ployz-e2e",
    about = "Host runtime E2E harness for prebuilt node images"
)]
pub(crate) struct Cli {
    #[arg(long, default_value = DEFAULT_IMAGE)]
    pub(crate) image: String,

    #[arg(long, value_enum)]
    pub(crate) scenario: Vec<Scenario>,

    #[arg(long, value_enum, default_value_t = ZfsMode::Off)]
    pub(crate) zfs: ZfsMode,

    #[arg(long, value_name = "PATH", default_value = ".e2e-artifacts")]
    pub(crate) artifacts_dir: PathBuf,

    #[arg(long)]
    pub(crate) keep_failed: bool,

    #[arg(long)]
    pub(crate) fail_fast: bool,

    #[arg(long)]
    pub(crate) parallel: bool,

    #[command(subcommand)]
    pub(crate) command: Option<Command>,
}

#[derive(Debug, Subcommand)]
pub(crate) enum Command {
    /// Print scenarios as a JSON array (for CI matrix fanout).
    Ls,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum ZfsMode {
    Off,
    Fake,
    Real,
}

impl ZfsMode {
    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Fake => "fake",
            Self::Real => "real",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
#[clap(rename_all = "snake_case")]
pub(crate) enum Scenario {
    MeshBootstrapJoinSmoke,
    NodeRestartAdoptsDataPlane,
    WireguardPartitionReconnect,
    DeployHttpAcmeGatewaySmoke,
    MvpThreeNodeParitySmoke,
    DockerBridgeForwardSmoke,
    ImagePushExistingImage,
    DeployImageAvailability,
    LocalBuildImageAvailability,
    DrainAwareRedeployRealSmoke,
    MigrateServiceRealSmoke,
    VolumeCloneBranchRealSmoke,
}

impl Scenario {
    const DEFAULT: [Self; 8] = [
        Self::MeshBootstrapJoinSmoke,
        Self::NodeRestartAdoptsDataPlane,
        Self::WireguardPartitionReconnect,
        Self::DeployHttpAcmeGatewaySmoke,
        Self::DockerBridgeForwardSmoke,
        Self::ImagePushExistingImage,
        Self::DeployImageAvailability,
        Self::LocalBuildImageAvailability,
    ];

    #[must_use]
    pub(crate) fn default_order(zfs_mode: ZfsMode) -> Vec<Self> {
        let mut scenarios = Self::DEFAULT.to_vec();
        match zfs_mode {
            ZfsMode::Off | ZfsMode::Fake => {}
            ZfsMode::Real => {
                scenarios.push(Self::MigrateServiceRealSmoke);
                scenarios.push(Self::DrainAwareRedeployRealSmoke);
            }
        }
        scenarios
    }

    #[must_use]
    pub(crate) fn ci_zfs_mode(self) -> ZfsMode {
        match self {
            Self::DrainAwareRedeployRealSmoke
            | Self::MigrateServiceRealSmoke
            | Self::VolumeCloneBranchRealSmoke => ZfsMode::Real,
            Self::MeshBootstrapJoinSmoke
            | Self::NodeRestartAdoptsDataPlane
            | Self::WireguardPartitionReconnect
            | Self::DeployHttpAcmeGatewaySmoke
            | Self::MvpThreeNodeParitySmoke
            | Self::DockerBridgeForwardSmoke
            | Self::ImagePushExistingImage
            | Self::DeployImageAvailability
            | Self::LocalBuildImageAvailability => ZfsMode::Off,
        }
    }

    pub(crate) fn validate_zfs_mode(self, zfs_mode: ZfsMode) -> Result<(), String> {
        match self {
            Self::DrainAwareRedeployRealSmoke
            | Self::MigrateServiceRealSmoke
            | Self::VolumeCloneBranchRealSmoke
                if zfs_mode != ZfsMode::Real =>
            {
                Err(format!("{} requires --zfs real", self.as_str()))
            }
            Self::MeshBootstrapJoinSmoke
            | Self::NodeRestartAdoptsDataPlane
            | Self::WireguardPartitionReconnect
            | Self::DeployHttpAcmeGatewaySmoke
            | Self::MvpThreeNodeParitySmoke
            | Self::ImagePushExistingImage
            | Self::DeployImageAvailability
            | Self::LocalBuildImageAvailability
            | Self::DockerBridgeForwardSmoke
            | Self::DrainAwareRedeployRealSmoke
            | Self::MigrateServiceRealSmoke
            | Self::VolumeCloneBranchRealSmoke => Ok(()),
        }
    }

    #[must_use]
    pub(crate) fn node_names(self) -> &'static [&'static str] {
        match self {
            Self::DockerBridgeForwardSmoke => &["founder"],
            Self::MvpThreeNodeParitySmoke => &["founder", "peer", "edge"],
            Self::ImagePushExistingImage
            | Self::DeployImageAvailability
            | Self::LocalBuildImageAvailability => &["founder", "peer"],
            Self::VolumeCloneBranchRealSmoke => &["founder"],
            Self::MeshBootstrapJoinSmoke
            | Self::NodeRestartAdoptsDataPlane
            | Self::WireguardPartitionReconnect
            | Self::DeployHttpAcmeGatewaySmoke
            | Self::DrainAwareRedeployRealSmoke
            | Self::MigrateServiceRealSmoke => &["founder", "peer"],
        }
    }

    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::MeshBootstrapJoinSmoke => "mesh_bootstrap_join_smoke",
            Self::NodeRestartAdoptsDataPlane => "node_restart_adopts_data_plane",
            Self::WireguardPartitionReconnect => "wireguard_partition_reconnect",
            Self::DeployHttpAcmeGatewaySmoke => "deploy_http_acme_gateway_smoke",
            Self::MvpThreeNodeParitySmoke => "mvp_three_node_parity_smoke",
            Self::DockerBridgeForwardSmoke => "docker_bridge_forward_smoke",
            Self::ImagePushExistingImage => "image_push_existing_image",
            Self::DeployImageAvailability => "deploy_image_availability",
            Self::LocalBuildImageAvailability => "local_build_image_availability",
            Self::DrainAwareRedeployRealSmoke => "drain_aware_redeploy_real_smoke",
            Self::MigrateServiceRealSmoke => "migrate_service_real_smoke",
            Self::VolumeCloneBranchRealSmoke => "volume_clone_branch_real_smoke",
        }
    }

    #[must_use]
    pub(crate) fn runtime(self) -> &'static str {
        match self {
            Self::DockerBridgeForwardSmoke | Self::MvpThreeNodeParitySmoke => "docker",
            Self::VolumeCloneBranchRealSmoke => "host",
            Self::MeshBootstrapJoinSmoke
            | Self::NodeRestartAdoptsDataPlane
            | Self::WireguardPartitionReconnect
            | Self::ImagePushExistingImage
            | Self::DeployImageAvailability
            | Self::LocalBuildImageAvailability
            | Self::DeployHttpAcmeGatewaySmoke
            | Self::DrainAwareRedeployRealSmoke
            | Self::MigrateServiceRealSmoke => "host",
        }
    }

    #[must_use]
    pub(crate) fn node_environment(self) -> &'static [(&'static str, &'static str)] {
        match self {
            Self::DeployHttpAcmeGatewaySmoke => &[
                ("PLOYZ_ACME_DIRECTORY_URL", "https://pebble:14000/dir"),
                ("PLOYZ_ACME_ROOT_CA_PATH", "/e2e-pebble/pebble.minica.pem"),
                ("PLOYZ_GATEWAY_HTTPS_LISTEN_ADDR", "0.0.0.0:443"),
            ],
            Self::MvpThreeNodeParitySmoke => &[
                ("PLOYZ_GATEWAY_LISTEN_ADDR", "127.0.0.1:18080"),
                ("PLOYZ_GATEWAY_HTTPS_LISTEN_ADDR", "127.0.0.1:18443"),
            ],
            Self::MeshBootstrapJoinSmoke
            | Self::NodeRestartAdoptsDataPlane
            | Self::WireguardPartitionReconnect
            | Self::DockerBridgeForwardSmoke
            | Self::ImagePushExistingImage
            | Self::DeployImageAvailability
            | Self::LocalBuildImageAvailability
            | Self::DrainAwareRedeployRealSmoke
            | Self::MigrateServiceRealSmoke
            | Self::VolumeCloneBranchRealSmoke => &[],
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Scenario, ZfsMode};

    #[test]
    fn real_ci_order_adds_default_real_storage_scenarios() {
        let scenarios = Scenario::default_order(ZfsMode::Real);

        assert_eq!(
            scenarios,
            vec![
                Scenario::MeshBootstrapJoinSmoke,
                Scenario::NodeRestartAdoptsDataPlane,
                Scenario::WireguardPartitionReconnect,
                Scenario::DeployHttpAcmeGatewaySmoke,
                Scenario::DockerBridgeForwardSmoke,
                Scenario::ImagePushExistingImage,
                Scenario::DeployImageAvailability,
                Scenario::LocalBuildImageAvailability,
                Scenario::MigrateServiceRealSmoke,
                Scenario::DrainAwareRedeployRealSmoke,
            ]
        );
    }

    #[test]
    fn fake_zfs_does_not_add_e2e_storage_scenarios() {
        assert_eq!(
            Scenario::default_order(ZfsMode::Fake),
            Scenario::default_order(ZfsMode::Off)
        );
    }
}
