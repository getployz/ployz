//! What this host reports about itself: the runtime facts in the Machine record, the filesystem
//! under the store, and the Machine Pool `zpool list` shows.

use std::{
    fs::{self, OpenOptions},
    io::{self, Read, Seek, SeekFrom},
    os::unix::fs::OpenOptionsExt,
    path::Path,
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use ployz_core::{MachineId, MachineRuntime, MachineStorageObservation};

use crate::machine_pool;

const DOCKER_VERSION_TIMEOUT: Duration = Duration::from_secs(2);
pub(crate) const STORAGE_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(2);

/// [`local_runtime_blocking`] on a blocking thread, so a hung `docker` stalls no async worker.
///
/// # Panics
///
/// Resumes a panic of the probe.
pub async fn local_runtime(data_dir: &Path) -> MachineRuntime {
    let data_dir = data_dir.to_owned();
    tokio::task::spawn_blocking(move || local_runtime_blocking(&data_dir))
        .await
        .unwrap_or_else(|error| std::panic::resume_unwind(error.into_panic()))
}

/// What this host reports about itself for the Machine record, read on the calling thread.
///
/// It runs `docker version`, bounded by [`DOCKER_VERSION_TIMEOUT`], so a caller on an async
/// worker uses [`local_runtime`].
#[must_use]
pub fn local_runtime_blocking(data_dir: &Path) -> MachineRuntime {
    let store_filesystem = store_filesystem(data_dir);
    MachineRuntime {
        daemon_version: env!("CARGO_PKG_VERSION").into(),
        docker_version: docker_version(Path::new("docker"), DOCKER_VERSION_TIMEOUT),
        hostname: read_trimmed("/etc/hostname"),
        // Kernel architecture preserves distinctions lost by Rust's ARCH,
        // including ppc64/ppc64le and ARM generations.
        architecture: nix::sys::utsname::uname()
            .map(|system| system.machine().to_string_lossy().into_owned())
            .unwrap_or_else(|_| std::env::consts::ARCH.into()),
        os_pretty_name: fs::read_to_string("/etc/os-release")
            .ok()
            .and_then(|contents| {
                contents.lines().find_map(|line| {
                    line.strip_prefix("PRETTY_NAME=")
                        .map(|value| value.trim_matches('"').to_owned())
                })
            })
            .unwrap_or_default(),
        kernel_version: read_trimmed("/proc/sys/kernel/osrelease"),
        memory_total_bytes: crate::host_capacity::memory_capacity()
            .ok()
            .map(|(total, _)| total),
        running_builds: 0,
        pool_committable_bytes: None,
        sealing: None,
        // Corrosion syncs each commit; on tmpfs a sync reaches only memory.
        store_syncs_commits: store_filesystem.as_deref().map(|name| name != "tmpfs"),
        store_filesystem,
        capabilities: ployz_core::served_capabilities(),
        device_enforcement: None,
        management_identity: None,
        leaving: false,
    }
}

fn store_filesystem(data_dir: &Path) -> Option<String> {
    let store = crate::corrosion::CorrosionPaths::data_dir_under(data_dir);
    let stat = match rustix::fs::statfs(&store) {
        Err(rustix::io::Errno::NOENT) => rustix::fs::statfs(data_dir),
        stat => stat,
    };
    // Filesystem magic numbers are 32 bits; f_type's width and sign vary by target.
    stat.ok().map(|stat| filesystem_name(stat.f_type as u32))
}

fn filesystem_name(magic: u32) -> String {
    match magic {
        0x2fc1_2fc1 => "zfs".into(),
        0x9123_683e => "btrfs".into(),
        0x0102_1994 => "tmpfs".into(),
        0x794c_7630 => "overlay".into(),
        0xef53 => "ext4".into(),
        0x5846_5342 => "xfs".into(),
        other => format!("{other:#010x}"),
    }
}

pub(crate) async fn local_storage(
    program: &Path,
    timeout: Duration,
) -> Option<MachineStorageObservation> {
    let mut command = tokio::process::Command::new(program);
    command.args(machine_pool::ZPOOL_LIST).kill_on_drop(true);
    match tokio::time::timeout(timeout, command.output()).await {
        Ok(Ok(output)) if output.status.success() => {
            let output = String::from_utf8(output.stdout).ok()?;
            match machine_pool::one_usable(&output) {
                Ok(Some(pool)) => Some(MachineStorageObservation::Pool {
                    size_bytes: pool.size_bytes(),
                    used_bytes: pool.used_bytes(),
                    free_bytes: pool.free_bytes(),
                }),
                Ok(None) => Some(MachineStorageObservation::Ready),
                Err(_) => None,
            }
        }
        Ok(Err(error)) if error.kind() == io::ErrorKind::NotFound => {
            Some(MachineStorageObservation::Stateless)
        }
        Ok(Ok(_)) | Ok(Err(_)) | Err(_) => None,
    }
}

fn docker_version(program: &Path, timeout: Duration) -> String {
    let output_path =
        std::env::temp_dir().join(format!(".ployzd-docker-version-{}", MachineId::random()));
    let Ok(mut output) = OpenOptions::new()
        .create_new(true)
        .read(true)
        .write(true)
        .mode(0o600)
        .open(&output_path)
    else {
        return String::new();
    };

    let version = (|| {
        let stdout = output.try_clone().ok()?;
        let mut child = Command::new(program)
            .args(["version", "--format", "{{.Server.Version}}"])
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let deadline = Instant::now() + timeout;
        let status = loop {
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) if Instant::now() < deadline => {
                    thread::sleep(Duration::from_millis(10));
                }
                Ok(None) | Err(_) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        };
        if !status.success() {
            return None;
        }
        output.seek(SeekFrom::Start(0)).ok()?;
        let mut version = String::new();
        output.read_to_string(&mut version).ok()?;
        Some(version.trim().to_owned())
    })()
    .unwrap_or_default();

    drop(output);
    let _ = fs::remove_file(output_path);
    version
}

