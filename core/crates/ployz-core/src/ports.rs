//! Fixed network ports owned by Ployz.

/// Fixed TCP port for Machine RPC.
pub const MACHINE_API_PORT: u16 = 7569;
/// Fixed UDP port for Corrosion gossip between Machines.
pub const CORROSION_GOSSIP_PORT: u16 = 7570;
/// Fixed TCP port for the Machine-local Corrosion API.
pub const CORROSION_API_PORT: u16 = 7571;
/// Where the Machine-local Corrosion API listens: loopback on [`CORROSION_API_PORT`].
pub const CORROSION_API_ADDRESS: std::net::SocketAddr = std::net::SocketAddr::V4(
    std::net::SocketAddrV4::new(std::net::Ipv4Addr::LOCALHOST, CORROSION_API_PORT),
);
/// Fixed TCP port for direct image transfer between Machines.
pub const UNREGISTRY_PORT: u16 = 7572;
/// Fixed UDP port for the iroh management transport.
pub const MANAGEMENT_PORT: u16 = 7573;
/// Fixed TCP port a writer Machine streams Volume snapshots from.
pub const VOLUME_SEND_PORT: u16 = 7574;
/// Fixed UDP port for the WireGuard mesh.
pub const WIREGUARD_PORT: u16 = 51820;

#[cfg(test)]
mod tests {
    use super::CORROSION_API_ADDRESS;

    #[test]
    fn the_corrosion_api_listens_on_loopback_port_7571() {
        assert_eq!(CORROSION_API_ADDRESS.to_string(), "127.0.0.1:7571");
    }
}
