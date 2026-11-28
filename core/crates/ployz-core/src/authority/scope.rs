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

use super::{
    SigningPublicKey,
    canonical::{CanonicalError, Timestamp},
    trusted_key::PasskeyApprover,
};
use crate::{
    ContainerKind, DockerVolumeName, Namespace, ResolvedServiceSpec, SealedScope, ServiceName,
    VolumeOrigin,
};

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

/// Where a key's caps hold: the Environments, or Services in them, it names.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum KeyPlaces {
    /// A CI key made before every key named its Environments. It reaches none, and a
    /// Machine refuses everything it signs until a person makes a new key in its place.
    #[serde(rename = "cluster")]
    Unscoped,
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
    /// Whether a key of `kind` may hold this scope: caps within its ceiling, and the
    /// places it names.
    #[must_use]
    pub fn fits(&self, kind: KeyKind) -> bool {
        self.caps.within(kind.ceiling()) && self.places != KeyPlaces::Unscoped
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
        /// The Sealed parts and persistent Volumes the spec reaches. Empty until the
        /// verifier holds the spec.
        reach: &'a Reach,
    },
    Rule,
    /// One commit approved for one preview of a preview rule.
    Commit,
    Key,
    Join,
    PrivateAccess,
    /// A new Volume made from a restore point of another.
    Copy,
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
            Self::Commit => "a preview's commit",
            Self::Key => "a key change",
            Self::Join => "a join",
            Self::PrivateAccess => "Private Access",
            Self::Copy => "a copy of a Volume",
        }
    }

    const fn caps(self) -> CapSet {
        CapSet(match self {
            Self::Version {
                reaches_host: true, ..
            } => CapSet::bit(Cap::Spec) | CapSet::bit(Cap::HostReach),
            Self::Version { .. } | Self::Rule | Self::Commit | Self::Copy => CapSet::bit(Cap::Spec),
            Self::Key => CapSet::bit(Cap::Key),
            Self::Join | Self::PrivateAccess => CapSet::bit(Cap::Join),
        })
    }
}

impl KeyPlace {
    /// Whether this one place holds a version of `service` in `namespace` that opens
    /// `values`: each part sealed in this place's Environment, and under a Service place,
    /// each sealed for that Service alone.
    fn holds(&self, namespace: &Namespace, service: &ServiceName, values: &[ValueReach]) -> bool {
        &self.namespace == namespace
            && self
                .service
                .as_ref()
                .is_none_or(|held| &held.name == service)
            && values.iter().all(|value| {
                value.environment == self.environment
                    && self
                        .service
                        .as_ref()
                        .is_none_or(|held| value.scope == SealedScope::Service(held.id))
            })
    }
}

/// Why [`allows`] refused.
pub(super) enum Denied {
    /// The key's kind, caps or places don't reach what the statement asks.
    Out,
    /// A place holds the version, but none holds it with every Sealed part it opens.
    Values,
    /// The key's grant names no entry for a Volume the spec mounts.
    Volume(DockerVolumeName),
    /// The key's grant names no places.
    Unscoped,
}

/// Whether a key of `kind` may approve `ask`. `grant` is its grant, `None` for a Device or
/// passkey, which holds its kind's ceiling over the whole Cluster and every Volume. A grant
/// signs only a version that forgets nothing, of a Service one of its places holds.
pub(super) fn allows(kind: KeyKind, grant: Option<&Grant>, ask: Ask<'_>) -> Result<(), Denied> {
    if !ask.caps().within(kind.ceiling()) {
        return Err(Denied::Out);
    }
    let Some(grant) = grant else {
        return Ok(());
    };
    let KeyPlaces::Only(places) = &grant.scope.places else {
        return Err(Denied::Unscoped);
    };
    let Ask::Version {
        namespace,
        service,
        forgets: false,
        reach,
        ..
    } = ask
    else {
        return Err(Denied::Out);
    };
    let holds = |values: &[ValueReach]| {
        places
            .iter()
            .any(|place| place.holds(namespace, service, values))
    };
    if !ask.caps().within(grant.scope.caps) || !holds(&[]) {
        return Err(Denied::Out);
    }
    if !holds(&reach.values) {
        return Err(Denied::Values);
    }
    match reach
        .volumes
        .iter()
        .find(|volume| !grant.volumes.contains(volume))
    {
        Some(missing) => Err(Denied::Volume(missing.name.clone())),
        None => Ok(()),
    }
}

