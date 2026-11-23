//! Local Machine record persistence and Live Observation operations.

use std::collections::{BTreeMap, BTreeSet};
#[cfg(test)]
use std::{fs, path::Path};

use ipnet::Ipv4Net;
use ployz_core::{
    ClientKey, DeviceCode, DeviceKey, LocalMachinePhase, Machine, MachineId, ManagementClientLabel,
    PinnedAddresses, RecoverySeed, SelectedEndpoint, WireGuardPublicKey,
};
use reqwest::Url;
use serde::{Deserialize, Serialize};

use crate::management::ManagementSecret;
use crate::network::WireGuardPrivateKey;

mod record_wire;
use record_wire::LocalMachineRecordWire;

mod ingress;
pub(crate) mod local_machine;
mod record_owner;
pub(crate) mod store;

pub use record_owner::{RecordOwner, RecordOwnerStopped};
pub(crate) use store::EnrollPlan;
#[cfg(test)]
use store::PENDING_RESET_FILE_NAME;
pub(crate) use store::{InterruptedReset, Opened};
pub use store::{LocalMachineStore, StoreError};

#[cfg(test)]
pub(crate) use local_machine::PeerInspect;
pub(crate) use local_machine::{
    Admit, AdmittedBy, AdmittedSpec, Call, Caller, RecoveryError, RuntimeWatchTelemetry, Scope,
    SealingError,
};
#[cfg(test)]
pub(crate) use local_machine::{EnsurePause, EnsurePausePoint};
pub use local_machine::{Error as LocalMachineError, FullCaller, LocalMachine};

/// [`LocalMachineRecord::has_cluster_identity`] for a phase read on its own.
pub(crate) fn has_cluster_identity(phase: &LocalMachinePhase) -> bool {
    matches!(
        phase,
        LocalMachinePhase::Joining | LocalMachinePhase::Participating
    )
}

#[cfg(test)]
mod enrollment_tests;
#[cfg(test)]
mod record_tests;
#[cfg(test)]
mod register_tests;
#[cfg(test)]
pub(crate) mod test_support;

/// Founder-only seed for immutable replicated Cluster values.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FoundingCluster {
    /// Cluster IPv4 pool selected by the founder.
    pub network: Ipv4Net,
    /// The Organization's Sealing Key this founder recovers rather than generates.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovery: Option<RecoverySeed>,
    /// What its founding client said granted this founding, kept so Cloud enrollment resumes
    /// only the Cluster its claim founded. Absent when that client, or the daemon, predates 0.3.15.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grant: Option<ployz_core::FoundingGrant>,
}

/// Whether a participating Machine founded the Cluster or joined known peers.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "origin", rename_all = "snake_case")]
pub enum ParticipationOrigin {
    /// This Machine founded the Cluster with these immutable values.
    Founder {
        /// Cluster values selected by this founder.
        cluster: FoundingCluster,
    },
    /// This Machine joined through these bootstrap Machines.
    Join {
        /// Machines retained as bootstrap peers.
        bootstrap: Vec<Machine>,
    },
}

impl ParticipationOrigin {
    fn founding(&self) -> ployz_core::Founding {
        match self {
            Self::Founder { cluster } => match cluster.grant {
                None | Some(ployz_core::FoundingGrant::Unrecognized) => {
                    ployz_core::Founding::Unrecorded
                }
                Some(ployz_core::FoundingGrant::Standalone) => ployz_core::Founding::Standalone,
                Some(ployz_core::FoundingGrant::Claim { claim }) => {
                    ployz_core::Founding::Claim { claim }
                }
            },
            Self::Join { .. } => ployz_core::Founding::Joined,
        }
    }

    fn cluster_network(&self) -> Option<Ipv4Net> {
        match self {
            Self::Founder { cluster } => Some(cluster.network),
            Self::Join { .. } => None,
        }
    }

    fn recovery_seed(&self) -> Option<&RecoverySeed> {
        match self {
            Self::Founder { cluster } => cluster.recovery.as_ref(),
            Self::Join { .. } => None,
        }
    }

