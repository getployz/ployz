//! Dataplane preparation models.

use base64::Engine as _;
use ipnet::{IpNet, Ipv4Net};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::fmt;
use std::net::{Ipv4Addr, SocketAddr};
use std::str::FromStr;

use crate::ids::MachineId;
use crate::operation::FailureMessage;

pub const DEFAULT_WIREGUARD_LISTEN_PORT: u16 = 51820;
pub const MIN_WIREGUARD_MTU: u32 = 1280;
pub const MAX_WIREGUARD_MTU: u32 = 1420;
/// A previously-established peer silent beyond this age is not healthy.
pub const MAX_HEALTHY_WIREGUARD_HANDSHAKE_AGE_SECONDS: u64 = 275;
pub const DEFAULT_ENDPOINT_SUPERNET: &str = "10.210.0.0/16";

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "ts",
    ts(type = "Brand<string, \"DataplaneProjectionRevision\">")
)]
#[serde(transparent)]
pub struct DataplaneProjectionRevision(String);

impl DataplaneProjectionRevision {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn for_members(members: &[DataplaneProjectionMember]) -> Self {
        let mut digest = Sha256::new();
        hash_field(&mut digest, b"ployz-dataplane-projection-v1");
        digest.update((members.len() as u64).to_be_bytes());
        for member in members {
            hash_field(&mut digest, member.machine_id.as_str().as_bytes());
            hash_field(&mut digest, member.endpoint_subnet.as_string().as_bytes());
            hash_field(&mut digest, member.wireguard_public_key.as_str().as_bytes());
            digest.update((member.mesh_endpoints.len() as u64).to_be_bytes());
            for endpoint in &member.mesh_endpoints {
                hash_field(&mut digest, endpoint.to_string().as_bytes());
            }
        }
        Self(format!("{:x}", digest.finalize()))
    }
}

