//! Container counters read from a cgroup v2 directory.

use std::{fs, path::Path};

/// One read of a container's cgroup. A field is `None` when its file could
/// not be read or parsed, never zero.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct CgroupCounters {
    pub cpu_usec: Option<u64>,
    /// `memory.current` less `inactive_file`, the page cache the kernel
    /// reclaims first; `docker stats` reports the same figure.
    pub mem_bytes: Option<u64>,
    pub mem_limit: Option<MemoryLimit>,
    pub io: Option<IoBytes>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MemoryLimit {
    Unlimited,
    Bytes(u64),
}

/// Bytes read and written, summed over every block device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct IoBytes {
    pub read: u64,
    pub write: u64,
}

pub(crate) fn read_cgroup(dir: &Path) -> CgroupCounters {
    let file = |name: &str| fs::read_to_string(dir.join(name)).ok();
    let cpu_usec = file("cpu.stat").and_then(|text| keyed(&text, "usage_usec"));
    let mem_bytes = file("memory.current")
        .and_then(|text| single(&text))
        .and_then(|current| {
            let inactive = file("memory.stat").and_then(|text| keyed(&text, "inactive_file"))?;
            Some(current.saturating_sub(inactive))
        });
    let mem_limit = file("memory.max").and_then(|text| match complete_lines(&text)?.as_slice() {
        ["max"] => Some(MemoryLimit::Unlimited),
        [bytes] => bytes.parse().ok().map(MemoryLimit::Bytes),
        _ => None,
    });
    let io = file("io.stat").and_then(|text| io_bytes(&text));
    CgroupCounters {
        cpu_usec,
        mem_bytes,
        mem_limit,
        io,
    }
}

/// The kernel ends every line with a newline, so a file that does not end
/// in one was cut short and none of it is trusted.
fn complete_lines(text: &str) -> Option<Vec<&str>> {
    if !text.is_empty() && !text.ends_with('\n') {
        return None;
    }
    Some(text.lines().collect())
}

fn single(text: &str) -> Option<u64> {
    match complete_lines(text)?.as_slice() {
        [value] => value.parse().ok(),
        _ => None,
    }
}

/// The value of `key` in a flat-keyed file such as `cpu.stat`.
fn keyed(text: &str, key: &str) -> Option<u64> {
    complete_lines(text)?
        .into_iter()
        .find_map(|line| line.strip_prefix(key)?.strip_prefix(' '))?
        .parse()
        .ok()
}

/// Sums `rbytes` and `wbytes` over `io.stat`'s `MAJ:MIN key=value…` lines.
/// An empty file is a cgroup that has done no I/O yet.
fn io_bytes(text: &str) -> Option<IoBytes> {
    complete_lines(text)?
        .into_iter()
        .try_fold(IoBytes { read: 0, write: 0 }, |total, line| {
            let mut read = None;
            let mut write = None;
            for field in line.split_whitespace().skip(1) {
                match field.split_once('=') {
                    Some(("rbytes", value)) => read = value.parse::<u64>().ok(),
                    Some(("wbytes", value)) => write = value.parse::<u64>().ok(),
                    _ => {}
                }
            }
            Some(IoBytes {
                read: total.read.checked_add(read?)?,
                write: total.write.checked_add(write?)?,
            })
        })
}

#[cfg(test)]
mod tests {
    use std::{fs, path::Path, process::Command};

    use super::{CgroupCounters, IoBytes, MemoryLimit, read_cgroup};
    use crate::test_dir::TestDir;

    const CPU_STAT: &str = "usage_usec 141587\nuser_usec 33163\nsystem_usec 108424\n\
                            nr_periods 0\nnr_throttled 0\nthrottled_usec 0\n";
    const MEMORY_STAT: &str = "anon 3555328\nfile 7372800\nactive_file 7311360\n\
                               inactive_file 61440\nunevictable 0\n";
    const IO_STAT: &str = "253:0 rbytes=4096 wbytes=12288 rios=1 wios=3 dbytes=0 dios=0\n\
                           8:0 rbytes=1000 wbytes=24 rios=2 wios=1 dbytes=0 dios=0\n";

    fn cgroup(files: &[(&str, &str)]) -> TestDir {
        let dir = TestDir::new("ployzd-observe-cgroup");
        fs::create_dir_all(&dir.0).unwrap();
        for (name, text) in files {
            fs::write(dir.0.join(name), text).unwrap();
        }
        dir
    }

    fn normal() -> Vec<(&'static str, &'static str)> {
        vec![
            ("cpu.stat", CPU_STAT),
            ("memory.current", "19529728\n"),
            ("memory.stat", MEMORY_STAT),
            ("memory.max", "268435456\n"),
            ("io.stat", IO_STAT),
        ]
    }

    fn with(name: &'static str, text: &'static str) -> Vec<(&'static str, &'static str)> {
        let mut files = normal();
        files.retain(|(file, _)| *file != name);
        files.push((name, text));
        files
    }

