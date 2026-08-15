use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::{Map, Value};

use super::spec::{HealthcheckSpec, ResolvedServiceSpec};
use crate::{ContainerAddress, ContainerId, MachineId, ServiceId, ServiceName};

crate::value::open_string_enum!(HealthObservation, Unrecognized {
    NotConfigured => "not_configured",
    Starting => "starting",
    Healthy => "healthy",
    Unhealthy => "unhealthy",
});

/// Docker state as observed, including the untouched value of a future state.
#[derive(Clone, Debug, PartialEq)]
pub enum ContainerRuntimeObservation {
    Created,
    Running { health: HealthObservation },
    Paused,
    Restarting,
    Exited { code: i64 },
    Removing,
    Dead,
    Unknown { raw: Value },
}

impl Serialize for ContainerRuntimeObservation {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        let mut object = Map::new();
        match self {
            Self::Created => insert_state(&mut object, "created"),
            Self::Running { health } => {
                insert_state(&mut object, "running");
                object.insert(
                    "health".into(),
                    serde_json::to_value(health).map_err(serde::ser::Error::custom)?,
                );
            }
            Self::Paused => insert_state(&mut object, "paused"),
            Self::Restarting => insert_state(&mut object, "restarting"),
            Self::Exited { code } => {
                insert_state(&mut object, "exited");
                object.insert("code".into(), Value::from(*code));
            }
            Self::Removing => insert_state(&mut object, "removing"),
            Self::Dead => insert_state(&mut object, "dead"),
            Self::Unknown { raw } => return raw.serialize(serializer),
        }
        Value::Object(object).serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for ContainerRuntimeObservation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let raw = Value::deserialize(deserializer)?;
        let Some(object) = raw.as_object() else {
            return Ok(Self::Unknown { raw });
        };
        let Some(state) = object.get("state").and_then(Value::as_str) else {
            return Ok(Self::Unknown { raw });
        };

        match state {
            "created" => Ok(Self::Created),
            "running" => {
                let health = object
                    .get("health")
                    .cloned()
                    .ok_or_else(|| D::Error::missing_field("health"))?;
                Ok(Self::Running {
                    health: serde_json::from_value(health).map_err(D::Error::custom)?,
                })
            }
            "paused" => Ok(Self::Paused),
            "restarting" => Ok(Self::Restarting),
            "exited" => {
                let code = object
                    .get("code")
                    .and_then(Value::as_i64)
                    .ok_or_else(|| D::Error::missing_field("code"))?;
                Ok(Self::Exited { code })
            }
            "removing" => Ok(Self::Removing),
            "dead" => Ok(Self::Dead),
            _ => Ok(Self::Unknown { raw }),
        }
    }
}

fn insert_state(object: &mut Map<String, Value>, state: &'static str) {
    object.insert("state".into(), Value::String(state.into()));
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerKind {
    ServiceContainer,
    PreDeployHook,
}

/// A local observation of one managed container. Replication redacts it at the store boundary.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContainerObservation {
    pub container_id: ContainerId,
    /// Generated Docker name for display, never identity or selection.
    pub display_name: String,
    /// Docker creation time, used only to select the newest observed Service spec.
    #[serde(default)]
    pub created_at_unix_nanos: i64,
    pub machine_id: MachineId,
    pub service_id: ServiceId,
    pub service_name: ServiceName,
    pub kind: ContainerKind,
    pub runtime: ContainerRuntimeObservation,
    /// Effective Docker health check, including image-inherited configuration.
    #[serde(default)]
    pub effective_healthcheck: Option<HealthcheckSpec>,
    /// Historical spec used to create this container; not a current Service spec.
    pub resolved_spec: ResolvedServiceSpec,
    #[serde(default)]
    pub address: Option<ContainerAddress>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
}
