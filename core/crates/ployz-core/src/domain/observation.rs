use std::{
    collections::BTreeMap,
    fmt::{self, Write as _},
};

use serde::{Deserialize, Deserializer, Serialize, Serializer, de::Error as _};
use serde_json::Value;
use thiserror::Error;
use ts_rs::TS;

use super::{
    rotation::{CopyRotation, Gen},
    spec::{HealthcheckSpec, ResolvedServiceSpec},
};
use crate::labels::{self, LabelMap};
use crate::{
    ContainerAddress, ContainerId, DeploymentLogId, HeldDoor, MachineId, Namespace,
    QualifiedService, ServiceId, ServiceName,
};

/// Value that replaces each environment value in a redacted observation.
pub const REDACTED_ENVIRONMENT_VALUE: &str = "<redacted>";

crate::value::open_string_enum!(HealthObservation, Unrecognized {
    NotConfigured => "not_configured",
    Starting => "starting",
    Healthy => "healthy",
    Unhealthy => "unhealthy",
    // Passed since its current start, failing now; still routed.
    Failing => "failing",
    // Withdrawn from the Ingress Proxies ahead of a stop; never routed.
    Stopping => "stopping",
});

/// The newest healthcheck result a Machine saw for a Container. Inspection
/// reports it fresh and it is never replicated; deploy failure reasons, which
/// the whole Organization reads, quote it.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum LastHealthCheck {
    Exited { code: i64, output: String },
    HttpStatus { status: u16 },
    HttpUnreachable { error: HttpCheckError },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum HttpCheckError {
    ConnectionRefused,
    TimedOut,
    Other,
}

const LAST_CHECK_OUTPUT_CHARS: usize = 120;

impl LastHealthCheck {
    /// Keeps only the first non-blank line of `output`, cut to a length a reason can quote.
    #[must_use]
    pub fn exited(code: i64, output: &str) -> Self {
        Self::Exited {
            code,
            output: check_output_line(output),
        }
    }
}

/// The first non-blank line of `output`, cut to a length a reason can quote. Ployz Cloud's uptime
/// probes cut their error details with it too.
#[must_use]
pub fn check_output_line(output: &str) -> String {
    let line = output
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())
        .unwrap_or_default();
    let mut cut: String = line.chars().take(LAST_CHECK_OUTPUT_CHARS).collect();
    if line.chars().nth(LAST_CHECK_OUTPUT_CHARS).is_some() {
        cut.push('…');
    }
    cut
}

impl fmt::Display for LastHealthCheck {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Exited { code, output } if output.is_empty() => {
                write!(f, "last check exited {code}")
            }
            Self::Exited { code, output } => {
                write!(f, "last check exited {code}: ")?;
                output.chars().try_for_each(|c| {
                    if c.is_control() {
                        write!(f, "{}", c.escape_default())
                    } else {
                        f.write_char(c)
                    }
                })
            }
            Self::HttpStatus { status } => write!(f, "last check: HTTP {status}"),
            Self::HttpUnreachable { error } => f.write_str(match error {
                HttpCheckError::ConnectionRefused => "last check: connection refused",
                HttpCheckError::TimedOut => "last check: timed out",
                HttpCheckError::Other => "last check: request failed",
            }),
        }
    }
}

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

impl fmt::Display for ContainerRuntimeObservation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Created => f.write_str("created"),
            Self::Running { health } => {
                write!(f, "running (health: {})", health.as_str().escape_debug())
            }
            Self::Paused => f.write_str("paused"),
            Self::Restarting => f.write_str("restarting"),
            Self::Exited { code } => write!(f, "exited with code {code}"),
            Self::Removing => f.write_str("removing"),
            Self::Dead => f.write_str("dead"),
            Self::Unknown { raw } => write!(
                f,
                "unrecognized runtime state: {}",
                raw.to_string().escape_debug()
            ),
        }
    }
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
    /// True when the container's process isn't running and can serve nothing.
    #[must_use]
    pub fn at_rest(&self) -> bool {
        matches!(self, Self::Created | Self::Exited { .. } | Self::Dead)
    }

    /// Eligible for traffic: Running with health Healthy, Failing or NotConfigured.
    #[must_use]
    pub fn may_serve(&self) -> bool {
        matches!(
            self,
            Self::Running {
                health: HealthObservation::Healthy
                    | HealthObservation::Failing
                    | HealthObservation::NotConfigured,
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

impl fmt::Display for ContainerKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::ServiceContainer => "Service",
            Self::PreDeployHook => "pre-deploy hook",
        })
    }
}

