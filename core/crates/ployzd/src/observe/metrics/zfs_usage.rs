//! Volume usage from `zfs list`. ZFS commands can hang on a sick Pool, so
//! every run has a deadline and at most one runs at a time.

use std::{
    collections::BTreeMap,
    io,
    num::NonZeroU64,
    path::{Path, PathBuf},
    process::Stdio,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use tokio::{io::AsyncReadExt, process::Command, sync::oneshot};

pub(crate) const ZFS_LIST_TIMEOUT: Duration = Duration::from_secs(5);

/// One `zfs list` result: every dataset's usage, or why there is none.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum ZfsUsage {
    Measured(BTreeMap<String, DatasetUsage>),
    Gap(ZfsUsageGap),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct DatasetUsage {
    /// Bytes the dataset references, excluding snapshots and children.
    pub(crate) referenced: u64,
    pub(crate) refquota: Option<NonZeroU64>,
    /// Bytes the dataset can still take; on a Pool's root, the Pool's free
    /// space.
    pub(crate) available: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum ZfsUsageGap {
    /// No `zfs list` has finished yet.
    NotYetRead,
    NotInstalled,
    TimedOut,
    Failed,
    Unparsable,
}

pub(crate) struct ZfsUsageReader {
    program: PathBuf,
    timeout: Duration,
    running: Arc<tokio::sync::Mutex<()>>,
    last: Mutex<Arc<ZfsUsage>>,
}

impl ZfsUsageReader {
    pub(crate) fn new(program: PathBuf, timeout: Duration) -> Self {
        Self {
            program,
            timeout,
            running: Arc::new(tokio::sync::Mutex::new(())),
            last: Mutex::new(Arc::new(ZfsUsage::Gap(ZfsUsageGap::NotYetRead))),
        }
    }

    /// Runs `zfs list`, or returns the last result while another call runs it.
    pub(crate) async fn read(&self) -> Arc<ZfsUsage> {
        let Ok(running) = Arc::clone(&self.running).try_lock_owned() else {
            return self.last();
        };
        let (sender, receiver) = oneshot::channel();
        let (program, timeout) = (self.program.clone(), self.timeout);
        // A killed `zfs` stuck in kernel I/O can outlive the deadline; the run
        // stays in flight until it exits so stuck processes never pile up.
        tokio::spawn(async move {
            let _running = running;
            list(&program, timeout, sender).await;
        });
        let usage = Arc::new(receiver.await.unwrap_or(ZfsUsage::Gap(ZfsUsageGap::Failed)));
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Arc::clone(&usage);
        usage
    }

    fn last(&self) -> Arc<ZfsUsage> {
        Arc::clone(&self.last.lock().unwrap_or_else(PoisonError::into_inner))
    }
}

async fn list(program: &Path, timeout: Duration, result: oneshot::Sender<ZfsUsage>) {
    let spawned = Command::new(program)
        .args(["list", "-Hp", "-o", "name,referenced,refquota,available"])
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .kill_on_drop(true)
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            let _ = result.send(ZfsUsage::Gap(ZfsUsageGap::NotInstalled));
            return;
        }
        Err(_) => {
            let _ = result.send(ZfsUsage::Gap(ZfsUsageGap::Failed));
            return;
        }
    };
    let mut stdout = Vec::new();
    let finished = tokio::time::timeout(timeout, async {
        let mut pipe = child.stdout.take()?;
        let (read, status) = tokio::join!(pipe.read_to_end(&mut stdout), child.wait());
        read.ok()?;
        status.ok()
    })
    .await;
    let usage = match finished {
        Ok(Some(status)) if status.success() => String::from_utf8(stdout)
            .ok()
            .and_then(|stdout| parse(&stdout))
            .map_or(ZfsUsage::Gap(ZfsUsageGap::Unparsable), ZfsUsage::Measured),
        Ok(_) => ZfsUsage::Gap(ZfsUsageGap::Failed),
        Err(_) => ZfsUsage::Gap(ZfsUsageGap::TimedOut),
    };
    let _ = result.send(usage);
    if finished.is_err() {
        let _ = child.start_kill();
        let _ = child.wait().await;
    }
}

