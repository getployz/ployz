//! Compile authored owners and attachments into versioned node snapshots and producers.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use ts_rs::TS;

use super::*;

/// Node snapshots and variable producers compiled from one authored document.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CompiledEnvironmentIntent {
    pub node_snapshots: Vec<CompiledEnvironmentNode>,
    pub variable_producers: Vec<SavedVariableProducer>,
}

/// An owner identity with a typed snapshot and optional private registry material.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct CompiledEnvironmentNode {
    pub environment_id: String,
    pub node_id: String,
    pub node_lineage_id: String,
    /// The configuration owns its wire discriminator and snapshot version.
    #[serde(flatten)]
    #[ts(flatten)]
    pub snapshot: CompiledNodeSnapshot,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub encrypted_registry_username: Option<EncryptedSecretValue>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub encrypted_registry_secret: Option<EncryptedSecretValue>,
}

/// A compiled configuration whose wire identity cannot disagree with its payload.
#[derive(Clone, Debug, PartialEq, Deserialize)]
#[serde(try_from = "CompiledNodeSnapshotWire")]
pub struct CompiledNodeSnapshot(pub CompiledNodeConfig);

#[derive(Deserialize, TS)]
#[serde(rename_all = "camelCase")]
struct CompiledNodeSnapshotWire {
    node_type: EnvironmentNodeType,
    config_version: u8,
    config: CompiledNodeConfig,
}

impl Serialize for CompiledNodeSnapshot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        use serde::ser::SerializeStruct as _;
        let mut wire = serializer.serialize_struct("CompiledNodeSnapshot", 3)?;
        wire.serialize_field("nodeType", &self.0.node_type())?;
        wire.serialize_field("configVersion", &self.0.snapshot_version())?;
        wire.serialize_field("config", &self.0)?;
        wire.end()
    }
}

impl TryFrom<CompiledNodeSnapshotWire> for CompiledNodeSnapshot {
    type Error = ConfigError;

    fn try_from(wire: CompiledNodeSnapshotWire) -> Result<Self, Self::Error> {
        if wire.node_type != wire.config.node_type()
            || wire.config_version != wire.config.snapshot_version()
        {
            return Err(ConfigError::at(
                "snapshot",
                "Configuration does not match its node type or snapshot version",
            ));
        }
        Ok(Self(wire.config))
    }
}

/// The configuration payload associated with each Environment Node family.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(untagged)]
pub enum CompiledNodeConfig {
    Service(Box<ServiceConfig>),
    Volume(VolumeConfig),
}

impl CompiledNodeConfig {
    /// The Environment Node family represented by this configuration.
    #[must_use]
    pub const fn node_type(&self) -> EnvironmentNodeType {
        match self {
            Self::Service(_) => EnvironmentNodeType::Service,
            Self::Volume(_) => EnvironmentNodeType::Volume,
        }
    }

    /// Snapshot envelope version; Service settings have their own version inside it.
    #[must_use]
    pub const fn snapshot_version(&self) -> u8 {
        match self {
            Self::Service(_) => 1,
            Self::Volume(_) => 2,
        }
    }
}

/// The name-bearing configuration of a Volume node snapshot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
pub struct VolumeConfig {
    #[ts(type = "2")]
    pub version: u8,
    pub name: String,
}

/// A variable value associated with its stable producer owner and lineage.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "camelCase")]
pub struct SavedVariableProducer {
    #[ts(type = "'service'")]
    pub owner_scope: String,
    pub owner_id: String,
    pub owner_lineage_id: String,
    pub key: String,
    pub value: SavedVariableValue,
}

/// Render structured references with current owner slugs while escaping literal template syntax.
#[must_use]
pub fn render_variable_parts(parts: &[ValuePart], slugs: &BTreeMap<String, String>) -> String {
    parts
        .iter()
        .map(|part| match part {
            ValuePart::Text { value } => value.replace("${{", "$${{"),
            ValuePart::Ref {
                owner: ValuePartOwner::Self_,
                key,
            } => format!("${{{{ {key} }}}}"),
            ValuePart::Ref {
                owner: ValuePartOwner::Service { lineage_id },
                key,
            } => {
                let slug = slugs
                    .get(lineage_id)
                    .map(String::as_str)
                    .unwrap_or("<deleted>");
                format!("${{{{ {slug}.{key} }}}}")
            }
        })
        .collect()
}

fn env_value(variable: &SavedVariableIntent, slugs: &BTreeMap<String, String>) -> ServiceEnvValue {
    match &variable.value {
        SavedVariableValue::Literal { value } => ServiceEnvValue::Literal {
            value: value.clone(),
            parts: None,
        },
        SavedVariableValue::Secret { encrypted_value } => ServiceEnvValue::Secret {
            variable_id: Some(variable.id.clone()),
            encrypted_value: encrypted_value.clone(),
            fingerprint: variable.value_fingerprint.clone(),
            interpolated: None,
        },
        SavedVariableValue::Template { parts } => ServiceEnvValue::Literal {
            value: render_variable_parts(parts, slugs),
            parts: Some(parts.clone()),
        },
    }
}