    fn bootstrap(&self) -> &[Machine] {
        match self {
            Self::Founder { .. } => &[],
            Self::Join { bootstrap } => bootstrap,
        }
    }
}

/// On-disk Local Machine Phase. Each variant owns only that phase's fields.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum LocalMachineBody {
    Uninitialized {
        id: MachineId,
    },
    Joining {
        machine: Machine,
        bootstrap: Vec<Machine>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        min_store_version: BTreeMap<String, i64>,
        /// Joined under the trust its admitting Machine sealed, and still waiting for the
        /// restrictions that Machine answers the confirmation with. It participates only
        /// once `ConfirmJoin` hands them over.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        awaits_restrictions: bool,
    },
    Participating {
        machine: Machine,
        /// Founder or join authority, as one non-contradictory value.
        #[serde(flatten)]
        origin: ParticipationOrigin,
    },
    Resetting {
        prior: Box<LocalMachinePrior>,
    },
}

/// Live body wrapped by Resetting. Cannot itself be Resetting.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "phase", rename_all = "snake_case")]
pub enum LocalMachinePrior {
    Uninitialized {
        id: MachineId,
    },
    Joining {
        machine: Machine,
        bootstrap: Vec<Machine>,
        #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
        min_store_version: BTreeMap<String, i64>,
    },
    Participating {
        machine: Machine,
        /// Founder or join authority, as one non-contradictory value.
        #[serde(flatten)]
        origin: ParticipationOrigin,
    },
}

/// Persisted local Machine record: a phase-tagged body plus the key material
/// every phase requires.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "LocalMachineRecordWire")]
pub struct LocalMachineRecord {
    /// Phase-tagged lifecycle body.
    body: LocalMachineBody,
    /// WireGuard private key; required in every phase.
    wireguard_private_key: WireGuardPrivateKey,
    /// Management Identity secret; minted when the record is born.
    management_secret: ManagementSecret,
    /// Management Client slots by label. An absent label has no slot. Always
    /// written, even when empty, because readers require it.
    management_clients: BTreeMap<ManagementClientLabel, ManagementClientSlot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard_mtu: Option<u32>,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub selected_endpoints: BTreeMap<MachineId, SelectedEndpoint>,
    /// Always written, so only a record from 0.3.0-beta.7 or earlier lacks it.
    pinned_addresses: PinnedAddresses,
    /// Where the observe process pushes crash events. Absent means buffer only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    push_target: Option<Url>,
    /// Whether every live `device-` slot was written through Device approval. Absent on
    /// records from before Devices were Full-only, when any client could name one.
    #[serde(default)]
    device_slots_vetted: bool,
    /// Whether only trusted Members peer here. Every enrollment sets it. Written under its
    /// old name, so an older daemon that reads this record enforces too.
    #[serde(default, rename = "device_enforcement")]
    filters_peers: bool,
    #[serde(default)]
    trusted_machines: Vec<ployz_core::authority::Member>,
    /// Passkeys a Full caller approved here. They approve workflows and rules only and hold no slot.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    passkeys: Vec<ployz_core::authority::TrustedPasskey>,
    /// CI and agent keys a person granted here. They sign Service versions inside their scope
    /// and hold no slot. A daemon from before scoped keys ignores them, so it trusts none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    grants: Vec<ployz_core::authority::TrustedGrant>,
    #[serde(default)]
    membership_cluster: Option<ployz_core::SealingPublicKey>,
    #[serde(default)]
    enrollment: Option<ployz_core::EnrollRequest>,
    /// Keys this Machine stopped trusting. Never shrinks, so a removed key is never
    /// trusted here again.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    removed_keys: Vec<ployz_core::authority::KeyRef>,
    /// The keys of `removed_keys` removed as a CI or agent key, so another Member's removal
    /// can tell them from a Device. A key removed before this was kept is in neither.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    removed_grants: Vec<ployz_core::authority::SigningPublicKey>,
    /// Members this Machine saw removed: by a Forget it ran, or by its store showing their
    /// row deleted, never by their absence. Never shrinks.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    removed_members: Vec<RemovedMember>,
}

