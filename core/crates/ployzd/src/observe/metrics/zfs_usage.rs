//! Volume usage from `zfs list`. ZFS commands can hang on a sick Pool, so
//! every run has a deadline and at most one runs at a time.

use std::{
    collections::BTreeMap,
    io,
    num::NonZeroU64,
    path::PathBuf,
    sync::{Arc, Mutex, PoisonError},
    time::Duration,
};

use tokio::process::Command;

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
    running: tokio::sync::Mutex<()>,
    last: Mutex<Arc<ZfsUsage>>,
}

impl ZfsUsageReader {
    pub(crate) fn new(program: PathBuf, timeout: Duration) -> Self {
        Self {
            program,
            timeout,
            running: tokio::sync::Mutex::new(()),
            last: Mutex::new(Arc::new(ZfsUsage::Gap(ZfsUsageGap::NotYetRead))),
        }
    }

    /// Runs `zfs list`, or returns the last result while another call runs it.
    pub(crate) async fn read(&self) -> Arc<ZfsUsage> {
        let Ok(_running) = self.running.try_lock() else {
            return self.last();
        };
        let usage = Arc::new(self.list().await);
        *self.last.lock().unwrap_or_else(PoisonError::into_inner) = Arc::clone(&usage);
        usage
    }

    fn last(&self) -> Arc<ZfsUsage> {
        Arc::clone(&self.last.lock().unwrap_or_else(PoisonError::into_inner))
    }

    async fn list(&self) -> ZfsUsage {
        let mut command = Command::new(&self.program);
        command
            .args(["list", "-Hp", "-o", "name,referenced,refquota"])
            .kill_on_drop(true);
        match tokio::time::timeout(self.timeout, command.output()).await {
            Ok(Ok(output)) if output.status.success() => String::from_utf8(output.stdout)
                .ok()
                .and_then(|stdout| parse(&stdout))
                .map_or(ZfsUsage::Gap(ZfsUsageGap::Unparsable), ZfsUsage::Measured),
            Ok(Err(error)) if error.kind() == io::ErrorKind::NotFound => {
                ZfsUsage::Gap(ZfsUsageGap::NotInstalled)
            }
            Ok(Ok(_) | Err(_)) => ZfsUsage::Gap(ZfsUsageGap::Failed),
            Err(_) => ZfsUsage::Gap(ZfsUsageGap::TimedOut),
        }
    }
}

fn parse(output: &str) -> Option<BTreeMap<String, DatasetUsage>> {
    output.lines().map(parse_line).collect()
}

fn parse_line(line: &str) -> Option<(String, DatasetUsage)> {
    let mut fields = line.split('\t');
    let (Some(name), Some(referenced), Some(refquota), None) =
        (fields.next(), fields.next(), fields.next(), fields.next())
    else {
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
        },
    ))
}

#[cfg(test)]
mod tests {
    use std::{fs, os::unix::fs::PermissionsExt, path::Path};

    use super::*;

    fn usage(referenced: u64, refquota: u64) -> DatasetUsage {
        DatasetUsage {
            referenced,
            refquota: NonZeroU64::new(refquota),
        }
    }

    fn fake_zfs(dir: &Path, body: &str) -> PathBuf {
        let path = dir.join("zfs");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[test]
    fn referenced_and_refquota_are_read_per_dataset() {
        let parsed = parse(
            "ployz\t25600\t0\n\
             ployz/volumes\t24576\t-\n\
             ployz/volumes/db\t104882176\t1073741824\n",
        )
        .unwrap();
        assert_eq!(
            parsed,
            BTreeMap::from([
                ("ployz".to_owned(), usage(25600, 0)),
                ("ployz/volumes".to_owned(), usage(24576, 0)),
                (
                    "ployz/volumes/db".to_owned(),
                    usage(104_882_176, 1_073_741_824)
                ),
            ])
        );
    }

    #[test]
    fn writing_100_mb_into_a_volume_grows_its_referenced_bytes_by_100_mb() {
        // Captured on an Incus Machine before and after `dd` wrote 100 MiB into a
        // Managed volume.
        const VOLUME: &str =
            "ployz/ployz/verify-production_vol-b611d842-26d8-42dd-9f04-f1e6bdd13df3";
        let before = parse(include_str!("fixtures/zfs_list_before_write.txt")).unwrap();
        let after = parse(include_str!("fixtures/zfs_list_after_write.txt")).unwrap();

        let (before, after) = (before.get(VOLUME).unwrap(), after.get(VOLUME).unwrap());
        assert_eq!(after.refquota, NonZeroU64::new(1_000_000_000));
        let grown = after.referenced - before.referenced;
        assert!(
            (100 << 20..101 << 20).contains(&grown),
            "grew {grown} bytes"
        );
    }

    #[test]
    fn dash_none_and_zero_refquota_mean_no_quota() {
        for refquota in ["-", "none", "0"] {
            assert_eq!(
                parse(&format!("pool/a\t10\t{refquota}\n")),
                Some(BTreeMap::from([("pool/a".to_owned(), usage(10, 0))])),
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
            "pool/a\t10\n",
            "pool/a\t10\t-\textra\n",
            "pool/a\t10K\t-\n",
            "pool/a\t10\t1G\n",
        ] {
            assert_eq!(parse(output), None, "{output:?}");
        }
    }

    #[tokio::test]
    async fn a_listing_becomes_a_measurement() {
        let dir = tempfile::tempdir().unwrap();
        let zfs = fake_zfs(dir.path(), r#"printf 'pool/a\t10\t-\n'"#);
        let reader = ZfsUsageReader::new(zfs, ZFS_LIST_TIMEOUT);
        assert_eq!(
            *reader.read().await,
            ZfsUsage::Measured(BTreeMap::from([("pool/a".to_owned(), usage(10, 0))]))
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
                r#"echo run >> {runs}; n=$(wc -l < {runs}); sleep 0.5; printf 'pool/a\t%s\t-\n' "$n""#,
                runs = runs.display()
            ),
        );
        let reader = ZfsUsageReader::new(zfs, ZFS_LIST_TIMEOUT);
        let first = reader.read().await;

        let (running, concurrent) = tokio::join!(reader.read(), reader.read());

        assert!(Arc::ptr_eq(&concurrent, &first));
        assert_eq!(
            *running,
            ZfsUsage::Measured(BTreeMap::from([("pool/a".to_owned(), usage(2, 0))]))
        );
        assert_eq!(fs::read_to_string(&runs).unwrap().lines().count(), 2);
    }
}
