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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Scenario {
    SingleNodeInit,
    MachineAddBasic,
    MachineDisableEnableCycle,
    TwoNodeEqualSplitAddDenied,
    ThreeNodeMajorityAddSucceeds,
    WireguardReconnect,
    DeploySmoke,
    BridgeForwardSmoke,
}

impl Scenario {
    const ALL: [Self; 8] = [
        Self::SingleNodeInit,
        Self::MachineAddBasic,
        Self::MachineDisableEnableCycle,
        Self::TwoNodeEqualSplitAddDenied,
        Self::ThreeNodeMajorityAddSucceeds,
        Self::WireguardReconnect,
        Self::DeploySmoke,
        Self::BridgeForwardSmoke,
    ];

    #[must_use]
    pub(crate) fn default_order() -> Vec<Self> {
        Self::ALL.to_vec()
    }

    #[must_use]
    pub(crate) fn node_names(self) -> &'static [&'static str] {
        match self {
            Self::SingleNodeInit | Self::DeploySmoke | Self::BridgeForwardSmoke => &["founder"],
            Self::MachineAddBasic => &["founder", "joiner"],
            Self::MachineDisableEnableCycle | Self::WireguardReconnect => &["founder", "peer"],
            Self::TwoNodeEqualSplitAddDenied => &["founder", "peer", "target1", "target2"],
            Self::ThreeNodeMajorityAddSucceeds => {
                &["founder", "peer1", "peer2", "target1", "target2"]
            }
        }
    }

    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SingleNodeInit => "single_node_init",
            Self::MachineAddBasic => "machine_add_basic",
            Self::MachineDisableEnableCycle => "machine_disable_enable_cycle",
            Self::TwoNodeEqualSplitAddDenied => "two_node_equal_split_add_denied",
            Self::ThreeNodeMajorityAddSucceeds => "three_node_majority_add_succeeds",
            Self::WireguardReconnect => "wireguard_reconnect",
            Self::DeploySmoke => "deploy_smoke",
            Self::BridgeForwardSmoke => "bridge_forward_smoke",
        }
    }

    #[must_use]
    pub(crate) fn runtime(self) -> &'static str {
        match self {
            Self::BridgeForwardSmoke => "docker",
            Self::SingleNodeInit
            | Self::MachineAddBasic
            | Self::MachineDisableEnableCycle
            | Self::TwoNodeEqualSplitAddDenied
            | Self::ThreeNodeMajorityAddSucceeds
            | Self::WireguardReconnect
            | Self::DeploySmoke => "host",
        }
    }
}
