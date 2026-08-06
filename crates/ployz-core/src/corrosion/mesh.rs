//! Pure addressing and deterministic roster-fold policy for the builtin mesh.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::net::{Ipv6Addr, SocketAddr};

use ipnet::Ipv6Net;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::{
    ClusterDocument, MachineDocument, MachineTransport, MeshProvider, NamedAcceptedRow,
    NamedReadReport, PeerDocument, PeerTransport, ShadowConflict, SkippedRow,
};
use crate::ids::{ClusterId, MachineRowId, PeerId};
use crate::network::{MachineEndpointSubnet, MachineEndpointSupernet, WireGuardPublicKey};

/// v1 product ceiling, kept equal to the shipped eBPF route-map capacity.
pub const MAX_BUILTIN_WIREGUARD_MEMBERS: usize = 256;

/// The deterministic unique-local prefix shared by one builtin mesh.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(transparent)]
pub struct BuiltinWireguardClusterPrefix(Ipv6Net);

impl BuiltinWireguardClusterPrefix {
    #[must_use]
    pub const fn network(self) -> Ipv6Net {
        self.0
    }
}

impl fmt::Display for BuiltinWireguardClusterPrefix {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// One member's deterministic `/112` cryptokey-routing identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(transparent)]
pub struct BuiltinWireguardMemberSubnet(Ipv6Net);

impl BuiltinWireguardMemberSubnet {
    #[must_use]
    pub const fn network(self) -> Ipv6Net {
        self.0
    }
}

impl fmt::Display for BuiltinWireguardMemberSubnet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// The first address in one member's derived `/112`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[cfg_attr(feature = "ts", ts(type = "string"))]
#[serde(transparent)]
pub struct BuiltinWireguardMemberAddress(Ipv6Addr);

impl BuiltinWireguardMemberAddress {
    #[must_use]
    pub const fn get(self) -> Ipv6Addr {
        self.0
    }
}

impl fmt::Display for BuiltinWireguardMemberAddress {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        self.0.fmt(formatter)
    }
}

/// Both derived address forms needed when binding or installing a peer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BuiltinWireguardMemberIdentity {
    subnet: BuiltinWireguardMemberSubnet,
    bind_address: BuiltinWireguardMemberAddress,
}

impl BuiltinWireguardMemberIdentity {
    #[must_use]
    pub const fn subnet(self) -> BuiltinWireguardMemberSubnet {
        self.subnet
    }

    #[must_use]
    pub const fn bind_address(self) -> BuiltinWireguardMemberAddress {
        self.bind_address
    }
}

/// Derives `fd || sha256(cluster_id)[0..40]` as a `/48`.
#[must_use]
pub fn derive_builtin_wireguard_cluster_prefix(
    cluster_id: &ClusterId,
) -> BuiltinWireguardClusterPrefix {
    let digest = Sha256::digest(cluster_id.as_str().as_bytes());
    let [first, second, third, fourth, fifth, ..] = digest.as_slice() else {
        unreachable!("SHA-256 always returns 32 bytes");
    };
    let octets = [
        0xfd, *first, *second, *third, *fourth, *fifth, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
    ];
    BuiltinWireguardClusterPrefix(
        Ipv6Net::new(Ipv6Addr::from(octets), 48).expect("a fixed /48 prefix is valid"),
    )
}

