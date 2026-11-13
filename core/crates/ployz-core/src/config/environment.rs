//! Authored Environment documents, identity admission, and private-value redaction.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::{ConfigFileName, ConfigName, ContainerPath};

use super::{
    AWAITING_FINGERPRINT, AuthoredServiceConfig, ConfigError, EnvironmentNodeType, Generator,
    ReviewNodeIdentity, SecretCiphertext, ServiceGitBranch, ServiceSource, ValuePart,
    ValuePartOwner, parse_service_config,
};

/// Complete authored Environment state; compiled artifacts are not accepted here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedEnvironmentIntent {
    #[ts(type = "1")]
    pub version: u8,
    pub environment_slug: String,
    pub services: Vec<SavedServiceIntent>,
    pub volumes: Vec<SavedVolumeIntent>,
    /// Written only when there are some, so documents from before Configs keep
    /// their bytes and fingerprints.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<SavedConfigIntent>>")]
    pub configs: Vec<SavedConfigIntent>,
}

impl SavedEnvironmentIntent {
    /// Make the Services that build repository `repository_id` track `branch`: every
    /// one, or with `only`, those whose lineage is in it.
    pub fn retarget(&mut self, repository_id: u64, branch: &str, only: Option<&BTreeSet<&str>>) {
        for service in &mut self.services {
            if !service.config.source.builds_repository(repository_id)
                || only.is_some_and(|only| !only.contains(service.lineage_id.as_str()))
            {
                continue;
            }
            if let ServiceSource::Git { branch: at, .. } = &mut service.config.source {
                *at = ServiceGitBranch::Connected {
                    name: branch.to_owned(),
                };
            }
        }
    }
}

/// A Service, Volume or Config of a saved document, seen the same way: what it is, its
/// id, its lineage and the name it answers to.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SavedNode<'a> {
    pub kind: EnvironmentNodeType,
    pub id: &'a str,
    pub lineage: &'a str,
    pub name: &'a str,
}

impl SavedEnvironmentIntent {
    /// Every Service, then every Volume, then every Config.
    pub fn nodes(&self) -> impl Iterator<Item = SavedNode<'_>> {
        let services = self.services.iter().map(|service| SavedNode {
            kind: EnvironmentNodeType::Service,
            id: service.id.as_str(),
            lineage: service.lineage_id.as_str(),
            name: service.slug.as_str(),
        });
        let volumes = self.volumes.iter().map(|volume| SavedNode {
            kind: EnvironmentNodeType::Volume,
            id: volume.resource_id.as_str(),
            lineage: volume.resource_lineage_id.as_str(),
            name: volume.name.as_str(),
        });
        let configs = self.configs.iter().map(|config| SavedNode {
            kind: EnvironmentNodeType::Config,
            id: config.resource_id.as_str(),
            lineage: config.resource_lineage_id.as_str(),
            name: config.name.as_str(),
        });
        services.chain(volumes).chain(configs)
    }

    /// The node of lineage `lineage`: a Service before a Volume before a Config.
    #[must_use]
    pub fn node_by_lineage(&self, lineage: &str) -> Option<SavedNode<'_>> {
        self.nodes().find(|node| node.lineage == lineage)
    }

    /// The node `identity` names, by id.
    #[must_use]
    pub fn node_by_identity(&self, identity: &ReviewNodeIdentity) -> Option<SavedNode<'_>> {
        self.nodes()
            .find(|node| node.kind == identity.node_type && node.id == identity.id)
    }

    /// Add a copy of the node `identity` names in `source`, if it holds one.
    pub fn push_node(&mut self, source: &Self, identity: &ReviewNodeIdentity) {
        match identity.node_type {
            EnvironmentNodeType::Service => self.services.extend(
                source
                    .services
                    .iter()
                    .find(|service| service.id == identity.id)
                    .cloned(),
            ),
            EnvironmentNodeType::Volume => self.volumes.extend(
                source
                    .volumes
                    .iter()
                    .find(|volume| volume.resource_id == identity.id)
                    .cloned(),
            ),
            EnvironmentNodeType::Config => self.configs.extend(
                source
                    .configs
                    .iter()
                    .find(|config| config.resource_id == identity.id)
                    .cloned(),
            ),
        }
    }

    /// The Volume `attachment` mounts.
    #[must_use]
    pub fn volume_of(&self, attachment: &VolumeAttachment) -> Option<&SavedVolumeIntent> {
        self.volumes
            .iter()
            .find(|volume| volume.resource_id == attachment.volume_resource_id)
    }

    /// The Config `attachment` mounts.
    #[must_use]
    pub fn config_of(&self, attachment: &ConfigAttachment) -> Option<&SavedConfigIntent> {
        self.configs
            .iter()
            .find(|config| config.resource_id == attachment.config_resource_id)
    }

    /// Drop every attachment of the Volume or Config `resource_id` (a `Service` kind
    /// holds none), returning the lineages of the Services that mounted it.
    pub fn detach_resource(
        &mut self,
        kind: EnvironmentNodeType,
        resource_id: &str,
    ) -> BTreeSet<String> {
        let mut detached = BTreeSet::new();
        for service in &mut self.services {
            let held = service.volume_attachments.len() + service.config_attachments.len();
            match kind {
                EnvironmentNodeType::Volume => service
                    .volume_attachments
                    .retain(|mount| mount.volume_resource_id != resource_id),
                EnvironmentNodeType::Config => service
                    .config_attachments
                    .retain(|mount| mount.config_resource_id != resource_id),
                EnvironmentNodeType::Service => {}
            }
            if service.volume_attachments.len() + service.config_attachments.len() != held {
                detached.insert(service.lineage_id.clone());
            }
        }
        detached
    }
}

