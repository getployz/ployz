//! Where the Server's disk went: Volumes, mirror slots, the Observe Store,
//! and everything else.
//!
//! `zfs list` can hang on a sick Pool and the Observe Store walk touches
//! every file, so both run every 60 s on their own task. The 10 s sampler
//! takes the last reading and its own `statvfs`, and never waits on either.

use std::{
    fs, io,
    os::unix::fs::MetadataExt,
    path::{Path, PathBuf},
    time::Duration,
};

use ployz_core::{DATASET_ROOT, MIRROR_ROOT, metrics::DiskBreakdown};
use tokio::{
    sync::watch,
    time::{MissedTickBehavior, interval},
};

use super::{
    host::FsBytes,
    zfs_usage::{ZfsUsage, ZfsUsageReader},
};
use crate::{machine_pool, observe::layout::StoreRoot};

const INTERVAL: Duration = Duration::from_secs(60);
const BLOCK_BYTES: u64 = 512;

/// What a Pool dataset holds, from its place under the Pool.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum DatasetPlace<'name> {
    /// The root copy of the named Volume, the one its replicas mount.
    Volume(&'name str),
    /// A mirror slot, or its `fs` child.
    Slot,
    Other,
}

impl<'name> DatasetPlace<'name> {
    pub(super) fn of(dataset: &'name str) -> Self {
        let Some((_pool, rest)) = dataset.split_once('/') else {
            return Self::Other;
        };
        if let Some(volume) = rest
            .strip_prefix(DATASET_ROOT)
            .and_then(|rest| rest.strip_prefix('/'))
            .filter(|volume| !volume.contains('/'))
        {
            return Self::Volume(volume);
        }
        match rest
            .strip_prefix(MIRROR_ROOT)
            .and_then(|rest| rest.strip_prefix('/'))
        {
            Some(_) => Self::Slot,
            None => Self::Other,
        }
    }
}

/// The Machine Pool's bytes, as far as they sit on the filesystem holding the
/// Observe Store.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum PoolBytes {
    /// No Pool lives on this filesystem: a Docker-only Server, or a Pool on
    /// its own disks.
    Elsewhere,
    /// A Pool lives here, but `zfs list` or its mount gave no answer.
    Unknown,
    Measured {
        volumes: u64,
        mirror_slots: u64,
        /// Bytes the Pool can still take. Its backing file holds them on the
        /// filesystem already, so they are free space, not Other.
        free: u64,
    },
}

/// Where a root-backed Pool would be found.
#[derive(Clone, Debug)]
pub(super) struct PoolPaths {
    backing: PathBuf,
    mount_root: PathBuf,
}

impl PoolPaths {
    pub(super) fn system() -> Self {
        Self {
            backing: PathBuf::from(machine_pool::BACKING_FILE),
            mount_root: PathBuf::from(machine_pool::MOUNT_ROOT),
        }
    }
}

/// Whether the Pool's backing file shares the Observe Store's filesystem, and
/// what its mounted root says is free.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Placement {
    Elsewhere,
    Unmounted,
    Mounted { free: u64 },
}

/// The last 60 s reading.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct SlowReading {
    pub(super) pool: PoolBytes,
    pub(super) observe_store: Option<u64>,
}

impl Default for SlowReading {
    fn default() -> Self {
        Self {
            pool: PoolBytes::Unknown,
            observe_store: None,
        }
    }
}

/// Reads the slow disk sources every 60 s into `latest` until dropped.
pub(super) async fn run(
    store: StoreRoot,
    zfs: ZfsUsageReader,
    paths: PoolPaths,
    latest: watch::Sender<SlowReading>,
) {
    let mut ticks = interval(INTERVAL);
    ticks.set_missed_tick_behavior(MissedTickBehavior::Skip);
    loop {
        ticks.tick().await;
        let usage = zfs.read().await;
        let (store, paths) = (store.clone(), paths.clone());
        let read = tokio::task::spawn_blocking(move || {
            let placement = locate_pool(store.path(), &paths);
            SlowReading {
                pool: pool_bytes(&usage, placement),
                observe_store: store_bytes(store.path()),
            }
        });
        match read.await {
            Ok(reading) => {
                latest.send_replace(reading);
            }
            Err(error) => tracing::warn!(%error, "disk usage task failed"),
        }
    }
}

