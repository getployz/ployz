use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::fmt::{self, Display, Formatter};
use std::net::Ipv4Addr;

use ipnet::Ipv4Net;
use mvp_bus::IslandId;
use mvp_identity::NodeId;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{MeshError, MeshResult};

const CONTAINER_CLUSTER: &str = "10.210.0.0/16";
const CONTAINER_SUBNET_PREFIX_LEN: u8 = 24;
const CONTAINER_SUBNET_COUNT: u32 = 1 << (CONTAINER_SUBNET_PREFIX_LEN - cluster_prefix_len());

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ContainerSubnet(Ipv4Net);

impl ContainerSubnet {
    pub fn new(value: Ipv4Net) -> MeshResult<Self> {
        if value.prefix_len() != CONTAINER_SUBNET_PREFIX_LEN {
            return Err(MeshError::InvalidContainerSubnet {
                subnet: value.to_string(),
                message: format!("expected /{CONTAINER_SUBNET_PREFIX_LEN}"),
            });
        }
        let cluster = container_cluster();
        if !cluster.contains(&value.network()) || !cluster.contains(&value.broadcast()) {
            return Err(MeshError::InvalidContainerSubnet {
                subnet: value.to_string(),
                message: format!("outside container cluster {cluster}"),
            });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_net(self) -> Ipv4Net {
        self.0
    }

    #[must_use]
    pub fn docker_ipam_subnet(self) -> String {
        self.0.to_string()
    }

    #[must_use]
    pub fn docker_gateway_ip(self) -> ContainerIp {
        self.host_ip(1)
    }

    #[must_use]
    pub fn first_service_ip(self) -> ContainerIp {
        self.host_ip(2)
    }

    #[must_use]
    pub fn wireguard_allowed_cidr(self) -> String {
        self.0.to_string()
    }

    fn host_ip(self, offset: u32) -> ContainerIp {
        ContainerIp(Ipv4Addr::from(u32::from(self.0.network()) + offset))
    }
}

impl Display for ContainerSubnet {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

impl Serialize for ContainerSubnet {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for ContainerSubnet {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let subnet = value.parse::<Ipv4Net>().map_err(|source| {
            serde::de::Error::custom(format!("invalid container subnet CIDR: {source}"))
        })?;
        Self::new(subnet).map_err(serde::de::Error::custom)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct ContainerIp(Ipv4Addr);

impl ContainerIp {
    #[must_use]
    pub fn new(value: Ipv4Addr) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn address(self) -> Ipv4Addr {
        self.0
    }
}

impl Display for ContainerIp {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

#[must_use]
pub fn derive_container_subnet(island: &IslandId, node_id: &NodeId) -> ContainerSubnet {
    let index = container_subnet_index(island, node_id);
    container_subnet_at(index).expect("derived container subnet index is in range")
}

pub fn derive_container_subnet_plan(
    island: &IslandId,
    node_ids: impl IntoIterator<Item = NodeId>,
) -> MeshResult<BTreeMap<NodeId, ContainerSubnet>> {
    let nodes = node_ids.into_iter().collect::<BTreeSet<_>>();
    let mut occupied = HashSet::new();
    let mut assignments = BTreeMap::new();
    for node_id in nodes {
        let start = container_subnet_index(island, &node_id);
        let subnet = first_available_subnet(start, &occupied)?;
        occupied.insert(subnet.as_net());
        assignments.insert(node_id, subnet);
    }
    Ok(assignments)
}

fn first_available_subnet(
    start_index: u32,
    occupied: &HashSet<Ipv4Net>,
) -> MeshResult<ContainerSubnet> {
    for offset in 0..CONTAINER_SUBNET_COUNT {
        let index = (start_index + offset) % CONTAINER_SUBNET_COUNT;
        let subnet = container_subnet_at(index)
            .expect("container subnet scan index is bounded by subnet count");
        if !occupied.contains(&subnet.as_net()) {
            return Ok(subnet);
        }
    }
    Err(MeshError::ContainerSubnetExhausted {
        cluster: container_cluster().to_string(),
        prefix_len: CONTAINER_SUBNET_PREFIX_LEN,
    })
}

fn container_subnet_index(island: &IslandId, node_id: &NodeId) -> u32 {
    let input = format!("{}:{}", island.as_str(), node_id.as_str());
    let hash = blake3::hash(input.as_bytes());
    let bytes = hash.as_bytes();
    u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]) % CONTAINER_SUBNET_COUNT
}

fn container_subnet_at(index: u32) -> MeshResult<ContainerSubnet> {
    if index >= CONTAINER_SUBNET_COUNT {
        return Err(MeshError::ContainerSubnetExhausted {
            cluster: container_cluster().to_string(),
            prefix_len: CONTAINER_SUBNET_PREFIX_LEN,
        });
    }
    let cluster = container_cluster();
    let network = Ipv4Addr::from(u32::from(cluster.network()) + (index << 8));
    ContainerSubnet::new(
        Ipv4Net::new(network, CONTAINER_SUBNET_PREFIX_LEN)
            .expect("derived container subnet is valid"),
    )
}

fn container_cluster() -> Ipv4Net {
    CONTAINER_CLUSTER
        .parse()
        .expect("container cluster CIDR is valid")
}

const fn cluster_prefix_len() -> u8 {
    16
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use mvp_bus::IslandId;
    use mvp_identity::NodeId;

    use super::{ContainerSubnet, derive_container_subnet, derive_container_subnet_plan};

    #[test]
    fn container_subnet_derivation_is_stable_and_node_scoped() {
        let island = IslandId::new("prod");
        let node = NodeId::new("node-a");

        let first = derive_container_subnet(&island, &node);
        let second = derive_container_subnet(&island, &node);
        let other_node = derive_container_subnet(&island, &NodeId::new("node-b"));

        assert_eq!(first, second);
        assert_ne!(first, other_node);
        assert!(first.to_string().starts_with("10.210."));
    }

    #[test]
    fn container_subnet_plan_assigns_unique_subnets_for_representative_nodes() {
        let plan = derive_container_subnet_plan(
            &IslandId::new("prod"),
            (0..64).map(|index| NodeId::new(format!("node-{index}"))),
        )
        .expect("subnet plan");

        let unique = plan
            .values()
            .map(|subnet| subnet.as_net())
            .collect::<HashSet<_>>();

        assert_eq!(plan.len(), 64);
        assert_eq!(unique.len(), 64);
    }

    #[test]
    fn container_subnet_exposes_docker_and_wireguard_values() {
        let subnet: ContainerSubnet =
            ContainerSubnet::new("10.210.42.0/24".parse().expect("valid container subnet"))
                .expect("container subnet");

        assert_eq!(subnet.docker_ipam_subnet(), "10.210.42.0/24");
        assert_eq!(subnet.docker_gateway_ip().to_string(), "10.210.42.1");
        assert_eq!(subnet.first_service_ip().to_string(), "10.210.42.2");
        assert_eq!(subnet.wireguard_allowed_cidr(), "10.210.42.0/24");
    }

    #[test]
    fn container_subnet_rejects_wrong_cluster_or_prefix() {
        assert!(ContainerSubnet::new("10.210.1.0/25".parse().expect("valid cidr")).is_err());
        assert!(ContainerSubnet::new("10.211.1.0/24".parse().expect("valid cidr")).is_err());
    }

    #[test]
    fn typed_container_subnet_and_ip_json_round_trip_as_strings() {
        let subnet = ContainerSubnet::new("10.210.7.0/24".parse().expect("valid cidr"))
            .expect("container subnet");
        let subnet_json = serde_json::to_string(&subnet).expect("subnet serializes");
        let ip_json = serde_json::to_string(&subnet.first_service_ip()).expect("ip serializes");

        assert_eq!(subnet_json, r#""10.210.7.0/24""#);
        assert_eq!(ip_json, r#""10.210.7.2""#);
        assert_eq!(
            serde_json::from_str::<ContainerSubnet>(&subnet_json).expect("subnet deserializes"),
            subnet
        );
    }
}