/// A Service owner with authored settings, variables, and resource relationships.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedServiceIntent {
    pub id: String,
    pub lineage_id: String,
    pub slug: String,
    pub config: AuthoredServiceConfig,
    pub variables: Vec<SavedVariableIntent>,
    pub volume_attachments: Vec<VolumeAttachment>,
    /// Written only when there are some, like [`SavedEnvironmentIntent::configs`].
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<ConfigAttachment>>")]
    pub config_attachments: Vec<ConfigAttachment>,
}

impl SavedServiceIntent {
    /// The variable of this Service that `id` names, or else the one `key` names.
    #[must_use]
    pub fn variable_like(&self, id: &str, key: &str) -> Option<&SavedVariableIntent> {
        self.variables
            .iter()
            .find(|variable| variable.id == id)
            .or_else(|| self.variables.iter().find(|variable| variable.key == key))
    }

    /// The variable of this Service that `key` names, or else the one `id` names: for
    /// a document whose ids may predate a rewrite that kept its keys.
    #[must_use]
    pub fn variable_like_key_first(&self, id: &str, key: &str) -> Option<&SavedVariableIntent> {
        self.variables
            .iter()
            .find(|variable| variable.key == key)
            .or_else(|| self.variables.iter().find(|variable| variable.id == id))
    }
}

/// A Config owner reference and the Service directory where its files appear.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ConfigAttachment {
    pub config_resource_id: String,
    pub mount_dir: ContainerPath,
}

impl ConfigAttachment {
    /// Whether `dir` names one directory one way: absolute, below `/`, with no
    /// empty, `.` or `..` segment and no trailing `/`, so equal directories compare
    /// equal.
    #[must_use]
    pub fn is_canonical_dir(dir: &str) -> bool {
        dir.strip_prefix('/').is_some_and(|rest| {
            rest.split('/')
                .all(|segment| !matches!(segment, "" | "." | ".."))
        })
    }
}

/// A named folder of text files that Services mount read-only.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedConfigIntent {
    pub resource_id: String,
    pub resource_lineage_id: String,
    pub name: ConfigName,
    pub files: BTreeMap<ConfigFileName, SavedConfigFile>,
}

impl SavedConfigIntent {
    /// A file at a path another file needs as its directory, with that other file:
    /// `a` beside `a/b`. No folder holds both.
    #[must_use]
    pub fn file_in_the_way(&self) -> Option<(&ConfigFileName, &ConfigFileName)> {
        let names: BTreeMap<&str, &ConfigFileName> = self
            .files
            .keys()
            .map(|name| (name.as_str(), name))
            .collect();
        self.files.keys().find_map(|name| {
            name.as_str()
                .match_indices('/')
                .find_map(|(at, _)| names.get(&name.as_str()[..at]))
                .map(|file| (*file, name))
        })
    }
}

