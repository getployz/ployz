//! Ingress DTOs: certificate material, routes, doors and private access.

use serde::{Deserialize, Serialize};
use std::{
    collections::{BTreeMap, BTreeSet},
    fmt,
};
use ts_rs::TS;

use super::error::RpcError;
use crate::{
    CertificateHost, Ciphertext, ContainerAddress, ContainerId, ContainerObservation, MachineId,
    QualifiedService, ServiceId,
};

/// Publish or clear operator-supplied Certificate Material for one certificate hostname.
///
/// Published material is served as given; ACME never orders, renews, or overwrites it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct PublishCertificateMaterialRequest {
    pub hostname: CertificateHost,
    pub change: CertificateMaterialChange,
}

/// Set replaces the hostname's material; Clear removes published material and
/// returns the hostname to ACME.
#[derive(Clone, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "action", rename_all = "snake_case")]
pub enum CertificateMaterialChange {
    Set {
        certificate_chain_pem: String,
        private_key_pem: String,
    },
    Clear,
}

// Requests may be logged; the private key never is.
impl fmt::Debug for CertificateMaterialChange {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Set {
                certificate_chain_pem,
                ..
            } => formatter
                .debug_struct("Set")
                .field("certificate_chain_pem", certificate_chain_pem)
                .field("private_key_pem", &"<redacted>")
                .finish(),
            Self::Clear => formatter.write_str("Clear"),
        }
    }
}

/// The certificate row holds the published material, or no longer holds published material.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct CertificateMaterialPublished {}

/// Read which copies of one Service each live proxy routes, from the target
/// Machine's replicated store.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadRoutesRequest {
    pub service: QualifiedService,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RoutesRead {
    /// Each Machine not reported Down and the copies its proxy acks. `None` when its
    /// row for the Service has not arrived or does not decode: unknown, not empty.
    #[serde(default)]
    pub routes: BTreeMap<MachineId, Option<BTreeSet<ContainerId>>>,
    /// The Machine list replicated in full, so `routes` names every live Machine.
    #[serde(default)]
    pub complete: bool,
    /// Machines the membership sample reported Down; their routes no longer count.
    #[serde(default)]
    pub down: BTreeSet<MachineId>,
}

/// Read the address this Machine's proxy routes each copy at.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReadRoutedAddressesRequest {}

/// What this Machine's proxy serves, per Service and copy. `None` when this daemon
/// can't confirm what its proxy serves, such as before its first load or after it
/// wrote a config the proxy may or may not have read.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct RoutedAddresses {
    #[serde(default)]
    pub routes: Option<BTreeMap<QualifiedService, BTreeMap<ContainerId, ContainerAddress>>>,
}

/// One way into a Machine's Ingress Proxy. An unknown Door from a newer client
/// reads as `Unrecognized`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum IngressDoor {
    /// The holder's host ports 80 and 443.
    Public,
    /// A Tunnel Connector in the holder's network, reaching Caddy's loopback
    /// listener. The holder publishes no host port for it.
    Tunnel,
    #[serde(other)]
    Unrecognized,
}

impl IngressDoor {
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Public => "public",
            Self::Tunnel => "tunnel",
            Self::Unrecognized => "unrecognized",
        }
    }
}

/// The Doors a Machine's Ingress Proxy should answer on.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(transparent)]
pub struct IngressDoors(pub BTreeSet<IngressDoor>);

impl IngressDoors {
    #[must_use]
    pub fn public() -> Self {
        Self(BTreeSet::from([IngressDoor::Public]))
    }

    #[must_use]
    pub fn tunnel() -> Self {
        Self(BTreeSet::from([IngressDoor::Tunnel]))
    }

    /// No Door: the holder publishes no port and no connector runs, so nothing reaches
    /// the Machine's Caddy until a Door opens.
    #[must_use]
    pub fn closed() -> Self {
        Self(BTreeSet::new())
    }

    /// Both Doors, which a switch holds open while the other side catches up.
    #[must_use]
    pub fn both() -> Self {
        Self(BTreeSet::from([IngressDoor::Public, IngressDoor::Tunnel]))
    }

    #[must_use]
    pub fn is_public_only(&self) -> bool {
        *self == Self::public()
    }

    #[must_use]
    pub fn has(&self, door: IngressDoor) -> bool {
        self.0.contains(&door)
    }

    /// Whether every Door is one this release knows. None at all is known: it closes the Machine.
    #[must_use]
    pub fn is_known(&self) -> bool {
        !self.has(IngressDoor::Unrecognized)
    }

    /// The Doors as `ployz ingress` lists them.
    #[must_use]
    pub fn names(&self) -> Vec<&'static str> {
        self.0.iter().map(|door| door.as_str()).collect()
    }
}

