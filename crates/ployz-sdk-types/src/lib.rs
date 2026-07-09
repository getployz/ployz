#![forbid(unsafe_code)]

//! Public schema and type export surface for SDK consumers.
//!
//! This crate should expose generated or generation-ready wire types. It should
//! not contain orchestration logic.
//!
//! Error evidence carries two shapes on purpose: `Unavailable { message }`
//! variants hold a plain rendered `String` — transient plumbing evidence for a
//! reader, never dispatched on — while [`FailureMessage`] fields belong to
//! durable failure records on operations, the typed audience of the Operation
//! Rules. A new error field takes `String` unless it lands on an operation
//! record.

mod cloud_bootstrap;
mod core_replace;
mod core_types;
mod deploy;
mod logs;
mod machine;
mod namespace;
pub mod operation_api;
mod ops;
mod runtime;
mod service;
pub mod typescript;

pub use cloud_bootstrap::*;
pub use core_replace::*;
pub use core_types::*;
pub use deploy::*;
pub use logs::*;
pub use machine::*;
pub use namespace::*;
pub use ops::*;
pub use runtime::*;
pub use service::*;