/// One file of a Config. Its content is template parts whose references always
/// name a Service: a Config belongs to no Service, so a bare `${{ KEY }}` has no owner.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedConfigFile {
    pub content: Vec<ValuePart>,
    pub mode: FileMode,
    pub uid: u32,
    pub gid: u32,
}

impl SavedConfigFile {
    /// The service lineages this file reads through references.
    pub fn referenced_lineages(&self) -> impl Iterator<Item = &str> {
        self.content.iter().filter_map(|part| match part {
            ValuePart::Ref {
                owner: ValuePartOwner::Service { lineage_id },
                ..
            } => Some(lineage_id.as_str()),
            ValuePart::Ref { .. } | ValuePart::Text { .. } => None,
        })
    }
}

/// A file's permission bits, at most `0777`: no setuid, setgid or sticky bit.
/// Written as octal text such as `"0444"`.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize, TS)]
#[serde(try_from = "String", into = "String")]
#[ts(as = "String")]
pub struct FileMode(u32);

impl FileMode {
    /// What a file gets unless it asks otherwise: readable by everyone, writable by none.
    pub const READ_ONLY: Self = Self(0o444);
    /// `--executable`: readable and runnable by everyone.
    pub const EXECUTABLE: Self = Self(0o555);

    /// Parse octal text such as `0644` or `644`.
    ///
    /// # Errors
    /// Returns ConfigError unless `text` is 1 to 4 octal digits at most `0777`.
    pub fn parse(text: &str) -> Result<Self, ConfigError> {
        let invalid = || ConfigError::at("mode", "Expected an octal file mode from 0000 to 0777");
        if text.is_empty() || text.len() > 4 || !text.bytes().all(|b| (b'0'..=b'7').contains(&b)) {
            return Err(invalid());
        }
        let bits = u32::from_str_radix(text, 8).map_err(|_| invalid())?;
        if bits > 0o777 {
            return Err(invalid());
        }
        Ok(Self(bits))
    }

    /// The mode as a number, at most `0o777`.
    #[must_use]
    pub fn bits(self) -> u32 {
        self.0
    }
}

impl std::fmt::Display for FileMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:04o}", self.0)
    }
}

impl TryFrom<String> for FileMode {
    type Error = ConfigError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(&value)
    }
}

impl From<FileMode> for String {
    fn from(value: FileMode) -> Self {
        value.to_string()
    }
}

/// A Volume owner reference and the Service path where it is mounted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VolumeAttachment {
    pub volume_resource_id: String,
    pub mount_path: String,
}

impl VolumeAttachment {
    /// The mount path with empty and `.` segments dropped and `..` applied, the
    /// form Config mount directories and file paths are compared in.
    #[must_use]
    pub fn container_path(&self) -> String {
        let mut segments = Vec::new();
        for segment in self.mount_path.split('/') {
            match segment {
                "" | "." => {}
                ".." => {
                    segments.pop();
                }
                segment => segments.push(segment),
            }
        }
        format!("/{}", segments.join("/"))
    }
}

/// An authored Volume name attached to stable resource and lineage identities.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedVolumeIntent {
    pub resource_id: String,
    pub resource_lineage_id: String,
    pub name: String,
    pub storage: VolumeKind,
    /// Whether more than one container may write it: replicas of one Service, or
    /// several Services. Off, the Store refuses a second writer. Written only when on,
    /// so documents from before it read as off.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(as = "Option<bool>", optional)]
    pub shared_writes: bool,
    /// The restore point its data comes from, when a restore made it. Written only
    /// when set, so other documents keep their bytes and fingerprints.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub restored_from: Option<RestoredFrom>,
}

/// A restore point of another Volume in the same Environment.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct RestoredFrom {
    /// The source Volume's resource ID.
    pub volume: String,
    /// When the point was taken, in Unix seconds: the source's writes after it are
    /// not in the restored Volume.
    #[ts(type = "number")]
    pub at: u64,
}

/// Storage chosen for a Volume: ordinary Docker storage or bounded, provisioned storage.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum VolumeKind {
    Docker {},
    Provisioned {
        maximum_bytes: crate::ProvisionedVolumeMaximumBytes,
    },
}

impl VolumeKind {
    /// New Cloud Volumes have a five GB storage limit unless Docker storage is explicitly chosen.
    #[must_use]
    pub fn provisioned_default() -> Self {
        Self::Provisioned {
            maximum_bytes: crate::ProvisionedVolumeMaximumBytes::try_from(5_000_000_000)
                .expect("five GB is a positive byte count"),
        }
    }
}

