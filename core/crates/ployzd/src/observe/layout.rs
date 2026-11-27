//! The observe store's shape on disk.
//!
//! ```text
//! <data_dir>/observe/                   other version dirs here are deleted
//!   harvester.lock                      held by the one running observe
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

use crate::written::{Content, Written, file_holds, plain_file, staging};

const STORE_DIR: &str = ployz_core::OBSERVE_DIR_NAME;
const VERSION_DIR: &str = "v1";
pub(super) const LOCK_FILE: &str = "harvester.lock";

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

/// Whether `<data_dir>/observe` holds nothing but what `ployzd observe` writes there,
/// entry by entry down to every file: [`LOCK_FILE`], and a `v1` with the crash outbox, the
/// settled dir and `resume-from`, each file named and filled as its writer leaves it.
///
/// The daemon starts observe before it claims `data_dir`, so a first start can find the
/// store already there, and still being written. Anything else under `observe`, at any depth,
/// is someone else's and keeps the daemon from claiming, and later resetting, the directory.
/// An entry gone by the time it is read was observe's, replaced or removed mid-scan.
///
/// # Errors
///
/// Returns an error when the store's dirs or files cannot be read.
pub fn holds_only_store(data_dir: &Path) -> io::Result<bool> {
    dir_holding(&data_dir.join(STORE_DIR), &|name, kind, path| match name {
        LOCK_FILE => file_holds(kind, path, Some(&Written::new(Content::Empty, false))),
        VERSION_DIR => dir_holding(path, &|name, kind, path| match name {
            OUTBOX_DIR => dir_holding(path, &|name, _, path| match name {
                // An entry kept aside is Ployz's, as long as it is a plain file.
                CRASH_DIR => dir_holding(path, &|name, kind, path| {
                    if super::ship::crash_kept(name) {
                        Ok(kind.is_file() && plain_file(path)?)
                    } else {
                        file_holds(kind, path, super::ship::crash_file(name).as_ref())
                    }
                }),
                _ => Ok(false),
            }),
            SETTLED_DIR => files_written(path, super::settled::file),
            name => {
                let resume =
                    staging(name, RESUME_FILE).map(|staged| Written::new(Content::I64, staged));
                file_holds(kind, path, resume.as_ref())
            }
        }),
        _ => Ok(false),
    })
}

/// Accepts one entry of a dir by its name, its type (a symlink unfollowed) and its path.
type Accepts<'a> = &'a dyn Fn(&str, fs::FileType, &Path) -> io::Result<bool>;

/// Whether `dir` is a real dir, not a symlink, whose every entry `ours` accepts.
fn dir_holding(dir: &Path, ours: Accepts<'_>) -> io::Result<bool> {
    let entries = match fs::symlink_metadata(dir) {
        Ok(metadata) if !metadata.is_dir() => return Ok(false),
        Ok(_) => fs::read_dir(dir),
        Err(error) => Err(error),
    };
    let entries = match entries {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        entries => entries?,
    };
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let Some(name) = name.to_str() else {
            return Ok(false);
        };
        let kind = match entry.file_type() {
            Err(error) if error.kind() == io::ErrorKind::NotFound => continue,
            kind => kind?,
        };
        if !ours(name, kind, &entry.path())? {
            return Ok(false);
        }
    }
    Ok(true)
}

