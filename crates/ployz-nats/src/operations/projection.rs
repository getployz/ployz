use ployz_core::ids::OperationId;
use ployz_core::ops::{EventSequence, OperationStatus};

pub(super) fn status_id(status: &OperationStatus) -> &OperationId {
    match status {
        OperationStatus::Deploy { id, .. } => id,
    }
}

pub(super) fn status_sequence(status: &OperationStatus) -> EventSequence {
    match status {
        OperationStatus::Deploy {
            last_event_sequence,
            ..
        } => *last_event_sequence,
    }
}

pub(super) fn next_event_sequence(status: &OperationStatus) -> EventSequence {
    EventSequence::try_new(status_sequence(status).get() + 1)
        .expect("next event sequence stays greater than zero")
}