/// An authored variable, including stable identity and comparison fingerprint.
///
/// It reads and writes as [`StoredVariable`], the shape every stored document holds.
#[derive(Clone, Debug, Eq, PartialEq, Deserialize, TS)]
#[serde(try_from = "StoredVariable")]
#[ts(as = "StoredVariable")]
pub struct SavedVariableIntent {
    pub id: String,
    pub key: String,
    pub description: Option<String>,
    pub exported: bool,
    pub value: VariableValue,
}

impl SavedVariableIntent {
    /// How the Store makes this variable's value.
    #[must_use]
    pub const fn generator(&self) -> Option<&Generator> {
        self.value.generator()
    }

    /// Whether this generated variable has no value of its own yet: a copy or Sync
    /// brought its [`Generator`], and the Store makes the value before it saves.
    #[must_use]
    pub fn awaits_value(&self) -> bool {
        self.value.awaits_value()
    }
}

/// What a variable holds, and the fingerprint that compares it without reading it.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum VariableValue {
    /// Text or references, as written.
    Plain {
        text: PlainText,
        fingerprint: ValueFingerprint,
    },
    /// A Generated Value: plain text the Store made from `generator`. A Sync base can
    /// hold one emptied, with no text but its fingerprint.
    Generated {
        generator: Generator,
        value: String,
        fingerprint: ValueFingerprint,
    },
    /// A Generated Value a copy, Sync or merge brought without its value: the Store
    /// makes this Environment's own before it saves, so Working and Saved State never
    /// keep one. Sync bases do, as it arrived.
    Arriving { generator: Generator },
    /// A secret. Its ciphertext is absent in the browser-readable authored document:
    /// Cloud keeps it privately and captures it into each immutable publication.
    Sealed {
        ciphertext: Option<SecretCiphertext>,
        fingerprint: ValueFingerprint,
    },
    /// A secret that arrived without its value: Sync never carries one up or across.
    /// A Deployment leaves it out until a value is set.
    NotSet,
}

/// A plain variable's text: literal, or with references.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PlainText {
    Literal(String),
    Template(Vec<ValuePart>),
}

impl VariableValue {
    /// The value a stored generated literal is: [`Self::Arriving`] when it holds the
    /// fingerprint an arriving one is stored with, else [`Self::Generated`].
    ///
    /// # Errors
    /// [`VariableError`] when `fingerprint` is empty.
    pub(crate) fn generated(
        generator: Generator,
        value: String,
        fingerprint: String,
    ) -> Result<Self, VariableError> {
        if value.is_empty() && fingerprint == AWAITING_FINGERPRINT {
            return Ok(Self::Arriving { generator });
        }
        Ok(Self::Generated {
            generator,
            value,
            fingerprint: ValueFingerprint::parse(fingerprint)?,
        })
    }

    #[must_use]
    pub const fn generator(&self) -> Option<&Generator> {
        match self {
            Self::Generated { generator, .. } | Self::Arriving { generator } => Some(generator),
            Self::Plain { .. } | Self::Sealed { .. } | Self::NotSet => None,
        }
    }

    /// Whether the Store still makes this generated value: one that arrived without
    /// it, or was emptied.
    #[must_use]
    pub fn awaits_value(&self) -> bool {
        match self {
            Self::Arriving { .. } => true,
            Self::Generated { value, .. } => value.is_empty(),
            Self::Plain { .. } | Self::Sealed { .. } | Self::NotSet => false,
        }
    }

    /// The fingerprint that compares this value; none for one not set or still arriving.
    #[must_use]
    pub const fn fingerprint(&self) -> Option<&ValueFingerprint> {
        match self {
            Self::Plain { fingerprint, .. }
            | Self::Generated { fingerprint, .. }
            | Self::Sealed { fingerprint, .. } => Some(fingerprint),
            Self::Arriving { .. } | Self::NotSet => None,
        }
    }

