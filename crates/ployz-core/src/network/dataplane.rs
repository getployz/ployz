//! Dataplane preparation models.

use base64::Engine as _;
use ipnet::{IpNet, Ipv4Net};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::fmt;
use std::net::Ipv4Addr;
use std::str::FromStr;

pub const DEFAULT_WIREGUARD_LISTEN_PORT: u16 = 51820;
pub const MIN_WIREGUARD_MTU: u32 = 1280;
pub const MAX_WIREGUARD_MTU: u32 = 1420;
/// A previously-established peer silent beyond this age is not healthy.
pub const MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS: u64 = 275;
pub const DEFAULT_ENDPOINT_SUPERNET: &str = "10.210.0.0/16";

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct MachineEndpointSupernet(Ipv4Net);

impl MachineEndpointSupernet {
    pub fn try_new(value: impl AsRef<str>) -> Result<Self, MachineEndpointSupernetError> {
        let value = value.as_ref();
        let net = value
            .parse::<Ipv4Net>()
            .map_err(|_| MachineEndpointSupernetError::Invalid {
                value: value.to_owned(),
            })?;
        if net.prefix_len() != 16 || net.addr() != net.network() {
            return Err(MachineEndpointSupernetError::Invalid {
                value: value.to_owned(),
            });
        }
        Ok(Self(net))
    }

    #[must_use]
    pub fn default_v1() -> Self {
        Self::try_new(DEFAULT_ENDPOINT_SUPERNET).expect("default endpoint supernet is valid")
    }

    pub fn allocate_next(
        &self,
        assigned: impl IntoIterator<Item = MachineEndpointSubnet>,
    ) -> Result<MachineEndpointSubnet, MachineEndpointSubnetAllocationError> {
        let assigned = assigned.into_iter().collect::<BTreeSet<_>>();
        let octets = self.0.network().octets();
        for third_octet in 0..=u8::MAX {
            let candidate = MachineEndpointSubnet::try_new(format!(
                "{}.{}.{}.0/24",
                octets[0], octets[1], third_octet
            ))
            .expect("candidate from /16 supernet is a valid /24");
            if !assigned.contains(&candidate) {
                return Ok(candidate);
            }
        }
        Err(MachineEndpointSubnetAllocationError::Exhausted {
            supernet: self.as_string(),
        })
    }

    #[must_use]
    pub fn as_string(&self) -> String {
        self.0.to_string()
    }

    #[must_use]
    pub fn contains_subnet(&self, subnet: &MachineEndpointSubnet) -> bool {
        let IpNet::V4(subnet) = subnet.ipnet() else {
            unreachable!("machine endpoint subnets are always IPv4");
        };
        self.0.contains(&subnet.network()) && self.0.contains(&subnet.broadcast())
    }

    #[must_use]
    pub fn contains_ipv4(&self, address: Ipv4Addr) -> bool {
        self.0.contains(&address)
    }
}

impl fmt::Debug for MachineEndpointSupernet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("MachineEndpointSupernet")
            .field(&self.0.to_string())
            .finish()
    }
}