fn read_trimmed(path: &str) -> String {
    fs::read_to_string(path)
        .map(|value| value.trim().to_owned())
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use std::os::unix::fs::PermissionsExt;

    use super::*;

    #[test]
    fn filesystem_names_cover_the_ones_that_refuse_synchronous_writes() {
        assert_eq!(filesystem_name(0x2fc1_2fc1), "zfs");
        assert_eq!(filesystem_name(0x9123_683e), "btrfs");
        assert_eq!(filesystem_name(0x0102_1994), "tmpfs");
        assert_eq!(filesystem_name(0x794c_7630), "overlay");
        assert_eq!(filesystem_name(0xef53), "ext4");
        assert_eq!(filesystem_name(0x5846_5342), "xfs");
        assert_eq!(filesystem_name(0x1234_abcd), "0x1234abcd");
        assert_eq!(filesystem_name(0x6969), "0x00006969");
    }

    #[test]
    fn store_filesystem_is_named_before_and_after_the_store_exists() {
        let root = tempfile::tempdir().unwrap();
        let expected = filesystem_name(rustix::fs::statfs(root.path()).unwrap().f_type as u32);
        assert_eq!(store_filesystem(root.path()), Some(expected.clone()));
        fs::create_dir(root.path().join("corrosion")).unwrap();
        assert_eq!(store_filesystem(root.path()), Some(expected));
        assert_eq!(store_filesystem(&root.path().join("missing")), None);
    }

    #[tokio::test]
    async fn current_zpool_evidence_distinguishes_states_and_is_not_persisted() {
        let root = std::env::temp_dir().join(format!(
            "ployzd-storage-observation-{}",
            ployz_core::MachineId::random()
        ));
        fs::create_dir_all(&root).unwrap();
        let program = root.join("zpool");

        assert_eq!(
            local_storage(&program, Duration::from_secs(1)).await,
            Some(MachineStorageObservation::Stateless)
        );

        for (script, expected) in [
            ("#!/bin/sh\nexit 1\n", None),
            (
                "#!/bin/sh\nprintf 'tank\\t2147483648\\t1932735283\\t214748365\\tONLINE\\toff\\n'\n",
                Some(MachineStorageObservation::Pool {
                    size_bytes: std::num::NonZeroU64::new(2_147_483_648).unwrap(),
                    used_bytes: 1_932_735_283,
                    free_bytes: 214_748_365,
                }),
            ),
            (
                "#!/bin/sh\nprintf 'tank\\t4294967296\\t3865470566\\t429496730\\tONLINE\\toff\\n'\n",
                Some(MachineStorageObservation::Pool {
                    size_bytes: std::num::NonZeroU64::new(4_294_967_296).unwrap(),
                    used_bytes: 3_865_470_566,
                    free_bytes: 429_496_730,
                }),
            ),
            (
                "#!/bin/sh\nexit 0\n",
                Some(MachineStorageObservation::Ready),
            ),
            (
                "#!/bin/sh\nprintf 'tank\\t2147483648\\t0\\t2147483648\\tONLINE\\ton\\n'\n",
                None,
            ),
            (
                "#!/bin/sh\nprintf 'tank\\t2147483648\\t0\\t2147483648\\tFAULTED\\toff\\n'\n",
                None,
            ),
            (
                "#!/bin/sh\nprintf 'alpha\\t2147483648\\t0\\t2147483648\\tONLINE\\toff\\nbeta\\t2147483648\\t0\\t2147483648\\tDEGRADED\\toff\\n'\n",
                None,
            ),
        ] {
            fs::write(&program, script).unwrap();
            fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();
            assert_eq!(
                local_storage(&program, Duration::from_secs(1)).await,
                expected
            );
        }

        fs::write(&program, "#!/bin/sh\nwhile :; do :; done\n").unwrap();
        assert_eq!(
            local_storage(&program, Duration::from_millis(10)).await,
            None
        );

        fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn docker_runtime_probe_is_bounded() {
        let root = std::env::temp_dir().join(format!(
            "ployzd-docker-version-{}",
            ployz_core::MachineId::random()
        ));
        fs::create_dir_all(&root).unwrap();
        let program = root.join("docker");
        fs::write(&program, "#!/bin/sh\nexec sleep 30\n").unwrap();
        fs::set_permissions(&program, fs::Permissions::from_mode(0o700)).unwrap();

        let started = Instant::now();
        let version = docker_version(&program, Duration::from_millis(25));
        let elapsed = started.elapsed();
        fs::remove_dir_all(root).unwrap();

        assert!(version.is_empty());
        assert!(elapsed < Duration::from_secs(1), "probe took {elapsed:?}");
    }

    #[tokio::test]
    async fn the_async_probe_reads_what_the_blocking_probe_reads() {
        let root = tempfile::tempdir().unwrap();
        assert_eq!(
            local_runtime(root.path()).await,
            local_runtime_blocking(root.path())
        );
    }
}
