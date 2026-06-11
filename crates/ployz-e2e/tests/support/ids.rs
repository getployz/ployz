//! Typed-id constructors shared by the e2e test binaries: panic loudly when a
//! test literal does not satisfy the id's invariants.

use ployz_core::ids::{NodeId, OperationId, RevisionId, ServiceId};
use ployz_core::ops::{
    EventSequence, OperationEventReplayLimit, OperationIdempotencyKey, RouteHostname, RoutePort,
};

#[must_use]
pub fn node_id(value: &str) -> NodeId {
    NodeId::try_new(value).expect("valid node id")
}

#[must_use]
pub fn operation_id(value: &str) -> OperationId {
    OperationId::try_new(value).expect("valid operation id")
}

#[must_use]
pub fn idempotency_key(value: &str) -> OperationIdempotencyKey {
    OperationIdempotencyKey::try_new(value).expect("valid idempotency key")
}

#[must_use]
pub fn service_id(value: &str) -> ServiceId {
    ServiceId::try_new(value).expect("valid service id")
}

#[must_use]
pub fn revision_id(value: &str) -> RevisionId {
    RevisionId::try_new(value).expect("valid revision id")
}

#[must_use]
pub fn route_hostname(value: &str) -> RouteHostname {
    RouteHostname::try_new(value).expect("valid route hostname")
}

#[must_use]
pub fn route_port(value: u16) -> RoutePort {
    RoutePort::try_new(value).expect("valid route port")
}

#[must_use]
pub fn event_sequence(value: u64) -> EventSequence {
    EventSequence::try_new(value).expect("valid event sequence")
}

#[must_use]
pub fn event_replay_limit(value: u16) -> OperationEventReplayLimit {
    OperationEventReplayLimit::try_new(value).expect("valid event replay limit")
}