pub(crate) use ployz_core::RemovedMember;

/// Public keys held by one Management Client slot. No secret is persisted.
///
/// Unlike the rest of the record this stays strict: an unrecognized field here
/// is key or secret material this daemon cannot judge, so the record fails closed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum ManagementClientSlot {
    /// Tombstone left by Clear, keeping the cleared slot's public keys. They are
    /// never admitted again, so a redial with one learns its removal was confirmed.
    Cleared { was: LiveSlot },
    #[serde(untagged)]
    Live(LiveSlot),
}

/// A slot whose keys the management transport admits.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum LiveSlot {
    Pending {
        pending: ClientKey,
    },
    Active {
        accepted: ClientKey,
    },
    Rotating {
        accepted: ClientKey,
        pending: ClientKey,
    },
}

impl LiveSlot {
    fn accepted(self) -> Option<ClientKey> {
        match self {
            Self::Active { accepted } | Self::Rotating { accepted, .. } => Some(accepted),
            Self::Pending { .. } => None,
        }
    }

    fn pending(self) -> Option<ClientKey> {
        match self {
            Self::Pending { pending } | Self::Rotating { pending, .. } => Some(pending),
            Self::Active { .. } => None,
        }
    }

    fn holds(self, remote: &ClientKey) -> bool {
        self.accepted().as_ref() == Some(remote) || self.pending().as_ref() == Some(remote)
    }
}

impl ManagementClientSlot {
    fn live(self) -> Option<LiveSlot> {
        match self {
            Self::Live(slot) => Some(slot),
            Self::Cleared { .. } => None,
        }
    }

    fn accepted(self) -> Option<ClientKey> {
        self.live().and_then(LiveSlot::accepted)
    }

    fn pending(self) -> Option<ClientKey> {
        self.live().and_then(LiveSlot::pending)
    }

    /// Whether this tombstone holds `remote`.
    fn clears(self, remote: &ClientKey) -> bool {
        matches!(self, Self::Cleared { was } if was.holds(remote))
    }
}

impl LocalMachineRecord {
    /// Whether any Management Client slot holds an accepted or pending key.
    #[must_use]
    pub fn has_management_clients(&self) -> bool {
        self.management_clients().next().is_some()
    }

    /// Labels of the Management Client slots holding an accepted or pending key;
    /// Cleared tombstones are not listed.
    pub fn management_clients(&self) -> impl Iterator<Item = &ManagementClientLabel> {
        self.management_clients
            .iter()
            .filter(|(_, slot)| slot.live().is_some())
            .map(|(label, _)| label)
    }

    #[must_use]
    pub fn passkeys(&self) -> &[ployz_core::authority::TrustedPasskey] {
        &self.passkeys
    }

    #[must_use]
    pub fn grants(&self) -> &[ployz_core::authority::TrustedGrant] {
        &self.grants
    }

    /// Every key this Machine trusts, from its own record: its Devices, its passkeys and
    /// its grants, read fresh at each call.
    #[must_use]
    pub fn trusted_keys(&self) -> ployz_core::authority::TrustedKeys {
        ployz_core::authority::TrustedKeys::new(
            self.devices(),
            self.passkeys.clone(),
            self.grants.clone(),
        )
    }

