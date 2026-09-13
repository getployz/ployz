//! Authored Environment documents, identity admission, and private-value redaction.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use ts_rs::TS;

use super::{
    AuthoredServiceConfig, ConfigError, EncryptedSecretValue, ValuePart, parse_service_config,
};

/// Complete authored Environment state; compiled artifacts are not accepted here.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedEnvironmentIntent {
    #[ts(type = "1")]
    pub version: u8,
    pub environment_slug: String,
    pub services: Vec<SavedServiceIntent>,
    pub variable_groups: Vec<SavedVariableGroupIntent>,
    pub volumes: Vec<SavedVolumeIntent>,
}

/// A Service owner with authored settings, variables, and resource relationships.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedServiceIntent {
    pub id: String,
    pub lineage_id: String,
    pub slug: String,
    /// Settings and private credential material change through one owned interface.
    #[serde(flatten)]
    #[ts(flatten)]
    pub configuration: SavedServiceConfiguration,
    pub variables: Vec<SavedVariableIntent>,
    pub variable_group_attachments: Vec<VariableGroupAttachment>,
    pub volume_attachments: Vec<VolumeAttachment>,
}

/// Authored settings and their private registry material, owned as one restore state.
/// The settings getter is immutable; restore replaces the reference and its value together.
///
/// ```compile_fail
/// use ployz_core::config::{SavedServiceConfiguration, ServiceSource};
/// fn replace_reference_only(configuration: &mut SavedServiceConfiguration, source: ServiceSource) {
///     configuration.settings().source = source;
/// }
/// ```
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "SavedServiceConfigurationWire")]
pub struct SavedServiceConfiguration {
    settings: AuthoredServiceConfig,
    credentials: Option<RegistryCredentialPayload>,
}

#[derive(Clone, Debug, PartialEq)]
struct RegistryCredentialPayload {
    username: Option<EncryptedSecretValue>,
    secret: EncryptedSecretValue,
}

#[derive(Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SavedServiceConfigurationWire {
    config: AuthoredServiceConfig,
    encrypted_registry_username: Option<EncryptedSecretValue>,
    encrypted_registry_secret: Option<EncryptedSecretValue>,
}

impl Serialize for SavedServiceConfiguration {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;
        let mut wire = serializer.serialize_struct("SavedServiceConfiguration", 3)?;
        wire.serialize_field("config", &self.settings)?;
        wire.serialize_field(
            "encryptedRegistryUsername",
            &self.encrypted_registry_username(),
        )?;
        wire.serialize_field("encryptedRegistrySecret", &self.encrypted_registry_secret())?;
        wire.end()
    }
}

impl TryFrom<SavedServiceConfigurationWire> for SavedServiceConfiguration {
    type Error = ConfigError;

    fn try_from(wire: SavedServiceConfigurationWire) -> Result<Self, Self::Error> {
        let invalid = || ConfigError::at("credentials", "Invalid private registry material");
        if wire
            .encrypted_registry_username
            .as_ref()
            .is_some_and(|value| value.version != 1)
            || wire
                .encrypted_registry_secret
                .as_ref()
                .is_some_and(|value| value.version != 1)
        {
            return Err(invalid());
        }
        let credentials = match (
            wire.encrypted_registry_username,
            wire.encrypted_registry_secret,
        ) {
            (username, Some(secret)) => Some(RegistryCredentialPayload { username, secret }),
            (None, None) => None,
            (Some(_), None) => return Err(invalid()),
        };
        Ok(Self {
            settings: wire.config,
            credentials,
        })
    }
}

impl SavedServiceConfiguration {
    /// View authored settings without exposing an independent credential-reference mutation.
    #[must_use]
    pub fn settings(&self) -> &AuthoredServiceConfig {
        &self.settings
    }

    /// Restore an authored setting; credential references and their private values move together.
    ///
    /// # Errors
    /// Returns ConfigError when the path has no restorable change or would yield invalid settings.
    /// Failure leaves the complete current configuration unchanged.
    pub fn restore_setting(&mut self, baseline: &Self, path: &str) -> Result<(), ConfigError> {
        let current = super::ServiceConfig::from(self.settings.clone());
        let prior = super::ServiceConfig::from(baseline.settings.clone());
        if !super::compare_service_settings(&current, Some(&prior))
            .iter()
            .any(|row| row.path == path && row.can_restore)
        {
            return Err(ConfigError::at(
                "path",
                "Requested setting is not a restorable change",
            ));
        }
        let restore_credentials = path == "source"
            || path == "source.credentials"
            || (path.starts_with("source.")
                && std::mem::discriminant(&self.settings.source)
                    != std::mem::discriminant(&baseline.settings.source));
        let settings = super::restore_service_setting(current, &prior, path)?.settings;
        let credentials = if restore_credentials {
            &baseline.credentials
        } else {
            &self.credentials
        };
        *self = Self {
            settings,
            credentials: credentials.clone(),
        };
        Ok(())
    }

    fn normalize(&mut self) -> Result<(), ConfigError> {
        self.settings =
            parse_service_config(serde_json::to_value(&self.settings).expect("settings are JSON"))?
                .settings;
        Ok(())
    }

    fn redact(&mut self) {
        self.credentials = None;
    }

    pub(super) fn encrypted_registry_username(&self) -> Option<&EncryptedSecretValue> {
        self.credentials
            .as_ref()
            .and_then(|value| value.username.as_ref())
    }

    pub(super) fn encrypted_registry_secret(&self) -> Option<&EncryptedSecretValue> {
        self.credentials.as_ref().map(|value| &value.secret)
    }
}