/// Bring this Machine's Ingress Proxy, the holder and the Caddy in its network,
/// to the daemon's pinned wiring for these Doors.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SetIngressDoorsRequest {
    pub doors: IngressDoors,
    /// The Ingress Proxy Service's id. The operator allocates it, and every
    /// Machine derives the holder's id from it, so all Machines agree.
    pub proxy_service: ServiceId,
    /// The Cloudflare tunnel token sealed to this Cluster under the
    /// `ingress-tunnel` Platform sealing purpose. Required with the Tunnel Door
    /// and ignored without it. A client sends it only to a daemon advertising
    /// `ployz.ingress.tunnel.v1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sealed_token: Option<Ciphertext>,
}

/// What the daemon did to one Ingress component.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum DoorChange {
    /// It already ran canonical and needed no Docker action.
    Unchanged,
    /// An existing canonical Container was started.
    Started,
    /// Caddy was restarted to join the running holder's network again.
    Rejoined,
    /// A new Container was created and started.
    Created,
    #[serde(other)]
    Unrecognized,
}

/// One Ingress component as the route left it.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct DoorOutcome {
    pub now: ContainerObservation,
    pub change: DoorChange,
}

/// The Docker stage of [`SetIngressDoorsRequest`] that failed.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum IngressStage {
    Holder,
    Proxy,
    Tunnel,
    #[serde(other)]
    Unrecognized,
}

/// A Docker stage that failed after an earlier stage completed. Admission and
/// enforcement failures are ordinary RPC errors instead.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct IngressDoorsFailure {
    pub stage: IngressStage,
    pub error: RpcError,
}

/// What [`SetIngressDoorsRequest`] completed, in order. A `failure` names the
/// stage that stopped the route, and the fields before it are the completed prefix.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
pub struct IngressDoorsSet {
    /// The holder, once its stage completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub holder: Option<DoorOutcome>,
    /// The Caddy in the holder's network, once its stage completed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub proxy: Option<DoorOutcome>,
    /// The Tunnel Connector in the holder's network, once its stage completed.
    /// Absent when the Tunnel Door was not asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub tunnel: Option<DoorOutcome>,
    /// Reserved Containers the route removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired: Vec<ContainerId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub failure: Option<IngressDoorsFailure>,
}

/// Turn Private Access on or off on this Machine: place the Tailscale router that
/// advertises the Cluster network to a tailnet, or remove it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SetPrivateAccessRequest {
    pub enabled: bool,
    /// The Tailscale auth key sealed to this Cluster under the `private-access`
    /// Platform sealing purpose. Required to turn Private Access on and ignored
    /// when it turns off. A client sends it only to a daemon advertising
    /// `ployz.private-access.v1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub sealed_auth_key: Option<Ciphertext>,
    /// A person's approval of `sealed_auth_key`. A Machine that enforces Device approval
    /// turns Private Access on for an Operator only with one; a Full caller needs none,
    /// and turning off needs none. A client sends it only to a daemon advertising
    /// `ployz.private-access.approval.v1`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub approval: Option<crate::authority::KeyApproval>,
}

/// Why a Private Access router is not serving.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum PrivateAccessProblem {
    /// Tailscale refused the auth key: it is revoked, expired or not valid.
    KeyRefused,
    /// The router did not stay running.
    NotRunning,
    #[serde(other)]
    Unrecognized,
}

/// A router that was placed and then did not serve.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
pub struct PrivateAccessFailure {
    pub problem: PrivateAccessProblem,
    /// The last thing the router said, without the auth key.
    pub detail: String,
}

/// What [`SetPrivateAccessRequest`] did on one Machine.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize, TS)]
pub struct PrivateAccessSet {
    /// The router once placed. Absent when Private Access turned off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub router: Option<DoorOutcome>,
    /// Routers the route removed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub retired: Vec<ContainerId>,
    /// Set when the router was placed and did not serve.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub failure: Option<PrivateAccessFailure>,
}

/// A listed Machine whose proxy has not acked the copies a reader waits for.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Unacked {
    /// Its row arrived without them.
    Stale(MachineId),
    /// Its row has not arrived or does not decode.
    Unanswered(MachineId),
}

impl Unacked {
    #[must_use]
    pub const fn machine(&self) -> MachineId {
        match self {
            Self::Stale(machine) | Self::Unanswered(machine) => *machine,
        }
    }
}

impl RoutesRead {
    /// Each listed Machine outside this read's `down` whose proxy acks no copies
    /// `satisfied` accepts.
    pub fn unacked<'read>(
        &'read self,
        satisfied: impl Fn(&BTreeSet<ContainerId>) -> bool + 'read,
    ) -> impl Iterator<Item = Unacked> + 'read {
        self.routes
            .iter()
            .filter(|(machine, _)| !self.down.contains(*machine))
            .filter_map(move |(machine, routed)| match routed {
                None => Some(Unacked::Unanswered(*machine)),
                Some(routed) if !satisfied(routed) => Some(Unacked::Stale(*machine)),
                Some(_) => None,
            })
    }
}
