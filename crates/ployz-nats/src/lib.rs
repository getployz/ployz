#![forbid(unsafe_code)]

//! NATS and JetStream adapters.
//!
//! This crate owns the concrete NATS boundary: connection setup, resource
//! bootstrap, KV, streams, Object Store, services, schedules, and permission
//! rendering. Product policy belongs in `ployz-core`; process wiring belongs
//! in `ployzd`.

pub mod bootstrap;
pub mod connect;
pub mod kv;
pub mod objects;
pub mod permissions;
pub mod replication;
pub mod schedules;
pub mod services;
pub mod streams;
