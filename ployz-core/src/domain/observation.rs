use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::{Map, Value};
use thiserror::Error;

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

/// A Service Container after its role has been proven from a mixed observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ContainerObservation", into = "ContainerObservation")]
pub struct ServiceContainer {
    observation: ContainerObservation,
}

/// A Hook Container after its role has been proven from a mixed observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "ContainerObservation", into = "ContainerObservation")]
pub struct HookContainer {
    observation: ContainerObservation,
}

/// Exactly one role-proven view of a mixed [`ContainerObservation`].
#[derive(Clone, Debug, PartialEq)]
pub enum Container {
    Service(ServiceContainer),
    Hook(HookContainer),
}

/// Borrowed role-proven view of one Service member.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ContainerRef<'a> {
    Service(&'a ServiceContainer),
    Hook(&'a HookContainer),
}

/// Rejected conversion from a mixed Container observation to a requested role.
#[derive(Clone, Copy, Debug, Eq, Error, PartialEq)]
#[error("cannot convert a {actual:?} Container observation to a {requested:?} view")]
pub struct ContainerRoleError {
    pub requested: ContainerKind,
    pub actual: ContainerKind,
}

impl ServiceContainer {
    /// Borrow the mixed observation this view was proven from.
    #[must_use]
    pub fn as_observation(&self) -> &ContainerObservation {
        &self.observation
    }

    /// Return the mixed observation this view was proven from.
    #[must_use]
    pub fn into_observation(self) -> ContainerObservation {
        self.observation
    }
}

impl HookContainer {
    /// Borrow the mixed observation this view was proven from.
    #[must_use]
    pub fn as_observation(&self) -> &ContainerObservation {
        &self.observation
    }

    /// Return the mixed observation this view was proven from.
    #[must_use]
    pub fn into_observation(self) -> ContainerObservation {
        self.observation
    }
}

impl AsRef<ContainerObservation> for ContainerObservation {
    fn as_ref(&self) -> &Self {
        self
    }
}

impl AsRef<ContainerObservation> for ServiceContainer {
    fn as_ref(&self) -> &ContainerObservation {
        self.as_observation()
    }
}

impl<'a> ContainerRef<'a> {
    /// Borrow the mixed observation this view was proven from.
    #[must_use]
    pub fn as_observation(self) -> &'a ContainerObservation {
        match self {
            Self::Service(container) => container.as_observation(),
            Self::Hook(container) => container.as_observation(),
        }
    }
}

impl AsRef<ContainerObservation> for ContainerRef<'_> {
    fn as_ref(&self) -> &ContainerObservation {
        self.as_observation()
    }
}

impl From<ServiceContainer> for ContainerObservation {
    fn from(container: ServiceContainer) -> Self {
        container.into_observation()
    }
}

impl From<HookContainer> for ContainerObservation {
    fn from(container: HookContainer) -> Self {
        container.into_observation()
    }
}

impl Container {
    /// Borrow the mixed observation this view was proven from.
    #[must_use]
    pub fn as_observation(&self) -> &ContainerObservation {
        match self {
            Self::Service(container) => container.as_observation(),
            Self::Hook(container) => container.as_observation(),
        }
    }

    /// Return the mixed observation this view was proven from.
    #[must_use]
    pub fn into_observation(self) -> ContainerObservation {
        match self {
            Self::Service(container) => container.into_observation(),
            Self::Hook(container) => container.into_observation(),
        }
    }
}

impl From<ContainerObservation> for Container {
    fn from(observation: ContainerObservation) -> Self {
        match observation.kind {
            ContainerKind::ServiceContainer => Self::Service(ServiceContainer { observation }),
            ContainerKind::PreDeployHook => Self::Hook(HookContainer { observation }),
        }
    }
}

impl From<ServiceContainer> for Container {
    fn from(container: ServiceContainer) -> Self {
        Self::Service(container)
    }
}

impl From<HookContainer> for Container {
    fn from(container: HookContainer) -> Self {
        Self::Hook(container)
    }
}

impl TryFrom<ContainerObservation> for ServiceContainer {
    type Error = ContainerRoleError;

    fn try_from(observation: ContainerObservation) -> Result<Self, Self::Error> {
        Container::from(observation).try_into()
    }
}

impl TryFrom<ContainerObservation> for HookContainer {
    type Error = ContainerRoleError;

    fn try_from(observation: ContainerObservation) -> Result<Self, Self::Error> {
        Container::from(observation).try_into()
    }
}

impl TryFrom<Container> for ServiceContainer {
    type Error = ContainerRoleError;

    fn try_from(container: Container) -> Result<Self, Self::Error> {
        match container {
            Container::Service(container) => Ok(container),
            Container::Hook(_) => Err(ContainerRoleError {
                requested: ContainerKind::ServiceContainer,
                actual: ContainerKind::PreDeployHook,
            }),
        }
    }
}

impl TryFrom<Container> for HookContainer {
    type Error = ContainerRoleError;

