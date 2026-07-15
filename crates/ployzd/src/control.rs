//! Control role ownership boundary.
//!
//! Durable operator intent, mortal operation evidence, passive projections,
//! Control process wiring, and NATS authorization coordination live here as
//! separately named responsibilities. Sharing the boundary does not merge
//! their truth or recovery semantics.

pub mod authorization;
pub mod intent;
pub mod operation_evidence;
pub mod operations;
pub mod operator_api;
pub mod process;
pub mod projection;
pub mod reconciler;
pub mod role_client;
pub mod sequencer;
pub mod store;
