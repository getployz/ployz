//! Linux daemon runtime for one Ployz Machine.

/// Numeric status returned by the Ployz Docker Volume plugin.
#[doc(hidden)]
#[derive(serde::Deserialize, serde::Serialize)]
pub struct VolumePluginStatus {
    /// Current ZFS dataset byte bound.
    pub bound_bytes: u64,
    /// Current referenced ZFS dataset bytes.
    pub used_bytes: u64,
}

mod docker_image;

pub(crate) mod certificates;
pub mod corrosion;
pub mod daemon;
pub mod diag;
pub mod dns;
pub mod docker;
#[doc(hidden)]
pub mod filesystem;
mod global_reconcile;
mod host_capacity;
mod hosted_dns;
pub(crate) mod ingress;
pub mod logs;
pub mod machine;
pub mod machine_api;
#[doc(hidden)]
pub mod machine_pool;
pub mod metrics;
pub mod network;
pub mod relay;
pub(crate) mod runtime_watch;
