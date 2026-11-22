//! What a trusted key may approve: its kind, and the caps and places its scope names.
//!
//! A Device reads as a full CLI key and a passkey as a person's key, both over the whole
//! Cluster. A CI or agent key is a grant a person signed: it may sign Service versions
//! only, and only inside the places its scope names. [`TrustedKeys::covers`] is the one
//! check of what a key may approve.
//!
//! [`TrustedKeys::covers`]: super::TrustedKeys::covers

use std::fmt;

use serde::{Deserialize, Deserializer, Serialize, Serializer};
use ts_rs::TS;
use uuid::Uuid;

use super::{SigningPublicKey, canonical::Timestamp, trusted_key::PasskeyApprover};
use crate::{DockerVolumeName, Namespace, ServiceName, VolumeOrigin};

/// One thing a key may do.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum Cap {
    /// Sign a Service version.
    Spec,
    /// Sign a spec that reaches its Server's host.
    HostReach,
    Exec,
    /// Change the keys a Machine trusts.
    Key,
    /// Admit a Machine, or turn Private Access on.
    Join,
}

const CAPS: [Cap; 5] = [Cap::Spec, Cap::HostReach, Cap::Exec, Cap::Key, Cap::Join];

/// A set of caps. It travels as a sorted list of names, and a name this build doesn't
/// know is dropped, which only narrows.
#[derive(Clone, Copy, Eq, PartialEq, TS)]
#[ts(as = "Vec<Cap>")]
pub struct CapSet(u8);

impl CapSet {
    pub const ALL: Self = Self(0b1_1111);

    #[must_use]
    pub fn of(caps: &[Cap]) -> Self {
        Self(caps.iter().fold(0, |bits, cap| bits | Self::bit(*cap)))
    }

    const fn bit(cap: Cap) -> u8 {
        1 << cap as u8
    }

    #[must_use]
    pub const fn holds(self, cap: Cap) -> bool {
        self.0 & Self::bit(cap) != 0
    }

    #[must_use]
    pub const fn within(self, ceiling: Self) -> bool {
        self.0 & !ceiling.0 == 0
    }

    #[must_use]
    pub const fn without(self, cap: Cap) -> Self {
        Self(self.0 & !Self::bit(cap))
    }

    fn caps(self) -> impl Iterator<Item = Cap> {
        CAPS.into_iter().filter(move |cap| self.holds(*cap))
    }
}

impl fmt::Debug for CapSet {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.debug_set().entries(self.caps()).finish()
    }
}

impl Serialize for CapSet {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_seq(self.caps())
    }
}

impl<'de> Deserialize<'de> for CapSet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let names = Vec::<serde_json::Value>::deserialize(deserializer)?;
        let known: Vec<Cap> = names
            .into_iter()
            .filter_map(|name| serde_json::from_value(name).ok())
            .collect();
        Ok(Self::of(&known))
    }
}

/// Who holds a key, which bounds what any scope can give it.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum KeyKind {
    /// A passkey. It opens no connection, so it never execs.
    Person,
    /// A Device.
    FullCli,
    Ci,
    Agent,
}

impl KeyKind {
    #[must_use]
    pub const fn ceiling(self) -> CapSet {
        match self {
            Self::FullCli => CapSet::ALL,
            Self::Person => CapSet::ALL.without(Cap::Exec),
            Self::Ci | Self::Agent => CapSet(1 << Cap::Spec as u8),
        }
    }

    /// Whether this kind's keys sign the whole Cluster's statements: keys, joins, rules
    /// and forgets. A CI or agent key signs Service versions only.
    #[must_use]
    pub const fn whole_cluster(self) -> bool {
        matches!(self, Self::Person | Self::FullCli)
    }
}

impl fmt::Display for KeyKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::Person => "person",
            Self::FullCli => "full CLI",
            Self::Ci => "CI",
            Self::Agent => "agent",
        })
    }
}

/// Where a key's caps hold.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum KeyPlaces {
    Cluster,
    /// Never empty.
    Only(#[serde(deserialize_with = "some_places")] Vec<KeyPlace>),
}

fn some_places<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<KeyPlace>, D::Error> {
    let places = Vec::<KeyPlace>::deserialize(deserializer)?;
    if places.is_empty() {
        return Err(serde::de::Error::custom("a scope names at least one place"));
    }
    Ok(places)
}

/// One Environment's Namespace, or one Service in it. A Sealed value names its
/// Environment by UUID while a version names its Namespace, and this Machine has no map
/// from one to the other, so a place names both and a person signs the pair together.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct KeyPlace {
    #[ts(type = "string")]
    pub environment: Uuid,
    pub namespace: Namespace,
    /// Absent for every Service of the Namespace.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub service: Option<PlaceService>,
}

/// A Service by the authored ID its Sealed values are bound to, and the runtime name its
/// versions name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct PlaceService {
    #[ts(type = "string")]
    pub id: Uuid,
    pub name: ServiceName,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct KeyScope {
    pub caps: CapSet,
    pub places: KeyPlaces,
}

