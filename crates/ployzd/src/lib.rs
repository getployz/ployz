#![forbid(unsafe_code)]

//! Ployz daemon process wiring.
//!
//! The daemon owns lifecycle, configuration, service registration,
//! controllers, machine-local services, and integration adapters. Product policy stays
//! in `ployz-core`; NATS mechanics stay in `ployz-nats`.

pub mod adapters {
    pub(crate) mod atomic_file;
    pub mod credentials;
    pub mod nats_authorization;
    pub mod nats_server;
}
pub mod certificate;
pub mod config;
pub mod control;
pub mod recovery;
pub mod role_testimony;
pub mod roles {
    pub mod control;
    pub mod dns {
        mod internal;
        pub use internal::InternalResolverHealth;
        pub mod process;
        pub(crate) mod protocol;
        pub(crate) mod service;
    }
    pub mod gateway {
        pub mod client;
        pub mod pingora;
        pub mod process;
        pub mod projection;
        pub(crate) mod protocol;
        pub mod route_table;
        pub mod source;
    }
    pub mod machine;
    pub mod nats_failover;
}
/// Compatibility facade for the Control-owned SQLite store.
pub mod core_store;
pub mod dispatch;
/// Compatibility facade for the daemon-local role testimony cache.
pub mod fact_cache;
/// Compatibility facade for the Control-owned Ingress Endpoint Projection.
pub mod ingress_endpoint;
/// Compatibility facade for Control-owned durable operator intent.
pub mod intent;
pub mod lease;
/// Compatibility facade for the Control-owned operator API.
pub mod operation_api;
/// Compatibility facade for Control-owned operation execution.
pub mod operations;
pub mod process_support;
pub mod role_cli;
pub mod seed;
pub mod service_catalog;
pub mod tasks;