    /// The literal text this value is, a generated one's included. An arriving one is
    /// `""`, the literal it is stored as until made.
    #[must_use]
    pub fn literal(&self) -> Option<&str> {
        match self {
            Self::Plain {
                text: PlainText::Literal(value),
                ..
            }
            | Self::Generated { value, .. } => Some(value),
            Self::Arriving { .. } => Some(""),
            Self::Plain {
                text: PlainText::Template(_),
                ..
            }
            | Self::Sealed { .. }
            | Self::NotSet => None,
        }
    }

    /// Whether this is a secret, with its value or without.
    #[must_use]
    pub const fn is_secret(&self) -> bool {
        matches!(self, Self::Sealed { .. } | Self::NotSet)
    }

    /// The template parts of this value; none unless it is a template.
    #[must_use]
    pub fn parts(&self) -> &[ValuePart] {
        match self {
            Self::Plain {
                text: PlainText::Template(parts),
                ..
            } => parts,
            Self::Plain {
                text: PlainText::Literal(_),
                ..
            }
            | Self::Generated { .. }
            | Self::Arriving { .. }
            | Self::Sealed { .. }
            | Self::NotSet => &[],
        }
    }

    /// The service lineages this value reads through template references.
    pub fn referenced_lineages(&self) -> impl Iterator<Item = &str> {
        self.parts().iter().filter_map(|part| match part {
            ValuePart::Ref {
                owner: ValuePartOwner::Service { lineage_id },
                ..
            } => Some(lineage_id.as_str()),
            ValuePart::Ref { .. } | ValuePart::Text { .. } => None,
        })
    }

    /// Whether `other` holds the same text or secret, whatever its fingerprint or
    /// generator.
    #[must_use]
    pub fn holds_same(&self, other: &Self) -> bool {
        self.stored() == other.stored()
    }

    /// This value as stored, without its fingerprint or generator.
    #[must_use]
    pub fn to_saved_value(&self) -> SavedVariableValue {
        match self.stored() {
            StoredValue::Literal { value } => SavedVariableValue::Literal {
                value: value.to_owned(),
            },
            StoredValue::Template { parts } => SavedVariableValue::Template {
                parts: parts.to_vec(),
            },
            StoredValue::Secret { encrypted_value } => SavedVariableValue::Secret {
                encrypted_value: encrypted_value.cloned(),
            },
            StoredValue::SecretWithoutValue => SavedVariableValue::SecretWithoutValue,
        }
    }

    /// The fingerprint as stored documents hold it, to compare with theirs: empty for
    /// one not set, and the awaiting marker for one still arriving.
    #[must_use]
    pub fn stored_fingerprint(&self) -> &str {
        match self {
            Self::Arriving { .. } => AWAITING_FINGERPRINT,
            Self::Plain { .. } | Self::Generated { .. } | Self::Sealed { .. } | Self::NotSet => {
                self.fingerprint().map_or("", ValueFingerprint::as_str)
            }
        }
    }

    /// This value as stored, borrowed.
    pub(crate) fn stored(&self) -> StoredValue<'_> {
        match self {
            Self::Plain {
                text: PlainText::Template(parts),
                ..
            } => StoredValue::Template { parts },
            Self::Plain { .. } | Self::Generated { .. } | Self::Arriving { .. } => {
                StoredValue::Literal {
                    value: self.literal().unwrap_or_default(),
                }
            }
            Self::Sealed { ciphertext, .. } => StoredValue::Secret {
                encrypted_value: ciphertext.as_ref(),
            },
            Self::NotSet => StoredValue::SecretWithoutValue,
        }
    }
}

impl PlainText {
    /// The text `parts` write: literal unless it reads a reference.
    #[must_use]
    pub fn of(mut parts: Vec<ValuePart>) -> Self {
        match parts.as_mut_slice() {
            [] => Self::Literal(String::new()),
            [ValuePart::Text { value }] => Self::Literal(std::mem::take(value)),
            _ => Self::Template(parts),
        }
    }
}

/// The fingerprint that compares a variable's value without reading it. Never empty:
/// only a secret without a value has none, and it holds no fingerprint.
#[derive(Clone, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ValueFingerprint(String);

impl ValueFingerprint {
    /// # Errors
    /// [`VariableError`] when `fingerprint` is empty.
    pub fn parse(fingerprint: impl Into<String>) -> Result<Self, VariableError> {
        let fingerprint = fingerprint.into();
        if fingerprint.is_empty() {
            return Err(VariableError::Fingerprint);
        }
        Ok(Self(fingerprint))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for ValueFingerprint {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.0)
    }
}

