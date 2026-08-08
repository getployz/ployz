//! Small value types shared by otherwise independent product domains.

mod routes;
mod text;

pub use routes::{RouteHostname, RouteHostnameError, RoutePort, RoutePortError, RouteTarget};
pub use text::{CancellationReason, FailureMessage, NonEmptyTextError};
