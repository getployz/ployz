//! The exits and OOM kills the harvester has settled, so a Docker event
//! delivered again settles nothing twice.
//!
//! A settlement is named `<cid>-<exit|oom>-<ts>`. `<name>.pending` holds one
//! whose crash is not queued yet, and renaming it to `<name>`
//! finishes it. A finished one is forgotten once no stream can deliver its
//! event again.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use ployz_core::{
    ContainerId,
    metrics::{MarkerKind, MetricMarker},
};
use serde::{Deserialize, Serialize};

use super::layout::{StoreRoot, create_store_dir};
use crate::written::{Content, Encoding, Json, Widest, Written, remove_written, staging, to_json};

const PENDING: &str = "pending";
const TEMP: &str = "tmp";

/// The file `name` is in the settled dir: a settlement as [`Key`] spells it, its pending copy,
/// or that copy staged by `atomic_write`, each holding the [`Unfinished`] saved under that key.
pub(super) fn file(name: &str) -> Option<Written> {
    let stem = name.split_once('.').map_or(name, |(stem, _)| stem);
    let key = Key::parse(stem)?;
    let settled = key.name();
    let staged =
        staging(name, &settled).or_else(|| staging(name, &format!("{settled}.{PENDING}")))?;
    Some(Written::new(
        Content::Json(Box::new(Json {
            identity: |unfinished: &Unfinished| Key::of(&unfinished.marker),
            named: Some(key),
            // Releases through v0.4.0-beta.2 also wrote whether the crash was appended.
            retired: &[("appended", serde_json::Value::is_boolean)],
            valid: Unfinished::valid,
            widest: Unfinished::widest,
            encoding: Encoding::Compact,
        })),
        staged,
    ))
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Kind {
    Exit,
    Oom,
}

impl Kind {
    fn name(self) -> &'static str {
        match self {
            Self::Exit => "exit",
            Self::Oom => "oom",
        }
    }
}

/// One exit or OOM kill of one container.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct Key {
    container: ContainerId,
    kind: Kind,
    ts: i64,
}

impl Key {
    /// A restart has no key: nothing settles it.
    pub(super) fn of(marker: &MetricMarker) -> Option<Self> {
        let kind = match marker.kind {
            MarkerKind::Exit { .. } => Kind::Exit,
            MarkerKind::Oom => Kind::Oom,
            MarkerKind::Restart => return None,
        };
        Some(Self {
            container: marker.container_id,
            kind,
            ts: marker.ts,
        })
    }

    pub(super) fn container(&self) -> ContainerId {
        self.container
    }

    pub(super) fn ts(&self) -> i64 {
        self.ts
    }

    fn name(&self) -> String {
        format!("{}-{}-{}", self.container, self.kind.name(), self.ts)
    }

