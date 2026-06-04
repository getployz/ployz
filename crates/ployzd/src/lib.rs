#![forbid(unsafe_code)]

//! Ployz daemon process wiring.
//!
//! The daemon owns lifecycle, configuration, service registration,
//! controllers, node-local services, and runtime adapters. Product policy stays
//! in `ployz-core`; NATS mechanics stay in `ployz-nats`; iroh byte transport
//! stays in `ployz-transport`.

pub mod app;
pub mod config;
pub mod controllers;
pub mod docker;
pub mod gateway;
pub mod iroh_tunnel;
pub mod nats_process;
pub mod node_agent;
pub mod services;
