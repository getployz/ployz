//! Docker bridge capacity and host telemetry probes.

use std::{
    collections::{HashMap, HashSet},
    net::IpAddr,
    time::{SystemTime, UNIX_EPOCH},
};

use bollard::{models::IpamConfig, query_parameters::ListContainersOptionsBuilder};
use ipnet::IpNet;
use ployz_core::{BridgeEndpointCapacity, ByteCapacity, MachineTelemetry};

use super::{Error, LocalDocker};

impl LocalDocker {
    pub(super) async fn bridge_capacity(&self) -> Result<BridgeEndpointCapacity, Error> {
        let network = self
            .client
            .inspect_network(crate::network::DOCKER_NETWORK_NAME, None)
            .await?;
        let usable_endpoints = usable_endpoint_count(
            network
                .ipam
                .and_then(|ipam| ipam.config)
                .as_deref()
                .ok_or(Error::MissingField("Docker bridge IPAM configuration"))?,
        )?;
        let attached_endpoints = self
            .client
            .list_containers(Some(
                ListContainersOptionsBuilder::default()
                    .all(true)
                    .filters(&HashMap::from([(
                        "network",
                        vec![crate::network::DOCKER_NETWORK_NAME],
                    )]))
                    .build(),
            ))
            .await?
            .len() as u64;
        Ok(BridgeEndpointCapacity::new(
            usable_endpoints,
            attached_endpoints,
        ))
    }

    pub(super) async fn telemetry(&self) -> Result<MachineTelemetry, Error> {
        let info = self.client.info().await?;
        let cpu_count = logical_cpu_count(info.ncpu)?;
        let docker_root = info
            .docker_root_dir
            .ok_or(Error::MissingField("Docker root directory"))?;
        let (docker_root_total_bytes, docker_root_free_bytes) = filesystem_space(&docker_root)?;
        let (memory_total_bytes, memory_available_bytes) = memory()?;
        let memory = ByteCapacity::parse(memory_total_bytes, memory_available_bytes)
            .map_err(|_| Error::MissingField("consistent host memory capacity"))?;
        let docker_root = ByteCapacity::parse(docker_root_total_bytes, docker_root_free_bytes)
            .map_err(|_| Error::MissingField("consistent Docker-root capacity"))?;
        Ok(MachineTelemetry::new(
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .map_or(0, |duration| duration.as_secs()),
            self.managed_container_ids().await?.len() as u64,
            cpu_count,
            load_average_milli()?,
            memory,
            docker_root,
        ))
    }
}

fn logical_cpu_count(ncpu: Option<i64>) -> Result<u64, Error> {
    ncpu.filter(|count| *count > 0)
        .and_then(|count| u64::try_from(count).ok())
        .ok_or(Error::MissingField("host logical CPU count"))
}

fn usable_endpoint_count(configs: &[IpamConfig]) -> Result<u64, Error> {
    if configs.is_empty() {
        return Err(Error::MissingField("Docker bridge IPAM configuration"));
    }

    let mut ipv4 = None;
    let mut ipv6 = None;
    for config in configs {
        let (family, usable) = usable_endpoints(config)?;
        let total = match family {
            IpAddr::V4(_) => &mut ipv4,
            IpAddr::V6(_) => &mut ipv6,
        };
        *total = Some(
            total
                .unwrap_or(0_u64)
                .checked_add(usable)
                .ok_or(Error::MissingField("representable Docker bridge capacity"))?,
        );
    }
    match (ipv4, ipv6) {
        (Some(ipv4), Some(ipv6)) => Ok(ipv4.min(ipv6)),
        (Some(usable), None) | (None, Some(usable)) => Ok(usable),
        (None, None) => Err(Error::MissingField("Docker bridge IPAM configuration")),
    }
}

fn usable_endpoints(config: &IpamConfig) -> Result<(IpAddr, u64), Error> {
    let invalid = || Error::MissingField("usable Docker bridge IPAM configuration");
    let subnet_text = config.subnet.as_deref().ok_or_else(invalid)?;
    let subnet = subnet_text.parse::<IpNet>().map_err(|_| invalid())?;
    let pool = config
        .ip_range
        .as_deref()
        .unwrap_or(subnet_text)
        .parse::<IpNet>()
        .map_err(|_| invalid())?;
    if !subnet.contains(&pool) {
        return Err(invalid());
    }

    let gateway = config
        .gateway
        .as_deref()
        .ok_or_else(invalid)?
        .parse::<IpAddr>()
        .map_err(|_| invalid())?;
    let auxiliary = config
        .auxiliary_addresses
        .iter()
        .flat_map(|addresses| addresses.values())
        .map(|address| address.parse::<IpAddr>().map_err(|_| invalid()))
        .collect::<Result<HashSet<_>, _>>()?;
    if !subnet.contains(&gateway) || auxiliary.iter().any(|address| !subnet.contains(address)) {
        return Err(invalid());
    }

    let mut reserved = auxiliary;
    reserved.insert(gateway);
    if let IpNet::V4(subnet) = subnet
        && subnet.prefix_len() < 31
    {
        reserved.insert(IpAddr::V4(subnet.network()));
        reserved.insert(IpAddr::V4(subnet.broadcast()));
    }
    let reserved = reserved
        .iter()
        .filter(|address| pool.contains(*address))
        .count() as u128;
    let usable = address_count(pool)
        .and_then(|addresses| addresses.checked_sub(reserved))
        .and_then(|addresses| addresses.try_into().ok())
        .ok_or_else(invalid)?;
    Ok((gateway, usable))
}