    /// Keys of the approved Devices: accepted slots whose `device-` label names their key,
    /// and that no grant holds, so a CI or agent key never reads as a Device.
    pub fn devices(&self) -> impl Iterator<Item = DeviceKey> + '_ {
        self.management_clients.iter().filter_map(|(label, slot)| {
            let key = DeviceKey::from_bytes(*slot.accepted()?.as_bytes());
            let granted = self.grants.iter().any(|held| held.grant.key == key.into());
            (DeviceCode::named_by(label) == Some(key.code()) && !granted).then_some(key)
        })
    }

    /// Whether this Machine accepts the row as a Cluster member using its own authority. A
    /// removed Member is refused first, pinned or not.
    pub(crate) fn accepts_machine(&self, machine: &Machine) -> bool {
        if self.removes_member(machine.id, Some(machine.public_key)) {
            return false;
        }
        if !self.filters_peers || self.pins_machine(machine) {
            return true;
        }
        ployz_core::authority::join::admits_row(
            machine,
            &self.trusted_keys(),
            self.membership_cluster,
        )
    }

    /// Whether this Machine's own record pins `machine` as a Member, with this ID, WireGuard
    /// key and subnet, and never saw it removed. Unlike [`Self::accepts_machine`], it reads no
    /// approval off the row, and a record that doesn't filter peers trusts no Member.
    pub(crate) fn trusts_member(&self, machine: &Machine) -> bool {
        self.filters_peers
            && !self.removes_member(machine.id, Some(machine.public_key))
            && self.pins_machine(machine)
    }

    #[must_use]
    pub fn removed_keys(&self) -> &[ployz_core::authority::KeyRef] {
        &self.removed_keys
    }

    /// The keys this Machine removed for good as CI or agent keys.
    #[must_use]
    pub fn removed_grants(&self) -> &[ployz_core::authority::SigningPublicKey] {
        &self.removed_grants
    }

    #[must_use]
    pub(crate) fn removed_members(&self) -> &[RemovedMember] {
        &self.removed_members
    }

    /// Whether this disk enrolled: it joins or participates, or names the enrollment it
    /// took, also while resetting.
    #[must_use]
    pub(crate) fn enrolled(&self) -> bool {
        self.enrollment.is_some()
            || matches!(
                self.body.live(),
                LiveParts::Joining { .. } | LiveParts::Participating { .. }
            )
    }

    /// Whether this Machine removed `key` for good.
    #[must_use]
    pub fn removes_key(&self, key: &ployz_core::authority::KeyRef) -> bool {
        self.removed_keys.contains(key)
    }

    /// Whether this Machine saw the Member with `id`, or with WireGuard key `public_key`,
    /// removed.
    #[must_use]
    pub(crate) fn removes_member(
        &self,
        id: MachineId,
        public_key: Option<WireGuardPublicKey>,
    ) -> bool {
        self.removed_members.iter().any(|removed| {
            removed.id == id || public_key.is_some_and(|key| removed.public_key == Some(key))
        })
    }

    /// Trusted Members whose ID `listed` lacks: the only ones whose row the store may show
    /// deleted. A missing row alone removes nobody.
    pub(crate) fn unlisted_members(&self, listed: &BTreeSet<MachineId>) -> Vec<MachineId> {
        self.trusted_machines
            .iter()
            .filter(|member| !listed.contains(&member.id))
            .map(|member| member.id)
            .collect()
    }

    /// Whether an installed WireGuard peer no accepted row names may stay. `answered` holds
    /// the IDs a complete snapshot has an accepted row or a removal for, and is `None` while
    /// the store can't say who left. A record that doesn't filter peers keeps the peer until
    /// such a snapshot. Otherwise only a trusted Member stays, until the snapshot answers
    /// for its ID, so a missing or refused row never cuts the peers carrying gossip.
    pub(crate) fn keeps_unlisted_peer(
        &self,
        key: &WireGuardPublicKey,
        answered: Option<&BTreeSet<MachineId>>,
    ) -> bool {
        if !self.filters_peers {
            return answered.is_none();
        }
        self.trusted_machines.iter().any(|member| {
            member.public_key == *key && answered.is_none_or(|ids| !ids.contains(&member.id))
        })
    }

    /// Trusted Members whose key is `installed` and whose ID `listed` lacks. Each stays until
    /// the store shows its row deleted, so only these need that evidence. Empty on a record
    /// that doesn't filter peers.
    pub(crate) fn unlisted_trusted_machines(
        &self,
        installed: impl Fn(&WireGuardPublicKey) -> bool,
        listed: &BTreeSet<MachineId>,
    ) -> Vec<MachineId> {
        if !self.filters_peers {
            return Vec::new();
        }
        self.trusted_machines
            .iter()
            .filter(|member| installed(&member.public_key) && !listed.contains(&member.id))
            .map(|member| member.id)
            .collect()
    }

    /// Whether only trusted Members peer here: every record but an awaiting one that never
    /// turned enforcement on.
    #[must_use]
    pub(crate) fn filters_peers(&self) -> bool {
        self.filters_peers
    }

    pub(crate) fn pins_machine(&self, machine: &Machine) -> bool {
        self.machine()
            .is_some_and(|own| ployz_core::authority::Member::from(own).matches(machine))
            || self
                .trusted_machines
                .iter()
                .any(|member| member.matches(machine))
    }

    /// Whether `remote` is a key some Clear left in a tombstone.
    #[must_use]
    pub fn clears_management_client(&self, remote: &ClientKey) -> bool {
        self.management_clients
            .values()
            .any(|slot| slot.clears(remote))
    }

    /// Public key `label` currently has accepted by the management transport.
    #[must_use]
    pub fn accepted_client(&self, label: &ManagementClientLabel) -> Option<ClientKey> {
        self.management_clients
            .get(label)
            .and_then(|slot| slot.accepted())
    }

    /// Public key `label` has awaiting an authenticated handover.
    #[must_use]
    pub fn pending_client(&self, label: &ManagementClientLabel) -> Option<ClientKey> {
        self.management_clients
            .get(label)
            .and_then(|slot| slot.pending())
    }

    /// Whether `remote` is any slot's accepted key, so it may run operational RPCs.
    #[must_use]
    pub fn accepts_management_client(&self, remote: &ClientKey) -> bool {
        self.management_clients
            .values()
            .any(|slot| slot.accepted().as_ref() == Some(remote))
    }

    #[must_use]
    pub fn pending_label(&self, remote: &ClientKey) -> Option<&ManagementClientLabel> {
        self.management_clients
            .iter()
            .find(|(_, slot)| slot.pending().as_ref() == Some(remote))
            .map(|(label, _)| label)
    }

    #[must_use]
    pub fn accepted_label(&self, remote: &ClientKey) -> Option<&ManagementClientLabel> {
        self.management_clients
            .iter()
            .find(|(_, slot)| slot.accepted().as_ref() == Some(remote))
            .map(|(label, _)| label)
    }

    /// What `remote` may call: the widest scope of any slot holding it as its accepted
    /// or pending key, or `None` when no slot holds it.
    #[must_use]
    pub fn management_scope(&self, remote: &ClientKey) -> Option<ployz_core::ManagementScope> {
        let held = self.management_clients.values().any(|slot| {
            slot.accepted().as_ref() == Some(remote) || slot.pending().as_ref() == Some(remote)
        });
        held.then(|| {
            if self.trusted_keys().full(remote) {
                ployz_core::ManagementScope::Full
            } else {
                ployz_core::ManagementScope::Operator
            }
        })
    }

    /// Whether `remote` is any slot's accepted or pending key, so it may connect.
    #[must_use]
    pub fn admits_management_client(&self, remote: &ClientKey) -> bool {
        self.accepts_management_client(remote) || self.pending_label(remote).is_some()
    }
}

