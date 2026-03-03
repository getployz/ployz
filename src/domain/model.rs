use derive_more::Display;
use rand::RngCore;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::Ipv6Addr;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
pub struct MachineId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
pub struct NetworkName(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
pub struct NetworkId(pub String);

impl NetworkId {
    pub fn random() -> Self {
        let mut bytes = [0u8; 16];
        rand::thread_rng().fill_bytes(&mut bytes);
        let mut value = String::with_capacity(32);
        for b in &bytes {
            use std::fmt::Write as _;
            let _ = write!(&mut value, "{b:02x}");
        }
        Self(value)
    }
}

#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct PublicKey(pub [u8; 32]);

impl fmt::Debug for PublicKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "PublicKey({:02x}{:02x}{:02x}{:02x}..)",
            self.0[0], self.0[1], self.0[2], self.0[3]
        )
    }
}

#[derive(Clone, Serialize, Deserialize)]
pub struct PrivateKey(pub [u8; 32]);

impl fmt::Debug for PrivateKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("PrivateKey(***)")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
#[display("{_0}")]
pub struct OverlayIp(pub Ipv6Addr);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MachineRecord {
    pub id: MachineId,
    pub network_id: NetworkId,
    pub network: NetworkName,
    pub public_key: PublicKey,
    pub overlay_ip: OverlayIp,
    pub endpoints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteRecord {
    pub id: String,
    pub network_id: NetworkId,
    pub network_name: NetworkName,
    pub issued_by: MachineId,
    pub expires_at: u64,
    pub nonce: String,
    pub max_uses: u32,
    pub used: u32,
    pub revoked: bool,
}

#[derive(Debug, Clone)]
pub enum MachineEvent {
    Added(MachineRecord),
    Updated(MachineRecord),
    Removed { id: MachineId },
}

/// Derive a deterministic overlay IP from a public key (fd00::/8 ULA + first 15 key bytes).
pub fn management_ip_from_key(key: &PublicKey) -> OverlayIp {
    let mut octets = [0u8; 16];
    octets[0] = 0xfd;
    octets[1..16].copy_from_slice(&key.0[..15]);
    OverlayIp(Ipv6Addr::from(octets))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn management_ip_deterministic() {
        let key = PublicKey([0xab; 32]);
        let ip1 = management_ip_from_key(&key);
        let ip2 = management_ip_from_key(&key);
        assert_eq!(ip1, ip2);
        assert!(ip1.0.segments()[0] >> 8 == 0xfd);
    }

    #[test]
    fn different_keys_different_ips() {
        let k1 = PublicKey([0x01; 32]);
        let k2 = PublicKey([0x02; 32]);
        assert_ne!(management_ip_from_key(&k1), management_ip_from_key(&k2));
    }
}
