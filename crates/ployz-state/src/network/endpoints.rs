use std::net::IpAddr;

/// Minimum MTU required for WireGuard overlay traffic.
/// Interfaces below this are excluded — they can't carry our tunneled IPv6 packets.
const MIN_ENDPOINT_MTU: u32 = 1280;

/// Enumerate routable IPs from local network interfaces.
/// Skips loopback, link-local (fe80::), overlay (fd00::/8), docker/bridge interfaces,
/// and any interface with MTU below [`MIN_ENDPOINT_MTU`].
pub fn list_routable_ips() -> Vec<IpAddr> {
    let ifaces = match if_addrs::get_if_addrs() {
        Ok(ifaces) => ifaces,
        Err(e) => {
            tracing::warn!(?e, "failed to enumerate network interfaces");
            return Vec::new();
        }
    };

    ifaces
        .into_iter()
        .filter(|iface| {
            let name = &iface.name;
            // Skip docker/bridge interfaces
            if name.starts_with("docker") || name.starts_with("br-") || name.starts_with("veth") {
                return false;
            }
            // Skip Tailscale — double-tunneling over ts0 breaks overlay traffic
            // (also caught by MTU check below since tailscale0 MTU=1280)
            if name == "tailscale0" || name.starts_with("ts") {
                return false;
            }
            // Skip interfaces with MTU too small for overlay traffic
            if let Some(mtu) = get_interface_mtu(name)
                && mtu < MIN_ENDPOINT_MTU
            {
                tracing::debug!(name, mtu, "skipping interface with low MTU");
                return false;
            }
            let ip = iface.ip();
            // Skip loopback
            if ip.is_loopback() {
                return false;
            }
            match ip {
                IpAddr::V6(v6) => {
                    // Skip link-local (fe80::/10)
                    let segments = v6.segments();
                    if segments[0] & 0xffc0 == 0xfe80 {
                        return false;
                    }
                    // Skip overlay prefix (fd00::/8)
                    if segments[0] >> 8 == 0xfd {
                        return false;
                    }
                    true
                }
                IpAddr::V4(_) => true,
            }
        })
        .map(|iface| iface.ip())
        .collect()
}

/// Query the MTU of a network interface via ioctl(SIOCGIFMTU).
fn get_interface_mtu(name: &str) -> Option<u32> {
    use std::ffi::CString;
    use std::os::unix::io::AsRawFd;

    // SIOCGIFMTU isn't in the libc crate on all platforms.
    #[cfg(target_os = "macos")]
    const SIOCGIFMTU: libc::c_ulong = 0xc020_6933;
    #[cfg(target_os = "linux")]
    const SIOCGIFMTU: libc::c_ulong = 0x8921;

    let sock = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
    let c_name = CString::new(name).ok()?;
    let name_bytes = c_name.as_bytes_with_nul();
    if name_bytes.len() > libc::IFNAMSIZ {
        return None;
    }

    unsafe {
        let mut ifr: libc::ifreq = std::mem::zeroed();
        std::ptr::copy_nonoverlapping(
            name_bytes.as_ptr(),
            ifr.ifr_name.as_mut_ptr() as *mut u8,
            name_bytes.len(),
        );
        let ret = libc::ioctl(sock.as_raw_fd(), SIOCGIFMTU as _, &mut ifr);
        if ret == 0 {
            Some(ifr.ifr_ifru.ifru_mtu as u32)
        } else {
            None
        }
    }
}

/// Detect all reachable endpoints for this node from local interfaces only.
/// Returns a list of `ip:port` strings.
pub async fn detect_endpoints(listen_port: u16) -> Vec<String> {
    list_routable_ips()
        .into_iter()
        .map(|ip| match ip {
            IpAddr::V6(v6) => format!("[{v6}]:{listen_port}"),
            IpAddr::V4(v4) => format!("{v4}:{listen_port}"),
        })
        .collect()
}
