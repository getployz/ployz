mod actor;
mod bridge;
mod error;
mod facts;
mod grants;
mod memory;
mod message;
mod subject;

pub use actor::BusActorHandle;
pub use bridge::{
    BridgeEndpoint, BridgeOrigin, BridgeRuleId, BridgeRuleViolation, BridgeState, ServiceImport,
    StreamImport, SubjectTransform,
};
pub use error::{ActorFailure, BridgeFailure, BusError, HandlerFailure, Result, SubjectParseError};
pub use facts::{
    Fact, FactContentHash, FactKey, FactKeyParseError, FactKeyPattern, FactPayload,
    FactWriteOutcome,
};
pub use grants::{BusSession, FactAccess, FactAuthorizer, Grant, IslandId, PrincipalId, QueueName};
pub use memory::{
    BusAuthority, BusRuntimeConfig, BusRuntimeSnapshot, HandlerOutcome, RequestContext,
};
pub(crate) use message::ResponseEnvelope;
pub use message::{BusMessage, Payload, RequestManyPolicy, RequestTarget, ResponseMessage};
pub use subject::{Subject, SubjectPattern};

pub mod harness {
    use crate::{BusActorHandle, BusAuthority};

    pub use crate::memory::MemoryBus as InMemoryBus;

    #[must_use]
    pub fn actor_with_authority() -> (BusActorHandle, BusAuthority, InMemoryBus) {
        let (bus, authority) = InMemoryBus::new_with_authority();
        let handle = BusActorHandle::spawn(bus.clone());
        (handle, authority, bus)
    }
}
