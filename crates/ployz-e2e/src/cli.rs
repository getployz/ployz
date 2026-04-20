use clap::{Parser, ValueEnum};
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

    #[arg(long, value_name = "PATH", default_value = ".e2e-artifacts")]
    pub(crate) artifacts_dir: PathBuf,

    #[arg(long)]
    pub(crate) keep_failed: bool,

    #[arg(long)]
    pub(crate) fail_fast: bool,

    #[arg(long)]
    pub(crate) parallel: bool,

    #[arg(long, value_name = "PATH")]
    pub(crate) junit_path: Option<PathBuf>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Scenario {
    SingleNodeInit,
    MachineAddBasic,
    QuorumSubnetCoordination,
    WireguardReconnect,
    DeploySmoke,
}

impl Scenario {
    const ALL: [Self; 5] = [
        Self::SingleNodeInit,
        Self::MachineAddBasic,
        Self::QuorumSubnetCoordination,
        Self::WireguardReconnect,
        Self::DeploySmoke,
    ];

    #[must_use]
    pub(crate) fn default_order() -> Vec<Self> {
        Self::ALL.to_vec()
    }

    #[must_use]
    pub(crate) fn node_names(self) -> &'static [&'static str] {
        match self {
            Self::SingleNodeInit | Self::DeploySmoke => &["founder"],
            Self::MachineAddBasic => &["founder", "joiner"],
            Self::WireguardReconnect => &["founder", "peer"],
            Self::QuorumSubnetCoordination => &["founder", "peer1", "peer2", "joiner1", "joiner2"],
        }
    }

    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SingleNodeInit => "single_node_init",
            Self::MachineAddBasic => "machine_add_basic",
            Self::QuorumSubnetCoordination => "quorum_subnet_coordination",
            Self::WireguardReconnect => "wireguard_reconnect",
            Self::DeploySmoke => "deploy_smoke",
        }
    }
}