/// One Sealed part a Container opens: the Environment it was sealed in, and whether it
/// was authored as Shared or for one Service, by that Service's authored ID.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ValueReach {
    pub environment: Uuid,
    pub scope: SealedScope,
}

/// What a Container reaches that a CI or agent key's grant must hold: every Sealed part
/// its values open, hook values included, and every persistent Volume it mounts. A
/// Machine keeps it with the admission, so a later start checks the same reach against
/// the key's scope then.
#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Reach {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub values: Vec<ValueReach>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<GrantVolume>,
}

impl Reach {
    /// Nothing, as a version asks before its verifier holds the spec.
    #[must_use]
    pub fn none() -> &'static Self {
        static NONE: Reach = Reach {
            values: Vec::new(),
            volumes: Vec::new(),
        };
        &NONE
    }

    /// What a Container of `kind` made from `spec` reaches. A Sealed part counts under the
    /// spec's Sealed identity: a spec with none opens no Sealed value. A mount counts for
    /// an Ordinary or Provisioned Volume, by the origin it expects and its physical name.
    ///
    /// # Errors
    /// Returns a [`CanonicalError`] when a Volume's birth has no canonical form.
    pub fn of(kind: ContainerKind, spec: &ResolvedServiceSpec) -> Result<Self, CanonicalError> {
        let mut reach = Self::default();
        if let Some(identity) = &spec.container.sealed_identity {
            let hook = match kind {
                ContainerKind::PreDeployHook => spec.pre_deploy.as_ref(),
                ContainerKind::ServiceContainer => None,
            };
            let values = spec
                .container
                .environment
                .values()
                .chain(hook.into_iter().flat_map(|hook| hook.environment.values()));
            for part in values.flat_map(crate::EnvValue::sealed_parts) {
                push_new(
                    &mut reach.values,
                    ValueReach {
                        environment: identity.environment,
                        scope: part.scope,
                    },
                );
            }
        }
        for volume in spec.volume_graph().mounted_volumes() {
            let Some(name) = volume.source.managed_docker_volume_name() else {
                continue;
            };
            push_new(
                &mut reach.volumes,
                GrantVolume {
                    origin: VolumeOrigin::expected(volume.source.birth(), name)?,
                    name: name.clone(),
                },
            );
        }
        Ok(reach)
    }
}

fn push_new<T: PartialEq>(list: &mut Vec<T>, item: T) {
    if !list.contains(&item) {
        list.push(item);
    }
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
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub name: Option<KeyName>,
    /// When the bundle that set this scope was issued. Only a newer bundle edits it.
    pub issued_at: Timestamp,
}

/// What a person calls a CI or agent key, such as the pipeline that holds it. Only a
/// label: printable, at most 64 characters. Control and Unicode format characters are
/// refused so a name can't make a terminal show another.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(type = "string")]
pub struct KeyName(String);

impl KeyName {
    /// # Errors
    /// Returns a [`ValueError`] for an empty, overlong or unprintable name.
    ///
    /// [`ValueError`]: crate::ValueError
    pub fn parse(value: &str) -> Result<Self, crate::ValueError> {
        if value.trim().is_empty()
            || value.chars().count() > 64
            || value.chars().any(super::trusted_key::unprintable)
        {
            return Err(crate::ValueError::new(
                "key name",
                value,
                "1 to 64 printable characters",
            ));
        }
        Ok(Self(value.to_owned()))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for KeyName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl TryFrom<String> for KeyName {
    type Error = crate::ValueError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<KeyName> for String {
    fn from(name: KeyName) -> Self {
        name.0
    }
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