/// Derives the member `/112` and its `::1` bind address from canonical key bytes.
#[must_use]
pub fn derive_builtin_wireguard_member(
    cluster_id: &ClusterId,
    public_key: &WireGuardPublicKey,
) -> BuiltinWireguardMemberIdentity {
    let prefix = derive_builtin_wireguard_cluster_prefix(cluster_id);
    let digest = Sha256::digest(public_key.decoded_bytes());
    let [key_0, key_1, key_2, key_3, key_4, key_5, key_6, key_7, ..] = digest.as_slice() else {
        unreachable!("SHA-256 always returns 32 bytes");
    };
    let [
        prefix_0,
        prefix_1,
        prefix_2,
        prefix_3,
        prefix_4,
        prefix_5,
        ..,
    ] = prefix.network().network().octets();
    let subnet_octets = [
        prefix_0, prefix_1, prefix_2, prefix_3, prefix_4, prefix_5, *key_0, *key_1, *key_2, *key_3,
        *key_4, *key_5, *key_6, *key_7, 0, 0,
    ];
    let subnet = Ipv6Net::new(Ipv6Addr::from(subnet_octets), 112)
        .expect("a cluster prefix plus 64 digest bits is a valid /112");
    let bind_octets = [
        prefix_0, prefix_1, prefix_2, prefix_3, prefix_4, prefix_5, *key_0, *key_1, *key_2, *key_3,
        *key_4, *key_5, *key_6, *key_7, 0, 1,
    ];
    BuiltinWireguardMemberIdentity {
        subnet: BuiltinWireguardMemberSubnet(subnet),
        bind_address: BuiltinWireguardMemberAddress(Ipv6Addr::from(bind_octets)),
    }
}

/// A roster identity whose ordering follows canonical underlying ULID text.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RosterMemberId {
    Machine { machine_id: MachineRowId },
    Peer { peer_id: PeerId },
}

impl RosterMemberId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::Machine { machine_id } => machine_id.as_str(),
            Self::Peer { peer_id } => peer_id.as_str(),
        }
    }

    const fn kind_order(&self) -> u8 {
        match self {
            Self::Machine { .. } => 0,
            Self::Peer { .. } => 1,
        }
    }
}

impl PartialOrd for RosterMemberId {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RosterMemberId {
    fn cmp(&self, other: &Self) -> Ordering {
        self.as_str()
            .cmp(other.as_str())
            .then_with(|| self.kind_order().cmp(&other.kind_order()))
    }
}

/// The identity namespace in which two accepted roster rows collided.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MeshIdentityClaim {
    ContainerSubnet {
        subnet: MachineEndpointSubnet,
    },
    WireguardPublicKey {
        public_key: WireGuardPublicKey,
    },
    DerivedIpv6 {
        subnet: BuiltinWireguardMemberSubnet,
    },
}

/// Evidence from the deterministic lowest-row-id identity law.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
pub struct MeshIdentityConflict {
    pub claim: MeshIdentityClaim,
    pub winner: RosterMemberId,
    pub loser: RosterMemberId,
}

/// All reader and identity-adjudication evidence retained by one mesh fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinWireguardRosterEvidence {
    pub machine_skipped: Vec<SkippedRow>,
    pub machine_shadows: Vec<ShadowConflict>,
    pub peer_skipped: Vec<SkippedRow>,
    pub peer_shadows: Vec<ShadowConflict>,
    pub address_mismatches: Vec<BuiltinWireguardKeyMismatch>,
    pub identity_conflicts: Vec<MeshIdentityConflict>,
}

/// Why a nonempty roster cannot authorize local convergence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BuiltinWireguardFenceReason {
    MissingLocalMachine,
    LocalIdentityConflict {
        claim: MeshIdentityClaim,
        winner: RosterMemberId,
    },
}

/// A stored transport identity that does not agree with deterministic truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "ts", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum BuiltinWireguardKeyMismatch {
    LocalPublicKey {
        machine_id: MachineRowId,
        stored: WireGuardPublicKey,
        local: WireGuardPublicKey,
    },
    StoredIpv6Address {
        member_id: RosterMemberId,
        #[cfg_attr(feature = "ts", ts(type = "string"))]
        stored: Ipv6Addr,
        derived: BuiltinWireguardMemberAddress,
    },
}

/// Local identity and address required before Corrosion can bind to the mesh.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredBuiltinWireguardLocal {
    pub public_key: WireGuardPublicKey,
    pub subnet_v6: BuiltinWireguardMemberSubnet,
    pub bind_address: BuiltinWireguardMemberAddress,
}

/// Whether one machine's container `/24` survived the independent subnet fold.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DesiredMachineContainerRoute {
    Claimed {
        subnet: MachineEndpointSubnet,
    },
    Withheld {
        subnet: MachineEndpointSubnet,
        winner: RosterMemberId,
    },
}

