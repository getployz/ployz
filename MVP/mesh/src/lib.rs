mod actor;
mod domain;
mod error;
mod invite;
mod linux;
mod snapshot;
mod wireguard;

pub use actor::{WireGuardActorHandle, WireGuardActorStatus};
pub use domain::{
    IrohEndpointId, MachineInvite, MeshNode, WireGuardOverlayCidr, WireGuardOverlayIp,
    WireGuardPrivateKey, WireGuardPublicKey, derive_overlay_ip,
};
pub use error::{MeshError, MeshResult};
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