    fn parse(name: &str) -> Option<Self> {
        let mut parts = name.splitn(3, '-');
        let container = ContainerId::parse(parts.next()?).ok()?;
        let kind = match parts.next()? {
            "exit" => Kind::Exit,
            "oom" => Kind::Oom,
            _ => return None,
        };
        let ts = parts.next()?.parse().ok()?;
        Some(Self {
            container,
            kind,
            ts,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct Unfinished {
    pub(super) marker: MetricMarker,
    /// The crash's outbox seq, taken once so queuing it again rewrites the
    /// same entry.
    pub(super) seq: Option<u64>,
}

impl Unfinished {
    /// Only a crash is queued, so only a crash's settlement takes a seq.
    fn valid(&self) -> bool {
        self.seq.is_none() || self.marker.is_crash()
    }

    /// The settlement as wide as it gets: a crash's, once its seq is as wide as a u64 gets.
    fn widest(&self) -> Self {
        Self {
            seq: self.marker.is_crash().then_some(u64::MAX),
            ..self.clone()
        }
    }
}

pub(super) enum Settlement {
    New,
    Unfinished(Box<Unfinished>),
    Finished,
}

pub(super) struct Settlements {
    dir: PathBuf,
}

impl Settlements {
    pub(super) fn open(store: &StoreRoot) -> Self {
        Self {
            dir: store.settled(),
        }
    }

    pub(super) fn get(&self, key: &Key) -> io::Result<Settlement> {
        match fs::read(self.pending(key)) {
            Ok(bytes) => {
                return serde_json::from_slice(&bytes)
                    .map(|unfinished| Settlement::Unfinished(Box::new(unfinished)))
                    .map_err(io::Error::other);
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        match fs::symlink_metadata(self.dir.join(key.name())) {
            Ok(metadata) if metadata.is_file() => Ok(Settlement::Finished),
            Ok(_) => Ok(Settlement::New),
            Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Settlement::New),
            Err(error) => Err(error),
        }
    }

    /// Saves `unfinished` pending under `key`.
    ///
    /// # Errors
    ///
    /// Returns an error of kind [`io::ErrorKind::FileTooLarge`], before anything is written,
    /// when the settlement is too large for the store at any seq it can take.
    pub(super) fn save(&self, key: &Key, unfinished: &Unfinished) -> io::Result<()> {
        // A crash's settlement takes a seq later, so it is measured as if it had one now.
        let widest = if unfinished.marker.is_crash() {
            Widest::WithSeq(unfinished.seq)
        } else {
            Widest::AsWritten
        };
        let json = to_json(unfinished, widest)?;
        create_store_dir(&self.dir)?;
        crate::filesystem::atomic_write(&self.pending(key), &json, 0o600)
    }

    pub(super) fn finish(&self, key: &Key) -> io::Result<()> {
        fs::rename(self.pending(key), self.dir.join(key.name()))?;
        fs::File::open(&self.dir)?.sync_all()
    }

    /// Gives up an unfinished settlement.
    pub(super) fn forget(&self, key: &Key) -> io::Result<()> {
        self.remove(&format!("{}.{PENDING}", key.name()))
    }

    /// Removes the settled file `name` when it holds what a settlement writes there.
    fn remove(&self, name: &str) -> io::Result<()> {
        remove_written(&self.dir.join(name), file(name).as_ref(), None).map(drop)
    }

    pub(super) fn count(&self) -> io::Result<usize> {
        self.entries().map(|entries| entries.len())
    }

    pub(super) fn unfinished(&self) -> io::Result<Vec<(Key, Unfinished)>> {
        let mut unfinished = Vec::new();
        for (key, extension) in self.entries()? {
            if extension.as_deref() == Some(PENDING)
                && let Settlement::Unfinished(entry) = self.get(&key)?
            {
                unfinished.push((key, *entry));
            }
        }
        Ok(unfinished)
    }

    /// Forgets finished settlements of events older than `before`.
    pub(super) fn forget_before(&self, before: i64) -> io::Result<()> {
        for (key, extension) in self.entries()? {
            if key.ts >= before {
                continue;
            }
            let name = match extension.as_deref() {
                None => key.name(),
                Some(TEMP) => format!("{}.{TEMP}", key.name()),
                Some(_) => continue,
            };
            self.remove(&name)?;
        }
        Ok(())
    }

    fn pending(&self, key: &Key) -> PathBuf {
        self.dir.join(format!("{}.{PENDING}", key.name()))
    }

    fn entries(&self) -> io::Result<Vec<(Key, Option<String>)>> {
        let entries = match fs::read_dir(&self.dir) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut keys = Vec::new();
        for entry in entries {
            let path = Path::new(&entry?.file_name()).to_owned();
            let Some(key) = path
                .file_stem()
                .and_then(|stem| stem.to_str())
                .and_then(Key::parse)
            else {
                continue;
            };
            let extension = path
                .extension()
                .map(|extension| extension.to_string_lossy().into_owned());
            keys.push((key, extension));
        }
        Ok(keys)
    }
}