/// A remote machine peer. Its container route can be withheld independently.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredBuiltinWireguardMachinePeer {
    pub machine_id: MachineRowId,
    pub public_key: WireGuardPublicKey,
    pub subnet_v6: BuiltinWireguardMemberSubnet,
    pub endpoint: Option<SocketAddr>,
    pub container_route: DesiredMachineContainerRoute,
}

/// A non-machine roaming peer, structurally unable to carry an IPv4 subnet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredBuiltinWireguardRoamingPeer {
    pub peer_id: PeerId,
    pub public_key: WireGuardPublicKey,
    pub subnet_v6: BuiltinWireguardMemberSubnet,
    pub endpoint: Option<SocketAddr>,
}

/// The complete pure desired state consumed by the privileged host adapter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DesiredBuiltinWireguardMesh {
    pub local: DesiredBuiltinWireguardLocal,
    pub local_container_subnet: MachineEndpointSubnet,
    pub cluster_container_prefix: MachineEndpointSupernet,
    pub machine_peers: Vec<DesiredBuiltinWireguardMachinePeer>,
    pub roaming_peers: Vec<DesiredBuiltinWireguardRoamingPeer>,
    pub ebpf_routes: Vec<MachineEndpointSubnet>,
    pub evidence: BuiltinWireguardRosterEvidence,
}

/// The accepted local row and occupied address space required to repair a lost `/24` claim.
///
/// This value is constructed only when the local machine lost the deterministic lowest-ULID
/// subnet fold, so consumers cannot mistake an ordinary remote conflict for authority to rewrite
/// the local row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BuiltinWireguardLocalSubnetReallocation {
    machine_id: MachineRowId,
    accepted_machine: MachineDocument,
    occupied_subnets: BTreeSet<MachineEndpointSubnet>,
}

impl BuiltinWireguardLocalSubnetReallocation {
    #[must_use]
    pub fn machine_id(&self) -> &MachineRowId {
        &self.machine_id
    }

    #[must_use]
    pub fn accepted_machine(&self) -> &MachineDocument {
        &self.accepted_machine
    }

    #[must_use]
    pub fn occupied_subnets(&self) -> &BTreeSet<MachineEndpointSubnet> {
        &self.occupied_subnets
    }

    #[must_use]
    pub fn into_parts(
        self,
    ) -> (
        MachineRowId,
        MachineDocument,
        BTreeSet<MachineEndpointSubnet>,
    ) {
        (
            self.machine_id,
            self.accepted_machine,
            self.occupied_subnets,
        )
    }
}

/// A roster fold outcome with destructive convergence guarded explicitly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuiltinWireguardMeshOutcome {
    NoRoster {
        evidence: BuiltinWireguardRosterEvidence,
    },
    Fenced {
        local_machine_id: MachineRowId,
        reason: BuiltinWireguardFenceReason,
        evidence: BuiltinWireguardRosterEvidence,
    },
    KeyMismatch {
        mismatches: Vec<BuiltinWireguardKeyMismatch>,
        evidence: BuiltinWireguardRosterEvidence,
    },
    ReallocateLocalContainerSubnet {
        repair: BuiltinWireguardLocalSubnetReallocation,
        evidence: BuiltinWireguardRosterEvidence,
    },
    Desired(DesiredBuiltinWireguardMesh),
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum BuiltinWireguardMeshError {
    #[error("builtin WireGuard mesh projection requires the builtin_wireguard provider")]
    WrongProvider,
    #[error("accepted roster row {member_id:?} has a non-WireGuard transport")]
    UnexpectedTransport { member_id: RosterMemberId },
    #[error("machine {machine_id} subnet {subnet:?} is outside cluster prefix {cluster_prefix:?}")]
    ContainerSubnetOutsideClusterPrefix {
        machine_id: MachineRowId,
        subnet: MachineEndpointSubnet,
        cluster_prefix: crate::network::MachineEndpointSupernet,
    },
    #[error(
        "builtin WireGuard roster has {accepted_members} accepted members; maximum is {maximum}"
    )]
    AcceptedRosterTooLarge {
        accepted_members: usize,
        maximum: usize,
    },
}

