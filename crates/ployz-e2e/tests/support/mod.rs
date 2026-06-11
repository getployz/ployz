//! Shared support for the e2e integration test binaries. Each binary
//! compiles this module separately and uses a subset of it, so per-binary
//! dead-code analysis is noise here.
#![allow(dead_code)]

pub mod dind;
pub mod http;
pub mod ids;
pub mod nats;
