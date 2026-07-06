#![forbid(unsafe_code)]

//! Ployz daemon process wiring.
//!
//! The daemon owns lifecycle, configuration, service registration,
//! controllers, machine-local services, and runtime adapters. Product policy stays
//! in `ployz-core`; NATS mechanics stay in `ployz-nats`.

pub mod adapters {
    pub(crate) mod atomic_file;
    pub mod credentials;
    pub mod docker;
    pub mod host_dataplane;
    pub mod nats_authorization;
    pub mod nats_server;
}
pub mod api_runtime;
pub mod config;
pub mod controllers;
pub mod roles {
    pub mod control;
    pub mod dns {
        pub mod process;
        pub mod projection;
        pub mod source;
    }
    pub mod gateway {
        pub mod pingora;
        pub mod process;
        pub mod projection;
        pub mod route_table;
        pub mod source;
    }
    pub mod machine;
}
pub mod operations {
    pub mod deploy;
    pub mod log;
    pub mod machine_lifecycle;
    pub mod machine_update;
}
pub mod core_store;
pub mod dispatch;
pub mod intent;
pub mod machine_roster;
pub mod namespace_intent;
pub mod operation_api;
pub mod process_support;
pub mod role_cli;
pub mod fact_cache;
pub mod service_catalog;
pub mod tasks;
