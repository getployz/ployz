use std::cmp::Ordering;
use std::net::{IpAddr, Ipv4Addr};
#[cfg(test)]
use std::net::Ipv6Addr;
use std::time::Duration;

const MIN_ENDPOINT_MTU: u32 = 1280;
const PUBLIC_IP_DISCOVERY_TIMEOUT: Duration = Duration::from_secs(3);
const CGNAT_LOW: Ipv4Addr = Ipv4Addr::new(100, 64, 0, 0);
const CGNAT_HIGH: Ipv4Addr = Ipv4Addr::new(100, 127, 255, 255);

pub fn list_routable_ips() -> Vec<IpAddr> {
    let interfaces = match if_addrs::get_if_addrs() {
        Ok(interfaces) => interfaces,
        Err(error) => {
            tracing::warn!(?error, "failed to enumerate network interfaces");
            return Vec::new();
        }
    };

    let mut ips: Vec<IpAddr> = interfaces
        .into_iter()
        .filter_map(|interface| {
            let ip = interface.ip();
            if interface_is_eligible(&interface.name, ip) {
                Some(ip)
            } else {
                None
            }
        })
        .collect();
    sort_endpoint_ips(&mut ips);
    ips
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

fn interface_is_eligible(name: &str, ip: IpAddr) -> bool {
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

    ip_is_eligible(ip)
}

fn ip_is_eligible(ip: IpAddr) -> bool {
    if ip.is_loopback() || ip.is_unspecified() || ip.is_multicast() {
        return false;
    }

    match ip {
        // Link-local endpoints are not durable transport candidates across a
        // fleet. Publishing them first makes restart ordering and endpoint
        // rotation worse, so they are excluded entirely.
        IpAddr::V4(v4) => !v4.is_link_local(),
        IpAddr::V6(v6) => !v6.is_unicast_link_local() && !v6.is_unique_local(),
    }
}

fn sort_endpoint_ips(ips: &mut Vec<IpAddr>) {
    // Store order matters because it becomes the default candidate order for
    // WireGuard endpoint selection. Keep directly routable private addresses
    // first, then CGNAT, then public addresses. That preserves the cheapest
    // likely-successful path before falling back to broader internet paths.
    ips.sort_by(compare_endpoint_ips);
    ips.dedup();
}

fn compare_endpoint_ips(left: &IpAddr, right: &IpAddr) -> Ordering {
    endpoint_priority(*left)
        .cmp(&endpoint_priority(*right))
        .then_with(|| match (left, right) {
            (IpAddr::V4(left), IpAddr::V4(right)) => left.octets().cmp(&right.octets()),
            (IpAddr::V6(left), IpAddr::V6(right)) => left.octets().cmp(&right.octets()),
            (IpAddr::V4(_), IpAddr::V6(_)) => Ordering::Less,
            (IpAddr::V6(_), IpAddr::V4(_)) => Ordering::Greater,
        })
}

fn endpoint_priority(ip: IpAddr) -> u8 {
    match ip {
        IpAddr::V4(v4) if v4.is_private() => 0,
        IpAddr::V4(v4) if is_cgnat(v4) => 1,
        IpAddr::V4(_) => 2,
        IpAddr::V6(_) => 3,
    }
}

fn is_cgnat(ip: Ipv4Addr) -> bool {
    (CGNAT_LOW..=CGNAT_HIGH).contains(&ip)
}

pub async fn detect_endpoints(listen_port: u16) -> Vec<String> {
    list_routable_ips()
        .into_iter()
        .map(|ip| match ip {
            IpAddr::V6(v6) => format!("[{v6}]:{listen_port}"),
            IpAddr::V4(v4) => format!("{v4}:{listen_port}"),
        })
        .collect()
}

pub async fn detect_advertised_endpoints(listen_port: u16) -> Vec<String> {
    let mut ips = list_routable_ips();
    if let Some(public_ip) = discover_public_ip().await {
        ips.push(public_ip);
    }
    sort_endpoint_ips(&mut ips);
    ips.into_iter()
        .map(|ip| match ip {
            IpAddr::V6(v6) => format!("[{v6}]:{listen_port}"),
            IpAddr::V4(v4) => format!("{v4}:{listen_port}"),
        })
        .collect()
}

async fn discover_public_ip() -> Option<IpAddr> {
    if let Ok(value) = std::env::var("PLOYZ_PUBLIC_IP")
        && let Some(ip) = parse_ip_value(&value)
    {
        return Some(ip);
    }

    let client = reqwest::Client::builder()
        .timeout(PUBLIC_IP_DISCOVERY_TIMEOUT)
        .build()
        .ok()?;
    for service in public_ip_services() {
        if let Some(ip) = query_public_ip_service(&client, &service).await {
            return Some(ip);
        }
    }

    None
}

fn parse_ip_value(value: &str) -> Option<IpAddr> {
    value
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .and_then(|line| line.parse().ok())
}

#[derive(Clone, Copy)]
enum PublicIpParser {
    Plaintext,
    CloudflareTrace,
}

struct PublicIpService {
    url: String,
    parser: PublicIpParser,
}

fn public_ip_services() -> Vec<PublicIpService> {
    if let Ok(urls) = std::env::var("PLOYZ_PUBLIC_IP_DISCOVERY_URLS") {
        return urls
            .split(',')
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .filter_map(|url| {
                let parser = if url.contains("cdn-cgi/trace") {
                    PublicIpParser::CloudflareTrace
                } else {
                    PublicIpParser::Plaintext
                };
                Some(PublicIpService {
                    url: url.to_string(),
                    parser,
                })
            })
            .collect();
    }

    vec![
        PublicIpService {
            url: "https://api.ipify.org".to_string(),
            parser: PublicIpParser::Plaintext,
        },
        PublicIpService {
            url: "https://ipinfo.io/ip".to_string(),
            parser: PublicIpParser::Plaintext,
        },
        PublicIpService {
            url: "https://ifconfig.co/ip".to_string(),
            parser: PublicIpParser::Plaintext,
        },
        PublicIpService {
            url: "https://cloudflare.com/cdn-cgi/trace".to_string(),
            parser: PublicIpParser::CloudflareTrace,
        },
    ]
}

async fn query_public_ip_service(
    client: &reqwest::Client,
    service: &PublicIpService,
) -> Option<IpAddr> {
    let response = client.get(&service.url).send().await.ok()?;
    if !response.status().is_success() {
        return None;
    }
    let body = response.text().await.ok()?;
    match service.parser {
        PublicIpParser::Plaintext => parse_ip_value(&body),
        PublicIpParser::CloudflareTrace => parse_cloudflare_trace(&body),
    }
}

fn parse_cloudflare_trace(body: &str) -> Option<IpAddr> {
    body.lines()
        .find_map(|line| line.strip_prefix("ip="))
        .and_then(|value| value.trim().parse().ok())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn endpoint_order_prefers_private_then_cgnat_then_public() {
        let mut ips = vec![
            IpAddr::V4(Ipv4Addr::new(198, 51, 100, 10)),
            IpAddr::V4(Ipv4Addr::new(100, 64, 1, 10)),
            IpAddr::V4(Ipv4Addr::new(10, 0, 0, 10)),
        ];
        sort_endpoint_ips(&mut ips);
        assert_eq!(
            ips,
            vec![
                IpAddr::V4(Ipv4Addr::new(10, 0, 0, 10)),
                IpAddr::V4(Ipv4Addr::new(100, 64, 1, 10)),
                IpAddr::V4(Ipv4Addr::new(198, 51, 100, 10)),
            ]
        );
    }

    #[test]
    fn ip_eligibility_filters_link_local_and_ula_addresses() {
        assert!(!ip_is_eligible(IpAddr::V4(Ipv4Addr::new(169, 254, 1, 20))));
        assert!(!ip_is_eligible(IpAddr::V6(
            "fe80::1".parse::<Ipv6Addr>().expect("link local"),
        )));
        assert!(!ip_is_eligible(IpAddr::V6(
            "fd00::1".parse::<Ipv6Addr>().expect("ula"),
        )));
        assert!(ip_is_eligible(IpAddr::V4(Ipv4Addr::new(10, 0, 0, 10))));
    }

    #[test]
    fn public_ip_services_use_override_urls_without_leaking_static_refs() {
        unsafe {
            std::env::set_var(
                "PLOYZ_PUBLIC_IP_DISCOVERY_URLS",
                "https://one.example/ip,https://two.example/cdn-cgi/trace",
            );
        }
        let services = public_ip_services();
        assert_eq!(services.len(), 2);
        assert_eq!(services[0].url, "https://one.example/ip");
        assert_eq!(services[1].url, "https://two.example/cdn-cgi/trace");

        unsafe {
            std::env::remove_var("PLOYZ_PUBLIC_IP_DISCOVERY_URLS");
        }
    }
}
