use super::{EventSequence, OperationId, OperationKind, OperationStatus};

impl OperationStatus {
    #[must_use]
    pub const fn id(&self) -> &OperationId {
        match self {
            Self::Deploy { id, .. } | Self::Cert { id, .. } | Self::MachineAdd { id, .. } => id,
        }
    }

    #[must_use]
    pub const fn kind(&self) -> OperationKind {
        match self {
            Self::Deploy { .. } => OperationKind::Deploy,
            Self::Cert { .. } => OperationKind::Cert,
            Self::MachineAdd { .. } => OperationKind::MachineAdd,
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
            } => *last_event_sequence,
        }
    }
}
