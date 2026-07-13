use std::net::SocketAddr;

use serde::{Deserialize, Serialize};

use crate::ops::FailureMessage;

use super::{DataplaneProjectionRevision, MachineEndpointSubnet, WireGuardPublicKey};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum EndpointBridgeStatus {
    Ready {
        subnet: MachineEndpointSubnet,
    },
    Missing,
    SubnetMismatch {
        expected: MachineEndpointSubnet,
        observed: MachineEndpointSubnet,
    },
    InvalidSubnet {
        observed: String,
    },
    Unavailable {
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DataplaneProjectionRevisions {
    pub declared_revision: DataplaneProjectionRevision,
    pub target_revision: DataplaneProjectionRevision,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum DataplaneProjectionTestimony {
    Applied {
        revisions: DataplaneProjectionRevisions,
    },
    Unusable {
        attempted_revisions: Option<DataplaneProjectionRevisions>,
        last_applied_revisions: Option<DataplaneProjectionRevisions>,
        failure: DataplaneProjectionFailure,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DataplaneProjectionFailure {
    FetchFailed {
        message: FailureMessage,
    },
    InvalidView {
        message: FailureMessage,
    },
    LocalMemberMissing,
    EndpointBridgeMissing,
    EndpointBridgeSubnetMismatch {
        expected: MachineEndpointSubnet,
        observed: MachineEndpointSubnet,
    },
    ApplyFailed {
        component: DataplaneProjectionComponent,
        message: FailureMessage,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum DataplaneProjectionComponent {
    EndpointBridge,
    WireGuard,
    Routes,
    Ebpf,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct NativeDataplaneProjectionStatus {
    pub endpoint_bridge: EndpointBridgeStatus,
    pub testimony: DataplaneProjectionTestimony,
}

impl NativeDataplaneProjectionStatus {
    #[must_use]
    pub fn unavailable(message: FailureMessage) -> Self {
        Self {
            endpoint_bridge: EndpointBridgeStatus::Unavailable {
                message: message.clone(),
            },
            testimony: DataplaneProjectionTestimony::Unusable {
                attempted_revisions: None,
                last_applied_revisions: None,
                failure: DataplaneProjectionFailure::FetchFailed { message },
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum NetworkStatusMode {
    Snapshot,
    ProbePathMtu,
}

impl NetworkStatusMode {
    #[must_use]
    pub const fn probes_path_mtu(self) -> bool {
        matches!(self, Self::ProbePathMtu)
    }
}

/// Live, machine-owned testimony about the configured Ployz native mesh.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct MachineDataplaneStatus {
    pub projection: NativeDataplaneProjectionStatus,
    pub wireguard: WireGuardStatus,
    pub ebpf_attachment: EbpfAttachmentStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardStatus {
    pub interface: String,
    pub configured_mtu: WireGuardConfiguredMtu,
    pub detected_mtu: WireGuardDetectedMtu,
    pub interface_mtu: WireGuardInterfaceMtu,
    pub peers: Vec<WireGuardPeerStatus>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "mode", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireGuardConfiguredMtu {
    Auto,
    Fixed { mtu: u32 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireGuardDetectedMtu {
    Detected { mtu: u32 },
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireGuardInterfaceMtu {
    Detected { mtu: u32 },
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardPeerStatus {
    pub public_key: WireGuardPublicKey,
    pub endpoint_subnet: WireGuardPeerEndpointSubnet,
    pub endpoint: Option<SocketAddr>,
    pub handshake: WireGuardHandshakeStatus,
    pub rtt: WireGuardRttStatus,
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub mtu_probe: WireGuardMtuProbe,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireGuardPeerEndpointSubnet {
    Missing,
    Valid { subnet: MachineEndpointSubnet },
    Invalid { value: String, message: String },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireGuardHandshakeStatus {
    Never,
    Ago { seconds: u64 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireGuardRttStatus {
    Measured { micros: u64 },
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireGuardMtuProbe {
    NotRequested,
    Measured { mtu: u32 },
    Unavailable { message: String },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum EbpfAttachmentStatus {
    Attached,
    Detached { message: String },
    Unknown { message: String },
}