/// Why a stored variable is not one.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum VariableError {
    #[error("only a secret without a value has no fingerprint")]
    Fingerprint,
}

/// A [`SavedVariableIntent`] as every stored document holds it: its fingerprint, value
/// and generator side by side.
#[derive(Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(rename = "SavedVariableIntent")]
pub(crate) struct StoredVariable {
    id: String,
    key: String,
    description: Option<String>,
    exported: bool,
    /// Empty exactly for a secret without a value.
    value_fingerprint: String,
    value: SavedVariableValue,
    /// How the Store makes this variable's value, which each Environment holds its own
    /// of. It counts only on a literal: it reads and is dropped beside any other value,
    /// so a document that sealed the value and kept the marker still reads. Written only
    /// when set, so other documents keep their bytes and fingerprints.
    #[serde(default)]
    #[ts(optional)]
    generated: Option<Generator>,
}

impl TryFrom<StoredVariable> for SavedVariableIntent {
    type Error = VariableError;

    fn try_from(stored: StoredVariable) -> Result<Self, Self::Error> {
        let fingerprint = stored.value_fingerprint;
        let value = match (stored.value, stored.generated) {
            (SavedVariableValue::Literal { value }, Some(generator)) => {
                VariableValue::generated(generator, value, fingerprint)?
            }
            (SavedVariableValue::Literal { value }, None) => VariableValue::Plain {
                text: PlainText::Literal(value),
                fingerprint: ValueFingerprint::parse(fingerprint)?,
            },
            (SavedVariableValue::Template { parts }, _) => VariableValue::Plain {
                text: PlainText::Template(parts),
                fingerprint: ValueFingerprint::parse(fingerprint)?,
            },
            (SavedVariableValue::Secret { encrypted_value }, _) => VariableValue::Sealed {
                ciphertext: encrypted_value,
                fingerprint: ValueFingerprint::parse(fingerprint)?,
            },
            (SavedVariableValue::SecretWithoutValue, _) if fingerprint.is_empty() => {
                VariableValue::NotSet
            }
            (SavedVariableValue::SecretWithoutValue, _) => return Err(VariableError::Fingerprint),
        };
        Ok(Self {
            id: stored.id,
            key: stored.key,
            description: stored.description,
            exported: stored.exported,
            value,
        })
    }
}

impl Serialize for SavedVariableIntent {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        StoredVariableRef {
            id: &self.id,
            key: &self.key,
            description: self.description.as_deref(),
            exported: self.exported,
            value_fingerprint: self.value.stored_fingerprint(),
            value: self.value.stored(),
            generated: self.value.generator(),
        }
        .serialize(serializer)
    }
}

/// [`StoredVariable`], borrowed to write.
#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct StoredVariableRef<'a> {
    id: &'a str,
    key: &'a str,
    description: Option<&'a str>,
    exported: bool,
    value_fingerprint: &'a str,
    value: StoredValue<'a>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generated: Option<&'a Generator>,
}

/// [`SavedVariableValue`], borrowed to write.
#[derive(PartialEq, Serialize)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub(crate) enum StoredValue<'a> {
    Literal {
        value: &'a str,
    },
    Template {
        parts: &'a [ValuePart],
    },
    Secret {
        encrypted_value: Option<&'a SecretCiphertext>,
    },
    SecretWithoutValue,
}

/// A variable's value as stored: literal text, structured references, or a privately
/// stored sealed value.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    tag = "kind",
    rename_all = "snake_case",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum SavedVariableValue {
    Literal {
        value: String,
    },
    Template {
        parts: Vec<ValuePart>,
    },
    Secret {
        /// Absent in the browser-readable authored document. Cloud keeps the
        /// ciphertext privately and captures it into each immutable publication.
        encrypted_value: Option<SecretCiphertext>,
    },
    /// A secret that arrived without its value: Sync never carries one up or across.
    /// It has no fingerprint, and a Deployment leaves it out until a value is set.
    SecretWithoutValue,
}

