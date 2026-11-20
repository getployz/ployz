//! Container DTOs: listing, inspection, wanted states, health verdicts, refusals and in-place limit changes.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeMap;
use ts_rs::TS;

use super::error::{RpcError, RpcErrorCode};
use super::evidence::{Departure, Evidence};
use crate::{
    ContainerId, ContainerKind, ContainerLimits, ContainerObservation, Namespace,
    ResolvedServiceSpec, authority::DeployApproval, authority::Signed,
};

/// Whether listed Container observations carry real environment values.
///
/// `Redacted` replaces every Service and pre-deploy hook environment value with
/// [`REDACTED_ENVIRONMENT_VALUE`](crate::REDACTED_ENVIRONMENT_VALUE), keeping the keys.
/// Callers that plan, compare, or copy a `resolved_spec` use `Included`.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EnvironmentValues {
    #[default]
    Redacted,
    Included,
}

/// List this Machine's managed Containers; environment values stay redacted unless the caller requests them.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct ListContainersRequest {
    /// Real or redacted environment values; see [`EnvironmentValues`].
    #[serde(default)]
    pub environment: EnvironmentValues,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InspectContainerRequest {
    pub container_id: ContainerId,
}

/// The place one Container creation lands on a Machine. The same slot names the
/// same Container across resends; a different spec in it is an occupied slot.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct SlotKey {
    pub namespace: Namespace,
    pub service: crate::ServiceName,
    pub kind: ContainerKind,
    pub creation_key: crate::CreationKey,
}

/// A Container named either by its Docker id or by the slot its creation landed on.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ContainerLocator {
    Id(ContainerId),
    Key(SlotKey),
}

/// Ask a Machine to make one Container match a wanted state.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct EnsureContainerRequest {
    pub want: ContainerWant,
    /// The approval of the Deploy this want is part of. A Machine under Device
    /// enforcement requires it from an Operator caller for a new creation, and takes
    /// it for a step on a Container in its Namespace; an older daemon ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub deploy: Option<Signed<DeployApproval>>,
    /// The automatic deploy rule this want is part of, with the pinned builder's image
    /// token for its Service. A Machine under Device enforcement takes it in place of a
    /// deploy approval from an Operator caller. Sent only to a daemon advertising
    /// [`crate::DEPLOY_RULE_CAPABILITY`]; an older daemon ignores it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub rule: Option<Box<crate::authority::RuleProof>>,
    /// The signed versions of the Service a new creation belongs to, at most
    /// [`crate::authority::version::MAX_PRESENTED`]. An older daemon ignores them.
    #[serde(
        default,
        skip_serializing_if = "Vec::is_empty",
        deserialize_with = "crate::authority::version::at_most_presented"
    )]
    #[ts(optional, as = "Option<Vec<Signed<crate::authority::ServiceVersion>>>")]
    pub versions: Vec<Signed<crate::authority::ServiceVersion>>,
}

/// The state a driver wants one Container in. The Machine converges to it and
/// answers with what it then observes.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "state", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "a Running want carries the whole resolved spec; one request holds one"
)]
pub enum ContainerWant {
    Running {
        container: RunTarget,
        rotation: RunRotation,
    },
    Stopped {
        container: ContainerLocator,
        drain: crate::Drain,
        #[serde(default)]
        stop: crate::StopOptions,
    },
    Gone {
        container: ContainerLocator,
        drain: crate::Drain,
        #[serde(default)]
        anonymous_volumes: AnonymousVolumes,
    },
    /// Take the copy out of routing for good. The Container keeps running.
    Out {
        container: ContainerLocator,
        /// Allow the Service to lose its last routed copy. Without it, an In copy
        /// goes Out only while another In copy is acked by every live proxy.
        allow_empty: bool,
        /// The rotation the caller read; `None` checks nothing.
        expect: Option<crate::CopyRotation>,
    },
    /// Take the copy out of routing for `run`, which alone may route it again. The
    /// Container keeps running until a `Stopped` want stops it.
    Held {
        container: ContainerLocator,
        run: crate::DeploymentLogId,
        /// The rotation the caller read; `None` checks nothing.
        expect: Option<crate::CopyRotation>,
    },
}

/// The rotation a `Running` want asks for.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum RunRotation {
    /// Write no rotation: a new copy stays Pending, an existing copy keeps its own.
    Pending,
    /// Route the copy once it is healthy. `run` is the run that holds it, when it is Held.
    In {
        run: Option<crate::DeploymentLogId>,
        /// The rotation the caller read; `None` checks nothing.
        expect: Option<crate::CopyRotation>,
    },
}

