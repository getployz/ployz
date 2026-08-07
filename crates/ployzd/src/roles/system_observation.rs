//! Live machine capacity observation shared by the Keeper status writer and
//! the API placement bid: free disk, free memory, and the load band, read at
//! the point of use and never stored here.

use std::fs;
use std::path::Path;

use ployz_core::corrosion::MachineLoadBand;
use rustix::fs::statvfs;

#[derive(Debug, Clone, Copy)]
pub(crate) struct SystemObservation {
    pub(crate) free_disk_bytes: u64,
    pub(crate) free_memory_bytes: u64,
    pub(crate) load: MachineLoadBand,
}

/// One local capacity resource could not be observed.
#[derive(Debug, thiserror::Error)]
#[error("could not observe {resource}: {detail}")]
pub(crate) struct SystemObservationError {
    pub(crate) resource: &'static str,
    pub(crate) detail: String,
}

impl SystemObservation {
    pub(crate) fn read() -> Result<Self, SystemObservationError> {
        let stat = statvfs(Path::new("/")).map_err(|source| SystemObservationError {
            resource: "root filesystem",
            detail: source.to_string(),
        })?;
        let free_disk_bytes = bytes_from_blocks(stat.f_bavail, stat.f_frsize);
        let meminfo =
            fs::read_to_string("/proc/meminfo").map_err(|source| SystemObservationError {
                resource: "available memory",
                detail: source.to_string(),
            })?;
        let free_memory_bytes = mem_available_bytes(&meminfo)?;
        let loadavg =
            fs::read_to_string("/proc/loadavg").map_err(|source| SystemObservationError {
                resource: "load average",
                detail: source.to_string(),
            })?;
        let one_minute = loadavg
            .split_ascii_whitespace()
            .next()
            .ok_or_else(|| SystemObservationError {
                resource: "load average",
                detail: "missing one-minute value".to_owned(),
            })?
            .parse::<f64>()
            .map_err(|source| SystemObservationError {
                resource: "load average",
                detail: source.to_string(),
            })?;
        let cpu_count = std::thread::available_parallelism()
            .map(|count| count.get())
            .unwrap_or(1) as f64;
        let normalized = one_minute / cpu_count;
        let load = if normalized < 0.5 {
            MachineLoadBand::Idle
        } else if normalized < 1.0 {
            MachineLoadBand::Normal
        } else {
            MachineLoadBand::Hot
        };
        Ok(Self {
            free_disk_bytes,
            free_memory_bytes,
            load,
        })
    }
}

fn mem_available_bytes(meminfo: &str) -> Result<u64, SystemObservationError> {
    let Some(line) = meminfo
        .lines()
        .find(|line| line.starts_with("MemAvailable:"))
    else {
        return Err(SystemObservationError {
            resource: "available memory",
            detail: "MemAvailable is absent from /proc/meminfo".to_owned(),
        });
    };
    let fields = line.split_ascii_whitespace().collect::<Vec<_>>();
    let [_, kibibytes, "kB"] = fields.as_slice() else {
        return Err(SystemObservationError {
            resource: "available memory",
            detail: "MemAvailable has an unexpected shape".to_owned(),
        });
    };
    kibibytes
        .parse::<u64>()
        .map(|value| value.saturating_mul(1_024))
        .map_err(|source| SystemObservationError {
            resource: "available memory",
            detail: source.to_string(),
        })
}

fn bytes_from_blocks(blocks: u64, block_size: u64) -> u64 {
    u64::try_from(u128::from(blocks).saturating_mul(u128::from(block_size))).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::mem_available_bytes;

    #[test]
    fn reads_mem_available_as_bytes() {
        assert_eq!(
            mem_available_bytes("MemTotal: 20 kB\nMemAvailable: 12 kB\n").expect("memory"),
            12 * 1_024
        );
    }
}
