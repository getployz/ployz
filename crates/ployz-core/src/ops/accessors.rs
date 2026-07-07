use crate::subjects::OperationProgressScope;

use super::{EventSequence, OperationId, OperationKind, OperationStatus};

impl OperationStatus {
    #[must_use]
    pub const fn id(&self) -> &OperationId {
        match self {
            Self::Deploy { id, .. }
            | Self::Cert { id, .. }
            | Self::MachineAdd { id, .. }
            | Self::MachineUpdate { id, .. }
            | Self::MachineLifecycle { id, .. }
            | Self::CoreReplace { id, .. } => id,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        match self {
            Self::Deploy { .. } => OperationKind::Deploy,
            Self::Cert { .. } => OperationKind::Cert,
            Self::MachineAdd { .. } => OperationKind::MachineAdd,
            Self::MachineUpdate { .. } => OperationKind::MachineUpdate,
            Self::MachineLifecycle { .. } => OperationKind::MachineLifecycle,
            Self::CoreReplace { .. } => OperationKind::CoreReplace,
        }
    }

    #[must_use]
    pub fn progress_scope(&self) -> OperationProgressScope {
        match self {
            Self::Deploy { namespace_id, .. } => OperationProgressScope::Namespace {
                namespace_id: namespace_id.clone(),
            },
            Self::Cert { .. } => OperationProgressScope::Cluster,
            Self::MachineAdd { machine_id, .. }
            | Self::MachineUpdate { machine_id, .. }
            | Self::MachineLifecycle { machine_id, .. }
            | Self::CoreReplace { machine_id, .. } => OperationProgressScope::Machine {
                machine_id: machine_id.clone(),
            },
        }
    }

    #[must_use]
    pub const fn last_event_sequence(&self) -> EventSequence {
        match self {
            Self::Deploy {
                last_event_sequence,
                ..
            }
            | Self::Cert {
                last_event_sequence,
                ..
            }
            | Self::MachineAdd {
                last_event_sequence,
                ..
            }
            | Self::MachineLifecycle {
                last_event_sequence,
                ..
            }
            | Self::CoreReplace {
                last_event_sequence,
                ..
            }
            | Self::MachineUpdate {
                last_event_sequence,
                ..
            } => *last_event_sequence,
        }
    }

    /// The sequence the next recorded event for this operation must carry.
    #[must_use]
    pub fn next_event_sequence(&self) -> EventSequence {
        EventSequence::try_new(self.last_event_sequence().get() + 1)
            .expect("next event sequence stays greater than zero")
    }
}
