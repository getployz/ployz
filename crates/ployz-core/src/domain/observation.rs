use std::collections::BTreeMap;

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::Value;
use thiserror::Error;
use ts_rs::TS;

use super::spec::{HealthcheckSpec, ResolvedServiceSpec};
use crate::{
    ContainerAddress, ContainerId, MachineId, ProjectName, QualifiedService, ServiceId, ServiceName,
};

crate::value::open_string_enum!(HealthObservation, Unrecognized {
    NotConfigured => "not_configured",
    Starting => "starting",
    Healthy => "healthy",
    Unhealthy => "unhealthy",
});

/// Docker state as observed, including the untouched value of a future state.
///
/// On the wire, a known state is `{ "state": "running", ... }`. A value this
/// reader cannot classify is `{ "state": "unrecognized", "raw": <as observed> }`,
/// so every reader sees a closed set of `state` spellings and a newer reader
/// recovers the observed value from `raw`. A known `state` with malformed
/// fields is an error, not an unknown state.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case", remote = "Self")]
pub enum ContainerRuntimeObservation {
    Created,
    Running {
        health: HealthObservation,
    },
    Paused,
    Restarting,
    Exited {
        code: i64,
    },
    Removing,
    Dead,
    #[serde(rename = "unrecognized")]
    Unknown {
        raw: Value,
    },
}

/// The `state` spellings this reader classifies; anything else is kept as observed.
const KNOWN_STATES: [&str; 8] = [
    "created",
    "running",
    "paused",
    "restarting",
    "exited",
    "removing",
    "dead",
    "unrecognized",
];

impl Serialize for ContainerRuntimeObservation {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        // `remote = "Self"` turns the derive into an inherent fn.
        Self::serialize(self, serializer)
    }
}

impl<'de> Deserialize<'de> for ContainerRuntimeObservation {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = Value::deserialize(deserializer)?;
        match Self::deserialize(&raw) {
            Ok(observation) => Ok(observation),
            Err(error) => {
                let known_state = raw
                    .get("state")
                    .and_then(Value::as_str)
                    .is_some_and(|state| KNOWN_STATES.contains(&state));
                if known_state {
                    Err(D::Error::custom(error))
                } else {
                    Ok(Self::Unknown { raw })
                }
            }
        }
    }
}

impl ContainerRuntimeObservation {
    /// Running with health Healthy or NotConfigured.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        matches!(
            self,
            Self::Running {
                health: HealthObservation::Healthy | HealthObservation::NotConfigured,
            }
        )
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ContainerKind {
    ServiceContainer,
    PreDeployHook,
}

/// Raw facts for admitting one Container observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(deny_unknown_fields)]
#[ts(rename = "ContainerObservation")]
pub struct ContainerObservationParts {
    pub container_id: ContainerId,
    /// Generated Docker name for display, never identity or selection.
    pub display_name: String,
    /// Docker creation time, used only to select the newest observed Service spec.
    #[serde(default)]
    pub created_at_unix_nanos: i64,
    pub machine_id: MachineId,
    pub project_name: ProjectName,
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

/// A coherent observation of one managed Container, retaining its historical spec.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(
    try_from = "ContainerObservationParts",
    into = "ContainerObservationParts"
)]
#[ts(as = "ContainerObservationParts")]
pub struct ContainerObservation {
    parts: ContainerObservationParts,
}

/// A Docker identity label disagrees with this Container's retained facts.
#[derive(Clone, Debug, Eq, PartialEq, Error)]
#[error("container label {label} disagrees with retained observation")]
pub struct ContainerObservationError {
    pub label: &'static str,
}

impl TryFrom<ContainerObservationParts> for ContainerObservation {
    type Error = ContainerObservationError;

    fn try_from(parts: ContainerObservationParts) -> Result<Self, Self::Error> {
        for (label, expected) in [
            ("ployz.service.id", parts.resolved_spec.service_id.as_str()),
            ("ployz.service.name", parts.resolved_spec.name.as_str()),
            ("ployz.project.name", parts.project_name.as_str()),
        ] {
            if parts
                .labels
                .get(label)
                .is_some_and(|value| value != expected)
            {
                return Err(ContainerObservationError { label });
            }
        }
        if (parts.labels.contains_key("ployz.managed")
            || parts.labels.contains_key("ployz.service.hook"))
            && parts.labels.contains_key("ployz.service.hook")
                != (parts.kind == ContainerKind::PreDeployHook)
        {
            return Err(ContainerObservationError {
                label: "ployz.service.hook",
            });
        }
        Ok(Self { parts })
    }
}

impl std::ops::Deref for ContainerObservation {
    type Target = ContainerObservationParts;

    fn deref(&self) -> &Self::Target {
        &self.parts
    }
}

impl From<ContainerObservation> for ContainerObservationParts {
    fn from(observation: ContainerObservation) -> Self {
        observation.parts
    }
}