static EMPTY_STORE_VERSION: BTreeMap<String, i64> = BTreeMap::new();

/// The fields the three live phases share, borrowed from a Body or its Prior.
/// A Resetting Machine reads them through the phase it left.
#[derive(Clone, Copy)]
enum LiveParts<'a> {
    Uninitialized {
        id: MachineId,
    },
    Joining {
        machine: &'a Machine,
        bootstrap: &'a [Machine],
        min_store_version: &'a BTreeMap<String, i64>,
    },
    Participating {
        machine: &'a Machine,
        origin: &'a ParticipationOrigin,
    },
}

impl<'a> LiveParts<'a> {
    fn id(self) -> MachineId {
        match self {
            Self::Uninitialized { id } => id,
            Self::Joining { machine, .. } | Self::Participating { machine, .. } => machine.id,
        }
    }

    fn machine(self) -> Option<&'a Machine> {
        match self {
            Self::Uninitialized { .. } => None,
            Self::Joining { machine, .. } | Self::Participating { machine, .. } => Some(machine),
        }
    }

    fn cluster_network(self) -> Option<Ipv4Net> {
        match self {
            Self::Participating { origin, .. } => origin.cluster_network(),
            Self::Uninitialized { .. } | Self::Joining { .. } => None,
        }
    }

    fn bootstrap(self) -> &'a [Machine] {
        match self {
            Self::Uninitialized { .. } => &[],
            Self::Joining { bootstrap, .. } => bootstrap,
            Self::Participating { origin, .. } => origin.bootstrap(),
        }
    }

    fn min_store_version(self) -> &'a BTreeMap<String, i64> {
        match self {
            Self::Joining {
                min_store_version, ..
            } => min_store_version,
            Self::Uninitialized { .. } | Self::Participating { .. } => &EMPTY_STORE_VERSION,
        }
    }
}

