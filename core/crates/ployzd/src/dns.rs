//! Observer-local Internal DNS serving and answer projection.

use std::time::Duration;

mod forward;
mod listeners;
mod process;
mod projection;
mod query;
mod service;
mod spec;

pub use process::{ABDICATED, DnsExit, serve};
pub use service::DnsService;

pub const PORT: u16 = 53;
const TCP_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const TCP_RESPONSE_BUFFER: usize = 32;

#[cfg(test)]
mod tests;
