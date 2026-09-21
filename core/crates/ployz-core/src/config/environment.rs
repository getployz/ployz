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
    pub volumes: Vec<SavedVolumeIntent>,
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
        intent.volumes.iter().map(|v| v.resource_id.as_str()),
        "resources.id",
        true,
    )?;
    unique(
        intent
            .volumes
            .iter()
            .map(|v| v.resource_lineage_id.as_str()),
        "resources.lineageId",
        true,
    )?;
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
    for service in &mut intent.services {
        service.variables.sort_by(|a, b| a.id.cmp(&b.id));
        service.volume_attachments.sort_by(|a, b| {
            (&a.mount_path, &a.volume_resource_id).cmp(&(&b.mount_path, &b.volume_resource_id))
        });
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