/// Decode and normalize an authored document while validating owner identities and relationships.
///
/// # Errors
/// Returns ConfigError for unsupported versions, duplicate or malformed identities, derived artifacts,
/// invalid settings, or attachments without an authored target.
pub fn parse_environment_intent(value: Value) -> Result<SavedEnvironmentIntent, ConfigError> {
    if value
        .get("services")
        .and_then(Value::as_array)
        .is_some_and(|services| {
            services.iter().any(|service| {
                service.get("config").is_some_and(|config| {
                    config.get("env").is_some() || config.get("mounts").is_some()
                })
            })
        })
    {
        return Err(ConfigError::at(
            "services.config",
            "Derived environment and attachments are not authored settings",
        ));
    }
    let mut intent: SavedEnvironmentIntent = serde_json::from_value(value)
        .map_err(|_| ConfigError::at("environment", "Invalid authored environment"))?;
    if intent.version != 1 || intent.environment_slug.is_empty() {
        return Err(ConfigError::at(
            "environment",
            "Invalid environment version or slug",
        ));
    }
    unique(
        intent.services.iter().map(|v| v.id.as_str()),
        "services.id",
        true,
    )?;
    unique(
        intent.services.iter().map(|v| v.lineage_id.as_str()),
        "services.lineageId",
        true,
    )?;
    unique(
        intent.services.iter().map(|v| v.slug.as_str()),
        "services.slug",
        false,
    )?;
    unique(
        intent
            .volumes
            .iter()
            .map(|v| v.resource_id.as_str())
            .chain(intent.configs.iter().map(|c| c.resource_id.as_str())),
        "resources.id",
        true,
    )?;
    unique(
        intent
            .volumes
            .iter()
            .map(|v| v.resource_lineage_id.as_str())
            .chain(
                intent
                    .configs
                    .iter()
                    .map(|c| c.resource_lineage_id.as_str()),
            ),
        "resources.lineageId",
        true,
    )?;
    unique(
        intent.configs.iter().map(|c| c.name.as_str()),
        "configs.name",
        false,
    )?;
    if intent.configs.iter().any(|c| c.file_in_the_way().is_some()) {
        return Err(ConfigError::at(
            "configs.files",
            "A Config file sits where another file needs a directory",
        ));
    }
    if intent
        .configs
        .iter()
        .flat_map(|c| c.files.values())
        .flat_map(|file| &file.content)
        .any(|part| {
            matches!(
                part,
                ValuePart::Ref {
                    owner: ValuePartOwner::Self_,
                    ..
                }
            )
        })
    {
        return Err(ConfigError::at(
            "configs.files.content",
            "A Config reference must name its Service",
        ));
    }
    unique(
        intent
            .services
            .iter()
            .flat_map(|v| &v.variables)
            .map(|v| v.id.as_str()),
        "variables.id",
        true,
    )?;
    for service in &mut intent.services {
        service.config = parse_service_config(
            serde_json::to_value(&service.config).expect("settings are JSON"),
        )?
        .settings;
        validate_variables(&service.variables)?;
        unique(
            service
                .volume_attachments
                .iter()
                .map(|a| a.volume_resource_id.as_str()),
            "volumeAttachments",
            true,
        )?;
        unique(
            service
                .volume_attachments
                .iter()
                .map(|a| a.mount_path.as_str()),
            "volumeAttachments.mountPath",
            false,
        )?;
        unique(
            service
                .config_attachments
                .iter()
                .map(|a| a.config_resource_id.as_str()),
            "configAttachments",
            true,
        )?;
        if service
            .config_attachments
            .iter()
            .any(|a| !ConfigAttachment::is_canonical_dir(a.mount_dir.as_str()))
        {
            return Err(ConfigError::at(
                "configAttachments.mountDir",
                "Expected an absolute directory like /etc/app, without . or .. segments or a trailing /",
            ));
        }
        unique(
            service
                .config_attachments
                .iter()
                .map(|a| a.mount_dir.as_str()),
            "configAttachments.mountDir",
            false,
        )?;
        if service.config_attachments.iter().any(|config| {
            service
                .volume_attachments
                .iter()
                .any(|volume| volume.container_path() == config.mount_dir.as_str())
        }) {
            return Err(ConfigError::at(
                "mountPath",
                "A Volume and a Config share one directory",
            ));
        }
        mounted_files(service, &intent.configs)?;
        if service.volume_attachments.iter().any(|a| {
            !intent
                .volumes
                .iter()
                .any(|v| v.resource_id == a.volume_resource_id)
        }) {
            return Err(ConfigError::at(
                "attachments",
                "Attachment must reference an authored resource",
            ));
        }
    }
    for volume in &intent.volumes {
        nonempty(&volume.name, "volumes.name")?;
    }
    Ok(intent)
}

