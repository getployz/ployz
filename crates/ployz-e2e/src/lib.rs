#![forbid(unsafe_code)]

//! End-to-end harness crate for real substrate tests.
//!
//! Keep this crate thin. Product behavior should live in the normal crates;
//! this crate owns process/container wiring for tests that need real services.

pub mod dind;

// Re-exported so gated integration tests can drive the same Docker client the
// harness library uses without declaring a second bollard dependency edge.
pub use bollard;
