//! Resource samples of Ployz containers and their Server.

#[cfg_attr(not(test), expect(dead_code, reason = "wired by the sampler in M6"))]
mod cgroup;
mod host;
#[cfg_attr(not(test), expect(dead_code, reason = "wired by the sampler in M6"))]
mod net;
mod zfs_io;
#[cfg_attr(not(test), expect(dead_code, reason = "wired by the sampler in M7"))]
mod zfs_usage;