fn address_count(pool: IpNet) -> Option<u128> {
    match pool {
        IpNet::V4(pool) => Some(1_u128 << (32 - pool.prefix_len())),
        IpNet::V6(pool) => 1_u128.checked_shl((128 - pool.prefix_len()).into()),
    }
}

fn filesystem_space(path: &str) -> Result<(u64, u64), Error> {
    let stat = nix::sys::statvfs::statvfs(path).map_err(std::io::Error::other)?;
    Ok((
        stat.blocks().saturating_mul(stat.fragment_size()),
        stat.blocks_available().saturating_mul(stat.fragment_size()),
    ))
}

fn memory() -> Result<(u64, u64), Error> {
    let memory = std::fs::read_to_string("/proc/meminfo")?;
    let value = |key| {
        memory
            .lines()
            .find_map(|line| {
                line.strip_prefix(key)
                    .and_then(|line| line.split_whitespace().next()?.parse::<u64>().ok())
            })
            .map(|kib| kib * 1024)
            .ok_or(Error::MissingField(key))
    };
    Ok((value("MemTotal:")?, value("MemAvailable:")?))
}

fn load_average_milli() -> Result<u64, Error> {
    let load = std::fs::read_to_string("/proc/loadavg")?;
    let load = load
        .split_whitespace()
        .next()
        .ok_or(Error::MissingField("load average"))?;
    Ok((load
        .parse::<f64>()
        .map_err(|_| Error::MissingField("load average"))?
        * 1000.0) as u64)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use super::*;

    #[test]
    fn host_logical_cpu_count_requires_a_positive_docker_value() {
        assert_eq!(logical_cpu_count(Some(8)).unwrap(), 8);
        for count in [None, Some(0), Some(-1)] {
            assert!(matches!(
                logical_cpu_count(count),
                Err(Error::MissingField("host logical CPU count"))
            ));
        }
    }

    fn ipam(
        subnet: &str,
        ip_range: Option<&str>,
        gateway: &str,
        auxiliary: &[(&str, &str)],
    ) -> IpamConfig {
        IpamConfig {
            subnet: Some(subnet.into()),
            ip_range: ip_range.map(Into::into),
            gateway: Some(gateway.into()),
            auxiliary_addresses: (!auxiliary.is_empty()).then(|| {
                auxiliary
                    .iter()
                    .map(|(name, address)| ((*name).into(), (*address).into()))
                    .collect::<HashMap<_, _>>()
            }),
        }
    }

    #[test]
    fn bridge_capacity_comes_from_live_subnets_and_attachments() {
        let attachments = ["endpoint-a", "endpoint-b"];
        let usable = usable_endpoint_count(&[
            ipam("10.0.0.0/29", None, "10.0.0.1", &[]),
            ipam("10.0.1.0/30", None, "10.0.1.1", &[]),
        ])
        .unwrap();
        let capacity = BridgeEndpointCapacity::new(usable, attachments.len() as u64);

        assert_eq!(capacity.usable_endpoints(), 6);
        assert_eq!(capacity.attached_endpoints(), 2);
        assert_eq!(capacity.free_endpoints(), 4);
    }

    #[test]
    fn a_full_live_bridge_has_no_free_capacity() {
        let attachments = ["a", "b", "c", "d", "e"];
        let usable = usable_endpoint_count(&[ipam("10.0.0.0/29", None, "10.0.0.1", &[])]).unwrap();
        let capacity = BridgeEndpointCapacity::new(usable, attachments.len() as u64);

        assert_eq!(capacity.usable_endpoints(), 5);
        assert_eq!(capacity.attached_endpoints(), 5);
        assert_eq!(capacity.free_endpoints(), 0);
    }

    #[test]
    fn restricted_range_and_live_reservations_limit_capacity() {
        let usable = usable_endpoint_count(&[ipam(
            "10.0.0.0/24",
            Some("10.0.0.64/28"),
            "10.0.0.65",
            &[("router", "10.0.0.66"), ("outside-range", "10.0.0.10")],
        )])
        .unwrap();

        assert_eq!(usable, 14);
    }

    #[test]
    fn unusable_ipam_is_not_reported_as_capacity() {
        assert!(usable_endpoint_count(&[]).is_err());
        assert!(usable_endpoint_count(&[IpamConfig::default()]).is_err());
        assert!(usable_endpoint_count(&[ipam("10.0.0.0/24", None, "fd00::1", &[])]).is_err());
        assert!(
            usable_endpoint_count(&[ipam("10.0.0.0/24", Some("10.0.1.0/28"), "10.0.0.1", &[],)])
                .is_err()
        );
    }

    #[test]
    fn ipv6_capacity_uses_ipv6_address_rules() {
        let usable = usable_endpoint_count(&[ipam("fd00::/120", None, "fd00::1", &[])]).unwrap();

        assert_eq!(usable, 255);
    }

    #[test]
    fn dual_stack_capacity_is_limited_by_the_smaller_family() {
        let usable = usable_endpoint_count(&[
            ipam("10.0.0.0/24", None, "10.0.0.1", &[]),
            ipam("fd00::/120", None, "fd00::1", &[]),
        ])
        .unwrap();

        assert_eq!(usable, 253);
    }
}
