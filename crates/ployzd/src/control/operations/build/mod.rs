//! Control-owned build placement and operation execution.

mod active_registry;
mod driver;
mod executor_session;
mod log_stream;
mod placement;
mod platform_session;

pub(crate) use driver::{BuildCancelDisposition, BuildOperationDriver};
