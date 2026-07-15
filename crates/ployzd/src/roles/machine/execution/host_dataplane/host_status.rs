use defguard_wireguard_rs::{WGApi, WireguardInterfaceApi, peer::Peer};
use futures_util::{StreamExt, TryStreamExt, stream};
use ipnet::Ipv4Net;
use ployz_core::dataplane::{
    MachineEndpointSubnet, NetworkStatusMode, WireGuardConfiguredMtu, WireGuardDetectedMtu,
    WireGuardHandshakeStatus, WireGuardInterfaceMtu, WireGuardMtuProbe,
    WireGuardPeerEndpointSubnet, WireGuardPeerStatus, WireGuardPublicKey, WireGuardRttStatus,
    WireGuardStatus,
};
use std::time::SystemTime;
use tokio::time::Instant;

use super::host_network::{
    HostWireGuardApi, WireGuardMtuPolicy, detect_wireguard_mtu, probe_wireguard_path_mtu,
    probe_wireguard_rtt, wireguard_host_ipv4,
};
const MAX_CONCURRENT_PEER_DIAGNOSTICS: usize = 4;

// WireGuard peer diagnostics finish before the machine endpoint so the host
// can still inspect eBPF attachment state and serialize the complete testimony.
pub(crate) const fn dataplane_status_budget(mode: NetworkStatusMode) -> std::time::Duration {
    match mode {
        NetworkStatusMode::Snapshot => std::time::Duration::from_secs(20),
        NetworkStatusMode::ProbePathMtu => std::time::Duration::from_secs(50),
    }
}

pub(super) async fn read_wireguard_status(
    wg_ifname: &str,
    policy: WireGuardMtuPolicy,
    mode: NetworkStatusMode,
) -> Result<WireGuardStatus, String> {
    let deadline = Instant::now() + dataplane_status_budget(mode);
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
    let (interface_mtu_value, interface_mtu) = match read_interface_mtu(wg_ifname).await {
        Ok(mtu) => (Some(mtu), WireGuardInterfaceMtu::Detected { mtu }),
        Err(message) => (None, WireGuardInterfaceMtu::Unavailable { message }),
    };
    let peers = collect_peer_statuses(
        wg_ifname,
        interface_mtu_value,
        mode,
        deadline,
        host.peers
            .into_iter()
            .map(|(key, peer)| (key.to_string(), peer)),
    )
    .await?;

    Ok(WireGuardStatus {
        interface: wg_ifname.to_owned(),
        configured_mtu,
        detected_mtu,
        interface_mtu,
        peers,
    })
}

async fn collect_peer_statuses(
    wg_ifname: &str,
    interface_mtu: Option<u32>,
    mode: NetworkStatusMode,
    deadline: Instant,
    peers: impl IntoIterator<Item = (String, Peer)>,
) -> Result<Vec<WireGuardPeerStatus>, String> {
    stream::iter(
        peers
            .into_iter()
            .map(|(key, peer)| peer_status(wg_ifname, interface_mtu, key, peer, mode, deadline)),
    )
    .buffered(MAX_CONCURRENT_PEER_DIAGNOSTICS)
    .try_collect()
    .await
}