fn hash_field(digest: &mut Sha256, value: &[u8]) {
    digest.update((value.len() as u64).to_be_bytes());
    digest.update(value);
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DataplaneProjectionMember {
    pub machine_id: MachineId,
    pub endpoint_subnet: MachineEndpointSubnet,
    pub mesh_endpoints: Vec<SocketAddr>,
    pub wireguard_public_key: WireGuardPublicKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
struct DataplaneProjectionWire {
    declared_members: Vec<DataplaneProjectionMember>,
    staged_member: Option<DataplaneProjectionMember>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(as = "DataplaneProjectionWire"))]
#[serde(try_from = "DataplaneProjectionWire", into = "DataplaneProjectionWire")]
pub struct DataplaneProjection {
    declared_revision: DataplaneProjectionRevision,
    target_revision: DataplaneProjectionRevision,
    declared_members: Vec<DataplaneProjectionMember>,
    target_members: Vec<DataplaneProjectionMember>,
}

impl DataplaneProjection {
    pub fn try_new(
        mut declared_members: Vec<DataplaneProjectionMember>,
        mut staged_member: Option<DataplaneProjectionMember>,
    ) -> Result<Self, DataplaneProjectionError> {
        for member in &mut declared_members {
            member.mesh_endpoints.sort();
            member.mesh_endpoints.dedup();
        }
        if let Some(member) = &mut staged_member {
            member.mesh_endpoints.sort();
            member.mesh_endpoints.dedup();
        }
        declared_members.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
        if declared_members
            .windows(2)
            .any(|pair| matches!(pair, [left, right] if left.machine_id == right.machine_id))
        {
            return Err(DataplaneProjectionError::DuplicateMachine);
        }
        let mut target_members = declared_members.clone();
        if let Some(staged_member) = staged_member {
            if declared_members
                .iter()
                .any(|member| member.machine_id == staged_member.machine_id)
            {
                return Err(DataplaneProjectionError::DuplicateMachine);
            }
            target_members.push(staged_member);
        }
        target_members.sort_by(|left, right| left.machine_id.cmp(&right.machine_id));
        let mut subnets = BTreeSet::new();
        let mut public_keys = BTreeSet::new();
        for member in &target_members {
            if !subnets.insert(member.endpoint_subnet.clone()) {
                return Err(DataplaneProjectionError::DuplicateEndpointSubnet);
            }
            if !public_keys.insert(member.wireguard_public_key.clone()) {
                return Err(DataplaneProjectionError::DuplicateWireGuardPublicKey);
            }
        }
        let declared_revision = DataplaneProjectionRevision::for_members(&declared_members);
        let target_revision = DataplaneProjectionRevision::for_members(&target_members);
        Ok(Self {
            declared_revision,
            target_revision,
            declared_members,
            target_members,
        })
    }

    #[must_use]
    pub fn declared_revision(&self) -> &DataplaneProjectionRevision {
        &self.declared_revision
    }

    #[must_use]
    pub fn target_revision(&self) -> &DataplaneProjectionRevision {
        &self.target_revision
    }

    #[must_use]
    pub fn declared_members(&self) -> &[DataplaneProjectionMember] {
        &self.declared_members
    }

    #[must_use]
    pub fn target_members(&self) -> &[DataplaneProjectionMember] {
        &self.target_members
    }

    #[must_use]
    pub fn staged_member(&self) -> Option<&DataplaneProjectionMember> {
        self.target_members.iter().find(|target| {
            !self
                .declared_members
                .iter()
                .any(|declared| declared.machine_id == target.machine_id)
        })
    }
}

impl TryFrom<DataplaneProjectionWire> for DataplaneProjection {
    type Error = DataplaneProjectionError;

    fn try_from(wire: DataplaneProjectionWire) -> Result<Self, Self::Error> {
        Self::try_new(wire.declared_members, wire.staged_member)
    }
}

impl From<DataplaneProjection> for DataplaneProjectionWire {
    fn from(projection: DataplaneProjection) -> Self {
        let staged_member = projection.staged_member().cloned();
        Self {
            declared_members: projection.declared_members,
            staged_member,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum DataplaneProjectionError {
    #[error("dataplane projection contains a duplicate machine")]
    DuplicateMachine,
    #[error("dataplane projection contains a duplicate endpoint subnet")]
    DuplicateEndpointSubnet,
    #[error("dataplane projection contains a duplicate WireGuard public key")]
    DuplicateWireGuardPublicKey,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct DataplaneMember {
    pub machine_id: MachineId,
    pub endpoint_subnet: MachineEndpointSubnet,
}

impl DataplaneMember {
    #[must_use]
    pub fn default_for_machine(machine_id: MachineId) -> Self {
        let endpoint_subnet = MachineEndpointSubnet::try_new(default_endpoint_subnet(&machine_id))
            .expect("default endpoint subnet is a valid CIDR");
        Self {
            machine_id,
            endpoint_subnet,
        }
    }
}

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

    /// Selects from the currently free `/24`s using fresh process randomness.
    pub fn allocate_random_free(
        &self,
        assigned: impl IntoIterator<Item = MachineEndpointSubnet>,
    ) -> Result<MachineEndpointSubnet, MachineEndpointSubnetAllocationError> {
        let assigned = assigned.into_iter().collect::<BTreeSet<_>>();
        let octets = self.0.network().octets();
        let free = (0..=u8::MAX)
            .map(|third_octet| {
                MachineEndpointSubnet::try_new(format!(
                    "{}.{}.{}.0/24",
                    octets[0], octets[1], third_octet
                ))
                .expect("candidate from /16 supernet is a valid /24")
            })
            .filter(|candidate| !assigned.contains(candidate))
            .collect::<Vec<_>>();
        if free.is_empty() {
            return Err(MachineEndpointSubnetAllocationError::Exhausted {
                supernet: self.as_string(),
            });
        }
        let random = u128::from(ulid::Ulid::new());
        let index = (random % free.len() as u128) as usize;
        Ok(free
            .into_iter()
            .nth(index)
            .expect("the random index is reduced modulo the nonempty free set"))
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardEbpfEndpointRoute {
    pub machine_id: MachineId,
    pub endpoint_subnet: String,
}

impl WireGuardEbpfEndpointRoute {
    #[must_use]
    pub fn default_for_machine(machine_id: &MachineId) -> Self {
        Self {
            machine_id: machine_id.clone(),
            endpoint_subnet: default_endpoint_subnet(machine_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardPeer {
    pub machine_id: MachineId,
    pub endpoint_subnet: String,
    pub active_endpoint: SocketAddr,
    pub candidate_endpoints: Vec<SocketAddr>,
    pub public_key: WireGuardPublicKey,
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

#[must_use]
pub fn default_endpoint_subnet(machine_id: &MachineId) -> String {
    let octet = trailing_machine_number(machine_id.as_str())
        .map(|number| ((number.saturating_sub(1)) % 254 + 1) as u8)
        .unwrap_or_else(|| stable_machine_octet(machine_id.as_str()));
    let supernet = MachineEndpointSupernet::default_v1();
    let [first, second, _, _] = supernet.0.network().octets();
    format!("{first}.{second}.{octet}.0/24")
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
    fn random_free_allocation_never_returns_an_occupied_subnet() {
        let supernet = MachineEndpointSupernet::try_new("10.199.0.0/16").expect("supernet");
        let assigned = (0..=u8::MAX)
            .filter(|third_octet| *third_octet != 73)
            .map(|third_octet| {
                MachineEndpointSubnet::try_new(format!("10.199.{third_octet}.0/24"))
                    .expect("subnet")
            });

        assert_eq!(
            supernet
                .allocate_random_free(assigned)
                .expect("one subnet remains free")
                .as_string(),
            "10.199.73.0/24"
        );
    }
}

fn trailing_machine_number(value: &str) -> Option<u16> {
    let digits = value
        .chars()
        .rev()
        .take_while(char::is_ascii_digit)
        .collect::<String>();
    if digits.is_empty() {
        return None;
    }
    digits.chars().rev().collect::<String>().parse().ok()
}

fn stable_machine_octet(value: &str) -> u8 {
    let hash = value.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0000_0100_0000_01b3)
    });
    (hash % 254 + 1) as u8
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum PloyzNativeMeshComponent {
    #[serde(rename = "wireguard")]
    WireGuard,
    EbpfForwarding,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct PloyzNativeMeshReady {
    pub wireguard: WireGuardReady,
    pub ebpf_forwarding: EbpfForwardingReady,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct WireGuardReady {
    pub public_key: WireGuardPublicKey,
    pub evidence: Vec<WireGuardReadyEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct EbpfForwardingReady {
    pub evidence: Vec<EbpfForwardingReadyEvidence>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum WireGuardReadyEvidence {
    HostPath { path: String },
    Command { program: String, args: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EbpfForwardingReadyEvidence {
    HostPath { path: String },
    Command { program: String, args: Vec<String> },
    PloyzTcBytecode { path: String, symbols: Vec<String> },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WireGuardEbpfPrepareError {
    Unavailable {
        machine_id: MachineId,
        component: PloyzNativeMeshComponent,
        message: FailureMessage,
    },
    InvalidReport {
        message: FailureMessage,
    },
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
    fn default_endpoint_subnet_uses_trailing_machine_number() {
        assert_eq!(
            default_endpoint_subnet(&machine_id("edge_2")),
            "10.210.2.0/24"
        );
        assert_eq!(
            default_endpoint_subnet(&machine_id("machine_255")),
            "10.210.1.0/24"
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

    fn machine_id(value: &str) -> MachineId {
        MachineId::try_new(value).expect("valid machine id")
    }

    fn projection_member(machine: &str, subnet: &str) -> DataplaneProjectionMember {
        DataplaneProjectionMember {
            machine_id: machine_id(machine),
            endpoint_subnet: MachineEndpointSubnet::try_new(subnet).expect("valid subnet"),
            mesh_endpoints: vec![
                "192.0.2.2:51820".parse().expect("endpoint"),
                "192.0.2.1:51820".parse().expect("endpoint"),
                "192.0.2.2:51820".parse().expect("endpoint"),
            ],
            wireguard_public_key: WireGuardPublicKey::try_new(
                base64::engine::general_purpose::STANDARD.encode(Sha256::digest(machine)),
            )
            .expect("public key"),
        }
    }

    #[test]
    fn projection_revision_is_canonical_and_promotion_stable() {
        let first = projection_member("machine_a", "10.198.1.0/24");
        let second = projection_member("machine_b", "10.198.2.0/24");
        let staged = DataplaneProjection::try_new(vec![first.clone()], Some(second.clone()))
            .expect("projection");
        let promoted =
            DataplaneProjection::try_new(vec![second, first], None).expect("promoted projection");

        assert_eq!(staged.target_revision, promoted.declared_revision);
        assert_eq!(promoted.declared_revision, promoted.target_revision);
        let [promoted_first, ..] = promoted.declared_members() else {
            panic!("promoted projection has a member");
        };
        assert_eq!(promoted_first.mesh_endpoints.len(), 2);
    }

    #[test]
    fn projection_rejects_duplicate_machine_subnet_and_public_key() {
        let first = projection_member("machine_a", "10.198.1.0/24");
        let mut duplicate_subnet = projection_member("machine_b", "10.198.1.0/24");
        let mut duplicate_key = projection_member("machine_c", "10.198.3.0/24");
        duplicate_key.wireguard_public_key = first.wireguard_public_key.clone();

        assert_eq!(
            DataplaneProjection::try_new(vec![first.clone()], Some(first.clone())),
            Err(DataplaneProjectionError::DuplicateMachine)
        );
        assert_eq!(
            DataplaneProjection::try_new(vec![first.clone()], Some(duplicate_subnet.clone())),
            Err(DataplaneProjectionError::DuplicateEndpointSubnet)
        );
        duplicate_subnet.endpoint_subnet =
            MachineEndpointSubnet::try_new("10.198.2.0/24").expect("subnet");
        assert_eq!(
            DataplaneProjection::try_new(vec![first], Some(duplicate_key)),
            Err(DataplaneProjectionError::DuplicateWireGuardPublicKey)
        );
    }

    #[test]
    fn projection_wire_contains_only_canonical_inputs_and_revalidates_them() {
        let projection = DataplaneProjection::try_new(
            vec![projection_member("machine_a", "10.198.1.0/24")],
            Some(projection_member("machine_b", "10.198.2.0/24")),
        )
        .expect("projection");
        let encoded = serde_json::to_value(&projection).expect("serialize projection");

        assert!(encoded.get("declared_members").is_some());
        assert!(encoded.get("staged_member").is_some());
        assert!(encoded.get("declared_revision").is_none());
        assert!(encoded.get("target_revision").is_none());
        assert!(encoded.get("target_members").is_none());
        assert_eq!(
            serde_json::from_value::<DataplaneProjection>(encoded).expect("deserialize projection"),
            projection
        );

        let duplicate = serde_json::json!({
            "declared_members": [projection_member("machine_a", "10.198.1.0/24")],
            "staged_member": projection_member("machine_a", "10.198.2.0/24"),
        });
        assert!(serde_json::from_value::<DataplaneProjection>(duplicate).is_err());
    }
}
