use clap::{Parser, ValueEnum};
use std::path::PathBuf;

const DEFAULT_IMAGE: &str = "ployz-e2e-node:test";

#[derive(Debug, Parser)]
#[command(
    name = "ployz-e2e",
    about = "HostExec E2E harness for prebuilt node images"
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
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub(crate) enum Scenario {
    SingleNodeInit,
    MachineAddBasic,
    MachineRemoveGuard,
    ReplaceMachine,
    SplitBrainConcurrentAddSubnetHeal,
    WireguardReconnect,
    DeploySmoke,
}

impl Scenario {
    const ALL: [Self; 7] = [
        Self::SingleNodeInit,
        Self::MachineAddBasic,
        Self::MachineRemoveGuard,
        Self::ReplaceMachine,
        Self::SplitBrainConcurrentAddSubnetHeal,
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
            Self::MachineAddBasic | Self::MachineRemoveGuard => &["founder", "joiner"],
            Self::ReplaceMachine => &["founder", "joiner", "replacement"],
            Self::WireguardReconnect => &["founder", "peer"],
            Self::SplitBrainConcurrentAddSubnetHeal => &[
                "founder", "peer", "joiner1", "joiner2", "joiner3", "joiner4", "joiner5", "joiner6",
            ],
        }
    }

    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SingleNodeInit => "single_node_init",
            Self::MachineAddBasic => "machine_add_basic",
            Self::MachineRemoveGuard => "machine_remove_guard",
            Self::ReplaceMachine => "replace_machine",
            Self::SplitBrainConcurrentAddSubnetHeal => {
                "split_brain_concurrent_add_subnet_heal"
            }
            Self::WireguardReconnect => "wireguard_reconnect",
            Self::DeploySmoke => "deploy_smoke",
        }
    }
}
