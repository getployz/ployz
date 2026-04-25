use ipnet::Ipv4Net;
use ployz_types::model::MachineId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResourceKey {
    Subnet(Ipv4Net),
    DeployNamespace(String),
    CertIssuance(String),
    AcmeAccount(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ReservationId(pub String);

impl ReservationId {
    #[must_use]
    pub fn random() -> Self {
        let mut bytes = [0u8; 16];
        rand::fill(&mut bytes);
        let mut value = String::with_capacity(32);
        for byte in bytes {
            use std::fmt::Write as _;
            let _ = write!(&mut value, "{byte:02x}");
        }
        Self(value)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reservation {
    pub id: ReservationId,
    pub key: ResourceKey,
    pub owner: MachineId,
    pub nonce: String,
    pub expires_at: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum VoteConflict {
    HeldBy {
        holder: MachineId,
        reservation_id: ReservationId,
    },
    AlreadyCommitted,
    OwnerMismatch,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Vote {
    Allow,
    Deny(VoteConflict),
}
