//! Napi package `@ployz/sdk`. Public payloads are generated from Rust.
//!
//! This crate is the workspace's only `unsafe_code` exception (napi-rs).

use napi_derive::napi;

/// npm package name. Connect, about, and close are later tickets.
#[must_use]
#[napi]
pub fn package_name() -> &'static str {
    "@ployz/sdk"
}