/// Raw facts for admitting one Container observation.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[ts(rename = "ContainerObservation")]
pub struct ContainerObservationParts {
    pub container_id: ContainerId,
    /// Generated Docker name for display, never identity or selection.
    pub display_name: String,
    /// Docker creation time, used only to select the newest observed Service spec.
    #[serde(default)]
    pub created_at_unix_nanos: i64,
    pub machine_id: MachineId,
    pub namespace: Namespace,
    pub kind: ContainerKind,
    pub runtime: ContainerRuntimeObservation,
    /// Effective Docker check (including image inheritance), or the Machine HTTP probe.
    #[serde(default)]
    pub effective_healthcheck: Option<HealthcheckSpec>,
    /// Historical spec used to create this container; not a current Service spec.
    pub resolved_spec: ResolvedServiceSpec,
    #[serde(default)]
    pub address: Option<ContainerAddress>,
    #[serde(default)]
    pub labels: BTreeMap<String, String>,
    /// Written only by the owning Machine. `None` reads `Pending`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub rotation: Option<CopyRotation>,
    /// From the `ployz.gen` label. `None` reads [`Gen::FIRST`].
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub r#gen: Option<Gen>,
    /// The owning Machine ended this exited run for a `Stopped` want, so its exit
    /// code is no crash. Never set on a later run, nor by older daemons.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    #[ts(optional, as = "Option<bool>")]
    pub stopped: bool,
    /// Which run of the Container this observation saw. `None` from older daemons and
    /// for a Container that never started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional = nullable)]
    pub run: Option<crate::ContainerRun>,
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
            (labels::SERVICE_ID, parts.resolved_spec.service_id.as_str()),
            (labels::SERVICE_NAME, parts.resolved_spec.name.as_str()),
            (labels::NAMESPACE, parts.namespace.as_str()),
        ] {
            if parts
                .labels
                .get(label)
                .is_some_and(|value| value != expected)
            {
                return Err(ContainerObservationError { label });
            }
        }
        if (parts.labels.has_label(labels::MANAGED) || parts.labels.has_label(labels::HOOK))
            && parts.labels.has_label(labels::HOOK) != (parts.kind == ContainerKind::PreDeployHook)
        {
            return Err(ContainerObservationError {
                label: labels::HOOK,
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

impl ContainerObservationParts {
    #[must_use]
    pub fn rotation(&self) -> &CopyRotation {
        const PENDING: &CopyRotation = &CopyRotation::Pending;
        self.rotation.as_ref().unwrap_or(PENDING)
    }

    #[must_use]
    pub fn copy_gen(&self) -> Gen {
        self.r#gen.unwrap_or(Gen::FIRST)
    }

    /// The Deploy this Container was created under, from its `ployz.deployment.id` label.
    #[must_use]
    pub fn deploy_label(&self) -> Option<DeploymentLogId> {
        self.deployment_label()
            .and_then(|label| DeploymentLogId::parse(label).ok())
    }

    /// The `ployz.deployment.id` label as written, whether or not it parses.
    #[must_use]
    pub fn deployment_label(&self) -> Option<&str> {
        self.labels.label(labels::DEPLOYMENT_ID)
    }

    /// The creation key this Container was made under, from its `ployz.creation.key` label.
    #[must_use]
    pub fn creation_key(&self) -> Option<&str> {
        self.labels.label(labels::CREATION_KEY)
    }

    /// Whether its creation key names a slot of `run`.
    #[must_use]
    pub fn made_under(&self, run: &DeploymentLogId) -> bool {
        self.creation_key()
            .and_then(|key| key.strip_prefix(run.as_str()))
            .is_some_and(|slot| slot.starts_with(':'))
    }
}

/// The newest Container of a set: the latest Docker creation time, ties to the larger
/// Container ID.
pub fn newest<'a>(
    containers: impl IntoIterator<Item = &'a ContainerObservation>,
) -> Option<&'a ContainerObservation> {
    containers
        .into_iter()
        .max_by_key(|container| container.recency_key())
}

impl ContainerObservation {
    /// Orders Containers oldest to newest: Docker creation time, then Container ID. Put
    /// it last in a larger tuple to rank by something else first.
    #[must_use]
    pub fn recency_key(&self) -> (i64, ContainerId) {
        (self.created_at_unix_nanos, self.container_id)
    }

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
        QualifiedService::new(self.namespace.clone(), self.service_name().clone())
    }

    /// Whether this is a Container of the reserved Ingress Proxy Service.
    #[must_use]
    pub fn is_system_ingress(&self) -> bool {
        self.identity() == QualifiedService::system_ingress()
    }

    /// The door this Container holds for its Server, when it is a door Container of a
    /// wiring Ployz builds. Whether the Container runs is for the caller to ask.
    #[must_use]
    pub fn door(&self) -> Option<HeldDoor> {
        if !self.identity().is_ingress_door() {
            return None;
        }
        HeldDoor::of_spec(&self.resolved_spec)
    }

    /// Replace every Service and pre-deploy hook environment value, keeping the keys,
    /// and empty every Config's content, keeping its name and mounts.
    pub fn redact_values(&mut self) {
        self.try_update(|parts| {
            let spec = &mut parts.resolved_spec;
            let hook = spec.pre_deploy.iter_mut().map(|hook| &mut hook.environment);
            for environment in std::iter::once(&mut spec.container.environment).chain(hook) {
                environment
                    .values_mut()
                    .for_each(|value| *value = REDACTED_ENVIRONMENT_VALUE.into());
            }
            spec.mount_graph.redact_config_content();
        })
        .expect("environment redaction preserves Container identity");
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
#[error("cannot convert a {actual} Container observation to a {requested} view")]
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

    /// Container Address when this Container may serve and is addressed.
    ///
    /// Presence is a per-Container runtime fact, not eligibility to receive
    /// traffic. Traffic also requires a selected Serving Shape.
    #[must_use]
    pub fn traffic_address(&self) -> Option<ContainerAddress> {
        let observation = self.as_observation();
        observation.runtime.may_serve().then_some(())?;
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
    use crate::{ContainerId, MachineId, Namespace, ResolvedServiceSpec, ServiceId, ServiceName};

    #[test]
    fn at_rest_is_created_exited_or_dead() {
        for (runtime, at_rest) in [
            (ContainerRuntimeObservation::Created, true),
            (ContainerRuntimeObservation::Exited { code: 0 }, true),
            (ContainerRuntimeObservation::Exited { code: 137 }, true),
            (ContainerRuntimeObservation::Dead, true),
            (
                ContainerRuntimeObservation::Running {
                    health: HealthObservation::Healthy,
                },
                false,
            ),
            (
                ContainerRuntimeObservation::Running {
                    health: HealthObservation::Unhealthy,
                },
                false,
            ),
            (ContainerRuntimeObservation::Paused, false),
            (ContainerRuntimeObservation::Restarting, false),
            (ContainerRuntimeObservation::Removing, false),
            (
                ContainerRuntimeObservation::Unknown {
                    raw: json!({ "state": "future" }),
                },
                false,
            ),
        ] {
            assert_eq!(runtime.at_rest(), at_rest, "{runtime}");
        }
    }

    #[test]
    fn may_serve_is_running_with_healthy_failing_or_not_configured() {
        assert!(
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Healthy,
            }
            .may_serve()
        );
        assert!(
            ContainerRuntimeObservation::Running {
                health: HealthObservation::Failing,
            }
            .may_serve()
        );
        assert!(
            ContainerRuntimeObservation::Running {
                health: HealthObservation::NotConfigured,
            }
            .may_serve()
        );
        assert!(
            !ContainerRuntimeObservation::Running {
                health: HealthObservation::Starting,
            }
            .may_serve()
        );
        assert!(
            !ContainerRuntimeObservation::Running {
                health: HealthObservation::Unhealthy,
            }
            .may_serve()
        );
        assert!(
            !ContainerRuntimeObservation::Running {
                health: HealthObservation::Unrecognized("degraded".into()),
            }
            .may_serve()
        );
        assert!(!ContainerRuntimeObservation::Created.may_serve());
        assert!(!ContainerRuntimeObservation::Paused.may_serve());
        assert!(!ContainerRuntimeObservation::Restarting.may_serve());
        assert!(!ContainerRuntimeObservation::Exited { code: 0 }.may_serve());
        assert!(!ContainerRuntimeObservation::Removing.may_serve());
        assert!(!ContainerRuntimeObservation::Dead.may_serve());
        assert!(
            !ContainerRuntimeObservation::Unknown {
                raw: json!({ "state": "future" })
            }
            .may_serve()
        );
    }

    #[test]
    fn the_stop_mark_is_on_the_wire_only_when_set_and_absent_reads_unstopped() {
        let mut stopped = observation(ContainerKind::ServiceContainer);
        stopped
            .try_update(|parts| {
                parts.runtime = ContainerRuntimeObservation::Exited { code: 1 };
                parts.stopped = true;
            })
            .unwrap();
        let wire = serde_json::to_value(&stopped).unwrap();
        assert_eq!(
            wire.get("runtime"),
            Some(&json!({"state": "exited", "code": 1}))
        );
        assert_eq!(wire.get("stopped"), Some(&json!(true)));

        let mut older = wire;
        older.as_object_mut().unwrap().remove("stopped");
        let decoded: ContainerObservation = serde_json::from_value(older).unwrap();
        assert!(
            !decoded.stopped,
            "an older daemon's observation never says stopped"
        );
        assert_eq!(
            serde_json::to_value(&decoded).unwrap().get("stopped"),
            None,
            "nor does an unstopped one"
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
    fn mixed_container_observation_keeps_namespace_and_retained_service_name_on_the_wire() {
        let service = observation(ContainerKind::ServiceContainer);
        let json = serde_json::to_value(&service).unwrap();

        assert_eq!(json.get("namespace"), Some(&json!("app")));
        assert_eq!(json.pointer("/resolved_spec/name"), Some(&json!("api")));
        assert_eq!(
            serde_json::from_value::<ContainerObservation>(json).unwrap(),
            service
        );
    }

    #[test]
    fn mixed_container_observation_rejects_missing_namespace() {
        let mut json = serde_json::to_value(observation(ContainerKind::ServiceContainer)).unwrap();
        json.as_object_mut()
            .expect("observation serializes as an object")
            .remove("namespace");
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
    fn redact_values_clears_config_content_and_keeps_mounts() {
        let mut observed: ContainerObservation = serde_json::from_value(serde_json::json!({
            "container_id": "a".repeat(64),
            "display_name": "api-test",
            "machine_id": "b".repeat(32),
            "namespace": "app",
            "kind": "service_container",
            "runtime": { "state": "created" },
            "resolved_spec": {
                "service_id": "c".repeat(32),
                "name": "api",
                "mode": { "mode": "replicated", "replicas": 1 },
                "container": {
                    "image": "alpine:3.23.3",
                    "environment": { "TOKEN": "service-secret" },
                    "pull_policy": "missing",
                    "config_mounts": [{ "config_name": "settings", "target": "/etc/settings.conf" }]
                },
                "configs": [{ "name": "settings", "content": b"token=config-secret".to_vec() }]
            }
        }))
        .unwrap();
        let mounts = observed.resolved_spec.config_mounts().to_vec();

        observed.redact_values();

        assert_eq!(
            observed.resolved_spec.configs(),
            [crate::ConfigSpec {
                name: "settings".into(),
                content: Vec::new(),
            }]
        );
        assert_eq!(observed.resolved_spec.config_mounts(), mounts.as_slice());
        assert_eq!(
            observed
                .resolved_spec
                .container
                .environment
                .get("TOKEN")
                .and_then(crate::EnvValue::text),
            Some(crate::REDACTED_ENVIRONMENT_VALUE)
        );
        let wire = serde_json::to_value(&observed).unwrap();
        assert_eq!(
            wire.pointer("/resolved_spec/configs"),
            Some(&serde_json::json!([{ "name": "settings", "content": [] }]))
        );
        assert_eq!(
            serde_json::from_value::<ContainerObservation>(wire).unwrap(),
            observed
        );
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
                parts.labels.insert("ployz.namespace".into(), "app".into());
            })
            .unwrap();
        for (label, value) in [
            ("ployz.service.name", "web".to_owned()),
            ("ployz.service.id", "b".repeat(32)),
            ("ployz.namespace", "other".to_owned()),
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
        // Replicated rows are read tolerantly: a stray grouping field is ignored,
        // and grouping still comes from the retained spec.
        let mut unknown = serde_json::to_value(&observed).unwrap();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("service_name".into(), "web".into());
        assert_eq!(
            serde_json::from_value::<ContainerObservation>(unknown).unwrap(),
            observed
        );
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

    #[test]
    fn admission_accepts_exactly_the_label_sets_consistent_with_the_retained_facts() {
        let service = ContainerKind::ServiceContainer;
        let hook = ContainerKind::PreDeployHook;
        let id = "a".repeat(32);
        // (labels, kind, rejected by)
        type Case<'a> = (&'a [(&'a str, &'a str)], ContainerKind, Option<&'a str>);
        let table: [Case; 22] = [
            (&[], service, None),
            (&[], hook, None),
            (&[("ployz.managed", "")], service, None),
            (&[("ployz.managed", "true")], service, None),
            (&[("ployz.managed", "")], hook, Some("ployz.service.hook")),
            (
                &[("ployz.managed", ""), ("ployz.service.hook", "pre-deploy")],
                service,
                Some("ployz.service.hook"),
            ),
            (
                &[("ployz.managed", ""), ("ployz.service.hook", "pre-deploy")],
                hook,
                None,
            ),
            (
                &[("ployz.managed", ""), ("ployz.service.hook", "future-hook")],
                hook,
                None,
            ),
            (
                &[("ployz.service.hook", "")],
                service,
                Some("ployz.service.hook"),
            ),
            (&[("ployz.service.hook", "")], hook, None),
            (&[("ployzd.managed", "")], hook, None),
            (&[("ployzd.managed", "")], service, None),
            (&[("com.example.other", "x")], hook, None),
            (&[("ployz.namespace", "app")], service, None),
            (
                &[("ployz.namespace", "other")],
                service,
                Some("ployz.namespace"),
            ),
            (&[("ployz.namespace", "")], service, Some("ployz.namespace")),
            (&[("ployz.service.name", "api")], service, None),
            (
                &[("ployz.service.name", "web")],
                service,
                Some("ployz.service.name"),
            ),
            (&[("ployz.service.id", &id)], service, None),
            (
                &[("ployz.service.id", "b")],
                service,
                Some("ployz.service.id"),
            ),
            (
                &[
                    ("ployz.service.id", "b"),
                    ("ployz.service.name", "web"),
                    ("ployz.namespace", "other"),
                ],
                service,
                Some("ployz.service.id"),
            ),
            (
                &[("ployz.namespace", "other"), ("ployz.service.hook", "")],
                service,
                Some("ployz.namespace"),
            ),
        ];
        for (labels, kind, rejected_by) in table {
            let mut parts = observation(kind).into_parts();
            parts.labels = labels
                .iter()
                .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
                .collect();
            let admitted = ContainerObservation::try_from(parts).map(|_| ());
            assert_eq!(
                admitted.err().map(|error| error.label),
                rejected_by,
                "{labels:?} on {kind:?}"
            );
        }
    }

    #[test]
    fn label_accessors_read_the_run_and_the_creation_key() {
        let mut parts: crate::ContainerObservationParts =
            observation(ContainerKind::ServiceContainer).into();
        let run = crate::DeploymentLogId::parse("deploy-7").unwrap();
        let other = crate::DeploymentLogId::parse("deploy-70").unwrap();
        let read = |parts: &crate::ContainerObservationParts| {
            ContainerObservation::try_from(parts.clone()).unwrap()
        };
        let bare = read(&parts);
        assert_eq!(bare.deployment_label(), None);
        assert_eq!(bare.creation_key(), None);
        assert!(!bare.made_under(&run));

        parts.labels.insert(
            crate::labels::DEPLOYMENT_ID.to_owned(),
            "deploy-7".to_owned(),
        );
        parts.labels.insert(
            crate::labels::CREATION_KEY.to_owned(),
            "deploy-7:0".to_owned(),
        );
        let stamped = read(&parts);
        assert_eq!(stamped.deployment_label(), Some("deploy-7"));
        assert_eq!(stamped.deploy_label(), Some(run.clone()));
        assert_eq!(stamped.creation_key(), Some("deploy-7:0"));
        assert!(stamped.made_under(&run));
        assert!(!stamped.made_under(&other));

        parts.labels.insert(
            crate::labels::DEPLOYMENT_ID.to_owned(),
            "not a run!".to_owned(),
        );
        let unparsed = read(&parts);
        assert_eq!(unparsed.deployment_label(), Some("not a run!"));
        assert_eq!(unparsed.deploy_label(), None);
    }

    #[test]
    fn only_the_reserved_ingress_identity_is_a_system_ingress_container() {
        let mut container = observation(ContainerKind::ServiceContainer);
        assert!(!container.is_system_ingress());

        let ingress = crate::QualifiedService::system_ingress();
        container
            .try_update(|parts| {
                parts.namespace = ingress.namespace.clone();
                parts.resolved_spec.name = ingress.name.clone();
            })
            .unwrap();
        assert!(container.is_system_ingress());

        let holder = crate::QualifiedService::system_ingress_holder();
        container
            .try_update(|parts| parts.resolved_spec.name = holder.name.clone())
            .unwrap();
        assert!(!container.is_system_ingress());
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
            rotation: None,
            r#gen: None,
            container_id: ContainerId::parse(id.to_string().repeat(64)).unwrap(),
            display_name: format!("api-{id}"),
            created_at_unix_nanos: 0,
            machine_id: MachineId::parse(id.to_string().repeat(32)).unwrap(),
            namespace: Namespace::parse("app").unwrap(),
            kind,
            runtime: ContainerRuntimeObservation::Created,
            effective_healthcheck: None,
            resolved_spec,
            address: None,
            labels: BTreeMap::new(),
            stopped: false,
            run: None,
        })
        .unwrap()
    }
}
