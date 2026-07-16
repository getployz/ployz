#![forbid(unsafe_code)]

//! Client-facing helpers for `ployz`.

pub mod api_client;
pub mod build;
pub mod certificate;
pub mod commands;
mod confirmation;
pub mod core;
pub mod deploy;
pub mod dispatcher;
mod execution_error;
mod execution_support;
pub mod ingress;
pub mod logs;
pub mod machine;
pub mod namespace;
pub mod network;
pub mod operation;
pub mod role_policy;
pub mod service;
mod shell;
pub mod ssh;
pub mod volume;