impl LocalMachinePrior {
    fn live(&self) -> LiveParts<'_> {
        match self {
            Self::Uninitialized { id } => LiveParts::Uninitialized { id: *id },
            Self::Joining {
                machine,
                bootstrap,
                min_store_version,
            } => LiveParts::Joining {
                machine,
                bootstrap,
                min_store_version,
            },
            Self::Participating { machine, origin } => LiveParts::Participating { machine, origin },
        }
    }
}

impl LocalMachineBody {
    /// The live phase's shared fields, or those of the phase a Resetting Machine left.
    fn live(&self) -> LiveParts<'_> {
        match self {
            Self::Uninitialized { id } => LiveParts::Uninitialized { id: *id },
            Self::Joining {
                machine,
                bootstrap,
                min_store_version,
                ..
            } => LiveParts::Joining {
                machine,
                bootstrap,
                min_store_version,
            },
            Self::Participating { machine, origin } => LiveParts::Participating { machine, origin },
            Self::Resetting { prior } => prior.live(),
        }
    }

    fn id(&self) -> MachineId {
        self.live().id()
    }

    fn phase(&self) -> LocalMachinePhase {
        match self {
            Self::Uninitialized { .. } => LocalMachinePhase::Uninitialized,
            Self::Joining { .. } => LocalMachinePhase::Joining,
            Self::Participating { .. } => LocalMachinePhase::Participating,
            Self::Resetting { .. } => LocalMachinePhase::Resetting,
        }
    }

    fn machine(&self) -> Option<&Machine> {
        self.live().machine()
    }

    fn cluster_network(&self) -> Option<Ipv4Net> {
        self.live().cluster_network()
    }

    fn recovery_seed(&self) -> Option<&RecoverySeed> {
        match self {
            Self::Participating { origin, .. } => origin.recovery_seed(),
            Self::Uninitialized { .. } | Self::Joining { .. } | Self::Resetting { .. } => None,
        }
    }

    fn bootstrap(&self) -> &[Machine] {
        self.live().bootstrap()
    }

    fn min_store_version(&self) -> &BTreeMap<String, i64> {
        self.live().min_store_version()
    }

    fn into_prior(self) -> LocalMachinePrior {
        match self {
            Self::Uninitialized { id } => LocalMachinePrior::Uninitialized { id },
            Self::Joining {
                machine,
                bootstrap,
                min_store_version,
                ..
            } => LocalMachinePrior::Joining {
                machine,
                bootstrap,
                min_store_version,
            },
            Self::Participating { machine, origin } => {
                LocalMachinePrior::Participating { machine, origin }
            }
            Self::Resetting { .. } => {
                unreachable!("prepare_reset rejects an already resetting Machine")
            }
        }
    }
}

