//! Typed-id constructors for test literals: panic loudly when a test
//! literal does not satisfy the id's invariants.

use ployz_core::ids::{
    CertId, ContainerId, NodeId, OperationId, OperationOwnerId, RevisionId, ServiceId, StepId,
};
use ployz_core::machine::{JoinTokenExpiresAt, JoinTokenRedeemedAt, MachineName, RawJoinToken};
use ployz_core::ops::{
    CancellationReason, EventSequence, FailureMessage, OperationEventReplayLimit,
    OperationIdempotencyKey, OperationLeaseExpiresAt, RouteHostname, RoutePort,
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
pub fn owner_id(value: &str) -> OperationOwnerId {
    OperationOwnerId::try_new(value).expect("valid operation owner id")
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
pub fn container_id(value: &str) -> ContainerId {
    ContainerId::try_new(value).expect("valid container id")
}

#[must_use]
pub fn step_id(value: &str) -> StepId {
    StepId::try_new(value).expect("valid step id")
}

#[must_use]
pub fn cert_id(value: &str) -> CertId {
    CertId::try_new(value).expect("valid cert id")
}

#[must_use]
pub fn machine_name(value: &str) -> MachineName {
    MachineName::try_new(value).expect("valid machine name")
}

#[must_use]
pub fn raw_join_token(value: &str) -> RawJoinToken {
    RawJoinToken::try_new(value).expect("valid raw join token")
}

#[must_use]
pub fn join_token_expires_at(value: u64) -> JoinTokenExpiresAt {
    JoinTokenExpiresAt::try_new(value).expect("valid join token expiry")
}

#[must_use]
pub fn join_token_redeemed_at(value: u64) -> JoinTokenRedeemedAt {
    JoinTokenRedeemedAt::try_new(value).expect("valid join time")
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

#[must_use]
pub fn lease_time(value: u64) -> OperationLeaseExpiresAt {
    OperationLeaseExpiresAt::try_new(value).expect("valid lease time")
}

#[must_use]
pub fn failure_message(value: &str) -> FailureMessage {
    FailureMessage::try_new(value).expect("valid failure message")
}

#[must_use]
pub fn cancellation_reason(value: &str) -> CancellationReason {
    CancellationReason::try_new(value).expect("valid cancellation reason")
}
