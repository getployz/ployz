//! Linux daemon runtime for one Ployz Machine.

pub(crate) mod boot_pass;
mod build;
pub(crate) mod forgotten_fence;

/// Numeric status returned by the Ployz Docker Volume plugin.
#[doc(hidden)]
#[derive(serde::Deserialize, serde::Serialize)]
pub struct VolumePluginStatus {
    /// Current ZFS dataset byte bound.
    pub bound_bytes: u64,
    /// Current referenced ZFS dataset bytes.
    pub used_bytes: u64,
}

/// Where Docker finds the Ployz Volume plugin, and where the daemon reaches it.
#[doc(hidden)]
pub const VOLUME_PLUGIN_SOCKET: &str = "/run/docker/plugins/ployz.sock";

mod docker_image;

pub(crate) mod certificates;
pub mod corrosion;
pub mod daemon;
pub mod diag;
pub mod dns;
pub mod docker;
pub mod faults;
pub(crate) mod filesystem;
mod host_capacity;
pub(crate) mod ingress;
pub mod installer;
pub mod logs;
mod lost_tail;
pub mod machine;
pub mod machine_api;
pub(crate) mod machine_api_socket;
#[doc(hidden)]
pub mod machine_pool;
pub mod management;
pub mod mutation;
pub mod network;
pub mod observe;
pub(crate) mod route_acks;
pub mod runtime_unit;
pub(crate) mod runtime_watch;
pub(crate) mod sealed_compat;
pub mod sealed_env;
pub mod sealing_key;
mod sealing_recovery;
pub mod socket_activation;
mod storage;
#[cfg(test)]
#[path = "../tests/test_dir/mod.rs"]
mod test_dir;
pub mod volume_copy;
pub mod volume_send;