impl ContainerObservation {
    /// Update observed facts atomically, retaining the previous observation on rejection.
    ///
    /// # Errors
    /// Returns an error if the edited facts disagree with retained identity labels.
    pub fn try_update(
        &mut self,
        update: impl FnOnce(&mut ContainerObservationParts),
    ) -> Result<(), ContainerObservationError> {
        let mut parts = self.parts.clone();
        update(&mut parts);
        *self = Self::try_from(parts)?;
        Ok(())
    }

    /// Release the facts for editing; admission must be checked again afterwards.
    #[must_use]
    pub fn into_parts(self) -> ContainerObservationParts {
        self.parts
    }

    /// Service deployment identity carried by this Container's retained spec.
    #[must_use]
    pub fn service_id(&self) -> ServiceId {
        self.resolved_spec.service_id
    }

    /// Service name carried by this Container's retained spec.
    #[must_use]
    pub fn service_name(&self) -> &ServiceName {
        &self.resolved_spec.name
    }

    /// Logical Service identity carried by this Container.
    #[must_use]
    pub fn identity(&self) -> QualifiedService {
        QualifiedService::new(self.project_name.clone(), self.service_name().clone())
    }
}

/// A Service Container after its role has been proven from a mixed observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "ContainerObservation", into = "ContainerObservation")]
#[ts(type = "ContainerObservation")]
pub struct ServiceContainer {
    observation: ContainerObservation,
}

