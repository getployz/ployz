//! The exits and OOM kills the harvester has settled, so a Docker event
//! delivered again settles nothing twice.
//!
//! A settlement is named `<cid>-<exit|oom>-<ts>`. `<name>.pending` holds one
//! whose marker or crash is not stored yet, and renaming it to `<name>`
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

const PENDING: &str = "pending";
const TEMP: &str = "tmp";

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
    /// A restart has no key: it is recorded only after an exit the open
    /// stream saw, which a resumed stream cannot repeat.
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

#[derive(Debug, Deserialize, Serialize)]
pub(super) struct Unfinished {
    pub(super) marker: MetricMarker,
    pub(super) appended: bool,
    /// The crash's outbox seq, taken once so queuing it again rewrites the
    /// same entry.
    pub(super) seq: Option<u64>,
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

    pub(super) fn save(&self, key: &Key, unfinished: &Unfinished) -> io::Result<()> {
        create_store_dir(&self.dir)?;
        let json = serde_json::to_vec(unfinished).map_err(io::Error::other)?;
        crate::filesystem::atomic_write(&self.pending(key), &json, 0o600)
    }

    pub(super) fn finish(&self, key: &Key) -> io::Result<()> {
        fs::rename(self.pending(key), self.dir.join(key.name()))?;
        fs::File::open(&self.dir)?.sync_all()
    }

    /// Gives up an unfinished settlement.
    pub(super) fn forget(&self, key: &Key) -> io::Result<()> {
        match fs::remove_file(self.pending(key)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
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
            match fs::remove_file(self.dir.join(name)) {
                Err(error) if error.kind() != io::ErrorKind::NotFound => return Err(error),
                _ => {}
            }
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
