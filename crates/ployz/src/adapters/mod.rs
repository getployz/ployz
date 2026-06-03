//! Adapter modules that may depend on Polis or other substrate crates.

#[cfg(any(test, feature = "test-support"))]
pub mod memory;
pub mod polis;
