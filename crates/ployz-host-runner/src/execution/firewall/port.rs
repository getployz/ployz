#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AssignedHostPort {
    pub port: u16,
    pub protocol: HostPortProtocol,
}

impl AssignedHostPort {
    #[must_use]
    pub const fn tcp(port: u16) -> Self {
        Self {
            port,
            protocol: HostPortProtocol::Tcp,
        }
    }

    #[must_use]
    pub const fn udp(port: u16) -> Self {
        Self {
            port,
            protocol: HostPortProtocol::Udp,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HostPortProtocol {
    Tcp,
    Udp,
}
