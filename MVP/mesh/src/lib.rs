mod actor;
mod container;
mod domain;
mod error;
mod forwarding;
mod invite;
mod linux;
mod snapshot;
mod wireguard;
#[cfg(all(feature = "linux-wireguard", target_os = "linux"))]
mod wireguard_linux;

pub use actor::{WireGuardActorHandle, WireGuardActorStatus};
pub use container::{
    ContainerIp, ContainerSubnet, derive_container_subnet, derive_container_subnet_plan,
};
pub use domain::{
    IrohEndpointId, MachineInvite, MeshNode, WireGuardOverlayCidr, WireGuardOverlayIp,
    WireGuardPrivateKey, WireGuardPublicKey, derive_overlay_ip,
};
pub use error::{MeshError, MeshResult};
pub use forwarding::{
    ForwardingCommand, ForwardingObservation, ForwardingRouteKey, IptablesRule,
    LinuxForwardingPlan, LinuxOverlayForwardingBackend, LinuxOverlayForwardingConfig,
    OverlayForwardingApplyReport, OverlayForwardingBackend, OverlayForwardingSnapshot,
    forwarding_route_key,
};
pub use invite::{
    InviteId, InviteSecret, JoinCommand, JoinCommandResult, JoinRequest, TombstoneCommand,
    TombstoneCommandResult, joined_fact_key, removal_started_fact_key,
    removal_started_fact_payload, tombstone_fact_key,
};
pub use linux::{
    HostNetworkApplyReport, HostNetworkBackend, HostNetworkEndpoint, HostNetworkRoute,
    HostNetworkSnapshot, HostServiceAddress, load_host_network_snapshot,
    write_host_network_snapshot,
};
pub use snapshot::{
    WireGuardAppliedSnapshot, WireGuardSnapshotPaths, load_applied_snapshot, write_applied_snapshot,
};
pub use wireguard::{
    MemoryWireGuardBackend, WireGuardBackend, WireGuardPeer, WireGuardPeerPlan, plan_full_mesh,
};
#[cfg(all(feature = "linux-wireguard", target_os = "linux"))]
pub use wireguard_linux::{LinuxWireGuardBackend, LinuxWireGuardConfig};
