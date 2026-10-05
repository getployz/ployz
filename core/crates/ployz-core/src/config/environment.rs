//! Authored Environment documents, identity admission, and private-value redaction.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use crate::{ConfigFileName, ConfigName, ContainerPath};

use super::{
    AuthoredServiceConfig, ConfigError, EncryptedSecretValue, ValuePart, ValuePartOwner,
    parse_service_config,
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

    /// The permission bits.
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
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedVariableIntent {
    pub id: String,
    pub key: String,
    pub description: Option<String>,
    pub exported: bool,
    /// Empty exactly for a [`SavedVariableValue::SecretWithoutValue`], checked by
    /// `validate_variables`. It stays beside `value`, not in each valued variant:
    /// moving it would reshape every stored Working State and Applied State.
    pub value_fingerprint: String,
    pub value: SavedVariableValue,
}

/// Literal text, structured references, or a privately stored sealed value.
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
        encrypted_value: Option<EncryptedSecretValue>,
    },
    /// A secret that arrived without its value: Sync never carries one up or across.
    /// It has no fingerprint, and Deploy refuses until a value is set.
    SecretWithoutValue,
}

impl SavedVariableValue {
    /// The template parts of this value; none unless it is a template.
    #[must_use]
    pub fn parts(&self) -> &[ValuePart] {
        match self {
            Self::Template { parts } => parts,
            Self::Literal { .. } | Self::Secret { .. } | Self::SecretWithoutValue => &[],
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
            service.volume_attachments.iter().any(|volume| {
                volume.mount_path.as_str().trim_end_matches('/') == config.mount_dir.as_str()
            })
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

/// Refuse a Service whose Config Mounts would put two files at one path, or a
/// file where another needs a directory.
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
        let at = volume.mount_path.trim_end_matches('/');
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
        // Only a secret without a value has no fingerprint, and it has none.
        if (variable.value == SavedVariableValue::SecretWithoutValue)
            != variable.value_fingerprint.is_empty()
        {
            return Err(ConfigError::at(
                "variables.valueFingerprint",
                "Only a secret without a value has no fingerprint",
            ));
        }
        if let SavedVariableValue::Secret { encrypted_value } = &variable.value
            && encrypted_value
                .as_ref()
                .is_some_and(|value| value.version != 1)
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
        if let SavedVariableValue::Secret { encrypted_value } = &mut variable.value {
            *encrypted_value = None;
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