/// A Hook Container after its role has been proven from a mixed observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(try_from = "ContainerObservation", into = "ContainerObservation")]
#[ts(type = "ContainerObservation")]
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

    /// Container Address when this Container is healthy and addressed.
    ///
    /// Presence is a per-Container runtime fact, not eligibility to receive
    /// traffic. Traffic also requires a selected Serving Shape.
    #[must_use]
    pub fn traffic_address(&self) -> Option<ContainerAddress> {
        let observation = self.as_observation();
        observation.runtime.is_healthy().then_some(())?;
        observation.address
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

    #[test]
    fn known_states_name_every_variant_spelling() {
        let variants = [
            ContainerRuntimeObservation::Created,
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            },
            ContainerRuntimeObservation::Paused,
            ContainerRuntimeObservation::Restarting,
            ContainerRuntimeObservation::Exited { code: 0 },
            ContainerRuntimeObservation::Removing,
            ContainerRuntimeObservation::Dead,
            ContainerRuntimeObservation::Unknown { raw: Value::Null },
        ];
        let spellings: Vec<String> = variants
            .iter()
            .map(|variant| {
                serde_json::to_value(variant)
                    .unwrap()
                    .get("state")
                    .and_then(Value::as_str)
                    .unwrap()
                    .to_owned()
            })
            .collect();
        assert_eq!(spellings, KNOWN_STATES);
        assert!(
            serde_json::from_value::<ContainerRuntimeObservation>(
                serde_json::json!({ "state": "running" })
            )
            .is_err()
        );
    }

    use serde_json::json;

    use super::{
        Container, ContainerKind, ContainerObservation, ContainerRoleError,
        ContainerRuntimeObservation, HealthObservation, HookContainer, KNOWN_STATES,
        ServiceContainer, Value,
    };
    use crate::{ContainerId, MachineId, ProjectName, ResolvedServiceSpec, ServiceId, ServiceName};

    #[test]
    fn is_healthy_is_running_with_healthy_or_not_configured() {
        assert!(
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            }
            .is_healthy()
        );
        assert!(
            ContainerRuntimeObservation::Running {
                health: HealthObservation::NotConfigured,
            }
            .is_healthy()
        );
        assert!(
            !ContainerRuntimeObservation::Running {
                health: HealthObservation::Starting,
            }
            .is_healthy()
        );
        assert!(
            !ContainerRuntimeObservation::Running {
                health: HealthObservation::Unhealthy,
            }
            .is_healthy()
        );
        assert!(
            !ContainerRuntimeObservation::Running {
                health: HealthObservation::Unrecognized("degraded".into()),
            }
            .is_healthy()
        );
        assert!(!ContainerRuntimeObservation::Created.is_healthy());
        assert!(!ContainerRuntimeObservation::Paused.is_healthy());
        assert!(!ContainerRuntimeObservation::Restarting.is_healthy());
        assert!(!ContainerRuntimeObservation::Exited { code: 0 }.is_healthy());
        assert!(!ContainerRuntimeObservation::Removing.is_healthy());
        assert!(!ContainerRuntimeObservation::Dead.is_healthy());
        assert!(
            !ContainerRuntimeObservation::Unknown {
                raw: json!({ "state": "future" })
            }
            .is_healthy()
        );
    }

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

    #[test]
    fn mixed_container_observation_keeps_project_and_retained_service_name_on_the_wire() {
        let service = observation(ContainerKind::ServiceContainer);
        let json = serde_json::to_value(&service).unwrap();

        assert_eq!(json.get("project_name"), Some(&json!("app")));
        assert_eq!(json.pointer("/resolved_spec/name"), Some(&json!("api")));
        assert_eq!(
            serde_json::from_value::<ContainerObservation>(json).unwrap(),
            service
        );
    }

    #[test]
    fn mixed_container_observation_rejects_missing_project_name() {
        let mut json = serde_json::to_value(observation(ContainerKind::ServiceContainer)).unwrap();
        json.as_object_mut()
            .expect("observation serializes as an object")
            .remove("project_name");
        assert!(serde_json::from_value::<ContainerObservation>(json).is_err());
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

    #[test]
    fn admission_rejects_conflicting_labels_and_preserves_previous_observation() {
        let mut observed = observation(ContainerKind::ServiceContainer);
        observed
            .try_update(|parts| {
                parts
                    .labels
                    .insert("ployz.service.name".into(), "api".into());
                parts
                    .labels
                    .insert("ployz.service.id".into(), "a".repeat(32));
                parts
                    .labels
                    .insert("ployz.project.name".into(), "app".into());
            })
            .unwrap();
        for (label, value) in [
            ("ployz.service.name", "web".to_owned()),
            ("ployz.service.id", "b".repeat(32)),
            ("ployz.project.name", "other".to_owned()),
            ("ployz.service.hook", "pre-deploy".to_owned()),
        ] {
            let mut raw = serde_json::to_value(&observed).unwrap();
            raw.get_mut("labels")
                .unwrap()
                .as_object_mut()
                .unwrap()
                .insert(label.into(), value.into());
            let parts: crate::ContainerObservationParts =
                serde_json::from_value(raw.clone()).unwrap();
            assert_eq!(
                ContainerObservation::try_from(parts).unwrap_err().label,
                label
            );
            assert!(
                serde_json::from_value::<ContainerObservation>(raw).is_err(),
                "{label}"
            );
        }
        let mut duplicated = serde_json::to_value(&observed).unwrap();
        duplicated
            .as_object_mut()
            .unwrap()
            .insert("service_name".into(), "web".into());
        assert!(serde_json::from_value::<ContainerObservation>(duplicated).is_err());
        let previous = observed.clone();
        assert!(
            observed
                .try_update(|parts| {
                    parts.resolved_spec.name = ServiceName::parse("web").unwrap();
                })
                .is_err()
        );
        assert_eq!(observed, previous);
    }

    #[test]
    fn wire_identity_and_grouping_come_from_each_retained_spec() {
        let mut old = observation(ContainerKind::ServiceContainer);
        old.try_update(|parts| {
            parts.runtime = ContainerRuntimeObservation::Unknown {
                raw: json!({"state": "future"}),
            };
            parts.resolved_spec.mode = crate::ServiceMode::Global;
        })
        .unwrap();
        let mut newer = old.clone();
        newer
            .try_update(|parts| {
                parts.container_id = ContainerId::parse("3".repeat(64)).unwrap();
                parts.created_at_unix_nanos = 1;
                parts.resolved_spec.container.image = "api:new".into();
                parts.resolved_spec.service_id = ServiceId::parse("b".repeat(32)).unwrap();
            })
            .unwrap();
        let wire = serde_json::to_value(&newer).unwrap();
        assert!(wire.get("service_name").is_none());
        assert!(wire.get("service_id").is_none());
        assert_eq!(
            serde_json::from_value::<ContainerObservation>(wire).unwrap(),
            newer
        );
        let mut hook = observation(ContainerKind::PreDeployHook);
        hook.try_update(|parts| {
            parts
                .labels
                .insert("ployz.service.hook".into(), "future-hook".into());
        })
        .unwrap();
        let grouped = crate::derive_services([old, newer, hook]);
        assert_eq!(grouped.len(), 1);
        let group = grouped.first().unwrap();
        assert_eq!(group.identity.to_string(), "app/api");
        assert_eq!(group.containers.len(), 2);
        assert_eq!(group.hook_containers.len(), 1);
        let slot = group.observed_global_slot().unwrap();
        assert_eq!(slot.identity().to_string(), "app/api");
        assert_eq!(slot.resolved_spec().container.image, "api:new");
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
        ContainerObservation::try_from(crate::ContainerObservationParts {
            container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
            display_name: format!("api-{id}"),
            created_at_unix_nanos: 0,
            machine_id: MachineId::parse(id.to_string().repeat(32)).unwrap(),
            project_name: ProjectName::parse("app").unwrap(),
            kind,
            runtime: ContainerRuntimeObservation::Created,
            effective_healthcheck: None,
            resolved_spec,
            address: None,
            labels: BTreeMap::new(),
        })
        .unwrap()
    }
}
