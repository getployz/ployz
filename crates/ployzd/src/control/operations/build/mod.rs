//! Control-owned build placement and operation execution.

mod active_registry;
mod driver;
mod log_stream;
mod placement;
mod platform_session;

pub(crate) use driver::{BuildCancelDisposition, BuildOperationDriver};
pub(crate) use placement::place_build_platforms;
