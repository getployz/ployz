#![forbid(unsafe_code)]

//! NATS adapters.
//!
//! This crate owns the concrete NATS boundary: connection setup, services,
//! and typed clients. Product policy belongs in `ployz-core`; process wiring
//! belongs in `ployzd`.

pub mod connect;
pub mod operation_api_client;
pub mod operation_event_subject;
pub mod permissions;
pub mod server_config;
pub mod service_protocol;
pub mod service_runtime;
pub mod services;
pub mod subjects;
pub mod typescript;
