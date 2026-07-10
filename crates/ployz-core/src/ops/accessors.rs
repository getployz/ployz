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
            | Self::CoreReplace { id, .. }
            | Self::NetworkRepair { id, .. }
            | Self::ServiceRestart { id, .. }
            | Self::ManagedLease { id, .. }
            | Self::NamespaceRemove { id, .. } => id,
            Self::VolumeRemove { id, .. } => id,
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
            Self::NetworkRepair { .. } => OperationKind::NetworkRepair,
            Self::ServiceRestart { .. } => OperationKind::ServiceRestart,
            Self::ManagedLease { .. } => OperationKind::ManagedLease,
            Self::NamespaceRemove { .. } => OperationKind::NamespaceRemove,
            Self::VolumeRemove { .. } => OperationKind::VolumeRemove,
        }
    }

    #[must_use]
    pub fn progress_scope(&self) -> OperationProgressScope {
        match self {
            Self::Deploy { namespace_id, .. } => OperationProgressScope::Namespace {
                namespace_id: namespace_id.clone(),
            },
            Self::ServiceRestart { namespace_id, .. }
            | Self::NamespaceRemove { namespace_id, .. } => OperationProgressScope::Namespace {
                namespace_id: namespace_id.clone(),
            },
            Self::VolumeRemove { namespace_id, .. } => OperationProgressScope::Namespace {
                namespace_id: namespace_id.clone(),
            },
            Self::Cert { .. } | Self::NetworkRepair { .. } | Self::ManagedLease { .. } => {
                OperationProgressScope::Cluster
            }
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
            | Self::NetworkRepair {
                last_event_sequence,
                ..
            }
            | Self::ServiceRestart {
                last_event_sequence,
                ..
            }
            | Self::ManagedLease {
                last_event_sequence,
                ..
            }
            | Self::NamespaceRemove {
                last_event_sequence,
                ..
            }
            | Self::MachineUpdate {
                last_event_sequence,
                ..
            }
            | Self::VolumeRemove {
                last_event_sequence,
                ..
            } => *last_event_sequence,
        }
    }

    /// The sequence the next recorded event for this operation must carry.
    pub fn next_event_sequence(&self) -> Result<EventSequence, NextEventSequenceError> {
        let last_event_sequence = self.last_event_sequence();
        let Some(next) = last_event_sequence.get().checked_add(1) else {
            return Err(NextEventSequenceError::Overflow {
                last_event_sequence,
            });
        };
        EventSequence::try_new(next).map_err(|_| NextEventSequenceError::Overflow {
            last_event_sequence,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NextEventSequenceError {
    #[error("operation event sequence overflowed after {}", .last_event_sequence.get())]
    Overflow { last_event_sequence: EventSequence },
}
