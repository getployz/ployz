//! Network domain contracts: dataplane, reachability, repair, status, and
//! ordinary internal service DNS.

pub mod dataplane;
pub mod internal_dns;
pub mod reachability;
pub mod repair;
pub mod status;

pub use dataplane::*;
pub use internal_dns::*;
pub use reachability::*;
pub use repair::*;
pub use status::*;
