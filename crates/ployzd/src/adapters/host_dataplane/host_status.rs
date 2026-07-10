use defguard_wireguard_rs::{WGApi, WireguardInterfaceApi, peer::Peer};
use futures_util::{StreamExt, TryStreamExt, stream};
use ipnet::Ipv4Net;
use ployz_core::dataplane::{
    WireGuardConfiguredMtu, WireGuardDetectedMtu, WireGuardHandshakeStatus, WireGuardInterfaceMtu,
    WireGuardMtuProbe, WireGuardPeerStatus, WireGuardPublicKey, WireGuardRttStatus,
    WireGuardStatus,
};
use std::net::Ipv4Addr;
use std::time::{Duration, SystemTime};
use tokio::process::Command;

use super::host_network::{
    HostWireGuardApi, MAX_WIREGUARD_MTU, MIN_WIREGUARD_MTU, WireGuardMtuPolicy,
    detect_wireguard_mtu, wireguard_host_ipv4,
};

const MAX_CONCURRENT_PEER_DIAGNOSTICS: usize = 4;

pub(super) async fn read_wireguard_status(
    wg_ifname: &str,
    policy: WireGuardMtuPolicy,
    probe: bool,
) -> Result<WireGuardStatus, String> {
    let api = WGApi::<HostWireGuardApi>::new(wg_ifname).map_err(|source| source.to_string())?;
    let host = api
        .read_interface_data()
        .map_err(|source| source.to_string())?;
    let configured_mtu = match policy {
        WireGuardMtuPolicy::Auto => WireGuardConfiguredMtu::Auto,
        WireGuardMtuPolicy::Fixed(mtu) => WireGuardConfiguredMtu::Fixed { mtu },
    };
    let detected_mtu = match detect_wireguard_mtu(wg_ifname).await {
        Ok(mtu) => WireGuardDetectedMtu::Detected { mtu: mtu.get() },
        Err(message) => WireGuardDetectedMtu::Unavailable { message },
    };
    let interface_mtu = match read_interface_mtu(wg_ifname).await {
        Ok(mtu) => WireGuardInterfaceMtu::Detected { mtu },
        Err(message) => WireGuardInterfaceMtu::Unavailable { message },
    };
    let peers = stream::iter(
        host.peers
            .into_iter()
            .map(|(key, peer)| peer_status(key.to_string(), peer, probe)),
    )
    .buffered(MAX_CONCURRENT_PEER_DIAGNOSTICS)
    .try_collect()
    .await?;

    Ok(WireGuardStatus {
        interface: wg_ifname.to_owned(),
        configured_mtu,
        detected_mtu,
        interface_mtu,
        peers,
    })
}

async fn peer_status(
    public_key: String,
    peer: Peer,
    probe: bool,
) -> Result<WireGuardPeerStatus, String> {
    let public_key = WireGuardPublicKey::try_new(public_key).map_err(|error| error.to_string())?;
    let endpoint_subnet = peer.allowed_ips.first().map(ToString::to_string);
    let target = peer
        .allowed_ips
        .iter()
        .find_map(|allowed| match allowed.address {
            std::net::IpAddr::V4(address) => Ipv4Net::new(address, allowed.cidr)
                .ok()
                .and_then(|subnet| wireguard_host_ipv4(subnet).ok()),
            std::net::IpAddr::V6(_) => None,
        });
    let (rtt, mtu_probe) = match target {
        Some(target) => {
            let rtt = match ping(target, None).await {
                Ok(output) => parse_ping_rtt_micros(&output).map_or_else(
                    || WireGuardRttStatus::Unavailable {
                        message: "ping returned no RTT".to_owned(),
                    },
                    |micros| WireGuardRttStatus::Measured { micros },
                ),
                Err(message) => WireGuardRttStatus::Unavailable { message },
            };
            let mtu_probe = if probe {
                probe_path_mtu(target).await
            } else {
                WireGuardMtuProbe::NotRequested
            };
            (rtt, mtu_probe)
        }
        None => (
            WireGuardRttStatus::Unavailable {
                message: "peer has no IPv4 overlay subnet".to_owned(),
            },
            if probe {
                WireGuardMtuProbe::Unavailable {
                    message: "peer has no IPv4 overlay subnet".to_owned(),
                }
            } else {
                WireGuardMtuProbe::NotRequested
            },
        ),
    };
    let handshake = peer
        .last_handshake
        .and_then(|timestamp| SystemTime::now().duration_since(timestamp).ok())
        .map_or(WireGuardHandshakeStatus::Never, |age| {
            WireGuardHandshakeStatus::Ago {
                seconds: age.as_secs(),
            }
        });
    Ok(WireGuardPeerStatus {
        public_key,
        endpoint_subnet,
        endpoint: peer.endpoint,
        handshake,
        rtt,
        rx_bytes: peer.rx_bytes,
        tx_bytes: peer.tx_bytes,
        mtu_probe,
    })
}

async fn probe_path_mtu(target: Ipv4Addr) -> WireGuardMtuProbe {
    let mut low = MIN_WIREGUARD_MTU;
    let mut high = MAX_WIREGUARD_MTU;
    if let Err(message) = ping(target, Some(low)).await {
        return WireGuardMtuProbe::Unavailable { message };
    }
    while low < high {
        let candidate = (low + high).div_ceil(2);
        if ping(target, Some(candidate)).await.is_ok() {
            low = candidate;
        } else {
            high = candidate - 1;
        }
    }
    WireGuardMtuProbe::Measured { mtu: low }
}

async fn ping(target: Ipv4Addr, mtu: Option<u32>) -> Result<String, String> {
    let mut command = Command::new("ping");
    command.args(["-n", "-c", "1", "-W", "1"]);
    if let Some(mtu) = mtu {
        command.args(["-M", "do", "-s", &icmp_payload_size(mtu).to_string()]);
    }
    command
        .arg(target.to_string())
        .env("LC_ALL", "C")
        .kill_on_drop(true);
    let output = tokio::time::timeout(Duration::from_secs(2), command.output())
        .await
        .map_err(|_| format!("ping {target} timed out"))?
        .map_err(|error| format!("start ping {target}: {error}"))?;
    if output.status.success() {
        Ok(String::from_utf8_lossy(&output.stdout).into_owned())
    } else {
        Err(format!(
            "ping {target} failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        ))
    }
}

const fn icmp_payload_size(mtu: u32) -> u32 {
    mtu.saturating_sub(28)
}

fn parse_ping_rtt_micros(output: &str) -> Option<u64> {
    let value = output.split("time=").nth(1)?.split_whitespace().next()?;
    let millis = value.trim_end_matches("ms").parse::<f64>().ok()?;
    Some((millis * 1_000.0).round() as u64)
}

async fn read_interface_mtu(ifname: &str) -> Result<u32, String> {
    getifs::interfaces()
        .map_err(|source| source.to_string())?
        .into_iter()
        .find(|interface| interface.name().as_str() == ifname)
        .map(|interface| interface.mtu())
        .ok_or_else(|| format!("interface {ifname} was not found"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_ping_rtt_and_accounts_for_probe_headers() {
        assert_eq!(
            parse_ping_rtt_micros("64 bytes from 10.198.2.254: icmp_seq=1 ttl=64 time=1.234 ms"),
            Some(1_234)
        );
        assert_eq!(icmp_payload_size(1420), 1392);
        assert_eq!(icmp_payload_size(20), 0);
    }
}
