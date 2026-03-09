use std::net::IpAddr;

/// Enumerate routable IPs from local network interfaces.
/// Skips loopback, link-local (fe80::), overlay (fd00::/8), and docker/bridge interfaces.
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

/// Try to detect public IP via external services.
pub async fn get_public_ip() -> Option<IpAddr> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(5))
        .build()
        .ok()?;

    let services = [
        "https://api.ipify.org",
        "https://ipinfo.io/ip",
        "http://ip-api.com/line/?fields=query",
    ];

    for url in services {
        match client.get(url).send().await {
            Ok(resp) if resp.status().is_success() => {
                if let Ok(body) = resp.text().await
                    && let Ok(ip) = body.trim().parse::<IpAddr>() {
                        return Some(ip);
                    }
            }
            Ok(_) => continue,
            Err(e) => {
                tracing::debug!(?e, url, "public IP lookup failed");
            }
        }
    }

    tracing::warn!("could not detect public IP from any service");
    None
}

/// Detect all reachable endpoints for this node, including public IP.
/// Returns a list of `ip:port` strings.
pub async fn detect_endpoints(listen_port: u16) -> Vec<String> {
    let mut ips = list_routable_ips();

    if let Some(public_ip) = get_public_ip().await
        && !ips.contains(&public_ip) {
            ips.insert(0, public_ip);
        }

    ips.into_iter()
        .map(|ip| match ip {
            IpAddr::V6(v6) => format!("[{v6}]:{listen_port}"),
            IpAddr::V4(v4) => format!("{v4}:{listen_port}"),
        })
        .collect()
}
