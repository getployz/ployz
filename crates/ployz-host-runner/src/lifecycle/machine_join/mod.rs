//! Machine Join Redemption, local application, and later Report evidence.

pub(crate) mod client;
pub(crate) mod execution;
mod material;
mod template_compatibility;

pub use material::*;
pub(super) use template_compatibility::promote_machine_join_template_release;