impl TryFrom<String> for MachineEndpointSupernet {
    type Error = MachineEndpointSupernetError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<MachineEndpointSupernet> for String {
    fn from(value: MachineEndpointSupernet) -> Self {
        value.0.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineEndpointSupernetError {
    #[error("machine endpoint supernet {value:?} is not an IPv4 /16 network")]
    Invalid { value: String },
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineEndpointSubnetAllocationError {
    #[error("machine endpoint supernet {supernet} has no free /24 subnets")]
    Exhausted { supernet: String },
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct MachineEndpointSubnet(IpNet);

impl MachineEndpointSubnet {
    pub fn try_new(value: impl AsRef<str>) -> Result<Self, MachineEndpointSubnetError> {
        let value = value.as_ref();
        let subnet = IpNet::from_str(value).map_err(|_| MachineEndpointSubnetError::Invalid {
            value: value.to_owned(),
        })?;
        match subnet {
            IpNet::V4(subnet) if subnet.prefix_len() == 24 && subnet.addr() == subnet.network() => {
                Ok(Self(IpNet::V4(subnet)))
            }
            IpNet::V4(_) | IpNet::V6(_) => Err(MachineEndpointSubnetError::Invalid {
                value: value.to_owned(),
            }),
        }
    }

    #[must_use]
    pub const fn ipnet(&self) -> IpNet {
        self.0
    }

    #[must_use]
    pub fn as_string(&self) -> String {
        self.0.to_string()
    }

    /// The subnet's last host (`.254`): the machine's own WireGuard address
    /// in its endpoint /24, assigned by the dataplane as the mesh-reachable
    /// host identity (see `wireguard_host_cidr`) — the image registry and any
    /// machine-addressed mesh listener bind here.
    #[must_use]
    pub fn host_address(&self) -> Ipv4Addr {
        let IpNet::V4(subnet) = self.0 else {
            unreachable!("machine endpoint subnet construction accepts only IPv4")
        };
        let mut octets = subnet.network().octets();
        octets[3] = 254;
        Ipv4Addr::from(octets)
    }

    /// The subnet's first host: Docker assigns it as the `ployz` bridge
    /// gateway, so it is the machine-local resolver's bind address.
    #[must_use]
    pub fn bridge_gateway_ipv4(&self) -> Ipv4Addr {
        let IpNet::V4(network) = self.0 else {
            unreachable!("MachineEndpointSubnet::try_new admits only IPv4 /24 networks");
        };
        Ipv4Addr::from(u32::from(network.network()) + 1)
    }
}

impl fmt::Debug for MachineEndpointSubnet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("MachineEndpointSubnet")
            .field(&self.0.to_string())
            .finish()
    }
}

impl TryFrom<String> for MachineEndpointSubnet {
    type Error = MachineEndpointSubnetError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<MachineEndpointSubnet> for String {
    fn from(value: MachineEndpointSubnet) -> Self {
        value.0.to_string()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum MachineEndpointSubnetError {
    #[error("machine endpoint subnet {value:?} is not an IPv4 /24 network")]
    Invalid { value: String },
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(try_from = "String", into = "String")]
pub struct WireGuardPublicKey(String);

impl WireGuardPublicKey {
    pub fn try_new(value: impl Into<String>) -> Result<Self, WireGuardPublicKeyError> {
        let value = value.into();
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&value)
            .map_err(|_| WireGuardPublicKeyError::InvalidEncoding {
                value: value.clone(),
            })?;
        if decoded.len() != 32 {
            return Err(WireGuardPublicKeyError::InvalidLength {
                value,
                decoded_bytes: decoded.len(),
            });
        }
        let canonical = base64::engine::general_purpose::STANDARD.encode(decoded);
        if value != canonical {
            return Err(WireGuardPublicKeyError::NonCanonical { value, canonical });
        }
        Ok(Self(value))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Returns the canonical 32-byte WireGuard public key material.
    #[must_use]
    pub fn decoded_bytes(&self) -> [u8; 32] {
        let decoded = base64::engine::general_purpose::STANDARD
            .decode(&self.0)
            .expect("validated WireGuard public key remains valid base64");
        decoded
            .try_into()
            .expect("validated WireGuard public key remains exactly 32 bytes")
    }
}

impl fmt::Debug for WireGuardPublicKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_tuple("WireGuardPublicKey")
            .field(&self.0)
            .finish()
    }
}

impl TryFrom<String> for WireGuardPublicKey {
    type Error = WireGuardPublicKeyError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::try_new(value)
    }
}

impl From<WireGuardPublicKey> for String {
    fn from(value: WireGuardPublicKey) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WireGuardPublicKeyError {
    #[error("WireGuard public key is not valid standard base64: {value:?}")]
    InvalidEncoding { value: String },
    #[error("WireGuard public key must decode to exactly 32 bytes, got {decoded_bytes}: {value:?}")]
    InvalidLength { value: String, decoded_bytes: usize },
    #[error(
        "WireGuard public key is not canonical: {value:?}; canonical spelling is {canonical:?}"
    )]
    NonCanonical { value: String, canonical: String },
}

/// The endpoint bridge's gateway address for a `/24` endpoint subnet: the
/// first host, which Docker assigns as the `ployz` bridge gateway and which a
/// container on that bridge reaches its machine-local resolver at.
#[must_use]
pub fn endpoint_bridge_gateway_ipv4(subnet: &str) -> Option<std::net::Ipv4Addr> {
    let net: Ipv4Net = subnet.parse().ok()?;
    net.hosts().next()
}

#[cfg(test)]
mod allocation_tests {
    use super::{MachineEndpointSubnet, MachineEndpointSupernet};

    #[test]
    fn endpoint_supernet_allocates_first_free_subnet() {
        let supernet = MachineEndpointSupernet::try_new("10.199.0.0/16").expect("supernet");
        let assigned = [
            MachineEndpointSubnet::try_new("10.199.0.0/24").expect("subnet"),
            MachineEndpointSubnet::try_new("10.199.1.0/24").expect("subnet"),
        ];

        assert_eq!(
            supernet
                .allocate_next(assigned)
                .expect("allocated")
                .as_string(),
            "10.199.2.0/24"
        );
    }

    #[test]
    fn endpoint_supernet_must_be_ipv4_slash_16() {
        assert!(MachineEndpointSupernet::try_new("10.199.0.0/24").is_err());
    }

    #[test]
    fn lowest_free_allocation_returns_the_only_unoccupied_subnet() {
        let supernet = MachineEndpointSupernet::try_new("10.199.0.0/16").expect("supernet");
        let assigned = (0..=u8::MAX)
            .filter(|third_octet| *third_octet != 73)
            .map(|third_octet| {
                MachineEndpointSubnet::try_new(format!("10.199.{third_octet}.0/24"))
                    .expect("subnet")
            });

        assert_eq!(
            supernet
                .allocate_next(assigned)
                .expect("one subnet remains free")
                .as_string(),
            "10.199.73.0/24"
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wireguard_public_key_requires_canonical_base64_for_32_bytes() {
        let canonical = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        assert_eq!(
            WireGuardPublicKey::try_new(canonical)
                .expect("32-byte standard base64 key")
                .as_str(),
            canonical
        );

        for invalid in [
            "",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA==",
            "___________________________________________=",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=\n",
        ] {
            assert!(
                WireGuardPublicKey::try_new(invalid).is_err(),
                "accepted invalid key {invalid:?}"
            );
        }
    }

    #[test]
    fn wireguard_public_key_serde_rejects_noncanonical_input() {
        let canonical = "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=";
        let key: WireGuardPublicKey = serde_json::from_value(serde_json::json!(canonical))
            .expect("canonical key deserializes");
        assert_eq!(
            serde_json::to_value(key).expect("key serializes"),
            canonical
        );
        assert!(
            serde_json::from_value::<WireGuardPublicKey>(serde_json::json!(
                "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA"
            ))
            .is_err()
        );
    }

    #[test]
    fn endpoint_bridge_gateway_is_first_subnet_host() {
        assert_eq!(
            endpoint_bridge_gateway_ipv4("10.42.7.0/24"),
            Some(std::net::Ipv4Addr::new(10, 42, 7, 1))
        );
    }

    #[test]
    fn machine_endpoint_subnet_bridge_gateway_is_first_host() {
        assert_eq!(
            MachineEndpointSubnet::try_new("10.42.7.0/24")
                .expect("valid endpoint subnet")
                .bridge_gateway_ipv4(),
            Ipv4Addr::new(10, 42, 7, 1)
        );
    }

    #[test]
    fn endpoint_bridge_gateway_rejects_invalid_subnet() {
        assert_eq!(endpoint_bridge_gateway_ipv4("not-a-subnet"), None);
    }

    #[test]
    fn machine_endpoint_subnet_accepts_only_ipv4_24_networks() {
        assert_eq!(
            MachineEndpointSubnet::try_new("10.42.1.0/24")
                .expect("valid ipv4")
                .as_string(),
            "10.42.1.0/24"
        );
        assert!(MachineEndpointSubnet::try_new("fd7a:115c:a1e0::/64").is_err());
        assert!(MachineEndpointSubnet::try_new("10.42.1.0/25").is_err());
        assert!(MachineEndpointSubnet::try_new("10.42.1.7/24").is_err());
        assert!(MachineEndpointSubnet::try_new("not-a-cidr").is_err());
    }
}
