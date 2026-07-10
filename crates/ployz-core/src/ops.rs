//! Operation models, sliced by operation kind: each kind module owns its
//! states, failures, transitions, and status projection. This spine owns
//! what spans kinds — `OperationKind`, `OperationStatus`, the event stream
//! shape, sequences, and the projection dispatcher — and re-exports every
//! kind's public items at this path.

use serde::{Deserialize, Serialize};

use crate::ids::{
    CertId, MachineId, NamespaceId, OperationId, ServiceId, SubjectToken, SubjectTokenError,
};
use crate::install::InstallArtifactVersion;
use crate::machine::{IssuedJoinToken, MachineName};
use crate::roles::InstallRolePolicy;
use crate::state::MachineLifecycle;
use crate::wire::{positive_u64_wire_error, positive_u64_wire_newtype};

mod accessors;
mod cert;
mod core_replace;
mod deploy;
mod event_subjects;
mod events;
mod machine_add;
mod machine_lifecycle;
mod machine_update;
mod namespace_remove;
mod projection;
mod replay;
mod routes;
mod service_restart;
mod text;
mod volume_remove;

pub use accessors::NextEventSequenceError;
pub use cert::{CertOperationFailure, CertOperationState, CertRunningStage, CertTransition};
pub use core_replace::{CoreReplaceFailure, CoreReplaceOperationState, CoreReplaceTransition};
pub use deploy::{
    ArtifactUnavailableReason, ControlPlaneCommitScope, DeployCleanupFailure,
    DeployCompletionOutcome, DeployEvidence, DeployFailureClass, DeployOperationFailure,
    DeployOperationState, DeployRunningStage, DeployTransition, HealthCheckFailure,
    RetainedArtifact, RouteCutoverFailureReason, UnusableMachine, project_deploy_transition,
    validate_fresh_deploy_evidence,
};
pub use events::{OperationEvent, OperationSubject, OperationSubjectRef};
pub use machine_add::{MachineAddOperationState, MachineAddOperationStateName};
pub use machine_lifecycle::{
    MachineLifecycleFailure, MachineLifecycleOperationState, MachineLifecycleTransition,
};
pub use machine_update::{
    MachineSubstrateVersions, MachineUpdateFailure, MachineUpdateOperationState,
    MachineUpdateTransition,
};
pub use namespace_remove::{
    NamespaceRemoveFailure, NamespaceRemoveOperationState, NamespaceRemoveRunningStage,
    NamespaceRemoveTransition, project_namespace_remove_transition,
};
pub use projection::{
    OperationProjection, ProjectionOperationState, StatusProjectionError, project_operation_event,
};
pub use replay::{
    OperationEventReplayCursor, OperationEventReplayLimit, OperationEventReplayLimitError,
    OperationEventReplayPage, OperationEventReplayRequest, ReplayedOperationEvent,
};
pub use routes::{RouteHostname, RouteHostnameError, RoutePort, RoutePortError, RouteTarget};
pub use service_restart::{
    ServiceRestartFailure, ServiceRestartOperationState, ServiceRestartRunningStage,
    ServiceRestartTransition, project_service_restart_transition,
};
pub use text::{CancellationReason, FailureMessage, NonEmptyTextError, OperatorHint};
pub use volume_remove::{
    VolumeRemoveFailure, VolumeRemoveOperationState, VolumeRemoveRunningStage,
    VolumeRemoveTransition, project_volume_remove_transition,
};

pub const MAX_OPERATION_EVENT_REPLAY_LIMIT: u16 = 512;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Deploy,
    Cert,
    MachineAdd,
    MachineUpdate,
    MachineLifecycle,
    CoreReplace,
    ServiceRestart,
    NamespaceRemove,
    VolumeRemove,
}

/// Operation status projection rebuilt from local operation evidence.
///
/// Changing this shape intentionally breaks operation status recovery unless
/// paired with evidence cleanup or migration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum OperationStatus {
    Deploy {
        id: OperationId,
        namespace_id: NamespaceId,
        service_id: ServiceId,
        state: DeployOperationState,
        last_event_sequence: EventSequence,
    },
    Cert {
        id: OperationId,
        cert_id: CertId,
        state: CertOperationState,
        last_event_sequence: EventSequence,
    },
    MachineAdd {
        id: OperationId,
        machine_id: MachineId,
        name: MachineName,
        roles: InstallRolePolicy,
        state: MachineAddOperationState,
        last_event_sequence: EventSequence,
    },
    MachineUpdate {
        id: OperationId,
        machine_id: MachineId,
        target_version: InstallArtifactVersion,
        state: MachineUpdateOperationState,
        last_event_sequence: EventSequence,
    },
    MachineLifecycle {
        id: OperationId,
        machine_id: MachineId,
        target: MachineLifecycle,
        state: MachineLifecycleOperationState,
        last_event_sequence: EventSequence,
    },
    CoreReplace {
        id: OperationId,
        machine_id: MachineId,
        successor_nats_url: crate::install::MachineJoinRuntimeNatsUrl,
        state: CoreReplaceOperationState,
        last_event_sequence: EventSequence,
    },
    ServiceRestart {
        id: OperationId,
        namespace_id: NamespaceId,
        service_id: ServiceId,
        state: ServiceRestartOperationState,
        last_event_sequence: EventSequence,
    },
    NamespaceRemove {
        id: OperationId,
        namespace_id: NamespaceId,
        state: NamespaceRemoveOperationState,
        last_event_sequence: EventSequence,
    },
    VolumeRemove {
        id: OperationId,
        namespace_id: NamespaceId,
        volume_name: crate::deploy::VolumeName,
        state: VolumeRemoveOperationState,
        last_event_sequence: EventSequence,
    },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[serde(deny_unknown_fields)]
