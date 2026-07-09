//! Core-owned operator intent: the durable operator decisions (roster, route
//! and serving bindings) and the `intent.get` service that projects them.

pub mod lease_intent;
pub mod machine_roster;
pub mod namespace_intent;
pub mod nats_authorizations;
pub mod service;