/// A Variable Group reference and its precedence within a Service.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VariableGroupAttachment {
    pub variable_group_id: String,
    pub sort_order: i64,
}

/// A Volume owner reference and the Service path where it is mounted.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct VolumeAttachment {
    pub volume_resource_id: String,
    pub mount_path: String,
}

/// A Variable Group owner with stable resource and producer identities.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedVariableGroupIntent {
    pub resource_id: String,
    pub resource_lineage_id: String,
    pub variable_group_id: String,
    pub variable_group_lineage_id: String,
    pub slug: String,
    pub name: String,
    pub variables: Vec<SavedVariableIntent>,
}

/// An authored Volume name attached to stable resource and lineage identities.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedVolumeIntent {
    pub resource_id: String,
    pub resource_lineage_id: String,
    pub name: String,
}

/// An authored variable, including stable identity and comparison fingerprint.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SavedVariableIntent {
    pub id: String,
    pub key: String,
    pub description: Option<String>,
    pub exported: bool,
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
                    config.get("env").is_some()
                        || config.get("mounts").is_some()
                        || config.get("variableGroupAttachments").is_some()
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
            .variable_groups
            .iter()
            .map(|v| v.variable_group_id.as_str()),
        "variableGroups.variableGroupId",
        true,
    )?;
    unique(
        intent
            .variable_groups
            .iter()
            .map(|v| v.variable_group_lineage_id.as_str()),
        "variableGroups.variableGroupLineageId",
        true,
    )?;
    unique(
        intent.variable_groups.iter().map(|v| v.slug.as_str()),
        "variableGroups.slug",
        false,
    )?;
    unique(
        intent
            .variable_groups
            .iter()
            .map(|v| v.resource_id.as_str())
            .chain(intent.volumes.iter().map(|v| v.resource_id.as_str())),
        "resources.id",
        true,
    )?;
    unique(
        intent
            .variable_groups
            .iter()
            .map(|v| v.resource_lineage_id.as_str())
            .chain(
                intent
                    .volumes
                    .iter()
                    .map(|v| v.resource_lineage_id.as_str()),
            ),
        "resources.lineageId",
        true,
    )?;
    unique(
        intent
            .services
            .iter()
            .flat_map(|v| &v.variables)
            .chain(intent.variable_groups.iter().flat_map(|v| &v.variables))
            .map(|v| v.id.as_str()),
        "variables.id",
        true,
    )?;
    for service in &mut intent.services {
        service.configuration.normalize()?;
        validate_variables(&service.variables)?;
        unique(
            service
                .variable_group_attachments
                .iter()
                .map(|a| a.variable_group_id.as_str()),
            "variableGroupAttachments",
            true,
        )?;
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
        if service.variable_group_attachments.iter().any(|a| {
            !intent
                .variable_groups
                .iter()
                .any(|g| g.variable_group_id == a.variable_group_id)
        }) || service.volume_attachments.iter().any(|a| {
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
    for group in &intent.variable_groups {
        nonempty(&group.name, "variableGroups.name")?;
        validate_variables(&group.variables)?;
    }
    for volume in &intent.volumes {
        nonempty(&volume.name, "volumes.name")?;
    }
    Ok(intent)
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
        nonempty(&variable.value_fingerprint, "variables.valueFingerprint")?;
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
    for service in &mut intent.services {
        service.configuration.redact();
    }
    for variable in intent
        .services
        .iter_mut()
        .flat_map(|service| &mut service.variables)
        .chain(
            intent
                .variable_groups
                .iter_mut()
                .flat_map(|group| &mut group.variables),
        )
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
        .variable_groups
        .sort_by(|a, b| a.resource_id.cmp(&b.resource_id));
    intent
        .volumes
        .sort_by(|a, b| a.resource_id.cmp(&b.resource_id));
    for service in &mut intent.services {
        service.variables.sort_by(|a, b| a.id.cmp(&b.id));
        service.variable_group_attachments.sort_by(|a, b| {
            (a.sort_order, &a.variable_group_id).cmp(&(b.sort_order, &b.variable_group_id))
        });
        service.volume_attachments.sort_by(|a, b| {
            (&a.mount_path, &a.volume_resource_id).cmp(&(&b.mount_path, &b.volume_resource_id))
        });
    }
    for group in &mut intent.variable_groups {
        group.variables.sort_by(|a, b| a.id.cmp(&b.id));
    }
    intent
}

/// Decode one authored variable and validate its identity and value evidence.
///
/// # Errors
/// Returns ConfigError for malformed identity, an empty key or fingerprint, or unsupported sealed data.
pub fn parse_saved_variable(value: Value) -> Result<SavedVariableIntent, ConfigError> {
    let variable: SavedVariableIntent = serde_json::from_value(value)
        .map_err(|_| ConfigError::at("variable", "Invalid authored variable"))?;
    unique(std::iter::once(variable.id.as_str()), "variable.id", true)?;
    validate_variables(std::slice::from_ref(&variable))?;
    Ok(variable)
}

// ts-rs's `as` derive does not forward flattening; the serialized wire owns that shape.
impl TS for SavedServiceConfiguration {
    type WithoutGenerics = Self;
    type OptionInnerType = Self;

    fn name(config: &ts_rs::Config) -> String {
        SavedServiceConfigurationWire::name(config)
    }

    fn inline(config: &ts_rs::Config) -> String {
        SavedServiceConfigurationWire::inline(config)
    }

    fn inline_flattened(config: &ts_rs::Config) -> String {
        SavedServiceConfigurationWire::inline_flattened(config)
    }

    fn visit_dependencies(visitor: &mut impl ts_rs::TypeVisitor) {
        SavedServiceConfigurationWire::visit_dependencies(visitor);
    }
}