/// Folds accepted machine and peer reader reports into builtin host desired state.
pub fn project_builtin_wireguard_mesh(
    cluster: &ClusterDocument,
    local_machine_id: MachineRowId,
    local_public_key: &WireGuardPublicKey,
    machines: NamedReadReport<MachineDocument>,
    peers: NamedReadReport<PeerDocument>,
) -> Result<BuiltinWireguardMeshOutcome, BuiltinWireguardMeshError> {
    if cluster.provider != MeshProvider::BuiltinWireguard {
        return Err(BuiltinWireguardMeshError::WrongProvider);
    }

    let NamedReadReport {
        accepted: accepted_machines,
        skipped: machine_skipped,
        shadows: machine_shadows,
    } = machines;
    let NamedReadReport {
        accepted: accepted_peers,
        skipped: peer_skipped,
        shadows: peer_shadows,
    } = peers;
    let mut evidence = BuiltinWireguardRosterEvidence {
        machine_skipped,
        machine_shadows,
        peer_skipped,
        peer_shadows,
        address_mismatches: Vec::new(),
        identity_conflicts: Vec::new(),
    };

    if accepted_machines.is_empty() && accepted_peers.is_empty() {
        return Ok(BuiltinWireguardMeshOutcome::NoRoster { evidence });
    }
    let Some(accepted_local_machine) = accepted_machines
        .iter()
        .find(|row| row.id.as_str() == local_machine_id.as_str())
        .map(|row| row.value.clone())
    else {
        return Ok(BuiltinWireguardMeshOutcome::Fenced {
            local_machine_id,
            reason: BuiltinWireguardFenceReason::MissingLocalMachine,
            evidence,
        });
    };
    let accepted_members = accepted_machines.len().saturating_add(accepted_peers.len());
    if accepted_members > MAX_BUILTIN_WIREGUARD_MEMBERS {
        return Err(BuiltinWireguardMeshError::AcceptedRosterTooLarge {
            accepted_members,
            maximum: MAX_BUILTIN_WIREGUARD_MEMBERS,
        });
    }

    let mut candidates = Vec::with_capacity(accepted_machines.len() + accepted_peers.len());
    for row in accepted_machines {
        candidates.push(Candidate::from_machine(cluster, row)?);
    }
    for row in accepted_peers {
        candidates.push(Candidate::from_peer(cluster, row)?);
    }
    candidates.sort_by(|left, right| left.id.cmp(&right.id));

    let local_member_id = RosterMemberId::Machine {
        machine_id: local_machine_id.clone(),
    };
    let mut mismatches = Vec::new();
    let mut excluded_identities = BTreeSet::new();
    for candidate in &candidates {
        let derived = derive_builtin_wireguard_member(&cluster.cluster_id, &candidate.public_key);
        if candidate.stored_address != derived.bind_address.get() {
            let mismatch = BuiltinWireguardKeyMismatch::StoredIpv6Address {
                member_id: candidate.id.clone(),
                stored: candidate.stored_address,
                derived: derived.bind_address,
            };
            if candidate.id == local_member_id {
                mismatches.push(mismatch);
            } else {
                evidence.address_mismatches.push(mismatch);
                excluded_identities.insert(candidate.id.clone());
            }
        }
        if candidate.id == local_member_id && candidate.public_key != *local_public_key {
            mismatches.push(BuiltinWireguardKeyMismatch::LocalPublicKey {
                machine_id: local_machine_id.clone(),
                stored: candidate.public_key.clone(),
                local: local_public_key.clone(),
            });
        }
    }
    if !mismatches.is_empty() {
        return Ok(BuiltinWireguardMeshOutcome::KeyMismatch {
            mismatches,
            evidence,
        });
    }

    let mut public_key_winners = BTreeMap::<WireGuardPublicKey, RosterMemberId>::new();
    let mut ipv6_winners = BTreeMap::<BuiltinWireguardMemberSubnet, RosterMemberId>::new();
    let mut subnet_winners = BTreeMap::<MachineEndpointSubnet, RosterMemberId>::new();
    for candidate in &candidates {
        if excluded_identities.contains(&candidate.id) {
            continue;
        }
        record_identity_claim(
            &mut public_key_winners,
            candidate.public_key.clone(),
            MeshIdentityClaim::WireguardPublicKey {
                public_key: candidate.public_key.clone(),
            },
            &candidate.id,
            &mut excluded_identities,
            &mut evidence.identity_conflicts,
        );
        let derived = derive_builtin_wireguard_member(&cluster.cluster_id, &candidate.public_key);
        record_identity_claim(
            &mut ipv6_winners,
            derived.subnet,
            MeshIdentityClaim::DerivedIpv6 {
                subnet: derived.subnet,
            },
            &candidate.id,
            &mut excluded_identities,
            &mut evidence.identity_conflicts,
        );
        if excluded_identities.contains(&candidate.id) {
            continue;
        }
        if let Some(subnet) = &candidate.subnet_v4 {
            if let Some(winner) = subnet_winners.get(subnet) {
                evidence.identity_conflicts.push(MeshIdentityConflict {
                    claim: MeshIdentityClaim::ContainerSubnet {
                        subnet: subnet.clone(),
                    },
                    winner: winner.clone(),
                    loser: candidate.id.clone(),
                });
            } else {
                subnet_winners.insert(subnet.clone(), candidate.id.clone());
            }
        }
    }

    if excluded_identities.contains(&local_member_id) {
        let conflict = evidence
            .identity_conflicts
            .iter()
            .find(|conflict| conflict.loser == local_member_id)
            .expect("an excluded identity has conflict evidence");
        return Ok(BuiltinWireguardMeshOutcome::Fenced {
            local_machine_id,
            reason: BuiltinWireguardFenceReason::LocalIdentityConflict {
                claim: conflict.claim.clone(),
                winner: conflict.winner.clone(),
            },
            evidence,
        });
    }

    let local_candidate = candidates
        .iter()
        .find(|candidate| candidate.id == local_member_id)
        .expect("the accepted local machine was checked above");
    let local_subnet = local_candidate
        .subnet_v4
        .as_ref()
        .expect("the local machine candidate always carries a container subnet");
    let local_subnet_winner = subnet_winners
        .get(local_subnet)
        .expect("every machine subnet was adjudicated");
    if *local_subnet_winner != local_member_id {
        return Ok(
            BuiltinWireguardMeshOutcome::ReallocateLocalContainerSubnet {
                repair: BuiltinWireguardLocalSubnetReallocation {
                    machine_id: local_machine_id,
                    accepted_machine: accepted_local_machine,
                    occupied_subnets: subnet_winners.into_keys().collect(),
                },
                evidence,
            },
        );
    }
    let local_container_subnet = local_subnet.clone();
    let local_identity = derive_builtin_wireguard_member(&cluster.cluster_id, local_public_key);
    let local = DesiredBuiltinWireguardLocal {
        public_key: local_public_key.clone(),
        subnet_v6: local_identity.subnet,
        bind_address: local_identity.bind_address,
    };
    debug_assert_eq!(local_candidate.public_key, local.public_key);

    let mut machine_peers = Vec::new();
    let mut roaming_peers = Vec::new();
    let mut ebpf_routes = Vec::new();
    for candidate in candidates {
        if candidate.id == local_member_id || excluded_identities.contains(&candidate.id) {
            continue;
        }
        let identity = derive_builtin_wireguard_member(&cluster.cluster_id, &candidate.public_key);
        match candidate.kind {
            CandidateKind::Machine { machine_id } => {
                let subnet = candidate
                    .subnet_v4
                    .expect("machine candidates always carry a container subnet");
                let winner = subnet_winners
                    .get(&subnet)
                    .expect("every machine subnet was adjudicated");
                let container_route = if *winner == candidate.id {
                    ebpf_routes.push(subnet.clone());
                    DesiredMachineContainerRoute::Claimed { subnet }
                } else {
                    DesiredMachineContainerRoute::Withheld {
                        subnet,
                        winner: winner.clone(),
                    }
                };
                machine_peers.push(DesiredBuiltinWireguardMachinePeer {
                    machine_id,
                    public_key: candidate.public_key,
                    subnet_v6: identity.subnet,
                    endpoint: candidate.endpoint,
                    container_route,
                });
            }
            CandidateKind::Peer { peer_id } => {
                roaming_peers.push(DesiredBuiltinWireguardRoamingPeer {
                    peer_id,
                    public_key: candidate.public_key,
                    subnet_v6: identity.subnet,
                    endpoint: candidate.endpoint,
                });
            }
        }
    }

    Ok(BuiltinWireguardMeshOutcome::Desired(
        DesiredBuiltinWireguardMesh {
            local,
            local_container_subnet,
            cluster_container_prefix: cluster.prefix.clone(),
            machine_peers,
            roaming_peers,
            ebpf_routes,
            evidence,
        },
    ))
}