pub struct OperationStatusSnapshot {
    pub status: OperationStatus,
}

impl OperationStatusSnapshot {
    #[must_use]
    pub fn new(status: OperationStatus) -> Self {
        Self { status }
    }
}

impl OperationStatus {
    #[must_use]
    pub fn deploy_accepted(
        id: OperationId,
        namespace_id: NamespaceId,
        service_id: ServiceId,
        event_sequence: EventSequence,
    ) -> Self {
        Self::Deploy {
            id,
            namespace_id,
            service_id,
            state: DeployOperationState::Accepted,
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub fn cert_accepted(id: OperationId, cert_id: CertId, event_sequence: EventSequence) -> Self {
        Self::Cert {
            id,
            cert_id,
            state: CertOperationState::Accepted,
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub fn machine_add_pending(
        id: OperationId,
        machine_id: MachineId,
        name: MachineName,
        roles: InstallRolePolicy,
        join_token: IssuedJoinToken,
        event_sequence: EventSequence,
    ) -> Self {
        Self::MachineAdd {
            id,
            machine_id,
            name,
            roles,
            state: MachineAddOperationState::Pending { join_token },
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub fn machine_update_accepted(
        id: OperationId,
        machine_id: MachineId,
        target_version: InstallArtifactVersion,
        event_sequence: EventSequence,
    ) -> Self {
        Self::MachineUpdate {
            id,
            machine_id,
            target_version,
            state: MachineUpdateOperationState::Accepted,
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub fn machine_lifecycle_accepted(
        id: OperationId,
        machine_id: MachineId,
        target: MachineLifecycle,
        event_sequence: EventSequence,
    ) -> Self {
        Self::MachineLifecycle {
            id,
            machine_id,
            target,
            state: MachineLifecycleOperationState::Accepted,
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub fn core_replace_accepted(
        id: OperationId,
        machine_id: MachineId,
        successor_nats_url: crate::install::MachineJoinRuntimeNatsUrl,
        event_sequence: EventSequence,
    ) -> Self {
        Self::CoreReplace {
            id,
            machine_id,
            successor_nats_url,
            state: CoreReplaceOperationState::Accepted,
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub fn service_restart_accepted(
        id: OperationId,
        namespace_id: NamespaceId,
        service_id: ServiceId,
        event_sequence: EventSequence,
    ) -> Self {
        Self::ServiceRestart {
            id,
            namespace_id,
            service_id,
            state: ServiceRestartOperationState::Accepted,
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub fn namespace_remove_accepted(
        id: OperationId,
        namespace_id: NamespaceId,
        event_sequence: EventSequence,
    ) -> Self {
        Self::NamespaceRemove {
            id,
            namespace_id,
            state: NamespaceRemoveOperationState::Accepted,
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub fn volume_remove_accepted(
        id: OperationId,
        namespace_id: NamespaceId,
        volume_name: crate::deploy::VolumeName,
        event_sequence: EventSequence,
    ) -> Self {
        Self::VolumeRemove {
            id,
            namespace_id,
            volume_name,
            state: VolumeRemoveOperationState::Accepted,
            last_event_sequence: event_sequence,
        }
    }

    #[must_use]
    pub const fn is_terminal(&self) -> bool {
        match self {
            Self::Deploy { state, .. } => state.is_terminal(),
            Self::Cert { state, .. } => state.is_terminal(),
            Self::MachineAdd { state, .. } => state.is_terminal(),
            Self::MachineUpdate { state, .. } => state.is_terminal(),
            Self::MachineLifecycle { state, .. } => state.is_terminal(),
            Self::CoreReplace { state, .. } => state.is_terminal(),
            Self::ServiceRestart { state, .. } => state.is_terminal(),
            Self::NamespaceRemove { state, .. } => state.is_terminal(),
            Self::VolumeRemove { state, .. } => state.is_terminal(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[cfg_attr(feature = "typescript", derive(ts_rs::TS))]
#[cfg_attr(
    feature = "typescript",
    ts(type = "Brand<string, \"OperationIdempotencyKey\">")
)]
#[serde(transparent)]
pub struct OperationIdempotencyKey(SubjectToken);

impl OperationIdempotencyKey {
    pub fn try_new(value: impl Into<String>) -> Result<Self, SubjectTokenError> {
        Ok(Self(SubjectToken::try_new(value)?))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        self.0.as_str()
    }
}

positive_u64_wire_newtype! {
    pub struct EventSequence;
    ts_brand: "Brand<string, \"EventSequence\">";
    accessor: get;
    error: EventSequenceError;
}

positive_u64_wire_error! {
    pub enum EventSequenceError;
    noun: "event sequence";
}
