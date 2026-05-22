//! Product-owned machine membership facts and reducer.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;

use crate::facts::{
    ProductFact, ProductFactEnvelope, ProductFactKey, ProductFactKind, ProductFactPayload,
    ProductFactResource, ProductFactTarget,
};
use crate::machine::{
    IrohEndpointId, MachineEpoch, MachineId, MachineMembership, MachineNetworkIdentity,
    MachineRemoval, MachineRemovalReason, MachineStatus, MachineTombstone, OverlayIp,
    WireGuardPublicKey,
};

pub const MACHINE_JOINED_KIND: &str = "ployz.machine.joined.v1";
pub const MACHINE_REMOVAL_STARTED_KIND: &str = "ployz.machine.removal_started.v1";
pub const MACHINE_TOMBSTONED_KIND: &str = "ployz.machine.tombstoned.v1";

#[derive(Debug, Clone, Error, PartialEq, Eq)]
pub enum MachineFactError {
    #[error("machine fact payload is invalid")]
    InvalidPayload,
    #[error("machine fact target does not match payload")]
    TargetMismatch,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MachineMembershipFact {
    Joined(MachineMembership),
    RemovalStarted(MachineRemoval),
    Tombstoned {
        tombstone: MachineTombstone,
        reason: MachineRemovalReason,
    },
}

impl MachineMembershipFact {
    #[must_use]
    pub fn joined(membership: MachineMembership) -> Self {
        Self::Joined(membership)
    }

    #[must_use]
    pub fn removal_started(removal: MachineRemoval) -> Self {
        Self::RemovalStarted(removal)
    }

    #[must_use]
    pub fn tombstoned(tombstone: MachineTombstone, reason: MachineRemovalReason) -> Self {
        Self::Tombstoned { tombstone, reason }
    }

    pub fn decode_payload(payload: &[u8]) -> Result<Self, MachineFactError> {
        let payload = serde_json::from_slice::<MachineFactPayload>(payload)
            .map_err(|_| MachineFactError::InvalidPayload)?;
        payload.try_into()
    }

    #[must_use]
    pub fn machine(&self) -> &MachineId {
        match self {
            Self::Joined(membership) => &membership.machine,
            Self::RemovalStarted(removal) => &removal.machine,
            Self::Tombstoned { tombstone, .. } => &tombstone.machine,
        }
    }

    #[must_use]
    pub fn epoch(&self) -> MachineEpoch {
        match self {
            Self::Joined(membership) => membership.epoch,
            Self::RemovalStarted(removal) => removal.epoch,
            Self::Tombstoned { tombstone, .. } => tombstone.epoch,
        }
    }

    fn payload(&self) -> MachineFactPayload {
        match self {
            Self::Joined(membership) => MachineFactPayload::Joined {
                machine: membership.machine.as_str().to_owned(),
                epoch: membership.epoch.value(),
                overlay_ip: membership.network.overlay_ip.canonical(),
                iroh_endpoint_id: membership.network.iroh_endpoint_id.as_str().to_owned(),
                wireguard_public_key: membership.network.wireguard_public_key.as_str().to_owned(),
            },
            Self::RemovalStarted(removal) => MachineFactPayload::RemovalStarted {
                machine: removal.machine.as_str().to_owned(),
                epoch: removal.epoch.value(),
                reason: removal.reason.as_str().to_owned(),
            },
            Self::Tombstoned { tombstone, reason } => MachineFactPayload::Tombstoned {
                machine: tombstone.machine.as_str().to_owned(),
                epoch: tombstone.epoch.value(),
                reason: reason.as_str().to_owned(),
            },
        }
    }

