//! The Log Store's shape on disk.
//!
//! ```text
//! <DockerRootDir>/ployz-observe/        other version dirs here are deleted
//!   v1/                                 StoreRoot; other entries are ignored
//!     containers/
//!       <cid>/                          one per container Ployz ran
//!         meta.json                     ContainerMeta
//!         gaps.jsonl                    one Gap per line
//!         scanned                       seq of the newest file the damage
//!                                       scan has read past
//!         first-set.json                FirstSeen, until max-file judges it
//!         <seq>-<ino>.log               a hardlink to one Docker log file, raw
//!     metrics/
//!       raw/
//!         <hour>.jsonl                  every metric row of one hour, one
//!                                       per line; <hour> is its start in
//!                                       Unix seconds
//!       rollup/
//!         <hour>.jsonl                  that hour folded to 5 minute points
//!                                       per Service and Deployment, plus
//!                                       the host
//!     outbox/
//!       crash/
//!         <seq>                         one crash marker waiting for Cloud,
//!                                       named by its zero-padded seq
//!         last_seq                      the newest seq handed out
//!         dropped                       entries lost to the cap or refused
//!                                       by Cloud
//!       pageviews/
//!         <seq>.jsonl                   up to 100 pageview, bots and gap
//!                                       rows waiting for Cloud, one per line
//!         last_seq                      the newest seq handed out
//!         dropped                       rows lost to the cap or refused
//!     analytics/
//!       salts.json                      the daily salts of the last ack
//! ```
//!
//! The store sits under `DockerRootDir` because a hardlink only works on the
//! filesystem that holds Docker's files. `seq` orders a container's files
//! oldest first; `ino` is the inode Docker wrote, so a rescan can tell which
//! of Docker's files the store already holds.