impl KeyScope {
    /// Whether a key of `kind` may hold this scope: caps within its ceiling, and for an
    /// agent key, never the whole Cluster.
    #[must_use]
    pub fn fits(&self, kind: KeyKind) -> bool {
        self.caps.within(kind.ceiling())
            && !(kind == KeyKind::Agent && self.places == KeyPlaces::Cluster)
    }
}

/// What a statement asks of the key that signed it.
#[derive(Clone, Copy, Debug)]
pub enum Ask<'a> {
    Version {
        namespace: &'a Namespace,
        service: &'a ServiceName,
        /// The version asks every Member to forget older ones.
        forgets: bool,
        /// The spec it admits reaches its Server's host, as [`super::deploy::host_reach`]
        /// says. A signed body names only a digest, so only the verifier holding the
        /// spec sets this.
        reaches_host: bool,
    },
    Rule,
    Key,
    Join,
    PrivateAccess,
}

impl Ask<'_> {
    /// What the refusal says the key may not approve.
    #[must_use]
    pub const fn what(self) -> &'static str {
        match self {
            Self::Version { forgets: true, .. } => "a forget",
            Self::Version {
                reaches_host: true, ..
            } => "a version that reaches its Server's host",
            Self::Version { .. } => "a version of this Service",
            Self::Rule => "an automatic deploy rule",
            Self::Key => "a key change",
            Self::Join => "a join",
            Self::PrivateAccess => "Private Access",
        }
    }

    const fn caps(self) -> CapSet {
        CapSet(match self {
            Self::Version {
                reaches_host: true, ..
            } => CapSet::bit(Cap::Spec) | CapSet::bit(Cap::HostReach),
            Self::Version { .. } | Self::Rule => CapSet::bit(Cap::Spec),
            Self::Key => CapSet::bit(Cap::Key),
            Self::Join | Self::PrivateAccess => CapSet::bit(Cap::Join),
        })
    }

    /// Whether a CI or agent key may sign it at all.
    const fn scoped(self) -> bool {
        matches!(self, Self::Version { forgets: false, .. })
    }
}

impl KeyPlaces {
    /// Whether these places hold what `ask` reaches: a version's Service, or the whole
    /// Cluster for every other statement.
    fn hold(&self, ask: Ask<'_>) -> bool {
        match (self, ask) {
            (Self::Cluster, _) => true,
            (
                Self::Only(places),
                Ask::Version {
                    namespace, service, ..
                },
            ) => places.iter().any(|place| {
                &place.namespace == namespace
                    && place
                        .service
                        .as_ref()
                        .is_none_or(|held| &held.name == service)
            }),
            (Self::Only(_), _) => false,
        }
    }
}

/// Whether a key of `kind` holding `scope` may approve `ask`.
pub(super) fn allows(kind: KeyKind, scope: Option<&KeyScope>, ask: Ask<'_>) -> bool {
    let (caps, places) = match scope {
        Some(scope) => (scope.caps, &scope.places),
        None => (CapSet::ALL, &KeyPlaces::Cluster),
    };
    (kind.whole_cluster() || ask.scoped())
        && ask.caps().within(caps)
        && ask.caps().within(kind.ceiling())
        && !(kind == KeyKind::Agent && *places == KeyPlaces::Cluster)
        && places.hold(ask)
}

/// The kinds a grant holds.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum GrantKind {
    Ci,
    Agent,
}

impl From<GrantKind> for KeyKind {
    fn from(kind: GrantKind) -> Self {
        match kind {
            GrantKind::Ci => Self::Ci,
            GrantKind::Agent => Self::Agent,
        }
    }
}

/// A persistent Volume a grant's key may mount, by the origin its mount expects and its
/// physical name.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct GrantVolume {
    pub origin: VolumeOrigin,
    pub name: DockerVolumeName,
}

/// A CI or agent key a person trusted, with the scope and Volumes they signed for it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct Grant {
    pub key: SigningPublicKey,
    pub kind: GrantKind,
    pub scope: KeyScope,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<GrantVolume>>")]
    pub volumes: Vec<GrantVolume>,
    /// When the bundle that set this scope was issued. Only a newer bundle edits it.
    pub issued_at: Timestamp,
}

/// A grant this Machine trusts, and who approved it here.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct TrustedGrant {
    pub grant: Grant,
    pub approved_by: PasskeyApprover,
}

/// Read a list of grants, skipping each entry this build can't read, such as one a newer
/// daemon wrote with a field it doesn't know. Skipping fails closed: that key is trusted
/// for nothing here.
///
/// # Errors
/// Returns the deserializer's error when the value is not a list.
pub fn readable_grants<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Vec<TrustedGrant>, D::Error> {
    Ok(Vec::<serde_json::Value>::deserialize(deserializer)?
        .into_iter()
        .filter_map(|entry| serde_json::from_value(entry).ok())
        .collect())
}

#[cfg(test)]
#[path = "scope_tests.rs"]
mod tests;