fn parse(output: &str) -> Option<BTreeMap<String, DatasetUsage>> {
    output.lines().map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<(String, DatasetUsage)> {
    let mut fields = line.split('\t');
    let (Some(name), Some(referenced), Some(refquota), Some(available), None) = (
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
        fields.next(),
    ) else {
        return None;
    };
    let refquota = match refquota {
        "-" | "none" => None,
        bytes => NonZeroU64::new(bytes.parse().ok()?),
    };
    Some((
        name.to_owned(),
        DatasetUsage {
            referenced: referenced.parse().ok()?,
            refquota,
            available: available.parse().ok()?,
        },
    ))
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, path::Path};

    use super::*;

    fn usage(referenced: u64, refquota: u64, available: u64) -> DatasetUsage {
        DatasetUsage {
            referenced,
            refquota: NonZeroU64::new(refquota),
            available,
        }
    }

    fn fake_zfs(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("zfs");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn referenced_refquota_and_available_are_read_per_dataset() {
        let parsed = parse(
            "ployz\t25600\t0\t9000\n\
             ployz/volumes\t24576\t-\t9000\n\
             ployz/volumes/db\t104882176\t1073741824\t900\n",
        )
        .unwrap();
        assert_eq!(
            parsed,
            BTreeMap::from([
                ("ployz".to_owned(), usage(25600, 0, 9000)),
                ("ployz/volumes".to_owned(), usage(24576, 0, 9000)),
                (
                    "ployz/volumes/db".to_owned(),
                    usage(104_882_176, 1_073_741_824, 900)
                ),
            ])
        );
    }

    #[test]
    fn writing_100_mb_into_a_volume_moves_100_mb_from_pool_free_to_the_volume() {
        // Captured on an Incus Machine before and after `dd` wrote 100 MiB into a
        // Managed volume.
        const VOLUME: &str =
            "ployz/ployz/verify-production_vol-5f155ddf-f688-49c3-92b3-5c6e88f99707";
        let before = parse(include_str!("fixtures/zfs_list_before_write.txt")).unwrap();
        let after = parse(include_str!("fixtures/zfs_list_after_write.txt")).unwrap();

        let volume = (before.get(VOLUME).unwrap(), after.get(VOLUME).unwrap());
        assert_eq!(volume.1.refquota, NonZeroU64::new(5_000_000_000));
        let grown = volume.1.referenced - volume.0.referenced;
        assert!(
            (100 << 20..101 << 20).contains(&grown),
            "grew {grown} bytes"
        );

        let pool = (before.get("ployz").unwrap(), after.get("ployz").unwrap());
        let taken = pool.0.available - pool.1.available;
        assert!(
            (100 << 20..101 << 20).contains(&taken),
            "the Pool lost {taken} free bytes"
        );
    }

    #[test]
    fn dash_none_and_zero_refquota_mean_no_quota() {
        for refquota in ["-", "none", "0"] {
            assert_eq!(
                parse(&format!("pool/a\t10\t{refquota}\t5\n")),
                Some(BTreeMap::from([("pool/a".to_owned(), usage(10, 0, 5))])),
                "refquota {refquota}"
            );
        }
    }

    #[test]
    fn no_datasets_is_an_empty_measurement() {
        assert_eq!(parse(""), Some(BTreeMap::new()));
    }

    #[test]
    fn a_malformed_line_rejects_the_whole_listing() {
        for output in [
            "pool/a\t10\t-\n",
            "pool/a\t10\t-\t5\textra\n",
            "pool/a\t10K\t-\t5\n",
            "pool/a\t10\t1G\t5\n",
            "pool/a\t10\t-\t5G\n",
        ] {
            assert_eq!(parse(output), None, "{output:?}");
        }
    }

    #[tokio::test]
    async fn a_listing_becomes_a_measurement() {
        let dir = tempfile::tempdir().unwrap();
        let zfs = fake_zfs(dir.path(), r#"printf 'pool/a\t10\t-\t5\n'"#);
        let reader = ZfsUsageReader::new(zfs, ZFS_LIST_TIMEOUT);
        assert_eq!(
            *reader.read().await,
            ZfsUsage::Measured(BTreeMap::from([("pool/a".to_owned(), usage(10, 0, 5))]))
        );
    }

    #[tokio::test]
    async fn a_hung_zfs_times_out_into_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let zfs = fake_zfs(dir.path(), "sleep 30");
        let reader = ZfsUsageReader::new(zfs, Duration::from_millis(200));
        let started = std::time::Instant::now();
        assert_eq!(*reader.read().await, ZfsUsage::Gap(ZfsUsageGap::TimedOut));
        assert!(started.elapsed() < Duration::from_secs(5));
    }

    #[tokio::test]
    async fn a_missing_binary_is_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let reader = ZfsUsageReader::new(dir.path().join("zfs"), ZFS_LIST_TIMEOUT);
        assert_eq!(
            *reader.read().await,
            ZfsUsage::Gap(ZfsUsageGap::NotInstalled)
        );
    }

    #[tokio::test]
    async fn a_failing_zfs_is_a_gap() {
        let dir = tempfile::tempdir().unwrap();
        let zfs = fake_zfs(dir.path(), "echo 'no pools' >&2; exit 1");
        let reader = ZfsUsageReader::new(zfs, ZFS_LIST_TIMEOUT);
        assert_eq!(*reader.read().await, ZfsUsage::Gap(ZfsUsageGap::Failed));
    }

    #[tokio::test]
    async fn a_call_during_a_run_returns_the_last_result_without_spawning() {
        let dir = tempfile::tempdir().unwrap();
        let runs = dir.path().join("runs");
        let zfs = fake_zfs(
            dir.path(),
            &format!(
                r#"echo run >> {runs}; n=$(wc -l < {runs}); sleep 0.5; printf 'pool/a\t%s\t-\t5\n' "$n""#,
                runs = runs.display()
            ),
        );
        let reader = ZfsUsageReader::new(zfs, ZFS_LIST_TIMEOUT);
        let first = reader.read().await;

        let (running, concurrent) = tokio::join!(reader.read(), reader.read());

        assert!(Arc::ptr_eq(&concurrent, &first));
        assert_eq!(
            *running,
            ZfsUsage::Measured(BTreeMap::from([("pool/a".to_owned(), usage(2, 0, 5))]))
        );
        assert_eq!(fs::read_to_string(&runs).unwrap().lines().count(), 2);
    }
}