use std::{
    fmt, fs, io,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

use ployz_core::ContainerId;
use serde::{Deserialize, Serialize};

const STORE_DIR: &str = "ployz-observe";
pub const VERSION_DIR: &str = "v1";
pub const CONTAINERS_DIR: &str = "containers";
pub const META_FILE: &str = "meta.json";
pub const META_TEMP_FILE: &str = "meta.tmp";
pub const GAPS_FILE: &str = "gaps.jsonl";
pub const LINKING_FILE: &str = "linking.tmp";
pub const SCANNED_FILE: &str = "scanned";
pub const SCANNED_TEMP_FILE: &str = "scanned.tmp";
pub const FIRST_SET_FILE: &str = "first-set.json";
pub const FIRST_SET_TEMP_FILE: &str = "first-set.tmp";
pub const METRICS_DIR: &str = "metrics";
pub const RAW_DIR: &str = "raw";
pub const ROLLUP_DIR: &str = "rollup";
const OUTBOX_DIR: &str = "outbox";
const CRASH_DIR: &str = "crash";
const PAGEVIEWS_DIR: &str = "pageviews";
const ANALYTICS_DIR: &str = "analytics";
const SALTS_FILE: &str = "salts.json";
const LOG_SUFFIX: &str = ".log";
const SEGMENT_SUFFIX: &str = ".jsonl";
const HOUR_SECS: u64 = 60 * 60;

/// The versioned root of the Log Store, `<DockerRootDir>/ployz-observe/v1`.
#[derive(Clone, Debug)]
pub struct StoreRoot(PathBuf);

impl StoreRoot {
    #[must_use]
    pub fn under(docker_root: &Path) -> Self {
        Self(docker_root.join(STORE_DIR).join(VERSION_DIR))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    #[must_use]
    pub fn containers(&self) -> PathBuf {
        self.0.join(CONTAINERS_DIR)
    }

    #[must_use]
    pub fn container(&self, id: &ContainerId) -> PathBuf {
        self.containers().join(id.as_str())
    }

    #[must_use]
    pub fn raw_metrics(&self) -> PathBuf {
        self.0.join(METRICS_DIR).join(RAW_DIR)
    }

    #[must_use]
    pub fn rollup_metrics(&self) -> PathBuf {
        self.0.join(METRICS_DIR).join(ROLLUP_DIR)
    }

    #[must_use]
    pub fn crash_outbox(&self) -> PathBuf {
        self.0.join(OUTBOX_DIR).join(CRASH_DIR)
    }

    #[must_use]
    pub fn pageviews_outbox(&self) -> PathBuf {
        self.0.join(OUTBOX_DIR).join(PAGEVIEWS_DIR)
    }

    #[must_use]
    pub fn salts(&self) -> PathBuf {
        self.0.join(ANALYTICS_DIR).join(SALTS_FILE)
    }

    /// `<DockerRootDir>/ployz-observe`, which holds the version dirs.
    #[must_use]
    pub fn base(&self) -> &Path {
        self.0.parent().unwrap_or(&self.0)
    }

    /// Creates the store, first deleting any version dir that is not `v1`.
    ///
    /// The store is disposable, so a format this release does not know is
    /// thrown away rather than migrated. Only names shaped like a version
    /// (`v` and digits) are deleted.
    ///
    /// # Errors
    ///
    /// Returns an error when a foreign version dir cannot be removed, the
    /// store dirs cannot be created, or one of them is a symlink or not a
    /// dir, since cleanup deletes beneath them.
    pub fn prepare(&self) -> io::Result<()> {
        let parent = self.base();
        create_store_dir(parent)?;
        for entry in fs::read_dir(parent)? {
            let entry = entry?;
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if name != VERSION_DIR && is_version_name(name) {
                tracing::info!(
                    version = name,
                    "deleting a Log Store this release cannot read"
                );
                fs::remove_dir_all(entry.path())?;
            }
        }
        create_store_dir(&self.0)?;
        create_store_dir(&self.containers())
    }
}

fn is_version_name(name: &str) -> bool {
    name.strip_prefix('v')
        .is_some_and(|digits| !digits.is_empty() && digits.bytes().all(|b| b.is_ascii_digit()))
}

pub(super) fn create_store_dir(path: &Path) -> io::Result<()> {
    create_private_dir(path)?;
    if fs::symlink_metadata(path)?.is_dir() {
        Ok(())
    } else {
        Err(io::Error::other(format!(
            "{} is not a dir; refusing to keep the Log Store there",
            path.display()
        )))
    }
}

/// Creates `path` as a 0700 dir when it does not exist.
///
/// # Errors
///
/// Returns an error when the dir cannot be created.
pub fn create_private_dir(path: &Path) -> io::Result<()> {
    match fs::DirBuilder::new().mode(0o700).create(path) {
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => Ok(()),
        result => result,
    }
}

/// The name of one stored log file, `<seq>-<ino>.log`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct LogFileName {
    pub seq: u64,
    pub ino: u64,
}

impl LogFileName {
    /// Parses a stored log file name. Only the exact form this type prints
    /// parses, so `01-2.log` or `1-2.log.bak` are someone else's files.
    #[must_use]
    pub fn parse(name: &[u8]) -> Option<Self> {
        let name = std::str::from_utf8(name).ok()?;
        let stem = name.strip_suffix(LOG_SUFFIX)?;
        let (seq, ino) = stem.split_once('-')?;
        let parsed = Self {
            seq: canonical_u64(seq)?,
            ino: canonical_u64(ino)?,
        };
        Some(parsed)
    }
}

fn canonical_u64(digits: &str) -> Option<u64> {
    if digits.is_empty()
        || !digits.bytes().all(|b| b.is_ascii_digit())
        || (digits.len() > 1 && digits.starts_with('0'))
    {
        return None;
    }
    digits.parse().ok()
}

impl fmt::Display for LogFileName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}-{}{LOG_SUFFIX}", self.seq, self.ino)
    }
}

/// The name of one raw metric segment, `<start>.jsonl`, where `start` is
/// the Unix second its hour begins.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct SegmentName {
    start_secs: u64,
}

impl SegmentName {
    /// The segment holding a row stamped `ts` Unix nanoseconds, or `None`
    /// before 1970.
    #[must_use]
    pub fn containing(ts: i64) -> Option<Self> {
        let secs = u64::try_from(ts.div_euclid(1_000_000_000)).ok()?;
        Some(Self {
            start_secs: secs - secs % HOUR_SECS,
        })
    }

    /// Parses a segment name. Only the exact form this type prints parses.
    #[must_use]
    pub fn parse(name: &[u8]) -> Option<Self> {
        let name = std::str::from_utf8(name).ok()?;
        let start_secs = canonical_u64(name.strip_suffix(SEGMENT_SUFFIX)?)?;
        (start_secs % HOUR_SECS == 0).then_some(Self { start_secs })
    }

    #[must_use]
    pub fn start_secs(self) -> u64 {
        self.start_secs
    }

