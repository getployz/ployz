//! Shared, read-only MTU selection for Keeper and Docker endpoint networks.

#[cfg(target_os = "linux")]
use futures_util::TryStreamExt;
use ployz_core::network::{MAX_WIREGUARD_MTU, MIN_WIREGUARD_MTU};
#[cfg(target_os = "linux")]
use rtnetlink::{
    RouteMessageBuilder,
    packet_route::{
        link::LinkAttribute,
        route::{RouteAttribute, RouteMetric},
    },
};
#[cfg(target_os = "linux")]
use std::net::Ipv4Addr;

const WIREGUARD_ENCAP_OVERHEAD: u32 = 80;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WireGuardMtu(u32);

impl WireGuardMtu {
    pub(crate) const fn fallback() -> Self {
        Self(MAX_WIREGUARD_MTU)
    }

    pub(crate) const fn get(self) -> u32 {
        self.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WireGuardMtuPolicy {
    Auto,
    Fixed(u32),
}

pub(crate) async fn resolve_wireguard_mtu(policy: WireGuardMtuPolicy, wg_ifname: &str) -> u32 {
    match policy {
        WireGuardMtuPolicy::Fixed(mtu) => mtu,
        WireGuardMtuPolicy::Auto => detect_wireguard_mtu(wg_ifname)
            .await
            .unwrap_or_else(|_| WireGuardMtu::fallback())
            .get(),
    }
}

pub(crate) async fn detect_wireguard_mtu(wg_ifname: &str) -> Result<WireGuardMtu, String> {
    detect_wireguard_mtu_platform(wg_ifname).await
}

#[cfg(target_os = "linux")]
async fn detect_wireguard_mtu_platform(wg_ifname: &str) -> Result<WireGuardMtu, String> {
    let (connection, handle, _) =
        rtnetlink::new_connection().map_err(|source| source.to_string())?;
    tokio::spawn(connection);

    let route = RouteMessageBuilder::<Ipv4Addr>::new()
        .destination_prefix(Ipv4Addr::new(1, 1, 1, 1), 32)
        .build();
    let mut routes = handle.route().get(route).execute();
    let route = routes
        .try_next()
        .await
        .map_err(|source| source.to_string())?
        .ok_or_else(|| "no route to public address".to_owned())?;
    let route_mtu = route.attributes.iter().find_map(|attribute| {
        if let RouteAttribute::Metrics(metrics) = attribute {
            return metrics.iter().find_map(|metric| {
                if let RouteMetric::Mtu(mtu) = metric {
                    Some(*mtu)
                } else {
                    None
                }
            });
        }
        None
    });
    let ifindex = route
        .attributes
        .iter()
        .find_map(|attribute| {
            if let RouteAttribute::Oif(ifindex) = attribute {
                Some(*ifindex)
            } else {
                None
            }
        })
        .ok_or_else(|| "route to public address had no output interface".to_owned())?;
    let link = handle
        .link()
        .get()
        .match_index(ifindex)
        .execute()
        .try_next()
        .await
        .map_err(|source| source.to_string())?
        .ok_or_else(|| format!("output interface {ifindex} was not found"))?;
    let mut link_name = None;
    let mut link_mtu = None;
    for attribute in &link.attributes {
        if let LinkAttribute::IfName(value) = attribute {
            link_name = Some(value.as_str());
        } else if let LinkAttribute::Mtu(value) = attribute {
            link_mtu = Some(*value);
        }
    }
    if link_name == Some(wg_ifname) {
        return Err(format!(
            "egress interface is the WireGuard interface {wg_ifname}"
        ));
    }
    let egress_mtu = route_mtu
        .or(link_mtu)
        .ok_or_else(|| "egress interface MTU was not reported".to_owned())?;
    Ok(wireguard_mtu_from_egress(egress_mtu))
}

#[cfg(not(target_os = "linux"))]
async fn detect_wireguard_mtu_platform(wg_ifname: &str) -> Result<WireGuardMtu, String> {
    let local_addr = getifs::best_local_ipv4_addrs()
        .map_err(|source| source.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "no default IPv4 route".to_owned())?;
    let interface = getifs::interface_by_index(local_addr.index())
        .map_err(|source| source.to_string())?
        .ok_or_else(|| format!("output interface {} was not found", local_addr.index()))?;
    if interface.name().as_str() == wg_ifname {
        return Err(format!(
            "egress interface is the WireGuard interface {wg_ifname}"
        ));
    }
    Ok(wireguard_mtu_from_egress(interface.mtu()))
}

fn wireguard_mtu_from_egress(egress_mtu: u32) -> WireGuardMtu {
    WireGuardMtu(
        egress_mtu
            .saturating_sub(WIREGUARD_ENCAP_OVERHEAD)
            .clamp(MIN_WIREGUARD_MTU, MAX_WIREGUARD_MTU),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wireguard_mtu_from_egress_subtracts_overhead_and_clamps() {
        assert_eq!(wireguard_mtu_from_egress(1500).get(), 1420);
        assert_eq!(wireguard_mtu_from_egress(9000).get(), 1420);
        assert_eq!(wireguard_mtu_from_egress(1300).get(), 1280);
    }
}
