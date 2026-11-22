//! The observe store's shape on disk.
//!
//! ```text
//! <data_dir>/observe/                   other version dirs here are deleted
//!   v1/                                 StoreRoot; other entries are ignored
//!     outbox/
//!       crash/
//!         <seq>                         one crash marker waiting for Cloud,
//!                                       named by its zero-padded seq
//!         last_seq                      the newest seq handed out
//!         dropped                       entries lost to the cap or refused
//!                                       by Cloud
//!     settled/
//!       <cid>-<exit|oom>-<ts>           an exit or OOM kill the harvester
//!                                       settled, kept while a resumed
//!                                       Docker event stream can repeat it
//!       <cid>-<exit|oom>-<ts>.pending   one whose crash is not queued yet
//!     resume-from                       the newest Docker event's time, where
//!                                       a restarted harvester resumes
//! ```

use std::{
    fs, io,
    os::unix::fs::DirBuilderExt,
    path::{Path, PathBuf},
};

const STORE_DIR: &str = "observe";
const VERSION_DIR: &str = "v1";

const OUTBOX_DIR: &str = "outbox";
const CRASH_DIR: &str = "crash";
const RETIRED_OUTBOXES: [&str; 2] = ["pageviews", "requests"];
const RETIRED_DIRS: [&str; 2] = ["analytics", "metrics"];
const SETTLED_DIR: &str = "settled";
const RESUME_FILE: &str = "resume-from";
/// Unix nanoseconds in a second, the unit metric rows are stamped in.
pub(crate) const NANOS_PER_SEC: i64 = 1_000_000_000;

/// The versioned root of the observe store, `<data_dir>/observe/v1`.
#[derive(Clone, Debug)]
pub struct StoreRoot(PathBuf);

impl StoreRoot {
    #[must_use]
    pub fn under(data_dir: &Path) -> Self {
        Self(data_dir.join(STORE_DIR).join(VERSION_DIR))
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.0
    }

    #[must_use]
    pub fn crash_outbox(&self) -> PathBuf {
        self.0.join(OUTBOX_DIR).join(CRASH_DIR)
    }

    #[must_use]
    pub fn settled(&self) -> PathBuf {
        self.0.join(SETTLED_DIR)
    }

    #[must_use]
    pub fn resume_from(&self) -> PathBuf {
        self.0.join(RESUME_FILE)
    }

    /// `<data_dir>/observe`, which holds the version dirs.
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
    /// dir, since retired dirs are deleted beneath them.
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
                    "deleting an observe store this release cannot read"
                );
                fs::remove_dir_all(entry.path())?;
            }
        }
        create_store_dir(&self.0)?;
        let retired = RETIRED_OUTBOXES
            .iter()
            .map(|name| self.0.join(OUTBOX_DIR).join(name))
            .chain(RETIRED_DIRS.iter().map(|name| self.0.join(name)));
        for path in retired {
            match fs::remove_dir_all(path) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
        }
        Ok(())
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
            "{} is not a dir; refusing to keep the observe store there",
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

/// What a container was, read from its labels when the store first saw it.
pub use ployz_core::labels::Class as ContainerKind;

/// What the harvester and the sampler read from one inspect's labels and state.
#[derive(Clone, Debug, Eq, PartialEq)]
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

#[cfg(test)]
mod tests {
    use super::StoreRoot;
    use crate::test_dir::TestDir;

    #[test]
    fn prepare_replaces_unknown_versions_and_leaves_other_entries() {
        let docker = TestDir::new("ployzd-observe-layout");
        let parent = docker.0.join("observe");
        std::fs::create_dir_all(parent.join("v2/outbox/x")).unwrap();
        std::fs::create_dir_all(parent.join("notes")).unwrap();
        std::fs::write(parent.join("v1x"), b"keep").unwrap();
        let root = StoreRoot::under(&docker.0);
        root.prepare().unwrap();
        root.prepare().unwrap();
        assert!(!parent.join("v2").exists());
        assert!(parent.join("notes").is_dir());
        assert!(parent.join("v1x").is_file());
        assert!(root.path().is_dir());
    }

    #[test]
    fn prepare_deletes_what_earlier_releases_left() {
        let docker = TestDir::new("ployzd-observe-layout-retired");
        let root = StoreRoot::under(&docker.0);
        let pageviews = root.path().join("outbox/pageviews");
        let segments = root.path().join("metrics/raw");
        for dir in [&pageviews, &segments] {
            std::fs::create_dir_all(dir).unwrap();
            std::fs::write(dir.join("1.jsonl"), b"{}\n").unwrap();
        }
        root.prepare().unwrap();
        root.prepare().unwrap();
        assert!(!pageviews.exists());
        assert!(!root.path().join("metrics").exists());
    }

    #[test]
    fn prepare_refuses_a_symlinked_store_dir() {
        let docker = TestDir::new("ployzd-observe-layout-symlink");
        let elsewhere = docker.0.join("elsewhere");
        std::fs::create_dir_all(&elsewhere).unwrap();
        std::fs::create_dir_all(docker.0.join("observe")).unwrap();
        std::os::unix::fs::symlink(&elsewhere, docker.0.join("observe/v1")).unwrap();
        assert!(StoreRoot::under(&docker.0).prepare().is_err());
    }
}