/// What `Gone` does with the Container's anonymous Volumes.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AnonymousVolumes {
    /// They go with the Container.
    #[default]
    Remove,
    /// They stay, since deleting data is a Volume decision.
    Keep,
}

impl From<crate::RemoveContainerPurpose> for AnonymousVolumes {
    fn from(purpose: crate::RemoveContainerPurpose) -> Self {
        match purpose {
            crate::RemoveContainerPurpose::Rollout => Self::Remove,
            crate::RemoveContainerPurpose::Retire => Self::Keep,
        }
    }
}

/// What `Running` runs: a new creation in a slot, or a Container that already exists.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "a new creation carries the whole resolved spec; one request holds one"
)]
pub enum RunTarget {
    New(NewContainer),
    Existing(ContainerLocator),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct NewContainer {
    pub slot: SlotKey,
    pub resolved_spec: ResolvedServiceSpec,
    /// Orders this copy among its Service's copies; stamped as the `ployz.gen` label.
    pub r#gen: crate::Gen,
    /// Correlation metadata, never part of the Service configuration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deployment_id: Option<crate::DeploymentLogId>,
    /// Credentials for pulling the image; used for this pull only, never stored.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub registry_auth: Option<crate::RegistryAuth>,
    /// A `PullImage` already placed the image, so creation uses it without pulling.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub image_prepared: bool,
    #[serde(default, skip_serializing_if = "AnswerWhen::is_started")]
    pub answer: AnswerWhen,
}

/// When the Machine answers a new creation: once it starts, or once its health settles.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum AnswerWhen {
    #[default]
    Started,
    /// After the Container is ready, fails, or outlives its health deadline; the
    /// answer carries a [`HealthVerdict`].
    Settled,
}

impl AnswerWhen {
    fn is_started(&self) -> bool {
        *self == Self::Started
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContainerList {
    pub containers: Vec<ContainerObservation>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ContainerDetails {
    pub container: ContainerObservation,
    /// Fresh Docker Config.Env; never copied into replicated observations.
    pub environment: Option<BTreeMap<String, String>>,
    /// Local image ID (`sha256:…`) the Container runs; `None` from older daemons.
    #[serde(default)]
    pub image_id: Option<String>,
    /// The newest healthcheck result; `None` without one, or from older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_check: Option<crate::LastHealthCheck>,
    /// Which run of the Container this is; `None` from older daemons.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub run: Option<crate::ContainerRun>,
    /// Docker's `ExitCode` for a Container that is restarting: the code of the run that
    /// just ended, which `Restarting` does not carry. `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_exit_code: Option<i64>,
    /// The owner is deciding right now whether to take this copy out of rotation, so a
    /// sibling's last-copy check must not count it. `false` from older daemons.
    #[serde(default)]
    pub leaving: bool,
}

/// The Machine's answer to `EnsureContainer`: the Container as now observed, and
/// the refusal when the want could not be met.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum EnsuredContainer {
    Ensured {
        now: ContainerNow,
        /// How the started Container's health settled, when the want asked to wait for it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        health: Option<HealthVerdict>,
    },
    Refused {
        now: ContainerNow,
        refusal: ContainerRefusal,
    },
}

/// What the Machine observes at the asked-for Container, after any effect.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "is", rename_all = "snake_case")]
#[expect(
    clippy::large_enum_variant,
    reason = "a present Container is the whole observation; one reply holds one"
)]
pub enum ContainerNow {
    Present(ContainerObservation),
    Absent { of: ContainerLocator },
}

/// How a started Container's health settled before the Machine answered.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, TS)]
#[serde(tag = "verdict", rename_all = "snake_case")]
pub enum HealthVerdict {
    Healthy,
    Failed {
        observation: crate::ContainerRuntimeObservation,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        last_check: Option<crate::LastHealthCheck>,
        /// Docker OOM-killed a process of the Container's run; `false` from older daemons.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(optional, as = "Option<bool>")]
        oom_killed: bool,
        /// The Leave that was removing the Server as it answered, so what the copy did
        /// since was the Leave's doing. `None` from older daemons.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        departing: Option<Departure>,
    },
    TimedOut {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        deadline_millis: Option<u64>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        last_check: Option<crate::LastHealthCheck>,
        /// Docker OOM-killed a process of the Container's run; `false` from older daemons.
        #[serde(default, skip_serializing_if = "std::ops::Not::not")]
        #[ts(optional, as = "Option<bool>")]
        oom_killed: bool,
        /// The Leave that was removing the Server as it answered, so what the copy did
        /// since was the Leave's doing. `None` from older daemons.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        #[ts(optional)]
        departing: Option<Departure>,
    },
}