/// A root-backed Pool sits in one file on the host root. When the Observe
/// Store shares that filesystem, the Pool's bytes are part of its total.
/// On a Docker-only Server the mount root is a plain directory on the same
/// filesystem, so its `statvfs` would count the root's free bytes twice.
fn locate_pool(store: &Path, paths: &PoolPaths) -> Placement {
    let Ok(store_dev) = fs::metadata(store).map(|meta| meta.dev()) else {
        return Placement::Unmounted;
    };
    match fs::metadata(&paths.backing) {
        Ok(backing) if backing.dev() == store_dev => {}
        Ok(_) => return Placement::Elsewhere,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Placement::Elsewhere,
        Err(_) => return Placement::Unmounted,
    }
    match fs::metadata(&paths.mount_root) {
        Ok(root) if root.dev() != store_dev => {}
        _ => return Placement::Unmounted,
    }
    match nix::sys::statvfs::statvfs(&paths.mount_root) {
        Ok(stat) => Placement::Mounted {
            free: stat.blocks_available().saturating_mul(stat.fragment_size()),
        },
        Err(_) => Placement::Unmounted,
    }
}

fn pool_bytes(usage: &ZfsUsage, placement: Placement) -> PoolBytes {
    let free = match placement {
        Placement::Elsewhere => return PoolBytes::Elsewhere,
        Placement::Unmounted => return PoolBytes::Unknown,
        Placement::Mounted { free } => free,
    };
    let ZfsUsage::Measured(datasets) = usage else {
        return PoolBytes::Unknown;
    };
    let (mut volumes, mut mirror_slots) = (0_u64, 0_u64);
    for (name, usage) in datasets {
        match DatasetPlace::of(name) {
            DatasetPlace::Volume(_) => volumes = volumes.saturating_add(usage.referenced),
            DatasetPlace::Slot => mirror_slots = mirror_slots.saturating_add(usage.referenced),
            DatasetPlace::Other => {}
        }
    }
    PoolBytes::Measured {
        volumes,
        mirror_slots,
        free,
    }
}

/// Bytes allocated to the Observe Store's files. Symlinks are not followed.
fn store_bytes(root: &Path) -> Option<u64> {
    let mut total = 0_u64;
    let mut dirs = vec![root.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        let entries = match fs::read_dir(&dir) {
            Ok(entries) => entries,
            Err(_) if dir == root => return None,
            Err(_) => continue,
        };
        for entry in entries.flatten() {
            let Ok(meta) = entry.metadata() else {
                continue;
            };
            if meta.is_dir() {
                dirs.push(entry.path());
            } else if meta.is_file() {
                total = total.saturating_add(meta.blocks().saturating_mul(BLOCK_BYTES));
            }
        }
    }
    Some(total)
}

/// Splits the filesystem holding the Observe Store. Free, Volumes, mirror
/// slots, the Observe Store, and Other sum to the total; Other is left out
/// when a part it would absorb was not measured.
pub(super) fn breakdown(filesystem: FsBytes, slow: SlowReading) -> DiskBreakdown {
    let (volumes, mirror_slots, pool_used, pool_free) = match slow.pool {
        PoolBytes::Elsewhere => (None, None, Some(0), 0),
        PoolBytes::Unknown => (None, None, None, 0),
        PoolBytes::Measured {
            volumes,
            mirror_slots,
            free,
        } => (
            Some(volumes),
            Some(mirror_slots),
            Some(volumes.saturating_add(mirror_slots)),
            free,
        ),
    };
    let free = filesystem
        .available
        .saturating_add(pool_free)
        .min(filesystem.total);
    let other = pool_used.zip(slow.observe_store).map(|(pool, store)| {
        filesystem
            .total
            .saturating_sub(free)
            .saturating_sub(pool)
            .saturating_sub(store)
    });
    DiskBreakdown {
        total: filesystem.total,
        free,
        volumes,
        mirror_slots,
        observe_store: slow.observe_store,
        other,
    }
}

#[cfg(test)]
mod tests {
    use std::{collections::BTreeMap, fs};

    use ployz_core::metrics::DiskBreakdown;

    use super::{
        DatasetPlace, Placement, PoolBytes, SlowReading, breakdown, pool_bytes, store_bytes,
    };
    use crate::{
        observe::metrics::{
            host::FsBytes,
            zfs_usage::{DatasetUsage, ZfsUsage, ZfsUsageGap},
        },
        test_dir::TestDir,
    };

    const GIB: u64 = 1 << 30;

    fn filesystem(total: u64, available: u64) -> FsBytes {
        FsBytes {
            total,
            used: total - available,
            available,
        }
    }

    fn parts_sum(disk: &DiskBreakdown) -> u64 {
        disk.free
            + disk.volumes.unwrap_or(0)
            + disk.mirror_slots.unwrap_or(0)
            + disk.observe_store.unwrap_or(0)
            + disk.other.unwrap_or(0)
    }