fn record_identity_claim<Key>(
    winners: &mut BTreeMap<Key, RosterMemberId>,
    key: Key,
    claim: MeshIdentityClaim,
    candidate: &RosterMemberId,
    excluded: &mut BTreeSet<RosterMemberId>,
    conflicts: &mut Vec<MeshIdentityConflict>,
) where
    Key: Ord,
{
    if let Some(winner) = winners.get(&key) {
        excluded.insert(candidate.clone());
        conflicts.push(MeshIdentityConflict {
            claim,
            winner: winner.clone(),
            loser: candidate.clone(),
        });
    } else {
        winners.insert(key, candidate.clone());
    }
}

#[derive(Debug)]
struct Candidate {
    id: RosterMemberId,
    kind: CandidateKind,
    public_key: WireGuardPublicKey,
    stored_address: Ipv6Addr,
    endpoint: Option<SocketAddr>,
    subnet_v4: Option<MachineEndpointSubnet>,
}

#[derive(Debug)]
enum CandidateKind {
    Machine { machine_id: MachineRowId },
    Peer { peer_id: PeerId },
}

impl Candidate {
    fn from_machine(
        cluster: &ClusterDocument,
        row: NamedAcceptedRow<MachineDocument>,
    ) -> Result<Self, BuiltinWireguardMeshError> {
        let machine_id = MachineRowId::try_new(row.id.as_str().to_owned())
            .expect("reader-accepted machine ids are canonical ULIDs");
        let id = RosterMemberId::Machine {
            machine_id: machine_id.clone(),
        };
        let MachineTransport::Wireguard {
            pubkey,
            addr_v6,
            endpoint,
            subnet_v4,
        } = row.value.transport
        else {
            return Err(BuiltinWireguardMeshError::UnexpectedTransport { member_id: id });
        };
        if !cluster.prefix.contains_subnet(&subnet_v4) {
            return Err(
                BuiltinWireguardMeshError::ContainerSubnetOutsideClusterPrefix {
                    machine_id,
                    subnet: subnet_v4,
                    cluster_prefix: cluster.prefix.clone(),
                },
            );
        }
        Ok(Self {
            id,
            kind: CandidateKind::Machine { machine_id },
            public_key: pubkey,
            stored_address: addr_v6,
            endpoint,
            subnet_v4: Some(subnet_v4),
        })
    }

    fn from_peer(
        _cluster: &ClusterDocument,
        row: NamedAcceptedRow<PeerDocument>,
    ) -> Result<Self, BuiltinWireguardMeshError> {
        let peer_id = PeerId::try_new(row.id.as_str().to_owned())
            .expect("reader-accepted peer ids are canonical ULIDs");
        let id = RosterMemberId::Peer {
            peer_id: peer_id.clone(),
        };
        let PeerTransport::Wireguard {
            pubkey,
            addr_v6,
            endpoint,
        } = row.value.transport
        else {
            return Err(BuiltinWireguardMeshError::UnexpectedTransport { member_id: id });
        };
        Ok(Self {
            id,
            kind: CandidateKind::Peer { peer_id },
            public_key: pubkey,
            stored_address: addr_v6,
            endpoint,
            subnet_v4: None,
        })
    }
}