impl HealthVerdict {
    /// The Leave that was removing the Server as it answered for a failed Container.
    #[must_use]
    pub const fn departing(&self) -> Option<&Departure> {
        match self {
            Self::Healthy => None,
            Self::Failed { departing, .. } | Self::TimedOut { departing, .. } => departing.as_ref(),
        }
    }

    /// Whether Docker OOM-killed a process of the failed Container's run.
    #[must_use]
    pub const fn oom_killed(&self) -> bool {
        match self {
            Self::Healthy => false,
            Self::Failed { oom_killed, .. } | Self::TimedOut { oom_killed, .. } => *oom_killed,
        }
    }

    /// Why the Container is not serving, or `None` when it is.
    #[must_use]
    pub fn into_failure(self) -> Option<crate::HealthFailure> {
        match self {
            Self::Healthy => None,
            Self::Failed {
                observation,
                last_check,
                ..
            } => Some(crate::HealthFailure::Runtime {
                observation,
                last_check,
            }),
            Self::TimedOut {
                deadline_millis,
                last_check,
                ..
            } => Some(crate::HealthFailure::TimedOut {
                deadline_millis,
                last_check,
            }),
        }
    }
}

impl EnsuredContainer {
    /// The observed Container, or the refusal as the error the replaced verbs answered.
    pub fn into_result(self) -> Result<ContainerNow, RpcError> {
        self.into_settled().map(|(now, _)| now)
    }

    /// The observed Container with its health verdict, or the refusal as an error.
    pub fn into_settled(self) -> Result<(ContainerNow, Option<HealthVerdict>), RpcError> {
        match self {
            Self::Ensured { now, health } => Ok((now, health)),
            Self::Refused { refusal, .. } => Err(refusal.into_error()),
        }
    }
}

impl ContainerNow {
    #[must_use]
    pub fn present(&self) -> Option<&ContainerObservation> {
        match self {
            Self::Present(observation) => Some(observation),
            Self::Absent { .. } => None,
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
pub struct ContainerRefusal {
    pub reason: ContainerRefusalReason,
    pub message: String,
    /// The capability a caller needs to make this request another way, such as
    /// the Ingress Doors route an older CLI's Caddy spec is refused for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub capability: Option<String>,
}

impl ContainerRefusal {
    /// The refusal as the error the replaced Container verbs answered.
    #[must_use]
    pub fn into_error(self) -> RpcError {
        let code = match self.reason {
            ContainerRefusalReason::NotFound => RpcErrorCode::NotFound,
            ContainerRefusalReason::NotParticipating
            | ContainerRefusalReason::SealingKeyMissing => RpcErrorCode::Unavailable,
            ContainerRefusalReason::InvalidIngress
            | ContainerRefusalReason::SealedValueUnopenable => RpcErrorCode::InvalidArgument,
            ContainerRefusalReason::NotEligible
            | ContainerRefusalReason::SlotOccupied
            | ContainerRefusalReason::VolumesNotPrepared
            | ContainerRefusalReason::LastIn
            | ContainerRefusalReason::OlderThanIn
            | ContainerRefusalReason::HeldByOtherRun
            | ContainerRefusalReason::RotationMismatch
            | ContainerRefusalReason::InRotation => RpcErrorCode::Conflict,
            ContainerRefusalReason::ImagePullFailed | ContainerRefusalReason::Unrecognized => {
                RpcErrorCode::Internal
            }
        };
        RpcError::new(code, self.message).with_details(self.capability.map_or(
            Value::Null,
            |capability| serde_json::json!({ "capability": capability }),
        ))
    }
}

/// Why a Machine would not meet a want. An unknown reason from a newer Machine
/// reads as `Unrecognized`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize, TS)]
#[serde(rename_all = "snake_case")]
pub enum ContainerRefusalReason {
    NotParticipating,
    NotEligible,
    InvalidIngress,
    SlotOccupied,
    ImagePullFailed,
    VolumesNotPrepared,
    NotFound,
    /// No other In copy of the Service is acked by every live proxy.
    LastIn,
    /// A newer copy of the Service is already In.
    OlderThanIn,
    HeldByOtherRun,
    /// The copy's rotation is not the one the want expected, or no move leads from it.
    RotationMismatch,
    /// The copy is routed, or held by a run that may route it again.
    InRotation,
    /// This Machine holds no Sealing Key to open the spec's Sealed values.
    SealingKeyMissing,
    /// A Sealed value is bound elsewhere or does not open with this Cluster's key.
    SealedValueUnopenable,
    #[serde(other)]
    Unrecognized,
}

/// Ask a Machine to apply `spec`'s CPU and memory limits to the running Service
/// Container `container_id` in place, and record `spec` as its resolved spec. The
/// Machine refuses any other difference between `spec` and the Container's spec.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateContainerLimitsRequest {
    pub container_id: ContainerId,
    pub spec: ResolvedServiceSpec,
}

/// The limits a Machine read back from Docker after an in-place update.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContainerLimitsUpdated {
    pub container_id: ContainerId,
    pub limits: ContainerLimits,
}

/// Why a Machine refused to change a Container's limits in place, before it
/// asked Docker for anything. Carried in `RpcError.details.reason`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LimitsRefusal {
    /// The spec names another Service than the Container runs.
    OtherService,
    /// The spec equals the Container's; there is nothing to apply.
    UpToDate,
    /// The change needs a new Container.
    NeedsRecreate,
    /// The Container is not running, so there is nothing serving to keep.
    NotRunning,
    /// The Container runs with more memory than the spec asks for.
    WouldLowerMemory,
    /// The Container runs with no memory limit, and the spec would set one.
    MemoryUnlimited,
}