    fn listing(datasets: &[(&str, u64)]) -> ZfsUsage {
        ZfsUsage::Measured(
            datasets
                .iter()
                .map(|(name, referenced)| {
                    (
                        (*name).to_owned(),
                        DatasetUsage {
                            referenced: *referenced,
                            refquota: None,
                        },
                    )
                })
                .collect::<BTreeMap<_, _>>(),
        )
    }

    #[test]
    fn datasets_are_placed_by_their_path_under_the_pool() {
        assert_eq!(
            DatasetPlace::of("ployz/ployz/prod_db"),
            DatasetPlace::Volume("prod_db")
        );
        assert_eq!(
            DatasetPlace::of("ployz/ployz-mirror/prod_db"),
            DatasetPlace::Slot
        );
        assert_eq!(
            DatasetPlace::of("ployz/ployz-mirror/prod_db/fs"),
            DatasetPlace::Slot
        );
        for other in [
            "ployz",
            "ployz/ployz",
            "ployz/ployz/db/child",
            "ployz/elsewhere",
        ] {
            assert_eq!(DatasetPlace::of(other), DatasetPlace::Other, "{other}");
        }
    }

    #[test]
    fn a_root_backed_pool_splits_into_volumes_slots_store_and_other_that_sum_to_the_total() {
        let usage = listing(&[
            ("ployz", 100),
            ("ployz/ployz", 200),
            ("ployz/ployz/prod_db", 3 * GIB),
            ("ployz/ployz/prod_cache", GIB),
            ("ployz/ployz-mirror/prod_web", 2 * GIB),
            ("ployz/ployz-mirror/prod_web/fs", GIB),
        ]);
        let pool = pool_bytes(&usage, Placement::Mounted { free: 10 * GIB });
        assert_eq!(
            pool,
            PoolBytes::Measured {
                volumes: 4 * GIB,
                mirror_slots: 3 * GIB,
                free: 10 * GIB,
            }
        );

        // The Pool's backing file holds 20 GiB of the root's 100 GiB.
        let disk = breakdown(
            filesystem(100 * GIB, 30 * GIB),
            SlowReading {
                pool,
                observe_store: Some(GIB),
            },
        );
        assert_eq!(disk.free, 40 * GIB, "the Pool's free bytes are free");
        assert_eq!(disk.other, Some(52 * GIB));
        assert_eq!(parts_sum(&disk), disk.total);
    }

    #[test]
    fn a_docker_only_server_leaves_volumes_unmeasured_and_still_sums() {
        for usage in [
            ZfsUsage::Gap(ZfsUsageGap::NotInstalled),
            listing(&[("tank/ployz/db", 5 * GIB)]),
        ] {
            let pool = pool_bytes(&usage, Placement::Elsewhere);
            assert_eq!(pool, PoolBytes::Elsewhere);
            let disk = breakdown(
                filesystem(100 * GIB, 30 * GIB),
                SlowReading {
                    pool,
                    observe_store: Some(GIB),
                },
            );
            assert_eq!(
                disk,
                DiskBreakdown {
                    total: 100 * GIB,
                    free: 30 * GIB,
                    volumes: None,
                    mirror_slots: None,
                    observe_store: Some(GIB),
                    other: Some(69 * GIB),
                }
            );
        }
    }

    #[test]
    fn an_unanswered_zfs_leaves_volumes_and_other_unmeasured() {
        for (usage, placement) in [
            (
                ZfsUsage::Gap(ZfsUsageGap::TimedOut),
                Placement::Mounted { free: GIB },
            ),
            (listing(&[("ployz/ployz/db", GIB)]), Placement::Unmounted),
        ] {
            let disk = breakdown(
                filesystem(100 * GIB, 30 * GIB),
                SlowReading {
                    pool: pool_bytes(&usage, placement),
                    observe_store: Some(GIB),
                },
            );
            assert_eq!((disk.volumes, disk.mirror_slots), (None, None));
            assert_eq!(disk.other, None, "Other would swallow the Volumes");
            assert_eq!(disk.free, 30 * GIB);
        }
    }

    #[test]
    fn the_store_counts_nested_files_and_a_missing_store_is_unmeasured() {
        let dir = TestDir::new("ployzd-metrics-disk");
        let store = dir.0.join("store");
        assert_eq!(store_bytes(&store), None);

        fs::create_dir_all(store.join("metrics/raw")).unwrap();
        assert_eq!(store_bytes(&store), Some(0));
        fs::write(store.join("metrics/raw/2025100908"), vec![1; 64 * 1024]).unwrap();
        assert!(store_bytes(&store).unwrap() >= 64 * 1024);
    }
}
