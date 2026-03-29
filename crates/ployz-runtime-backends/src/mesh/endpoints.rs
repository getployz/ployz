use async_trait::async_trait;
use ployz_runtime_api::EndpointDiscovery;
use ployz_types::Result;
use std::net::IpAddr;

const MIN_ENDPOINT_MTU: u32 = 1280;

#[derive(Clone, Copy, Default)]
pub struct HostEndpointDiscovery;

#[async_trait]
impl EndpointDiscovery for HostEndpointDiscovery {
    async fn detect_endpoints(&self, listen_port: u16) -> Result<Vec<String>> {
        Ok(list_routable_ips()
            .into_iter()
            .map(|ip| match ip {
                IpAddr::V6(v6) => format!("[{v6}]:{listen_port}"),
                IpAddr::V4(v4) => format!("{v4}:{listen_port}"),
            })
            .collect())
    }
}

fn list_routable_ips() -> Vec<IpAddr> {
    let interfaces = match if_addrs::get_if_addrs() {
        Ok(interfaces) => interfaces,
        Err(error) => {
            tracing::warn!(?error, "failed to enumerate network interfaces");
            return Vec::new();
        }
    };

    interfaces
        .into_iter()
        .filter(|interface| {
            let name = &interface.name;
            if name.starts_with("docker") || name.starts_with("br-") || name.starts_with("veth") {
                return false;
            }
            if name == "tailscale0" || name.starts_with("ts") {
                return false;
            }
            if let Some(mtu) = get_interface_mtu(name)
                && mtu < MIN_ENDPOINT_MTU
            {
                tracing::debug!(name, mtu, "skipping interface with low MTU");
                return false;
            }

            let ip = interface.ip();
            if ip.is_loopback() {
                return false;
            }

            match ip {
                IpAddr::V6(v6) => {
                    let [s0, ..] = v6.segments();
                    if s0 & 0xffc0 == 0xfe80 {
                        return false;
                    }
                    if s0 >> 8 == 0xfd {
                        return false;
                    }
                    true
                }
                IpAddr::V4(_) => true,
            }
        })
        .map(|interface| interface.ip())
        .collect()
}

fn get_interface_mtu(name: &str) -> Option<u32> {
    use std::ffi::CString;
    use std::os::unix::io::AsRawFd;

    #[cfg(target_os = "macos")]
    const SIOCGIFMTU: libc::c_ulong = 0xc020_6933;
    #[cfg(target_os = "linux")]
    const SIOCGIFMTU: libc::c_ulong = 0x8921;

    let socket = std::net::UdpSocket::bind("0.0.0.0:0").ok()?;
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
        let result = libc::ioctl(socket.as_raw_fd(), SIOCGIFMTU as _, &mut ifr);
        if result == 0 {
            Some(ifr.ifr_ifru.ifru_mtu as u32)
        } else {
            None
        }
    }
}