    fn try_from(container: Container) -> Result<Self, Self::Error> {
        match container {
            Container::Hook(container) => Ok(container),
            Container::Service(_) => Err(ContainerRoleError {
                requested: ContainerKind::PreDeployHook,
                actual: ContainerKind::ServiceContainer,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use serde_json::json;

    use super::{
        Container, ContainerKind, ContainerObservation, ContainerRoleError,
        ContainerRuntimeObservation, HookContainer, ServiceContainer,
    };
    use crate::{ContainerId, MachineId, ResolvedServiceSpec, ServiceId, ServiceName};

    #[test]
    fn mixed_observation_converts_to_exactly_one_role_proven_view() {
        let service = observation(ContainerKind::ServiceContainer);
        let hook = observation(ContainerKind::PreDeployHook);

        assert!(matches!(
            Container::from(service.clone()),
            Container::Service(_)
        ));
        assert!(matches!(Container::from(hook.clone()), Container::Hook(_)));
        assert!(ServiceContainer::try_from(service.clone()).is_ok());
        assert!(HookContainer::try_from(service).is_err());
        assert!(HookContainer::try_from(hook.clone()).is_ok());
        assert!(ServiceContainer::try_from(hook).is_err());
    }

    #[test]
    fn conversion_rejects_a_mismatched_requested_role() {
        let service = observation(ContainerKind::ServiceContainer);
        let hook = observation(ContainerKind::PreDeployHook);

        assert_eq!(
            HookContainer::try_from(service).unwrap_err(),
            ContainerRoleError {
                requested: ContainerKind::PreDeployHook,
                actual: ContainerKind::ServiceContainer,
            }
        );
        assert_eq!(
            ServiceContainer::try_from(hook).unwrap_err(),
            ContainerRoleError {
                requested: ContainerKind::ServiceContainer,
                actual: ContainerKind::PreDeployHook,
            }
        );
        assert!(matches!(
            HookContainer::try_from(Container::from(observation(
                ContainerKind::ServiceContainer
            ))),
            Err(ContainerRoleError {
                requested: ContainerKind::PreDeployHook,
                actual: ContainerKind::ServiceContainer,
            })
        ));
    }

    #[test]
    fn service_only_and_mixed_member_interfaces_do_not_inspect_kind() {
        let service = ServiceContainer::try_from(observation(ContainerKind::ServiceContainer))
            .expect("service observation converts to a Service Container");
        let hook = HookContainer::try_from(observation(ContainerKind::PreDeployHook))
            .expect("hook observation converts to a Hook Container");

        assert_eq!(start_service(&service), service_container_id());
        assert_eq!(
            stop_container(&Container::from(service)),
            service_container_id()
        );
        assert_eq!(stop_container(&Container::from(hook)), hook_container_id());
    }

    #[test]
    fn mixed_container_observation_keeps_kind_on_the_wire() {
        let service = observation(ContainerKind::ServiceContainer);
        let hook = observation(ContainerKind::PreDeployHook);
        let service_json = serde_json::to_value(&service).unwrap();
        let hook_json = serde_json::to_value(&hook).unwrap();

        assert_eq!(service_json.get("kind"), Some(&json!("service_container")));
        assert_eq!(hook_json.get("kind"), Some(&json!("pre_deploy_hook")));
        assert_eq!(
            serde_json::from_value::<ContainerObservation>(service_json).unwrap(),
            service
        );
        assert_eq!(
            serde_json::from_value::<ContainerObservation>(hook_json).unwrap(),
            hook
        );
    }

    fn start_service(container: &ServiceContainer) -> ContainerId {
        container.as_observation().container_id
    }

    fn stop_container(container: &Container) -> ContainerId {
        match container {
            Container::Service(container) => start_service(container),
            Container::Hook(container) => container.as_observation().container_id,
        }
    }

    fn service_container_id() -> ContainerId {
        ContainerId::parse("1".repeat(64)).unwrap()
    }

    fn hook_container_id() -> ContainerId {
        ContainerId::parse("2".repeat(64)).unwrap()
    }

    fn observation(kind: ContainerKind) -> ContainerObservation {
        let (id, image) = match kind {
            ContainerKind::ServiceContainer => ('1', "api"),
            ContainerKind::PreDeployHook => ('2', "hook"),
        };
        let service_id = ServiceId::parse("a".repeat(32)).unwrap();
        let service_name = ServiceName::parse("api").unwrap();
        let resolved_spec: ResolvedServiceSpec = serde_json::from_value(json!({
            "service_id": service_id,
            "name": service_name,
            "mode": { "mode": "replicated", "replicas": 1 },
            "container": { "image": image, "pull_policy": "missing" }
        }))
        .unwrap();
        ContainerObservation {
            container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
            display_name: format!("api-{id}"),
            created_at_unix_nanos: 0,
            machine_id: MachineId::parse(id.to_string().repeat(32)).unwrap(),
            service_id,
            service_name,
            kind,
            runtime: ContainerRuntimeObservation::Created,
            effective_healthcheck: None,
            resolved_spec,
            address: None,
            labels: BTreeMap::new(),
        }
    }
}
