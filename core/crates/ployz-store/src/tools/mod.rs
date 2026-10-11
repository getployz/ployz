//! Pure building blocks for the tools an agent reads the cluster through. No IO.

pub mod logs;
pub mod status;

pub use logs::{LineCap, LogExcerpt, LogText, SecretSet, Withheld};
pub use status::{ServiceStatus, service_status};
