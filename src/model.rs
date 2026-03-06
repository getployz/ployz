use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use derive_more::Display;
use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use std::fmt;
use std::net::{Ipv4Addr, Ipv6Addr};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
pub struct MachineId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
pub struct NetworkName(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
pub struct NetworkId(pub String);

impl NetworkId {
    pub fn random() -> Self {
        let mut bytes = [0u8; 16];
        rand::fill(&mut bytes);
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
    pub public_key: PublicKey,
    pub overlay_ip: OverlayIp,
    pub subnet: Option<Ipv4Net>,
    pub bridge_ip: Option<OverlayIp>,
    pub endpoints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct InviteRecord {
    pub id: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone)]
pub enum MachineEvent {
    Added(MachineRecord),
    Updated(MachineRecord),
    Removed { id: MachineId },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct JoinResponse {
    pub machine_id: MachineId,
    pub public_key: PublicKey,
    pub overlay_ip: OverlayIp,
    pub subnet: Option<Ipv4Net>,
    pub endpoints: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Display)]
pub struct WorkloadId(pub String);

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkloadRecord {
    pub id: WorkloadId,
    pub machine_id: MachineId,
    pub overlay_ip: Ipv4Addr,
    pub public_key: PublicKey,
    pub sidecar_container: String,
}

pub const JOIN_RESPONSE_PREFIX: &str = "PLOYZ_JOIN_RESPONSE:";

impl JoinResponse {
    pub fn encode(&self) -> Result<String, String> {
        let json = serde_json::to_string(self).map_err(|e| format!("serialize: {e}"))?;
        Ok(format!(
            "{}{}",
            JOIN_RESPONSE_PREFIX,
            URL_SAFE_NO_PAD.encode(json.as_bytes())
        ))
    }

    pub fn decode(s: &str) -> Result<Self, String> {
        let payload = s
            .strip_prefix(JOIN_RESPONSE_PREFIX)
            .ok_or_else(|| format!("missing prefix '{JOIN_RESPONSE_PREFIX}'"))?;
        let bytes = URL_SAFE_NO_PAD
            .decode(payload)
            .map_err(|e| format!("base64 decode: {e}"))?;
        serde_json::from_slice(&bytes).map_err(|e| format!("json decode: {e}"))
    }

    pub fn into_machine_record(self) -> MachineRecord {
        MachineRecord {
            id: self.machine_id,
            public_key: self.public_key,
            overlay_ip: self.overlay_ip,
            subnet: self.subnet,
            bridge_ip: None,
            endpoints: self.endpoints,
        }
    }
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

    #[test]
    fn join_response_encode_decode_roundtrip() {
        let resp = JoinResponse {
            machine_id: MachineId("joiner-1".into()),
            public_key: PublicKey([0xab; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)),
            subnet: Some("10.42.1.0/24".parse().unwrap()),
            endpoints: vec!["1.2.3.4:51820".into()],
        };

        let encoded = resp.encode().unwrap();
        assert!(encoded.starts_with(JOIN_RESPONSE_PREFIX));

        let decoded = JoinResponse::decode(&encoded).unwrap();
        assert_eq!(decoded.machine_id, resp.machine_id);
        assert_eq!(decoded.public_key, resp.public_key);
        assert_eq!(decoded.overlay_ip, resp.overlay_ip);
        assert_eq!(decoded.subnet, resp.subnet);
        assert_eq!(decoded.endpoints, resp.endpoints);
    }

    #[test]
    fn join_response_into_machine_record() {
        let resp = JoinResponse {
            machine_id: MachineId("joiner-1".into()),
            public_key: PublicKey([0xab; 32]),
            overlay_ip: OverlayIp(Ipv6Addr::new(0xfd00, 0, 0, 0, 0, 0, 0, 1)),
            subnet: None,
            endpoints: vec![],
        };
        let record = resp.into_machine_record();
        assert_eq!(record.id.0, "joiner-1");
        assert!(record.bridge_ip.is_none());
    }
}