    /// The first Unix second after this segment's hour.
    #[must_use]
    pub fn end_secs(self) -> u64 {
        self.start_secs.saturating_add(HOUR_SECS)
    }
}

impl fmt::Display for SegmentName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}{SEGMENT_SUFFIX}", self.start_secs)
    }
}

/// What a container was, read from its labels when the store first saw it.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ContainerKind {
    Service,
    PreDeployHook,
    /// Ingress, the unregistry, Corrosion, and other containers Ployz runs
    /// for itself. They share the one cap with apps.
    System,
}

/// The contents of `meta.json`.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct ContainerMeta {
    pub service: Option<String>,
    pub namespace: Option<String>,
    pub deployment: Option<String>,
    pub replica: String,
    pub kind: ContainerKind,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub exit_code: Option<i64>,
    pub oom_killed: bool,
}

/// Why a span of a container's output is missing from the store.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GapReason {
    /// Docker deleted a file before the store linked it.
    NotCaptured,
    /// A frame the decoder could not parse.
    Corrupt,
}

/// One line of `gaps.jsonl`. `from` and `to` are Unix nanoseconds; output
/// between them may be missing.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct Gap {
    pub from: i64,
    pub to: i64,
    pub reason: GapReason,
}

#[cfg(test)]
mod tests {
    use super::{LogFileName, SegmentName, StoreRoot};
    use crate::test_dir::TestDir;

    #[test]
    fn log_file_names_parse_only_in_their_printed_form() {
        let name = LogFileName {
            seq: 12,
            ino: 4_000_123,
        };
        assert_eq!(name.to_string(), "12-4000123.log");
        assert_eq!(LogFileName::parse(b"12-4000123.log"), Some(name));
        for foreign in [
            &b"012-4.log"[..],
            b"1-04.log",
            b"1-2.log.bak",
            b"1-2",
            b"-2.log",
            b"1-.log",
            b"1--2.log",
            b"+1-2.log",
            b"meta.json",
            b"99999999999999999999-1.log",
        ] {
            assert_eq!(LogFileName::parse(foreign), None, "{foreign:?}");
        }
    }

    #[test]
    fn segment_names_cover_one_hour_and_parse_only_in_their_printed_form() {
        let name = SegmentName::containing(1_760_000_123_456_789_000).unwrap();
        assert_eq!(name.to_string(), "1759996800.jsonl");
        assert_eq!(
            (name.start_secs(), name.end_secs()),
            (1_759_996_800, 1_760_000_400)
        );
        assert_eq!(
            SegmentName::containing(1_759_996_800_000_000_000),
            Some(name)
        );
        assert_eq!(
            SegmentName::containing(1_760_000_399_999_999_999),
            Some(name)
        );
        assert_eq!(SegmentName::parse(b"1759996800.jsonl"), Some(name));
        assert_eq!(SegmentName::containing(-1), None);
        for foreign in [
            &b"1759996801.jsonl"[..],
            b"01759996800.jsonl",
            b"1759996800.jsonl.tmp",
            b"1759996800",
            b".jsonl",
            b"1-2.log",
        ] {
            assert_eq!(SegmentName::parse(foreign), None, "{foreign:?}");
        }
    }

    #[test]
    fn prepare_replaces_unknown_versions_and_leaves_other_entries() {
        let docker = TestDir::new("ployzd-observe-layout");
        let parent = docker.0.join("ployz-observe");
        std::fs::create_dir_all(parent.join("v2/containers/x")).unwrap();
        std::fs::create_dir_all(parent.join("notes")).unwrap();
        std::fs::write(parent.join("v1x"), b"keep").unwrap();
        let root = StoreRoot::under(&docker.0);
        root.prepare().unwrap();
        root.prepare().unwrap();
        assert!(!parent.join("v2").exists());
        assert!(parent.join("notes").is_dir());
        assert!(parent.join("v1x").is_file());
        assert!(root.containers().is_dir());
    }

    #[test]
    fn prepare_refuses_a_symlinked_store_dir() {
        let docker = TestDir::new("ployzd-observe-layout-symlink");
        let elsewhere = docker.0.join("elsewhere");
        std::fs::create_dir_all(elsewhere.join("containers")).unwrap();
        std::fs::create_dir_all(docker.0.join("ployz-observe")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, docker.0.join("ployz-observe/v1")).unwrap();
        assert!(StoreRoot::under(&docker.0).prepare().is_err());
    }
}