    fn target(&self) -> Result<ProductFactTarget, MachineFactError> {
        let machine = self.machine().as_str();
        let epoch = self.epoch().value();
        let (key, kind) = match self {
            Self::Joined(_) => (
                format!(
                    "/facts/node/{machine}/joined/{epoch}/{}",
                    fact_payload_suffix(&self.payload())?
                ),
                MACHINE_JOINED_KIND,
            ),
            Self::RemovalStarted(_) => (
                format!(
                    "/facts/node/{machine}/removal_started/{epoch}/{}",
                    fact_payload_suffix(&self.payload())?
                ),
                MACHINE_REMOVAL_STARTED_KIND,
            ),
            Self::Tombstoned { .. } => (
                format!(
                    "/facts/node/{machine}/tombstoned/{epoch}/{}",
                    fact_payload_suffix(&self.payload())?
                ),
                MACHINE_TOMBSTONED_KIND,
            ),
        };
        Ok(ProductFactTarget::new(
            ProductFactResource::parse(format!("machine:{machine}"))
                .map_err(|_| MachineFactError::InvalidPayload)?,
            ProductFactKey::parse(key).map_err(|_| MachineFactError::InvalidPayload)?,
            ProductFactKind::parse(kind).map_err(|_| MachineFactError::InvalidPayload)?,
        ))
    }
}

impl ProductFact for MachineMembershipFact {
    type Error = MachineFactError;