/// Whether `dir` is a real dir holding only files `written` names, each as its writer leaves it.
fn files_written(dir: &Path, written: fn(&str) -> Option<Written>) -> io::Result<bool> {
    dir_holding(dir, &|name, kind, path| {
        file_holds(kind, path, written(name).as_ref())
    })
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
    use std::{
        fs,
        path::{Path, PathBuf},
        sync::atomic::{AtomicBool, Ordering},
        thread,
    };

    use ployz_core::metrics::{MarkerKind, MetricMarker};

    use super::{StoreRoot, holds_only_store};
    use crate::{
        filesystem::{atomic_write, temporary_path},
        observe::{
            settled::{Key, Settlements, Unfinished},
            ship::{Outbox, crash_marker},
        },
        test_dir::TestDir,
    };

    fn store(name: &str) -> (TestDir, StoreRoot) {
        let docker = TestDir::new(name);
        std::fs::create_dir_all(&docker.0).unwrap();
        let root = StoreRoot::under(&docker.0);
        root.prepare().unwrap();
        (docker, root)
    }

    fn settle(settlements: &Settlements, ts: i64, seq: u64) -> Key {
        let marker = crash_marker(ts);
        let key = Key::of(&marker).unwrap();
        let unfinished = Unfinished {
            marker,
            seq: Some(seq),
        };
        settlements.save(&key, &unfinished).unwrap();
        key
    }

    /// The one file in `dir` whose name `wanted` accepts.
    fn only(dir: &Path, wanted: impl Fn(&str) -> bool) -> PathBuf {
        let mut found = fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| wanted(path.file_name().unwrap().to_str().unwrap()));
        let path = found.next().unwrap();
        assert!(found.next().is_none());
        path
    }

    fn crash_entry(root: &StoreRoot) -> PathBuf {
        only(&root.crash_outbox(), |name| {
            name.len() == 20 && name.bytes().all(|byte| byte.is_ascii_digit())
        })
    }

    fn pending(root: &StoreRoot) -> PathBuf {
        only(&root.settled(), |name| name.ends_with(".pending"))
    }

    /// Markers of each form, one with a replica whose JSON needs escapes and multi-byte
    /// characters.
    fn markers() -> Vec<MetricMarker> {
        let exit = crash_marker(1_760_000_000_000_000_000);
        vec![
            exit.clone(),
            MetricMarker {
                deployment_id: None,
                replica: "wé😀\"\\\u{1}\nb".into(),
                kind: MarkerKind::Exit {
                    exit_code: None,
                    oom: true,
                    stopped: true,
                },
                ..exit.clone()
            },
            MetricMarker {
                replica: "wé😀\"\\\u{1}\nb".into(),
                kind: MarkerKind::Exit {
                    exit_code: Some(0),
                    oom: false,
                    stopped: true,
                },
                ..exit.clone()
            },
            MetricMarker {
                kind: MarkerKind::Oom,
                ..exit
            },
        ]
    }

    /// Checks the store is observe's with `file` staged empty or whole, as `atomic_write`
    /// leaves it, and is not with it staged cut short anywhere.
    fn staged_whole_is_its_own(docker: &Path, file: &Path) {
        let output = fs::read(file).unwrap();
        let staged = temporary_path(file);
        for len in 0..=output.len() {
            fs::write(&staged, output.get(..len).unwrap()).unwrap();
            assert_eq!(
                holds_only_store(docker).unwrap(),
                len == 0 || len == output.len(),
                "{} staged at {len} of {}",
                file.display(),
                output.len()
            );
        }
        fs::remove_file(staged).unwrap();
    }

    #[test]
    fn a_staged_copy_is_observes_only_empty_or_whole() {
        for marker in markers() {
            let (docker, root) = store("ployzd-observe-layout-staged");
            // Only a crash is queued, and only its settlement takes a seq.
            let seq = marker.is_crash().then(|| {
                let outbox = Outbox::open(&root).unwrap();
                let seq = outbox.reserve(marker.ts).unwrap();
                assert!(outbox.push_at(seq, marker.clone()).unwrap());
                staged_whole_is_its_own(&docker.0, &crash_entry(&root));
                seq
            });

            let key = Key::of(&marker).unwrap();
            let unfinished = Unfinished { marker, seq };
            Settlements::open(&root).save(&key, &unfinished).unwrap();
            staged_whole_is_its_own(&docker.0, &pending(&root));
        }
    }

    #[test]
    fn a_settlement_an_earlier_release_wrote_is_claimed_and_pruned() {
        let (docker, root) = store("ployzd-observe-layout-v0-3-15");
        // As v0.3.15 encoded its `Unfinished`, which also held `appended`.
        let marker = crash_marker(1);
        let path = root
            .settled()
            .join(format!("{}-exit-1", marker.container_id));
        fs::create_dir_all(root.settled()).unwrap();
        let v0_3_15 = format!(
            r#"{{"marker":{},"appended":true,"seq":5}}"#,
            serde_json::to_string(&marker).unwrap()
        );
        fs::write(&path, v0_3_15).unwrap();
        assert!(holds_only_store(&docker.0).unwrap());

        Settlements::open(&root).forget_before(2).unwrap();
        assert!(!path.exists());
    }

    #[test]
    fn everything_observe_writes_is_its_own() {
        let (docker, root) = store("ployzd-observe-layout-own");
        let _lock = super::super::lock_store(&root).unwrap();
        let outbox = Outbox::open(&root).unwrap();
        let queued = outbox.reserve(1).unwrap();
        outbox.push_at(queued, crash_marker(1)).unwrap();
        outbox.reserve(2).unwrap();
        let settlements = Settlements::open(&root);
        let finished = settle(&settlements, 1, queued);
        settlements.finish(&finished).unwrap();
        settle(&settlements, 2, queued + 1);
        atomic_write(&root.resume_from(), b"-42", 0o600).unwrap();
        std::fs::write(root.path().join("resume-from.tmp"), b"").unwrap();

        assert!(holds_only_store(&docker.0).unwrap());
    }

    #[test]
    fn observe_writing_mid_scan_never_makes_the_store_foreign() {
        let (docker, root) = store("ployzd-observe-layout-race");
        let settlements = Settlements::open(&root);
        let done = AtomicBool::new(false);
        thread::scope(|scope| {
            scope.spawn(|| {
                for at in 0..200_i64 {
                    atomic_write(&root.resume_from(), at.to_string().as_bytes(), 0o600).unwrap();
                    settle(&settlements, 1, at.unsigned_abs() + 1);
                }
                done.store(true, Ordering::Release);
            });
            while !done.load(Ordering::Acquire) {
                assert!(matches!(holds_only_store(&docker.0), Ok(true)));
            }
        });
    }

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
