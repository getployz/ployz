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
    Fact, FactContentHash, FactKey, FactKeyParseError, FactKeyPattern, FactWriteOutcome,
};
pub use grants::{BusSession, Grant, IslandId, PrincipalId, QueueName};
pub use memory::{
    BusAuthority, BusRuntimeConfig, BusRuntimeSnapshot, HandlerOutcome, RequestContext,
};
pub(crate) use message::ResponseEnvelope;
pub use message::{BusMessage, Payload, RequestManyPolicy, RequestTarget, ResponseMessage};
pub use subject::{Subject, SubjectPattern};

pub mod harness {
    pub use crate::memory::MemoryBus as InMemoryBus;
}