    fn encode(&self) -> Result<ProductFactEnvelope, Self::Error> {
        let payload =
            serde_json::to_vec(&self.payload()).map_err(|_| MachineFactError::InvalidPayload)?;
        Ok(ProductFactEnvelope::new(
            self.target()?,
            ProductFactPayload::new(payload).map_err(|_| MachineFactError::InvalidPayload)?,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MachineMembershipReducer {
    machine: MachineId,
}

impl MachineMembershipReducer {
    #[must_use]
    pub fn new(machine: MachineId) -> Self {
        Self { machine }
    }

    pub fn reduce<I>(&self, facts: I) -> Result<MachineStatus, MachineFactError>
    where
        I: IntoIterator<Item = MachineMembershipFact>,
    {
        let mut state = MachineReducerState::default();
        for fact in facts {
            if fact.machine() != &self.machine {
                return Err(MachineFactError::TargetMismatch);
            }
            state.apply(fact);
        }
        Ok(state.status())
    }
}

#[derive(Debug, Default)]
struct MachineReducerState {
    joined: Option<MachineMembership>,
    removing: Option<MachineRemoval>,
    tombstone: Option<MachineTombstone>,
    conflict: Option<(MachineId, MachineEpoch)>,
}

impl MachineReducerState {
    fn apply(&mut self, fact: MachineMembershipFact) {
        match fact {
            MachineMembershipFact::Joined(membership) => self.apply_joined(membership),
            MachineMembershipFact::RemovalStarted(removal) => self.apply_removal_started(removal),
            MachineMembershipFact::Tombstoned { tombstone, .. } => {
                self.apply_tombstone(tombstone);
            }
        }
    }

    fn apply_joined(&mut self, membership: MachineMembership) {
        if self.tombstone.is_some() {
            return;
        }

        if let Some((_machine, conflict_epoch)) = &self.conflict {
            if *conflict_epoch > membership.epoch {
                return;
            }
            if *conflict_epoch == membership.epoch {
                return;
            }
            self.conflict = None;
        }

        match &self.joined {
            Some(existing) if existing.epoch > membership.epoch => {}
            Some(existing) if existing.epoch == membership.epoch && existing != &membership => {
                self.conflict = Some((membership.machine.clone(), membership.epoch));
                self.joined = None;
            }
            Some(existing) if existing.epoch == membership.epoch => {}
            _ => {
                self.joined = Some(membership);
            }
        }
    }

    fn apply_removal_started(&mut self, removal: MachineRemoval) {
        if self.tombstone.is_some() {
            return;
        }
        let replace = match &self.removing {
            Some(existing) if existing.epoch > removal.epoch => false,
            Some(existing) if existing.epoch == removal.epoch => {
                removal.reason.as_str() < existing.reason.as_str()
            }
            _ => true,
        };
        if replace {
            self.removing = Some(removal);
        }
    }

    fn apply_tombstone(&mut self, tombstone: MachineTombstone) {
        let replace = match &self.tombstone {
            Some(existing) => tombstone.epoch >= existing.epoch,
            None => true,
        };
        if replace {
            self.tombstone = Some(tombstone);
        }
        self.joined = None;
        self.removing = None;
        self.conflict = None;
    }

    fn status(self) -> MachineStatus {
        if let Some(tombstone) = self.tombstone {
            return MachineStatus::Tombstoned(tombstone);
        }
        if let Some(removing) = self.removing {
            return MachineStatus::Removing(removing);
        }
        if let Some((machine, epoch)) = self.conflict {
            return MachineStatus::Conflicted { machine, epoch };
        }
        if let Some(joined) = self.joined {
            return MachineStatus::Joined(joined);
        }
        MachineStatus::Absent
    }
}

fn fact_payload_suffix(payload: &MachineFactPayload) -> Result<String, MachineFactError> {
    let bytes = serde_json::to_vec(payload).map_err(|_| MachineFactError::InvalidPayload)?;
    let digest = Sha256::digest(bytes);
    Ok(hex_prefix(&digest, 12))
}

fn hex_prefix(bytes: &[u8], len: usize) -> String {
    let mut suffix = String::with_capacity(len * 2);
    for byte in bytes.iter().take(len) {
        suffix.push_str(&format!("{byte:02x}"));
    }
    suffix
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
enum MachineFactPayload {
    Joined {
        machine: String,
        epoch: u64,
        overlay_ip: String,
        iroh_endpoint_id: String,
        wireguard_public_key: String,
    },
    RemovalStarted {
        machine: String,
        epoch: u64,
        reason: String,
    },
    Tombstoned {
        machine: String,
        epoch: u64,
        reason: String,
    },
}

impl TryFrom<MachineFactPayload> for MachineMembershipFact {
    type Error = MachineFactError;

    fn try_from(value: MachineFactPayload) -> Result<Self, Self::Error> {
        match value {
            MachineFactPayload::Joined {
                machine,
                epoch,
                overlay_ip,
                iroh_endpoint_id,
                wireguard_public_key,
            } => Ok(Self::Joined(MachineMembership::new(
                MachineId::parse(machine).map_err(|_| MachineFactError::InvalidPayload)?,
                MachineEpoch::new(epoch).map_err(|_| MachineFactError::InvalidPayload)?,
                MachineNetworkIdentity::new(
                    OverlayIp::parse(overlay_ip).map_err(|_| MachineFactError::InvalidPayload)?,
                    IrohEndpointId::parse(iroh_endpoint_id)
                        .map_err(|_| MachineFactError::InvalidPayload)?,
                    WireGuardPublicKey::parse(wireguard_public_key)
                        .map_err(|_| MachineFactError::InvalidPayload)?,
                ),
            ))),
            MachineFactPayload::RemovalStarted {
                machine,
                epoch,
                reason,
            } => Ok(Self::RemovalStarted(MachineRemoval {
                machine: MachineId::parse(machine).map_err(|_| MachineFactError::InvalidPayload)?,
                epoch: MachineEpoch::new(epoch).map_err(|_| MachineFactError::InvalidPayload)?,
                reason: MachineRemovalReason::parse(reason)
                    .map_err(|_| MachineFactError::InvalidPayload)?,
            })),
            MachineFactPayload::Tombstoned {
                machine,
                epoch,
                reason,
            } => Ok(Self::Tombstoned {
                tombstone: MachineTombstone {
                    machine: MachineId::parse(machine)
                        .map_err(|_| MachineFactError::InvalidPayload)?,
                    epoch: MachineEpoch::new(epoch)
                        .map_err(|_| MachineFactError::InvalidPayload)?,
                },
                reason: MachineRemovalReason::parse(reason)
                    .map_err(|_| MachineFactError::InvalidPayload)?,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn joined_fact_round_trips_through_product_payload() {
        let fact = MachineMembershipFact::joined(membership("node-a", 1, "fd00::1"));

        let envelope = fact.encode().expect("encode");
        let decoded =
            MachineMembershipFact::decode_payload(envelope.payload().as_bytes()).expect("decode");

        assert_eq!(decoded, fact);
        assert!(
            envelope
                .target()
                .key()
                .as_str()
                .starts_with("/facts/node/node-a/joined/1/")
        );
        assert_eq!(envelope.target().kind().as_str(), MACHINE_JOINED_KIND);
    }

    #[test]
    fn reducer_rebuilds_joined_membership() {
        let reducer = MachineMembershipReducer::new(machine("node-a"));

        let status = reducer
            .reduce([MachineMembershipFact::joined(membership(
                "node-a", 1, "fd00::1",
            ))])
            .expect("status");

        assert!(
            matches!(status, MachineStatus::Joined(joined) if joined == membership("node-a", 1, "fd00::1"))
        );
    }

    #[test]
    fn reducer_ignores_older_joined_membership() {
        let reducer = MachineMembershipReducer::new(machine("node-a"));

        let status = reducer
            .reduce([
                MachineMembershipFact::joined(membership("node-a", 3, "fd00::1")),
                MachineMembershipFact::joined(membership("node-a", 1, "fd00::2")),
            ])
            .expect("status");

        assert!(
            matches!(status, MachineStatus::Joined(joined) if joined == membership("node-a", 3, "fd00::1"))
        );
    }

    #[test]
    fn reducer_marks_same_epoch_different_identity_as_conflict() {
        let reducer = MachineMembershipReducer::new(machine("node-a"));

        let status = reducer
            .reduce([
                MachineMembershipFact::joined(membership("node-a", 3, "fd00::1")),
                MachineMembershipFact::joined(membership("node-a", 3, "fd00::2")),
            ])
            .expect("status");

        assert!(matches!(
            status,
            MachineStatus::Conflicted { machine, epoch }
                if machine.as_str() == "node-a" && epoch.value() == 3
        ));
    }

    #[test]
    fn reducer_reports_removing_ahead_of_joined() {
        let reducer = MachineMembershipReducer::new(machine("node-a"));

        let status = reducer
            .reduce([
                MachineMembershipFact::joined(membership("node-a", 3, "fd00::1")),
                MachineMembershipFact::removal_started(MachineRemoval {
                    machine: machine("node-a"),
                    epoch: epoch(4),
                    reason: MachineRemovalReason::parse("graceful-remove").expect("reason"),
                }),
            ])
            .expect("status");

        assert!(matches!(
            status,
            MachineStatus::Removing(removal)
                if removal.machine.as_str() == "node-a" && removal.epoch.value() == 4
        ));
    }

    #[test]
    fn reducer_keeps_tombstone_until_explicit_reinvite_exists() {
        let reducer = MachineMembershipReducer::new(machine("node-a"));

        let status = reducer
            .reduce([
                MachineMembershipFact::tombstoned(
                    MachineTombstone {
                        machine: machine("node-a"),
                        epoch: epoch(1),
                    },
                    MachineRemovalReason::parse("force-remove").expect("reason"),
                ),
                MachineMembershipFact::joined(membership("node-a", 9, "fd00::1")),
            ])
            .expect("status");

        assert!(matches!(
            status,
            MachineStatus::Tombstoned(tombstone)
                if tombstone.machine.as_str() == "node-a" && tombstone.epoch.value() == 1
        ));
    }

    fn membership(machine_id: &str, epoch_value: u64, overlay_ip: &str) -> MachineMembership {
        MachineMembership::new(
            machine(machine_id),
            epoch(epoch_value),
            MachineNetworkIdentity::new(
                OverlayIp::parse(overlay_ip).expect("overlay ip"),
                IrohEndpointId::parse(format!("iroh-{machine_id}")).expect("iroh endpoint"),
                WireGuardPublicKey::parse(format!("wg-{machine_id}")).expect("wireguard key"),
            ),
        )
    }

    fn machine(value: &str) -> MachineId {
        MachineId::parse(value).expect("machine")
    }

    fn epoch(value: u64) -> MachineEpoch {
        MachineEpoch::new(value).expect("epoch")
    }
}