    #[test]
    fn reads_every_counter_from_a_full_cgroup() {
        let dir = cgroup(&normal());
        assert_eq!(
            read_cgroup(&dir.0),
            CgroupCounters {
                cpu_usec: Some(141_587),
                mem_bytes: Some(19_529_728 - 61_440),
                mem_limit: Some(MemoryLimit::Bytes(268_435_456)),
                io: Some(IoBytes {
                    read: 5096,
                    write: 12_312
                }),
            }
        );
    }

    #[test]
    fn max_memory_is_unlimited_not_missing() {
        let dir = cgroup(&with("memory.max", "max\n"));
        assert_eq!(read_cgroup(&dir.0).mem_limit, Some(MemoryLimit::Unlimited));
    }

    #[test]
    fn a_missing_file_leaves_only_its_fields_empty() {
        let mut files = normal();
        files.retain(|(name, _)| *name != "io.stat");
        let read = read_cgroup(&cgroup(&files).0);
        assert_eq!(read.io, None);
        assert_eq!(read.cpu_usec, Some(141_587));
        assert_eq!(read.mem_limit, Some(MemoryLimit::Bytes(268_435_456)));

        let mut files = normal();
        files.retain(|(name, _)| *name != "memory.stat");
        assert_eq!(read_cgroup(&cgroup(&files).0).mem_bytes, None);

        assert_eq!(
            read_cgroup(&cgroup(&[]).0),
            CgroupCounters {
                cpu_usec: None,
                mem_bytes: None,
                mem_limit: None,
                io: None
            }
        );
    }

    #[test]
    fn an_idle_cgroup_has_done_zero_io() {
        let dir = cgroup(&with("io.stat", ""));
        assert_eq!(read_cgroup(&dir.0).io, Some(IoBytes { read: 0, write: 0 }));
    }

    #[test]
    fn a_truncated_file_is_not_trusted() {
        for (name, text) in [
            ("cpu.stat", "usage_usec 141587\nuser_us"),
            ("cpu.stat", "usage_us"),
            ("memory.current", "1952"),
            ("memory.current", ""),
            ("memory.max", "2684"),
            ("io.stat", "253:0 rbytes=4096 wbytes=12288\n8:0 rbytes=10"),
            ("io.stat", "253:0 rbytes=4096 wb\n"),
        ] {
            let read = read_cgroup(&cgroup(&with(name, text)).0);
            let field = match name {
                "cpu.stat" => read.cpu_usec.is_none(),
                "memory.current" => read.mem_bytes.is_none(),
                "memory.max" => read.mem_limit.is_none(),
                _ => read.io.is_none(),
            };
            assert!(field, "{name} {text:?} read as {read:?}");
        }
    }

    #[test]
    fn inactive_file_above_current_reads_as_zero() {
        let dir = cgroup(&with("memory.current", "1000\n"));
        assert_eq!(read_cgroup(&dir.0).mem_bytes, Some(0));
    }

    fn docker(args: &[&str]) -> String {
        let output = Command::new("docker").args(args).output().unwrap();
        assert!(
            output.status.success(),
            "docker {args:?}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        String::from_utf8(output.stdout).unwrap().trim().to_owned()
    }

    struct Removed(String);

    impl Drop for Removed {
        fn drop(&mut self) {
            let _ = Command::new("docker").args(["rm", "-f", &self.0]).output();
        }
    }

    /// The cgroup v2 directory of a running container, from the `0::` line of
    /// its init process. This holds for both the systemd and cgroupfs drivers.
    fn cgroup_dir(container: &str) -> std::path::PathBuf {
        let pid = docker(&["inspect", "--format", "{{.State.Pid}}", container]);
        let membership = fs::read_to_string(format!("/proc/{pid}/cgroup")).unwrap();
        let path = membership
            .lines()
            .find_map(|line| line.strip_prefix("0::"))
            .expect("cgroup v2 host");
        Path::new("/sys/fs/cgroup").join(path.trim_start_matches('/'))
    }

    #[test]
    #[ignore = "requires Docker on a cgroup v2 host and nginx:1.29-alpine"]
    fn reads_a_running_containers_cgroup() {
        for limit in [None, Some("256m")] {
            let mut args = vec!["run", "-d", "--rm"];
            args.extend(limit.iter().flat_map(|limit| ["--memory", *limit]));
            args.push("nginx:1.29-alpine");
            let id = docker(&args);
            let _removed = Removed(id.clone());
            let dir = cgroup_dir(&id);
            let read = read_cgroup(&dir);
            eprintln!("{}\n{read:#?}", dir.display());

            let mem_bytes = read.mem_bytes.unwrap();
            assert!(read.cpu_usec.is_some());
            assert!(read.io.is_some());
            match read.mem_limit.unwrap() {
                MemoryLimit::Unlimited => assert_eq!(limit, None),
                MemoryLimit::Bytes(bytes) => {
                    assert_eq!(bytes, 256 << 20);
                    assert!(mem_bytes < bytes);
                }
            }
        }
    }
}
