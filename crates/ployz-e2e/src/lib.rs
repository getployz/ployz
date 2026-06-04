#![forbid(unsafe_code)]

//! End-to-end harness crate for real substrate tests.
//!
//! Keep this crate thin. Product behavior should live in the normal crates;
//! this crate owns process/container wiring for tests that need real services.