impl LocalMachineRecord {
    /// Admit a local lifecycle body whose advertised identity matches its key.
    ///
    /// # Errors
    ///
    /// Rejects mismatched keys, empty local endpoints, and empty join bootstrap peers,
    /// including a Resetting record's retained prior state.
    pub fn parse(
        body: LocalMachineBody,
        wireguard_private_key: WireGuardPrivateKey,
    ) -> Result<Self, StoreError> {
        if let Some(machine) = body.machine() {
            if machine.public_key != wireguard_private_key.public_key() {
                return Err(StoreError::KeyMismatch);
            }
            if machine.advertised_endpoints.is_empty() {
                return Err(StoreError::MissingEndpoints);
            }
            if body.cluster_network().is_none() && body.bootstrap().is_empty() {
                return Err(StoreError::MissingPeers);
            }
        }
        Ok(Self::fresh(body, wireguard_private_key))
    }

    /// A record as it is born: its own secret, no slots, no pins, nothing vetted away.
    fn fresh(body: LocalMachineBody, wireguard_private_key: WireGuardPrivateKey) -> Self {
        Self {
            body,
            wireguard_private_key,
            management_secret: ManagementSecret::generate(),
            management_clients: BTreeMap::new(),
            wireguard_mtu: None,
            selected_endpoints: BTreeMap::new(),
            pinned_addresses: PinnedAddresses::default(),
            push_target: None,
            device_slots_vetted: true,
            filters_peers: false,
            trusted_machines: Vec::new(),
            passkeys: Vec::new(),
            grants: Vec::new(),
            membership_cluster: None,
            enrollment: None,
            removed_keys: Vec::new(),
            removed_grants: Vec::new(),
            removed_members: Vec::new(),
        }
    }

    #[must_use]
    pub fn pinned_addresses(&self) -> PinnedAddresses {
        self.pinned_addresses
    }

    /// Where the observe process pushes crash events, if Cloud set one.
    #[must_use]
    pub fn push_target(&self) -> Option<&Url> {
        self.push_target.as_ref()
    }

    /// Whether this Machine has joined or founded a Cluster, which `Joining` and
    /// `Participating` both hold and `Resetting` no longer does, though its record still
    /// names a Machine.
    #[must_use]
    pub fn has_cluster_identity(&self) -> bool {
        has_cluster_identity(&self.phase())
    }

    /// This Machine, while it has a Cluster identity.
    #[must_use]
    pub(crate) fn cluster_machine(&self) -> Option<&Machine> {
        self.machine().filter(|_| self.has_cluster_identity())
    }

    /// Admit work that runs only in a Cluster this Machine has joined.
    ///
    /// # Errors
    /// Returns [`LocalMachineError::NotParticipating`] in any other phase, `Joining` too.
    pub(crate) fn require_participating(&self) -> Result<(), LocalMachineError> {
        if self.phase() == LocalMachinePhase::Participating {
            Ok(())
        } else {
            Err(LocalMachineError::NotParticipating)
        }
    }

    #[must_use]
    pub fn management_secret(&self) -> &ManagementSecret {
        &self.management_secret
    }

    /// Inspect phase-specific fields without changing lifecycle or key identity.
    #[must_use]
    pub fn body(&self) -> &LocalMachineBody {
        &self.body
    }

    /// How this Machine came to its Cluster; only a Participating Machine has an answer.
    #[must_use]
    pub fn founding(&self) -> ployz_core::Founding {
        match &self.body {
            LocalMachineBody::Participating { origin, .. } => origin.founding(),
            LocalMachineBody::Uninitialized { .. }
            | LocalMachineBody::Joining { .. }
            | LocalMachineBody::Resetting { .. } => ployz_core::Founding::Unrecorded,
        }
    }

    /// Private material corresponding to the local advertised public key.
    #[must_use]
    pub fn private_key(&self) -> &WireGuardPrivateKey {
        &self.wireguard_private_key
    }

    /// Durable identity of this local Machine.
    #[must_use]
    pub fn id(&self) -> MachineId {
        self.body.id()
    }

    #[must_use]
    pub fn phase(&self) -> LocalMachinePhase {
        self.body.phase()
    }

    /// Advertised Machine when this phase owns one.
    #[must_use]
    pub fn machine(&self) -> Option<&Machine> {
        self.body.machine()
    }

