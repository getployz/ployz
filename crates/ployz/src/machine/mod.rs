//! Machine installation, Bootstrap Delivery, Operator Context, and lifecycle CLI behavior.
//!
//! Founder Bootstrap, Joiner Bootstrap, Machine Join Redemption, and Machine Join Report keep
//! separate typed contracts. Delivery never grants their authority: Host Runner owns local
//! mutation, while product lifecycle commands remain explicit NATS operations.

pub mod bootstrap;
pub mod command;
pub mod founder;
pub mod local_release;
pub mod operator_context;
pub(crate) mod runtime;
pub mod ssh;
