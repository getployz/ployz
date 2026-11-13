//! Machine lifecycle and status DTOs: the contract, enrollment and join approval, leave and forget, Machine lists and updates, endpoint selection and Cluster naming.

use ipnet::Ipv4Net;
use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    net::IpAddr,
};
use ts_rs::TS;

use super::error::RpcError;
use crate::{
    AdvertisedEndpoint, CapabilityName, Machine, MachineId, MachineName, MachineObservation,
    MachineRuntime, MachineUpdate, PinnedAddresses, SelectedEndpoint, StorageChoice,
    WireGuardPublicKey,
};

/// The empty payload of the capability-description command.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct DescribeContractRequest {}

/// This Machine's own enrollment facts, returned with every lifecycle refusal.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineNow {
    pub id: MachineId,
    pub phase: crate::LocalMachinePhase,
    #[serde(default)]
    pub machine: Option<Machine>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EnrollRequest {
    #[serde(rename = "as")]
    pub role: EnrollAs,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wireguard_mtu: Option<u32>,
    #[serde(default, skip_serializing_if = "PinnedAddresses::is_empty")]
    pub pinned: PinnedAddresses,
}

#[expect(
    clippy::large_enum_variant,
    reason = "a wire type, kept unboxed so serde and ts-rs see the payload"
)]
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EnrollAs {
    /// First Machine of a new Cluster.
    Founder {
        name: MachineName,
        #[ts(type = "string")]
        cluster_network: Ipv4Net,
        /// Complete policy committed in the first Machine assignment, before participation.
        initial_policy: crate::InitialMachinePolicy,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(type = "string | null")]
        public_ip: Option<IpAddr>,
        #[serde(default)]
        advertised_endpoints: Vec<AdvertisedEndpoint>,
        /// What grants this founding. A client from before 0.3.15 doesn't say, and a daemon
        /// from before 0.3.15 ignores it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        grant: Option<crate::FoundingGrant>,
    },
    /// Joins with the registration a Cluster member issued.
    Member { registration: Registered },
}

/// A resend replays `Enrolled` with the stored record; the daemon restarts only
/// after a fresh enrollment.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Enrolled {
    Enrolled {
        machine: Machine,
        /// Present after an approved join: the driver relays it in Register so the admitting
        /// Machine publishes this Machine's row.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        join_confirmation: Option<crate::authority::join::JoinConfirmation>,
    },
    /// Enrolled as someone else. `error.code` is `conflict`.
    Refused { now: MachineNow, error: RpcError },
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct LeaveRequest {
    /// A person's teardown naming this request, or the preview rule whose own preview it
    /// closes. A Machine under Device enforcement requires it from an Operator caller; an
    /// older daemon ignores it. Sent only to a daemon advertising
    /// [`crate::TEARDOWN_CAPABILITY`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub teardown: Option<Box<crate::authority::TeardownApproval>>,
}

/// A successful Leave cannot report a Machine that still participates.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum LeftPhase {
    Resetting,
    Uninitialized,
}

/// A resend on a Machine already Resetting or Uninitialized replies `Left` with that phase.
#[expect(
    clippy::large_enum_variant,
    reason = "a wire type, kept unboxed so serde and ts-rs see the payload"
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Left {
    Left {
        phase: LeftPhase,
        /// Publication removal failed best effort; the local leave happened.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        reset_warning: Option<String>,
    },
    /// An upgrade attempt is nonterminal. `error.code` is `conflict`.
    Refused { now: MachineNow, error: RpcError },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ForgetRequest {
    pub machine_id: MachineId,
    /// A person's teardown naming this request, or the preview rule whose own preview it
    /// closes. A Machine under Device enforcement requires it from an Operator caller; an
    /// older daemon ignores it. Sent only to a daemon advertising
    /// [`crate::TEARDOWN_CAPABILITY`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub teardown: Option<Box<crate::authority::TeardownApproval>>,
}

/// A missing row is success.
#[expect(
    clippy::large_enum_variant,
    reason = "a wire type, kept unboxed so serde and ts-rs see the payload"
)]
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum Forgotten {
    Forgotten,
    /// `machine_id` is this Machine; a Machine leaves itself with Leave. `error.code` is `invalid_argument`.
    Refused {
        now: MachineNow,
        error: RpcError,
    },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct RegisterRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub join_approval: Option<crate::authority::Signed<crate::authority::JoinApproval>>,
    /// The joiner's proof that it opened its approved capsule. Under Device enforcement the
    /// admitting Machine publishes an approved row only once a Register carries it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub join_confirmation: Option<crate::authority::join::JoinConfirmation>,
    /// Durable identity of the joining Machine.
    pub machine_id: MachineId,
    /// Client-selected subnet, required for Register publication.
    /// Allocation policy callers omit it before selecting an assignment.
    #[serde(default)]
    pub assigned_subnet: Option<crate::MachineSubnet>,
    /// Complete policy committed in the first Machine assignment, before participation.
    pub initial_policy: crate::InitialMachinePolicy,
    pub name: MachineName,
    pub storage: StorageChoice,
    pub public_key: WireGuardPublicKey,
    #[serde(default)]
    pub public_ip: Option<IpAddr>,
    pub advertised_endpoints: Vec<AdvertisedEndpoint>,
    #[serde(default)]
    pub runtime: MachineRuntime,
    /// The capabilities the client that runs this joiner's catch-up honors, such as
    /// [`SET_CLUSTER_DOORS_CAPABILITY`] when it follows the Cluster's declared Doors.
    /// Empty claims nothing. Not part of the retry identity.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub capabilities: Vec<String>,
}

/// Read the join requests waiting for a Device's approval on this Machine.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct PendingJoinsRequest {}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PendingJoins {
    pub joins: Vec<crate::authority::JoinApproval>,
}

