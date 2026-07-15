#![forbid(unsafe_code)]

//! Ployz daemon process wiring.
//!
//! The daemon owns lifecycle, configuration, service registration,
//! controllers, machine-local services, and integration adapters. Product policy stays
//! in `ployz-core`; NATS mechanics stay in `ployz-nats`.

mod adapters {
    pub(crate) mod atomic_file;
    pub mod credentials;
    pub mod nats_server;
}
mod certificate;
pub mod config;
mod control;
mod recovery;
mod role_testimony;
mod roles {
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
}
pub mod dispatch;
mod lease;
mod process_support;
pub mod role_cli;
mod seed;
mod service_catalog;
mod tasks;

#[cfg(test)]
mod tests;