async fn peer_status(
    wg_ifname: &str,
    interface_mtu: Option<u32>,
    public_key: String,
    peer: Peer,
    mode: NetworkStatusMode,
    deadline: Instant,
) -> Result<WireGuardPeerStatus, String> {
    let public_key = WireGuardPublicKey::try_new(public_key).map_err(|error| error.to_string())?;
    let endpoint_subnet = peer_endpoint_subnet(peer.allowed_ips.first().map(ToString::to_string));
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
            let rtt =
                match tokio::time::timeout_at(deadline, probe_wireguard_rtt(wg_ifname, target))
                    .await
                {
                    Ok(Ok(micros)) => WireGuardRttStatus::Measured { micros },
                    Ok(Err(message)) => WireGuardRttStatus::Unavailable { message },
                    Err(_) => WireGuardRttStatus::Unavailable {
                        message: "peer RTT probe exceeded the network status budget".to_owned(),
                    },
                };
            let mtu_probe = if mode.probes_path_mtu() {
                match interface_mtu {
                    Some(interface_mtu) => match tokio::time::timeout_at(
                        deadline,
                        probe_wireguard_path_mtu(wg_ifname, target, interface_mtu),
                    )
                    .await
                    {
                        Ok(Ok(mtu)) => WireGuardMtuProbe::Measured { mtu },
                        Ok(Err(message)) => WireGuardMtuProbe::Unavailable { message },
                        Err(_) => WireGuardMtuProbe::Unavailable {
                            message: "peer path MTU probe exceeded the network status budget"
                                .to_owned(),
                        },
                    },
                    None => WireGuardMtuProbe::Unavailable {
                        message: "WireGuard interface MTU is unavailable".to_owned(),
                    },
                }
            } else {
                WireGuardMtuProbe::NotRequested
            };
            (rtt, mtu_probe)
        }
        None => (
            WireGuardRttStatus::Unavailable {
                message: "peer has no IPv4 overlay subnet".to_owned(),
            },
            if mode.probes_path_mtu() {
                WireGuardMtuProbe::Unavailable {
                    message: "peer has no IPv4 overlay subnet".to_owned(),
                }
            } else {
                WireGuardMtuProbe::NotRequested
            },
        ),
    };
    let handshake = handshake_status(peer.last_handshake);
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

fn handshake_status(last_handshake: Option<SystemTime>) -> WireGuardHandshakeStatus {
    last_handshake
        .filter(|timestamp| *timestamp != SystemTime::UNIX_EPOCH)
        .and_then(|timestamp| SystemTime::now().duration_since(timestamp).ok())
        .map_or(WireGuardHandshakeStatus::Never, |age| {
            WireGuardHandshakeStatus::Ago {
                seconds: age.as_secs(),
            }
        })
}

fn peer_endpoint_subnet(value: Option<String>) -> WireGuardPeerEndpointSubnet {
    let Some(value) = value else {
        return WireGuardPeerEndpointSubnet::Missing;
    };
    MachineEndpointSubnet::try_new(value.clone()).map_or_else(
        |error| WireGuardPeerEndpointSubnet::Invalid {
            value,
            message: error.to_string(),
        },
        |subnet| WireGuardPeerEndpointSubnet::Valid { subnet },
    )
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
    use defguard_wireguard_rs::{key::Key, net::IpAddrMask};
    use std::net::{IpAddr, Ipv4Addr};

    fn peer_with_overlay_subnet() -> Peer {
        let mut peer = Peer::new(Key::generate());
        peer.allowed_ips = vec![IpAddrMask::new(
            IpAddr::V4(Ipv4Addr::new(10, 198, 2, 0)),
            24,
        )];
        peer
    }

    #[test]
    fn peer_endpoint_subnet_preserves_invalid_observed_values() {
        let status = peer_endpoint_subnet(Some("not-a-subnet".to_owned()));

        assert!(matches!(
            status,
            WireGuardPeerEndpointSubnet::Invalid { value, .. } if value == "not-a-subnet"
        ));
    }

    #[test]
    fn zero_wireguard_handshake_timestamp_is_never() {
        assert_eq!(
            handshake_status(Some(SystemTime::UNIX_EPOCH)),
            WireGuardHandshakeStatus::Never
        );
    }

    #[tokio::test(start_paused = true)]
    async fn expired_probe_budget_returns_every_peer_with_typed_unavailability() {
        let peer = peer_with_overlay_subnet();
        let public_key = peer.public_key.to_string();
        let deadline = Instant::now();
        tokio::time::advance(std::time::Duration::from_secs(1)).await;

        let statuses = collect_peer_statuses(
            "ployz0",
            Some(1420),
            NetworkStatusMode::ProbePathMtu,
            deadline,
            (0..200).map(|_| (public_key.clone(), peer.clone())),
        )
        .await
        .expect("expired diagnostics remain status testimony");

        assert!(
            statuses.len() == 200
                && statuses.iter().all(|status| {
                    matches!(status.rtt, WireGuardRttStatus::Unavailable { .. })
                        && matches!(status.mtu_probe, WireGuardMtuProbe::Unavailable { .. })
                })
        );
    }
}
