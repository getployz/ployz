//! Network domain contracts for the V2 mesh and internal service DNS.

pub mod dataplane;
pub mod internal_dns;
pub mod status;

pub use dataplane::*;
pub use internal_dns::*;
pub use status::*;
