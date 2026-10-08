//! The Log Store's shape on disk.
//!
//! ```text
//! <DockerRootDir>/ployz-observe/        other version dirs here are deleted
//!   v1/                                 StoreRoot; other entries are ignored
//!     containers/
//!       <cid>/                          one per container Ployz ran
//!         meta.json                     ContainerMeta
//!         gaps.jsonl                    one Gap per line
//!         <seq>-<ino>.log               a hardlink to one Docker log file, raw
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
/// The store's version dir under `<DockerRootDir>/ployz-observe`.
pub const VERSION_DIR: &str = "v1";
/// The dir of container dirs under the version dir.
pub const CONTAINERS_DIR: &str = "containers";
/// A container's metadata file.
pub const META_FILE: &str = "meta.json";
/// The name `atomic_write` gives `meta.json` while it writes it.
pub const META_TEMP_FILE: &str = "meta.tmp";
/// A container's gap list.
pub const GAPS_FILE: &str = "gaps.jsonl";
/// The name a log file takes while the harvester checks which inode it got.
pub const LINKING_FILE: &str = "linking.tmp";
const LOG_SUFFIX: &str = ".log";

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

fn create_store_dir(path: &Path) -> io::Result<()> {
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
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
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
    /// The container's name. Ployz labels no replica index, and the name is
    /// what tells two replicas of one service apart.
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
    use super::{LogFileName, StoreRoot};
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
