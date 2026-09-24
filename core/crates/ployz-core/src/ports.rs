//! Fixed network ports owned by Ployz.

/// Fixed TCP port for Machine RPC.
pub const MACHINE_API_PORT: u16 = 7569;
/// Fixed UDP port for Corrosion gossip between Machines.
pub const CORROSION_GOSSIP_PORT: u16 = 7570;
/// Fixed TCP port for the Machine-local Corrosion API.
pub const CORROSION_API_PORT: u16 = 7571;
/// Fixed TCP port for direct image transfer between Machines.
pub const UNREGISTRY_PORT: u16 = 7572;
/// Fixed UDP port for the iroh management transport.
pub const MANAGEMENT_PORT: u16 = 7573;
/// Fixed UDP port for the WireGuard mesh.
pub const WIREGUARD_PORT: u16 = 51820;
