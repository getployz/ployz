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
pub(crate) enum Scenario {
    SingleNodeInit,
    MachineAddBasic,
    MachineDrainStandbyActivateCycle,
    TwoNodeEqualSplitAddDenied,
    ThreeNodeMajorityAddSucceeds,
    DestroyWithDeadPeer,
    WireguardReconnect,
    DeploySmoke,
    BridgeForwardSmoke,
    VolumeSmoke,
    ZfsTransferSmoke,
    ZfsCutoverMeasure,
}

impl Scenario {
    const DEFAULT: [Self; 9] = [
        Self::SingleNodeInit,
        Self::MachineAddBasic,
        Self::MachineDrainStandbyActivateCycle,
        Self::TwoNodeEqualSplitAddDenied,
        Self::ThreeNodeMajorityAddSucceeds,
        Self::DestroyWithDeadPeer,
        Self::WireguardReconnect,
        Self::DeploySmoke,
        Self::BridgeForwardSmoke,
    ];

    #[must_use]
    pub(crate) fn default_order(zfs_mode: ZfsMode) -> Vec<Self> {
        let mut scenarios = Self::DEFAULT.to_vec();
        match zfs_mode {
            ZfsMode::Off => {}
            ZfsMode::Fake => scenarios.push(Self::VolumeSmoke),
            ZfsMode::Real => {
                scenarios.push(Self::VolumeSmoke);
                scenarios.push(Self::ZfsTransferSmoke);
            }
        }
        scenarios
    }

    pub(crate) fn validate_zfs_mode(self, zfs_mode: ZfsMode) -> Result<(), String> {
        match self {
            Self::VolumeSmoke if zfs_mode == ZfsMode::Off => {
                Err("volume_smoke requires --zfs fake or --zfs real".into())
            }
            Self::ZfsTransferSmoke if zfs_mode != ZfsMode::Real => {
                Err("zfs_transfer_smoke requires --zfs real".into())
            }
            Self::ZfsCutoverMeasure if zfs_mode != ZfsMode::Real => {
                Err("zfs_cutover_measure requires --zfs real".into())
            }
            Self::SingleNodeInit
            | Self::MachineAddBasic
            | Self::MachineDrainStandbyActivateCycle
            | Self::TwoNodeEqualSplitAddDenied
            | Self::ThreeNodeMajorityAddSucceeds
            | Self::DestroyWithDeadPeer
            | Self::WireguardReconnect
            | Self::DeploySmoke
            | Self::BridgeForwardSmoke
            | Self::VolumeSmoke
            | Self::ZfsTransferSmoke
            | Self::ZfsCutoverMeasure => Ok(()),
        }
    }

    #[must_use]
    pub(crate) fn node_names(self) -> &'static [&'static str] {
        match self {
            Self::SingleNodeInit | Self::BridgeForwardSmoke | Self::VolumeSmoke => &["founder"],
            Self::DeploySmoke | Self::ZfsTransferSmoke | Self::ZfsCutoverMeasure => {
                &["founder", "peer"]
            }
            Self::MachineAddBasic => &["founder", "joiner"],
            Self::MachineDrainStandbyActivateCycle | Self::WireguardReconnect => {
                &["founder", "peer"]
            }
            Self::TwoNodeEqualSplitAddDenied => &["founder", "peer", "target1", "target2"],
            Self::ThreeNodeMajorityAddSucceeds => {
                &["founder", "peer1", "peer2", "target1", "target2"]
            }
            Self::DestroyWithDeadPeer => &["founder", "peer1", "peer2"],
        }
    }

    #[must_use]
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::SingleNodeInit => "single_node_init",
            Self::MachineAddBasic => "machine_add_basic",
            Self::MachineDrainStandbyActivateCycle => "machine_drain_standby_activate_cycle",
            Self::TwoNodeEqualSplitAddDenied => "two_node_equal_split_add_denied",
            Self::ThreeNodeMajorityAddSucceeds => "three_node_majority_add_succeeds",
            Self::DestroyWithDeadPeer => "destroy_with_dead_peer",
            Self::WireguardReconnect => "wireguard_reconnect",
            Self::DeploySmoke => "deploy_smoke",
            Self::BridgeForwardSmoke => "bridge_forward_smoke",
            Self::VolumeSmoke => "volume_smoke",
            Self::ZfsTransferSmoke => "zfs_transfer_smoke",
            Self::ZfsCutoverMeasure => "zfs_cutover_measure",
        }
    }

    #[must_use]
    pub(crate) fn runtime(self) -> &'static str {
        match self {
            Self::BridgeForwardSmoke => "docker",
            Self::SingleNodeInit
            | Self::MachineAddBasic
            | Self::MachineDrainStandbyActivateCycle
            | Self::TwoNodeEqualSplitAddDenied
            | Self::ThreeNodeMajorityAddSucceeds
            | Self::DestroyWithDeadPeer
            | Self::WireguardReconnect
            | Self::DeploySmoke
            | Self::VolumeSmoke
            | Self::ZfsTransferSmoke
            | Self::ZfsCutoverMeasure => "host",
        }
    }
}
