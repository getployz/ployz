#![forbid(unsafe_code)]

//! Ployz daemon process wiring.
//!
//! The daemon owns lifecycle, configuration, service registration,
//! controllers, machine-local services, and runtime adapters. Product policy stays
//! in `ployz-core`; NATS mechanics stay in `ployz-nats`.

pub mod api_runtime;
pub mod backup_adapters;
pub mod backup_restore;
pub mod backup_runtime;
pub mod config;
pub mod control_runtime;
pub mod controllers;
pub mod daemon_runtime;
pub mod dataplane_runtime;
pub mod deploy_runtime;
pub mod deploy_worker;
pub mod dns;
pub mod dns_process_runtime;
pub mod dns_source;
pub mod docker;
pub mod gateway;
pub mod gateway_pingora;
pub mod gateway_process_runtime;
pub mod gateway_runtime;
pub mod gateway_source;
pub mod machine_credentials;
pub mod machine_runtime;
pub mod nats_authorization;
pub mod nats_process;
pub mod operation_api;
pub mod process_support;
pub mod role;
pub mod services;
pub mod tasks;