    /// This Machine's own enrollment facts, as lifecycle refusals report them.
    #[must_use]
    pub fn now(&self) -> ployz_core::MachineNow {
        ployz_core::MachineNow {
            id: self.id(),
            phase: self.phase(),
            machine: self.machine().cloned(),
        }
    }

    /// Cluster IPv4 pool when this participating Machine has one.
    #[must_use]
    pub fn cluster_network(&self) -> Option<Ipv4Net> {
        self.body.cluster_network()
    }

    /// The Sealing Key this founder waits to recover, when it founded without one.
    #[must_use]
    pub fn recovery_seed(&self) -> Option<&RecoverySeed> {
        self.body.recovery_seed()
    }

    /// Bootstrap Machines persisted for join and catch-up that this Machine accepts.
    #[must_use]
    pub fn bootstrap(&self) -> Vec<Machine> {
        self.body
            .bootstrap()
            .iter()
            .filter(|machine| self.accepts_machine(machine))
            .cloned()
            .collect()
    }

    /// Minimum store versions this joiner must reach before participating.
    #[must_use]
    pub fn min_store_version(&self) -> &BTreeMap<String, i64> {
        self.body.min_store_version()
    }

    fn into_resetting(self) -> Self {
        Self {
            body: LocalMachineBody::Resetting {
                prior: Box::new(self.body.into_prior()),
            },
            ..self
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn served_capabilities_match_a_contract_with_every_group() {
        use ployz_core::{MachineRpc, op};

        let root = std::env::temp_dir().join(format!(
            "ployzd-served-capabilities-{}",
            ployz_core::MachineId::random()
        ));
        let store =
            RecordOwner::spawn(LocalMachineStore::open(root.join("data")).unwrap()).unwrap();
        let (containers, _) =
            crate::docker::test_support::fake_runtime_with(Default::default()).await;
        let (replicated, server) = crate::corrosion::fake_cluster::store().await;
        let contract = crate::machine_api::MachineService::with_cluster(
            store,
            Some((
                replicated,
                crate::corrosion::AdminClient::new("/no/such/admin.sock"),
            )),
        )
        .with_optional_containers(Some(containers))
        .describe_contract(tonic::Request::new(
            op::DescribeContract::into_request(ployz_core::DescribeContractRequest {})
                .encode()
                .unwrap(),
        ))
        .await
        .unwrap()
        .into_inner()
        .decode_response()
        .unwrap()
        .decode::<op::DescribeContract>()
        .unwrap();
        server.abort();
        let _ = fs::remove_dir_all(root);

        let advertised: std::collections::BTreeSet<String> = contract
            .capabilities
            .iter()
            .map(|name| name.as_str().to_owned())
            .collect();
        assert_eq!(ployz_core::served_capabilities(), advertised);
    }

    #[test]
    fn prepared_reset_does_not_change_phase_until_commit() {
        let data_dir = std::env::temp_dir().join(format!(
            "ployzd-prepared-reset-{}",
            ployz_core::MachineId::random()
        ));
        let mut store = LocalMachineStore::open(&data_dir).unwrap();
        let prepared = store.prepare_reset().unwrap();

        assert_eq!(store.record().phase(), LocalMachinePhase::Uninitialized);
        assert!(data_dir.join(PENDING_RESET_FILE_NAME).exists());
        drop(prepared);
        assert!(!data_dir.join(PENDING_RESET_FILE_NAME).exists());

        let missing = store.prepare_reset().unwrap();
        fs::remove_file(data_dir.join(PENDING_RESET_FILE_NAME)).unwrap();
        assert!(missing.commit(&mut store).is_err());
        assert_eq!(store.record().phase(), LocalMachinePhase::Uninitialized);

        let prepared = store.prepare_reset().unwrap();
        store
            .persist_selected_endpoint(
                MachineId::random(),
                SelectedEndpoint(std::net::SocketAddr::from(([192, 0, 2, 4], 51820))),
            )
            .unwrap();
        prepared.commit(&mut store).unwrap();
        assert_eq!(store.record().phase(), LocalMachinePhase::Resetting);
        drop(store);
        fs::remove_dir_all(data_dir).unwrap();
    }
}
