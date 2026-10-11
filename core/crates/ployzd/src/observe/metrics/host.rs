//! Host totals for the Server: CPU jiffies, memory, per-interface bytes, and
//! filesystem usage of the mounts the caller names.
//!
//! Every value is raw and cumulative where the kernel keeps it that way. A
//! source that cannot be read or parsed leaves its field `None`, never zero.
#![cfg_attr(not(test), expect(dead_code, reason = "wired by the sampler in M6"))]

use std::{
    fs,
    path::{Path, PathBuf},
};

const PROC_ROOT: &str = "/proc";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct HostCounters {
    pub cpu: Option<CpuJiffies>,
    pub memory: Memory,
    /// `None` when `/proc/net/dev` is unreadable; a malformed line drops only
    /// its interface.
    pub interfaces: Option<Vec<InterfaceBytes>>,
    /// One entry per requested mount, in request order.
    pub mounts: Vec<MountUsage>,
}

/// Aggregate CPU time across all cores, in `USER_HZ` ticks since boot.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CpuJiffies {
    /// user, nice, system, irq, softirq, and steal.
    pub busy: u64,
    /// idle and iowait.
    pub idle: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct Memory {
    pub total: Option<u64>,
    pub available: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InterfaceBytes {
    pub name: String,
    pub rx: u64,
    pub tx: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MountUsage {
    pub path: PathBuf,
    pub bytes: Option<FsBytes>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FsBytes {
    pub total: u64,
    pub used: u64,
    /// What an unprivileged writer can still use; excludes reserved blocks.
    pub available: u64,
}

pub(crate) fn read_host(mounts: &[&Path]) -> HostCounters {
    read_host_from(Path::new(PROC_ROOT), mounts)
}

fn read_host_from(proc_root: &Path, mounts: &[&Path]) -> HostCounters {
    let read = |name: &str| fs::read_to_string(proc_root.join(name)).ok();
    HostCounters {
        cpu: read("stat").as_deref().and_then(parse_stat),
        memory: read("meminfo")
            .as_deref()
            .map(parse_meminfo)
            .unwrap_or_default(),
        interfaces: read("net/dev").as_deref().map(parse_net_dev),
        mounts: mounts
            .iter()
            .map(|path| MountUsage {
                path: path.to_path_buf(),
                bytes: statvfs_bytes(path),
            })
            .collect(),
    }
}

fn parse_stat(text: &str) -> Option<CpuJiffies> {
    let line = text.lines().find(|line| line.starts_with("cpu "))?;
    let fields = line
        .split_whitespace()
        .skip(1)
        .map(str::parse::<u64>)
        .collect::<Result<Vec<_>, _>>()
        .ok()?;
    // guest and guest_nice (fields 8 and 9) are already counted in user and
    // nice, so summing them would count guest time twice.
    let field = |i: usize| fields.get(i).copied().unwrap_or(0);
    if fields.len() < 4 {
        return None;
    }
    Some(CpuJiffies {
        busy: field(0) + field(1) + field(2) + field(5) + field(6) + field(7),
        idle: field(3) + field(4),
    })
}

fn parse_meminfo(text: &str) -> Memory {
    let mut memory = Memory::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let slot = match key {
            "MemTotal" => &mut memory.total,
            "MemAvailable" => &mut memory.available,
            _ => continue,
        };
        *slot = parse_kib(value);
    }
    memory
}

fn parse_kib(value: &str) -> Option<u64> {
    let mut parts = value.split_whitespace();
    let kib = parts.next()?.parse::<u64>().ok()?;
    match (parts.next(), parts.next()) {
        (Some("kB"), None) => kib.checked_mul(1024),
        _ => None,
    }
}

/// Every interface line of `/proc/net/dev`, headers and malformed lines dropped.
pub(super) fn parse_net_dev(text: &str) -> Vec<InterfaceBytes> {
    text.lines()
        .filter_map(|line| {
            let (name, counters) = line.split_once(':')?;
            let counters = counters
                .split_whitespace()
                .map(str::parse::<u64>)
                .collect::<Result<Vec<_>, _>>()
                .ok()?;
            Some(InterfaceBytes {
                name: name.trim().to_owned(),
                rx: *counters.first()?,
                tx: *counters.get(8)?,
            })
        })
        .collect()
}

fn statvfs_bytes(path: &Path) -> Option<FsBytes> {
    let stat = nix::sys::statvfs::statvfs(path).ok()?;
    let fragment = stat.fragment_size();
    let blocks = |count: u64| count.checked_mul(fragment);
    Some(FsBytes {
        total: blocks(stat.blocks())?,
        used: blocks(stat.blocks().checked_sub(stat.blocks_free())?)?,
        available: blocks(stat.blocks_available())?,
    })
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::{
        CpuJiffies, InterfaceBytes, Memory, parse_meminfo, parse_net_dev, parse_stat,
        read_host_from,
    };
    use crate::test_dir::TestDir;

    #[test]
    fn stat_sums_busy_and_idle_without_double_counting_guest_time() {
        let text = "cpu  100 2 30 4000 50 6 7 8 900 90\n\
                    cpu0 50 1 15 2000 25 3 3 4 450 45\n\
                    intr 12345\n";
        assert_eq!(
            parse_stat(text),
            Some(CpuJiffies {
                busy: 100 + 2 + 30 + 6 + 7 + 8,
                idle: 4000 + 50,
            })
        );
    }

    #[test]
    fn stat_without_a_readable_aggregate_line_is_none() {
        assert_eq!(parse_stat("cpu0 1 2 3 4 5 6 7 8\n"), None);
        assert_eq!(parse_stat("cpu  1 2 x 4 5 6 7 8\n"), None);
        assert_eq!(parse_stat("cpu  1 2 3\n"), None);
        assert_eq!(parse_stat(""), None);
    }

    #[test]
    fn meminfo_reads_total_and_available_in_bytes() {
        let text = "MemTotal:       16314564 kB\n\
                    MemFree:          812340 kB\n\
                    MemAvailable:   11002244 kB\n\
                    HugePages_Total:       0\n";
        assert_eq!(
            parse_meminfo(text),
            Memory {
                total: Some(16_314_564 * 1024),
                available: Some(11_002_244 * 1024),
            }
        );
    }

    #[test]
    fn meminfo_leaves_a_missing_or_malformed_field_none() {
        let text = "MemTotal:       16314564 kB\n\
                    MemAvailable:   1100 pages\n";
        assert_eq!(
            parse_meminfo(text),
            Memory {
                total: Some(16_314_564 * 1024),
                available: None,
            }
        );
        assert_eq!(parse_meminfo("MemTotal: 12"), Memory::default());
    }

    #[test]
    fn net_dev_reads_every_interface_and_skips_headers_and_malformed_lines() {
        let text = "\
Inter-|   Receive                                                |  Transmit
 face |bytes    packets errs drop fifo frame compressed multicast|bytes    packets errs drop fifo colls carrier compressed
    lo: 9000      90    0    0    0     0          0         0     9000      90    0    0    0     0       0          0
  eth0:123456789 1000    0    0    0     0          0         0 987654321  2000    0    0    0     0       0          0
 ployz0: 42 1 0 0
  wg0: 555       5    0    0    0     0          0         0      666       6    0    0    0     0       0          0
";
        assert_eq!(
            parse_net_dev(text),
            vec![
                InterfaceBytes {
                    name: "lo".to_owned(),
                    rx: 9000,
                    tx: 9000,
                },
                InterfaceBytes {
                    name: "eth0".to_owned(),
                    rx: 123_456_789,
                    tx: 987_654_321,
                },
                InterfaceBytes {
                    name: "wg0".to_owned(),
                    rx: 555,
                    tx: 666,
                },
            ]
        );
    }

    #[test]
    fn unreadable_sources_leave_every_field_none() {
        let dir = TestDir::new("host-metrics-missing");
        let missing = dir.0.join("not-a-mount");
        let host = read_host_from(&dir.0, &[&missing, Path::new("/")]);
        assert_eq!(host.cpu, None);
        assert_eq!(host.memory, Memory::default());
        assert_eq!(host.interfaces, None);
        let [absent, root] = host.mounts.as_slice() else {
            panic!("one entry per requested mount: {:?}", host.mounts);
        };
        assert_eq!(
            (absent.path.as_path(), absent.bytes),
            (missing.as_path(), None)
        );
        let root_bytes = root.bytes.expect("statvfs of / succeeds");
        assert!(root_bytes.used + root_bytes.available <= root_bytes.total);
    }

    #[test]
    #[ignore = "reads this machine's /proc and runs `free -b`"]
    fn real_memory_total_matches_free() {
        let host = super::read_host(&[Path::new("/"), Path::new("/var/lib/ployz-volumes")]);
        println!("{host:#?}");
        let free = std::process::Command::new("free")
            .arg("-b")
            .output()
            .expect("free runs");
        let free = String::from_utf8(free.stdout).expect("free prints UTF-8");
        println!("{free}");
        let free_total: u64 = free
            .lines()
            .find_map(|line| line.strip_prefix("Mem:"))
            .and_then(|rest| rest.split_whitespace().next())
            .and_then(|total| total.parse().ok())
            .expect("free prints a Mem: total");
        let total = host.memory.total.expect("MemTotal is readable");
        assert!(
            total.abs_diff(free_total) * 100 <= free_total,
            "{total} vs {free_total}"
        );
        assert!(host.cpu.is_some() && host.memory.available.is_some());
        assert!(
            host.interfaces
                .is_some_and(|interfaces| !interfaces.is_empty())
        );
    }
}