/// Carry Device authority to the Machine that will admit the join.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ApproveJoinRequest {
    pub approval: crate::authority::Signed<crate::authority::JoinApproval>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct JoinApproved {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListMachinesRequest {}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RuntimeWatchRequest {}

/// An operator's endpoint for reaching `peer` from the Machine that takes the request.
///
/// It sets only that Machine's own Selected Endpoint; `peer`'s record is unchanged.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectEndpointRequest {
    pub peer: MachineId,
    pub endpoint: SelectedEndpoint,
}

/// The Machine that took a [`SelectEndpointRequest`] now dials `endpoint` for `peer`.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct EndpointSelected {
    pub peer: MachineId,
    pub endpoint: SelectedEndpoint,
}

/// Commands are closed and own their typed payloads.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateMachineRequest {
    pub update: MachineUpdate,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct Registered {
    pub assigned_machine: Machine,
    pub visible_peers: Vec<Machine>,
    pub target_versions: BTreeMap<String, i64>,
    /// Absent from an issuer that holds no Sealing Key, or predates it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub sealing_key: Option<crate::SealedSealingKey>,
    /// The issuer's approved Devices, which a fresh joiner admits at Full scope.
    /// Absent from an issuer that predates Devices: the joiner starts with none.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<crate::DeviceKey>>")]
    pub devices: Vec<crate::DeviceKey>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MachineList {
    /// Enrollment facts observed by this Entry Machine.
    #[serde(default)]
    pub enrollment: Option<crate::EnrollmentSnapshot>,
    pub machines: Vec<MachineObservation>,
}

/// Read the Cluster Name on the target Machine's replica.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadClusterNameRequest {}

/// Name the Cluster. The target Machine writes it to its replica, and the last
/// write wins across the Cluster.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SetClusterNameRequest {
    pub name: crate::ClusterName,
}

/// The Cluster Name on the target Machine's replica.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClusterNamed {
    /// `None` when the Cluster has no name, or its row has not replicated to this
    /// Machine yet.
    #[serde(default)]
    pub name: Option<crate::ClusterName>,
}

/// Declare the Doors every ingress Server of the Cluster holds. The target Machine
/// writes them to its replica, and the last write wins across the Cluster. A daemon
/// that serves this route also carries the declaration in
/// [`EnrollmentSnapshot::doors`](crate::EnrollmentSnapshot::doors), and refuses a
/// Register whose client can't follow it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SetClusterDoorsRequest {
    pub doors: super::IngressDoors,
}

/// The Doors the target Machine's replica declares after the write.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ClusterDoorsDeclared {
    pub doors: super::IngressDoors,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct MachineUpdated {
    pub machine: Machine,
}

/// How a removed Machine left the Cluster.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Departed {
    /// It answered Leave with [`Left::Left`]: its reset committed. `row_warning` is that
    /// reply's `reset_warning`, why its own row removal failed after the reset.
    Reset { row_warning: Option<String> },
    /// Its Leave failed as unavailable or unreachable, so no successful Leave reply confirmed
    /// the reset: it may have reset and lost its reply. Its row was forgotten anyway.
    Unreachable { cause: String },
    /// It was already gone from the Cluster: a Forget ran, and whether its reset did is unknown.
    AlreadyForgotten,
}

impl Departed {
    /// The reset warning a reader older than [`Departed`] reads: why the reset isn't proven,
    /// or a reset's row warning.
    fn warning(&self) -> Option<String> {
        match self {
            Self::Reset { row_warning } => row_warning.clone(),
            Self::Unreachable { cause } => Some(format!(
                "target is unreachable; removing shared rows: {cause}"
            )),
            Self::AlreadyForgotten => {
                Some("its reset couldn't be confirmed: it was already gone from the Cluster".into())
            }
        }
    }
}

/// A Machine removal that went through. `reset_warning` is `departed` as prose, for readers
/// older than it; both come from one [`Departed`], so they can't disagree.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, TS)]
pub struct LocalMachineRemoved {
    reset_warning: Option<String>,
    departed: Departed,
}

impl LocalMachineRemoved {
    #[must_use]
    pub fn departed(&self) -> &Departed {
        &self.departed
    }

    #[must_use]
    pub fn reset_warning(&self) -> Option<&str> {
        self.reset_warning.as_deref()
    }
}

impl From<Departed> for LocalMachineRemoved {
    fn from(departed: Departed) -> Self {
        Self {
            reset_warning: departed.warning(),
            departed,
        }
    }
}

impl<'de> Deserialize<'de> for LocalMachineRemoved {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            departed: Departed,
        }
        Wire::deserialize(deserializer).map(|wire| wire.departed.into())
    }
}

/// The capabilities currently advertised by one Machine.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ContractDescription {
    pub machine_id: MachineId,
    pub protocol_major: u32,
    /// Diagnostic only. Callers select behavior using capability names.
    pub daemon_version: String,
    #[serde(default)]
    pub capabilities: BTreeSet<CapabilityName>,
    /// The Sealing Key the Machine holds, as Inspect's `sealing` says, so a caller can tell
    /// its Cluster without another call. Absent says nothing: the Machine holds none, isn't
    /// in a Cluster, or predates the field; Inspect still answers.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub sealing: Option<crate::Sealing>,
    /// The build attest key the Machine derives from that Sealing Key, which seals build
    /// secrets to a rule build's recipient. Absent like `sealing`, and from a daemon that
    /// predates rule builds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub attest: Option<crate::authority::AttestKey>,
}

impl ContractDescription {
    #[must_use]
    pub fn supports(&self, capability: &str) -> bool {
        self.capabilities
            .iter()
            .any(|advertised| advertised.as_str() == capability)
    }
}
