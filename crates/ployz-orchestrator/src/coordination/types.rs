use ipnet::Ipv4Net;
use ployz_types::model::MachineId;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ResourceKey {
    /// Subnet reservation for machine-add admission.
    ///
    /// Missing peers are acceptable only up to the strict-majority quorum
    /// boundary. Unreachable or retryable peers do not vote allow, and the
    /// operation proceeds only when a majority of the current active
    /// coordination set allows the reservation.
    Subnet(Ipv4Net),
    /// Namespace deploy admission.
    ///
    /// Missing required deploy participants are not acceptable. Deploys build
    /// a participant set, probe it before apply, and hold session-bound local
    /// namespace locks on every participant. Any required participant that
    /// cannot be reached blocks the deploy.
    DeployNamespace(String),
    /// Hostname-scoped ACME order creation.
    ///
    /// Missing peer inventory is not acceptable because the caller would not
    /// know which active machines to ask. Once the inventory is known,
    /// unreachable peers abstain and explicit reachable denials veto; cert
    /// issuance is background work and offline machines cannot serve HTTP-01
    /// traffic until they return.
    CertIssuance(String),
    /// Issuer-scoped ACME account creation.
    ///
    /// Same missing-peer policy as `CertIssuance`: missing peer inventory
    /// defers account creation, while unreachable peers from a known inventory
    /// abstain and explicit reachable denials veto. Orders are account-bound,
    /// so first-use account creation must not silently degrade to local-only.
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
