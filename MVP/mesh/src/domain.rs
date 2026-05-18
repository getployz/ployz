use std::fmt::{self, Display, Formatter};
use std::net::Ipv6Addr;

use mvp_bus::IslandId;
use mvp_identity::NodeId;
use mvp_projection::NodeProjection;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::{MeshError, MeshResult};

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct IrohEndpointId(String);

impl IrohEndpointId {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for IrohEndpointId {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WireGuardPublicKey(String);

impl WireGuardPublicKey {
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl Display for WireGuardPublicKey {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct WireGuardOverlayIp(Ipv6Addr);

impl WireGuardOverlayIp {
    #[must_use]
    pub fn new(value: Ipv6Addr) -> Self {
        Self(value)
    }

    #[must_use]
    pub fn address(self) -> Ipv6Addr {
        self.0
    }

    #[must_use]
    pub fn allowed_ip_cidr(self) -> WireGuardOverlayCidr {
        WireGuardOverlayCidr::host(self)
    }
}

impl Display for WireGuardOverlayIp {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        Display::fmt(&self.0, f)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct WireGuardOverlayCidr(WireGuardOverlayIp);

impl WireGuardOverlayCidr {
    #[must_use]
    pub fn host(ip: WireGuardOverlayIp) -> Self {
        Self(ip)
    }
}

impl Display for WireGuardOverlayCidr {
    fn fmt(&self, f: &mut Formatter<'_>) -> fmt::Result {
        write!(f, "{}/128", self.0)
    }
}

impl Serialize for WireGuardOverlayCidr {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(&self.to_string())
    }
}

impl<'de> Deserialize<'de> for WireGuardOverlayCidr {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        let Some((address, prefix)) = value.rsplit_once('/') else {
            return Err(serde::de::Error::custom(
                "wireguard overlay CIDR must include a /128 prefix",
            ));
        };
        if prefix != "128" {
            return Err(serde::de::Error::custom(
                "wireguard overlay CIDR only supports /128 host routes",
            ));
        }
        let address = address.parse::<Ipv6Addr>().map_err(|source| {
            serde::de::Error::custom(format!("invalid wireguard overlay address: {source}"))
        })?;
        Ok(Self::host(WireGuardOverlayIp::new(address)))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MeshNode {
    pub node_id: NodeId,
    pub epoch: u64,
    pub iroh_endpoint_id: IrohEndpointId,
    pub wg_public_key: WireGuardPublicKey,
    pub overlay_ip: WireGuardOverlayIp,
}

impl MeshNode {
    pub fn from_projection(node: &NodeProjection) -> MeshResult<Self> {
        if node.iroh_endpoint_id.is_empty() {
            return Err(MeshError::MissingMeshIdentity {
                node_id: node.node_id.clone(),
                field: "iroh_endpoint_id",
            });
        }
        if node.wg_public_key.is_empty() {
            return Err(MeshError::MissingMeshIdentity {
                node_id: node.node_id.clone(),
                field: "wg_public_key",
            });
        }
        let overlay_ip =
            node.overlay_ip
                .parse::<Ipv6Addr>()
                .map_err(|source| MeshError::InvalidOverlayIp {
                    node_id: node.node_id.clone(),
                    overlay_ip: node.overlay_ip.clone(),
                    source,
                })?;
        Ok(Self {
            node_id: node.node_id.clone(),
            epoch: node.epoch,
            iroh_endpoint_id: IrohEndpointId::new(node.iroh_endpoint_id.clone()),
            wg_public_key: WireGuardPublicKey::new(node.wg_public_key.clone()),
            overlay_ip: WireGuardOverlayIp::new(overlay_ip),
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineInvite {
    pub island: IslandId,
    pub bootstrap_endpoint: IrohEndpointId,
    pub invite_id: crate::InviteId,
    pub secret: crate::InviteSecret,
    pub expires_at_ms: u64,
}

impl MachineInvite {
    #[must_use]
    pub fn is_expired(&self, now_ms: u64) -> bool {
        now_ms >= self.expires_at_ms
    }
}

#[must_use]
pub fn derive_overlay_ip(island: &IslandId, node_id: &NodeId) -> WireGuardOverlayIp {
    let input = format!("{}:{}", island.as_str(), node_id.as_str());
    let hash = blake3::hash(input.as_bytes());
    let bytes = hash.as_bytes();
    WireGuardOverlayIp::new(Ipv6Addr::from([
        0xfd, bytes[1], bytes[2], bytes[3], bytes[4], bytes[5], bytes[6], bytes[7], bytes[8],
        bytes[9], bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
    ]))
}

#[cfg(test)]
mod tests {
    use super::{MeshNode, WireGuardOverlayCidr, WireGuardOverlayIp, derive_overlay_ip};
    use mvp_bus::IslandId;
    use mvp_identity::NodeId;
    use mvp_projection::NodeProjection;

    #[test]
    fn overlay_derivation_is_stable_and_island_scoped() {
        let island = IslandId::new("prod");
        let node = NodeId::new("node-1");

        let first = derive_overlay_ip(&island, &node);
        let second = derive_overlay_ip(&island, &node);
        let other_island = derive_overlay_ip(&IslandId::new("dev"), &node);

        assert_eq!(first, second);
        assert_ne!(first, other_island);
        assert!(first.to_string().starts_with("fd"));
    }

    #[test]
    fn mesh_node_validates_projection_identity() {
        let node = NodeProjection {
            node_id: NodeId::new("node-1"),
            epoch: 1,
            overlay_ip: "fd00::1".to_string(),
            iroh_endpoint_id: "iroh-node-1".to_string(),
            wg_public_key: "wg-node-1".to_string(),
        };

        let mesh = MeshNode::from_projection(&node).expect("mesh node validates");

        assert_eq!(mesh.node_id, NodeId::new("node-1"));
        assert_eq!(mesh.overlay_ip.allowed_ip_cidr().to_string(), "fd00::1/128");
    }

    #[test]
    fn overlay_cidr_json_round_trips_as_cidr_string() {
        let cidr = WireGuardOverlayIp::new("fd00::1".parse().expect("test ip")).allowed_ip_cidr();

        let json = serde_json::to_string(&cidr).expect("cidr serializes");

        assert_eq!(json, r#""fd00::1/128""#);
        let decoded: WireGuardOverlayCidr = serde_json::from_str(&json).expect("cidr deserializes");
        assert_eq!(decoded, cidr);
        assert!(serde_json::from_str::<WireGuardOverlayCidr>(r#""fd00::1""#).is_err());
        assert!(serde_json::from_str::<WireGuardOverlayCidr>(r#""fd00::1/64""#).is_err());
    }
}
