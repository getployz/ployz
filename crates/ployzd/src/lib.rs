#![forbid(unsafe_code)]

//! Ployz daemon process wiring.
//!
//! The daemon owns lifecycle, configuration, service registration,
//! controllers, node-local services, and runtime adapters. Product policy stays
//! in `ployz-core`; NATS mechanics stay in `ployz-nats`; iroh byte transport
//! stays in `ployz-transport`.

pub mod api_runtime;
pub mod app;
pub mod config;
pub mod control_runtime;
pub mod controllers;
pub mod daemon_runtime;
pub mod dataplane_runtime;
pub mod deploy_launcher;
pub mod deploy_runtime;
pub mod deploy_worker;
pub mod dns;
pub mod docker;
pub mod gateway;
pub mod gateway_process_runtime;
pub mod gateway_runtime;
pub mod gateway_source;
pub mod iroh_tunnel;
pub mod nats_process;
pub mod node_agent;
pub mod node_process_runtime;
pub mod node_protocol;
pub mod node_rpc;
pub mod node_runtime;
pub mod node_runtime_types;
pub mod node_service_runtime;
pub mod operation_api;
pub mod operation_lease;
pub mod projection;
pub mod role;
pub mod services;