/// Compile a validated authored document into node snapshots and variable producers.
///
#[must_use]
pub fn compile_environment_intent(
    environment_id: &str,
    intent: SavedEnvironmentIntent,
) -> CompiledEnvironmentIntent {
    let intent = canonicalize_environment_intent(intent);
    let slugs = intent
        .services
        .iter()
        .map(|s| (s.lineage_id.clone(), s.slug.clone()))
        .collect();
    let mut node_snapshots = Vec::new();
    let mut variable_producers = Vec::new();
    for service in &intent.services {
        let mut config = ServiceConfig::from(service.configuration.settings().clone());
        config.env = service
            .variables
            .iter()
            .map(|v| (v.key.clone(), env_value(v, &slugs)))
            .collect();
        config.mounts = service
            .volume_attachments
            .iter()
            .map(|a| {
                let volume = intent
                    .volumes
                    .iter()
                    .find(|v| v.resource_id == a.volume_resource_id)
                    .expect("validated attachment");
                ServiceDeployMount {
                    volume_resource_id: volume.resource_id.clone(),
                    volume_name: volume.name.clone(),
                    mount_path: a.mount_path.clone(),
                }
            })
            .collect();
        node_snapshots.push(CompiledEnvironmentNode {
            environment_id: environment_id.into(),
            node_id: service.id.clone(),
            node_lineage_id: service.lineage_id.clone(),
            snapshot: CompiledNodeSnapshot(CompiledNodeConfig::Service(Box::new(config))),
            encrypted_registry_username: service
                .configuration
                .encrypted_registry_username()
                .cloned(),
            encrypted_registry_secret: service.configuration.encrypted_registry_secret().cloned(),
        });
        for (key, value) in [
            (
                "PLOYZ_PRIVATE_DOMAIN",
                format!("{}-{}.internal", service.slug, intent.environment_slug),
            ),
            ("PORT", "3000".into()),
            ("PLOYZ_ENVIRONMENT_NAME", intent.environment_slug.clone()),
            (
                "PLOYZ_SERVICE_NAME",
                service.configuration.settings().name.clone(),
            ),
            ("PLOYZ_ENVIRONMENT_ID", environment_id.into()),
            ("PLOYZ_SERVICE_ID", service.id.clone()),
        ] {
            variable_producers.push(SavedVariableProducer {
                owner_scope: "service".into(),
                owner_id: service.id.clone(),
                owner_lineage_id: service.lineage_id.clone(),
                key: key.into(),
                value: SavedVariableValue::Literal { value },
            });
        }
        variable_producers.extend(service.variables.iter().map(|v| SavedVariableProducer {
            owner_scope: "service".into(),
            owner_id: service.id.clone(),
            owner_lineage_id: service.lineage_id.clone(),
            key: v.key.clone(),
            value: v.value.clone(),
        }));
    }
    node_snapshots.extend(intent.volumes.iter().map(|v| CompiledEnvironmentNode {
        environment_id: environment_id.into(),
        node_id: v.resource_id.clone(),
        node_lineage_id: v.resource_lineage_id.clone(),
        snapshot: CompiledNodeSnapshot(CompiledNodeConfig::Volume(VolumeConfig {
            version: 2,
            name: v.name.clone(),
        })),
        encrypted_registry_username: None,
        encrypted_registry_secret: None,
    }));
    variable_producers.sort_by(|a, b| {
        (&a.owner_scope, &a.owner_lineage_id, &a.key).cmp(&(
            &b.owner_scope,
            &b.owner_lineage_id,
            &b.key,
        ))
    });
    CompiledEnvironmentIntent {
        node_snapshots,
        variable_producers,
    }
}

// ts-rs's `as` derive does not forward flattening; the serialized wire owns that shape.
impl TS for CompiledNodeSnapshot {
    type WithoutGenerics = Self;
    type OptionInnerType = Self;

    fn name(config: &ts_rs::Config) -> String {
        CompiledNodeSnapshotWire::name(config)
    }

    fn inline(config: &ts_rs::Config) -> String {
        CompiledNodeSnapshotWire::inline(config)
    }

    fn inline_flattened(config: &ts_rs::Config) -> String {
        CompiledNodeSnapshotWire::inline_flattened(config)
    }

    fn visit_dependencies(visitor: &mut impl ts_rs::TypeVisitor) {
        CompiledNodeSnapshotWire::visit_dependencies(visitor);
    }
}
