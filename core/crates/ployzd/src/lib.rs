//! Linux daemon runtime for one Ployz Machine.

mod build;

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
pub(crate) mod filesystem;
mod host_capacity;
pub(crate) mod ingress;
pub mod installer;
pub mod logs;
pub mod machine;
pub mod machine_api;
pub(crate) mod machine_api_socket;
#[doc(hidden)]
pub mod machine_pool;
pub mod management;
pub mod mutation;
pub mod network;
pub(crate) mod runtime_watch;
pub mod socket_activation;
mod storage;
#[cfg(test)]
#[path = "../tests/test_dir/mod.rs"]
mod test_dir;