fn mounted_files(
    service: &SavedServiceIntent,
    configs: &[SavedConfigIntent],
) -> Result<(), ConfigError> {
    let mut paths = BTreeSet::new();
    for attachment in &service.config_attachments {
        let config = configs
            .iter()
            .find(|c| c.resource_id == attachment.config_resource_id)
            .ok_or_else(|| {
                ConfigError::at(
                    "attachments",
                    "Attachment must reference an authored resource",
                )
            })?;
        let dir = attachment.mount_dir.as_str();
        for name in config.files.keys() {
            if !paths.insert(format!("{dir}/{name}")) {
                return Err(ConfigError::at(
                    "configAttachments.mountDir",
                    "Two mounted Config files share one path",
                ));
            }
        }
    }
    let collides = paths.iter().any(|path| {
        path.match_indices('/')
            .any(|(at, _)| paths.contains(&path[..at]))
    });
    if collides {
        return Err(ConfigError::at(
            "configAttachments.mountDir",
            "A mounted Config file sits where another needs a directory",
        ));
    }
    let volume_on_file = service.volume_attachments.iter().any(|volume| {
        let at = volume.container_path();
        let at = at.as_str();
        paths.contains(at)
            || at
                .match_indices('/')
                .any(|(end, _)| paths.contains(&at[..end]))
    });
    if volume_on_file {
        return Err(ConfigError::at(
            "mountPath",
            "A Volume mounts where a Config file sits",
        ));
    }
    Ok(())
}

fn unique<'a>(
    values: impl Iterator<Item = &'a str>,
    path: &str,
    uuid: bool,
) -> Result<(), ConfigError> {
    let mut seen = BTreeSet::new();
    for value in values {
        if value.is_empty()
            || (uuid && uuid::Uuid::parse_str(value).is_err())
            || !seen.insert(value)
        {
            return Err(ConfigError::at(path, "Expected valid unique identities"));
        }
    }
    Ok(())
}

fn nonempty(value: &str, path: &str) -> Result<(), ConfigError> {
    if value.is_empty() {
        Err(ConfigError::at(path, "Expected a non-empty value"))
    } else {
        Ok(())
    }
}

fn validate_variables(variables: &[SavedVariableIntent]) -> Result<(), ConfigError> {
    unique(
        variables.iter().map(|v| v.key.as_str()),
        "variables.key",
        false,
    )?;
    for variable in variables {
        if let VariableValue::Sealed {
            ciphertext: Some(SecretCiphertext::Legacy(value)),
            ..
        } = &variable.value
            && value.version != 1
        {
            return Err(ConfigError::at(
                "variables.value",
                "Invalid encrypted secret",
            ));
        }
    }
    Ok(())
}

/// Remove private ciphertext while retaining public credential and variable comparison evidence.
#[must_use]
pub fn redact_environment_intent(mut intent: SavedEnvironmentIntent) -> SavedEnvironmentIntent {
    for variable in intent
        .services
        .iter_mut()
        .flat_map(|service| &mut service.variables)
    {
        if let VariableValue::Sealed { ciphertext, .. } = &mut variable.value {
            *ciphertext = None;
        }
    }
    intent
}

/// Order authored owners and relationships deterministically without changing attachment precedence.
#[must_use]
pub fn canonicalize_environment_intent(
    mut intent: SavedEnvironmentIntent,
) -> SavedEnvironmentIntent {
    intent.services.sort_by(|a, b| a.id.cmp(&b.id));
    intent
        .volumes
        .sort_by(|a, b| a.resource_id.cmp(&b.resource_id));
    intent
        .configs
        .sort_by(|a, b| a.resource_id.cmp(&b.resource_id));
    for service in &mut intent.services {
        service.config_attachments.sort_by(|a, b| {
            (&a.mount_dir, &a.config_resource_id).cmp(&(&b.mount_dir, &b.config_resource_id))
        });
        service.variables.sort_by(|a, b| a.id.cmp(&b.id));
        service.volume_attachments.sort_by(|a, b| {
            (&a.mount_path, &a.volume_resource_id).cmp(&(&b.mount_path, &b.volume_resource_id))
        });
    }
    intent
}