impl Evidence for LimitsRefusal {
    const KEY: Option<&'static str> = Some("reason");
}

impl std::fmt::Display for LimitsRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::OtherService => "the spec names another Service",
            Self::UpToDate => "the spec is already current",
            Self::NeedsRecreate => "the change needs a new Container",
            Self::NotRunning => "the Container is not running",
            Self::WouldLowerMemory => "the Container runs with a higher memory limit",
            Self::MemoryUnlimited => "the Container runs without a memory limit",
        })
    }
}

/// Ask a Machine to record `spec` as the resolved spec of the running Service
/// Container `container_id`, which keeps running untouched. The Machine refuses
/// unless [`admit_placement`] admits `spec` over the Container's spec, and unless
/// the Machine itself satisfies that placement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct UpdateContainerPlacementRequest {
    pub container_id: ContainerId,
    pub spec: ResolvedServiceSpec,
}

/// The Container whose recorded spec now carries the requested placement.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ContainerPlacementUpdated {
    pub container_id: ContainerId,
}

/// Why a Machine refused to record a Container's new placement in place.
/// Carried in `RpcError.details.reason`.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlacementRefusal {
    /// The spec names another Service than the Container runs.
    OtherService,
    /// The spec differs from the Container's in more than placement.
    NeedsRecreate,
    /// The Container is not running, so there is nothing serving to keep.
    NotRunning,
}

impl Evidence for PlacementRefusal {
    const KEY: Option<&'static str> = Some("reason");
}

/// What a Machine does with a placement request it admits.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PlacementAdmission {
    /// The spec is the Container's under another placement, and the Machine records it.
    Record,
    /// The Container's spec already carries the placement, as after a resent request.
    Recorded,
}

/// Whether `spec` may take the place of `stored`, the spec of a running Service
/// Container: only when the two are equal apart from a placement, counts by region
/// included, that a Replicated Service can change in place.
///
/// A Deploy asks this before it plans the request and the Machine asks it again
/// on receipt, so a Deploy sends only what a Machine records.
///
/// # Errors
///
/// Refuses a spec of another Service, and one that differs in more than placement.
pub fn admit_placement(
    stored: &ResolvedServiceSpec,
    spec: &ResolvedServiceSpec,
) -> Result<PlacementAdmission, PlacementRefusal> {
    if stored.service_id != spec.service_id || stored.name != spec.name {
        return Err(PlacementRefusal::OtherService);
    }
    let placed = ResolvedServiceSpec {
        placement: spec.placement.clone(),
        regions: spec.regions.clone(),
        ..stored.clone()
    };
    match crate::compare_specs(stored, &spec.to_requested()) {
        crate::SpecChange::UpToDate if placed == *spec => Ok(PlacementAdmission::Recorded),
        crate::SpecChange::NeedsPlacementUpdate if placed == *spec => {
            Ok(PlacementAdmission::Record)
        }
        crate::SpecChange::UpToDate
        | crate::SpecChange::NeedsPlacementUpdate
        | crate::SpecChange::NeedsUpdate
        | crate::SpecChange::NeedsRecreate => Err(PlacementRefusal::NeedsRecreate),
    }
}

impl std::fmt::Display for PlacementRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::OtherService => "the spec names another Service",
            Self::NeedsRecreate => "the change needs a new Container",
            Self::NotRunning => "the Container is not running",
        })
    }
}

/// What a Machine saw when it answered `NotFound` for a Container.
/// Carried in `RpcError.details.reason`; an answer without it claims nothing.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerNotFoundReason {
    /// Docker answered an inspect of this Container ID, and the Machine holds no
    /// Resolved Service Spec under this ID.
    NoResolvedSpec,
}

impl Evidence for ContainerNotFoundReason {
    const KEY: Option<&'static str> = Some("reason");
}
