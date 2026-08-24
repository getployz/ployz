//! Best-effort capacity observation of this daemon host.

use std::{fs, io, path::Path};

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(crate) struct HostCapacity {
    pub memory_total_bytes: Option<u64>,
    pub disk_total_bytes: Option<u64>,
    pub disk_available_bytes: Option<u64>,
}

/// Observe memory and the root filesystem that backs the Machine Pool.
pub(crate) fn observe() -> HostCapacity {
    observe_at(Path::new("/proc/meminfo"), Path::new("/"))
}

fn observe_at(meminfo: &Path, disk: &Path) -> HostCapacity {
    let memory_total_bytes = memory_capacity_at(meminfo).ok().map(|(total, _)| total);
    let disk = filesystem_space(disk).ok();
    HostCapacity {
        memory_total_bytes,
        disk_total_bytes: disk.map(|(total, _)| total),
        disk_available_bytes: disk.map(|(_, available)| available),
    }
}

pub(crate) fn filesystem_space(path: impl AsRef<Path>) -> io::Result<(u64, u64)> {
    let stat = nix::sys::statvfs::statvfs(path.as_ref()).map_err(io::Error::other)?;
    Ok((
        stat.blocks().saturating_mul(stat.fragment_size()),
        stat.blocks_available().saturating_mul(stat.fragment_size()),
    ))
}

pub(crate) fn memory_capacity() -> io::Result<(u64, u64)> {
    memory_capacity_at(Path::new("/proc/meminfo"))
}

fn memory_capacity_at(path: &Path) -> io::Result<(u64, u64)> {
    let memory = fs::read_to_string(path)?;
    let value = |key| {
        memory
            .lines()
            .find_map(|line| {
                line.strip_prefix(key)
                    .and_then(|line| line.split_whitespace().next()?.parse::<u64>().ok())
            })
            .map(|kib| kib * 1024)
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, key))
    };
    Ok((value("MemTotal:")?, value("MemAvailable:")?))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn observation_reports_root_total_and_available_bytes() {
        let observed = observe();
        let (total, available) = filesystem_space(Path::new("/")).unwrap();
        assert_eq!(observed.disk_total_bytes, Some(total));
        assert_eq!(observed.disk_available_bytes, Some(available));
        assert!(available <= total);
    }

    #[test]
    fn probe_failures_become_missing_observations() {
        let missing = Path::new("/definitely-not-a-ployz-capacity-path");
        assert_eq!(observe_at(missing, missing), HostCapacity::default());
    }
}
