//! Whether a file holds what a Ployz writer leaves, so the data dir's claim and every delete in
//! a store can tell Ployz's files from anyone else's.
//!
//! A file is Ployz's when its bytes parse as its writer's type, with only fields some Ployz
//! release wrote, keep the state that writer can produce, and, in the shape this release
//! writes, fit [`WRITE_CAP`]. Nothing is judged by its bytes beyond that, and a document cut
//! short is never Ployz's. A caller inspects a file and acts on what it inspected, under the
//! lock its store's writers take.

use std::{
    fs,
    io::{self, Read},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

use serde::{Serialize, de::DeserializeOwned};
use serde_json::Value;
use sha2::{Digest as _, Sha256};

/// The most bytes a writer puts in one file, checked against the widest form a document can
/// take, before anything is written for it.
pub(crate) const WRITE_CAP: usize = 1 << 20;

/// The most bytes read from one file. A file past it is never Ployz's, whatever spacing it
/// holds.
const READ_CAP: usize = 2 * WRITE_CAP;

/// The widest a u64 is written in decimal, as `u64::MAX` is.
const U64_WIDTH: usize = 20;

/// How a document can grow once written.
#[derive(Clone, Copy)]
pub(crate) enum Widest {
    /// It is written once, as it is.
    AsWritten,
    /// Its seq, now this (`None` written as `null`), can be rewritten as wide as a u64 gets.
    WithSeq(Option<u64>),
}

/// `value` as compact JSON, when its widest form fits [`WRITE_CAP`]. It is encoded once and
/// measured from that encoding.
///
/// # Errors
///
/// Returns an error when `value` cannot be serialized, and one of kind
/// [`io::ErrorKind::FileTooLarge`] when its widest form is past [`WRITE_CAP`].
pub(crate) fn to_json<T: Serialize>(value: &T, widest: Widest) -> io::Result<Vec<u8>> {
    let json = serde_json::to_vec(value).map_err(io::Error::other)?;
    let growth = match widest {
        Widest::AsWritten => 0,
        Widest::WithSeq(seq) => U64_WIDTH - seq.map_or("null".len(), |seq| seq.to_string().len()),
    };
    if json.len() + growth > WRITE_CAP {
        return Err(io::Error::new(
            io::ErrorKind::FileTooLarge,
            format!("past the {WRITE_CAP} bytes Ployz keeps in one file"),
        ));
    }
    Ok(json)
}

/// Whether `name` is `target` (`Some(false)`) or the copy `atomic_write` stages it in
/// (`Some(true)`).
pub(crate) fn staging(name: &str, target: &str) -> Option<bool> {
    if name == target {
        Some(false)
    } else if Path::new(name) == crate::filesystem::temporary_path(Path::new(target)) {
        Some(true)
    } else {
        None
    }
}

/// What one of Ployz's files holds once its writer is done.
pub(crate) enum Content {
    Empty,
    U64,
    I64,
    Json(Box<dyn Document>),
}

/// A file a Ployz writer makes: what it holds, and whether its writer also leaves it empty.
pub(crate) struct Written {
    content: Content,
    empty: bool,
}

impl Written {
    /// `staged` for a copy its writer creates empty before it writes the whole document in,
    /// as `atomic_write` does.
    pub(crate) fn new(content: Content, staged: bool) -> Self {
        Self {
            content,
            empty: staged,
        }
    }

    pub(crate) fn or_empty(self) -> Self {
        Self {
            empty: true,
            ..self
        }
    }

    fn judge(&self, bytes: &[u8]) -> Verdict {
        if bytes.len() > READ_CAP {
            return Verdict::Foreign;
        }
        let ours = if bytes.is_empty() {
            self.empty || matches!(self.content, Content::Empty)
        } else {
            match &self.content {
                Content::Empty => false,
                Content::U64 => decimal::<u64>(bytes),
                Content::I64 => decimal::<i64>(bytes),
                Content::Json(document) => return document.judge(bytes),
            }
        };
        if ours {
            Verdict::Ours
        } else {
            Verdict::Foreign
        }
    }
}

/// Whose a file is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Verdict {
    Ours,
    /// A document Ployz's writer could make but this release would refuse to write: past
    /// [`WRITE_CAP`] in the shape this release writes, as a release before the cap could.
    Oversized,
    Foreign,
}

/// Whether `bytes` are a `T` as `to_string` spells it, as every release wrote it.
fn decimal<T: std::str::FromStr + ToString>(bytes: &[u8]) -> bool {
    std::str::from_utf8(bytes)
        .ok()
        .and_then(|text| Some((text, text.parse::<T>().ok()?)))
        .is_some_and(|(text, value)| value.to_string() == text)
}

/// A JSON document one file name holds.
pub(crate) trait Document {
    /// Whose `bytes` are, judged as the document whole.
    fn judge(&self, bytes: &[u8]) -> Verdict;
}

/// How a writer encodes its documents.
#[derive(Clone, Copy)]
pub(crate) enum Encoding {
    /// `serde_json::to_vec`.
    Compact,
    /// `serde_json::to_writer_pretty`, then a newline.
    PrettyLine,
}

impl Encoding {
    fn encode<T: Serialize>(self, value: &T) -> Option<Vec<u8>> {
        match self {
            Self::Compact => serde_json::to_vec(value).ok(),
            Self::PrettyLine => serde_json::to_vec_pretty(value).ok().map(|mut json| {
                json.push(b'\n');
                json
            }),
        }
    }
}

/// A field an earlier release wrote at the top of a document, and the values it wrote there.
pub(crate) type Retired = (&'static str, fn(&Value) -> bool);

/// A `T` a writer encodes as `encoding`: of the file name's `identity`, holding only fields
/// `T` writes or an earlier release wrote (`retired`, at the top, each with the values it
/// wrote), in a state its writer can produce (`valid`), and, unless it holds a retired field,
/// whose `widest` form fits [`WRITE_CAP`].
pub(crate) struct Json<T, I> {
    pub(crate) identity: fn(&T) -> I,
    pub(crate) named: I,
    pub(crate) retired: &'static [Retired],
    pub(crate) valid: fn(&T) -> bool,
    pub(crate) widest: fn(&T) -> T,
    pub(crate) encoding: Encoding,
}

impl<T: Serialize + DeserializeOwned, I: PartialEq> Document for Json<T, I> {
    fn judge(&self, bytes: &[u8]) -> Verdict {
        let (Ok(raw), Ok(value)) = (
            serde_json::from_slice::<Value>(bytes),
            serde_json::from_slice::<T>(bytes),
        ) else {
            return Verdict::Foreign;
        };
        let Ok(canonical) = serde_json::to_value(&value) else {
            return Verdict::Foreign;
        };
        let mut legacy = false;
        for (field, wrote) in self.retired {
            if let Some(held) = raw.get(field) {
                if !wrote(held) {
                    return Verdict::Foreign;
                }
                legacy = true;
            }
        }
        if (self.identity)(&value) != self.named
            || !(self.valid)(&value)
            || !known_fields(&raw, &canonical, self.retired)
        {
            return Verdict::Foreign;
        }
        // The cap holds for the shape this release writes; an earlier shape had none.
        let fits = legacy
            || self
                .encoding
                .encode(&(self.widest)(&value))
                .is_some_and(|widest| widest.len() <= WRITE_CAP);
        if fits {
            Verdict::Ours
        } else {
            Verdict::Oversized
        }
    }
}

/// Whether every field `raw` holds is one `canonical`, the parsed value written again, holds
/// at the same place, or a `retired` top-level field: a parse drops a field it doesn't know.
fn known_fields(raw: &Value, canonical: &Value, retired: &[Retired]) -> bool {
    match (raw, canonical) {
        (Value::Object(raw), Value::Object(canonical)) => raw.iter().all(|(key, field)| {
            retired.iter().any(|(retired, _)| retired == key)
                || canonical
                    .get(key)
                    .is_some_and(|written| known_fields(field, written, &[]))
        }),
        (Value::Array(raw), Value::Array(canonical)) => {
            raw.len() == canonical.len()
                && raw
                    .iter()
                    .zip(canonical)
                    .all(|(field, written)| known_fields(field, written, &[]))
        }
        (Value::Object(_) | Value::Array(_), _) => false,
        _ => true,
    }
}

/// A sha256 of the bytes a caller inspected, to act on those bytes only.
pub(crate) type Digest = [u8; 32];

fn digest(bytes: &[u8]) -> Digest {
    Sha256::digest(bytes).into()
}

/// What sits at a path a writer names, read through the handle opened.
pub(crate) enum Inspection {
    Absent,
    /// Not a regular file of its own: a symlink, a dir, a device, or a file with another name.
    Odd,
    File {
        bytes: Vec<u8>,
        verdict: Verdict,
        /// The device and inode read, so an act on the path acts on this file only.
        id: (u64, u64),
    },
}

impl Inspection {
    pub(crate) fn verdict(&self) -> Option<Verdict> {
        match self {
            Self::Absent => None,
            Self::Odd => Some(Verdict::Foreign),
            Self::File { verdict, .. } => Some(*verdict),
        }
    }

    pub(crate) fn digest(&self) -> Option<Digest> {
        match self {
            Self::File { bytes, .. } => Some(digest(bytes)),
            Self::Absent | Self::Odd => None,
        }
    }
}

/// Inspects the file at `path` as `written` describes it, reading it through the handle
/// opened, never looked up again by path, since a writer can rename a new file over it. A
/// name no writer makes (`None`) is foreign.
///
/// # Errors
///
/// Returns an error when the file cannot be read.
pub(crate) fn inspect(path: &Path, written: Option<&Written>) -> io::Result<Inspection> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(nix::libc::O_NOFOLLOW | nix::libc::O_NONBLOCK)
        .open(path);
    let file = match file {
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Inspection::Absent),
        Err(error) if error.raw_os_error() == Some(nix::libc::ELOOP) => {
            return Ok(Inspection::Odd);
        }
        file => file?,
    };
    let metadata = file.metadata()?;
    // A writer only ever creates or renames a fresh file, so one of its files has no other name
    // a delete would leave pointing at it. A file renamed over since it was opened has none.
    if !metadata.is_file() || metadata.nlink() > 1 {
        return Ok(Inspection::Odd);
    }
    let mut bytes = Vec::new();
    file.take(READ_CAP as u64 + 1).read_to_end(&mut bytes)?;
    let verdict = written.map_or(Verdict::Foreign, |written| written.judge(&bytes));
    Ok(Inspection::File {
        bytes,
        verdict,
        id: (metadata.dev(), metadata.ino()),
    })
}

/// Whether `path` still names the file inspected as `id`.
fn still(path: &Path, id: (u64, u64)) -> io::Result<bool> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => Ok((metadata.dev(), metadata.ino()) == id),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error),
    }
}

/// Whether the entry at `path`, of type `kind` as its dir lists it, is the file `written`
/// describes holding what its writer leaves, or is gone by the time it is read.
pub(crate) fn file_holds(
    kind: fs::FileType,
    path: &Path,
    written: Option<&Written>,
) -> io::Result<bool> {
    if !kind.is_file() {
        return Ok(false);
    }
    Ok(inspect(path, written)?
        .verdict()
        .is_none_or(|verdict| verdict == Verdict::Ours))
}

/// Whether `path` is a regular file of its own, or gone.
pub(crate) fn plain_file(path: &Path) -> io::Result<bool> {
    Ok(!matches!(inspect(path, None)?, Inspection::Odd))
}

/// What [`remove_written`] did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Removal {
    Absent,
    Removed,
    /// Kept: it is not what Ployz writes there, or no longer the bytes the caller selected.
    Kept,
}

/// Removes the file at `path` only when it is the file `written` describes, holding what its
/// writer leaves, and, given `selected`, still the bytes the caller selected. Anything else
/// stays, and is logged. Callers hold the lock their store's writers take.
///
/// # Errors
///
/// Returns an error when the file cannot be read or removed.
pub(crate) fn remove_written(
    path: &Path,
    written: Option<&Written>,
    selected: Option<Digest>,
) -> io::Result<Removal> {
    let inspection = inspect(path, written)?;
    let Inspection::File { verdict, id, .. } = &inspection else {
        if matches!(inspection, Inspection::Absent) {
            return Ok(Removal::Absent);
        }
        tracing::warn!(path = %path.display(), "keeping a file Ployz did not write");
        return Ok(Removal::Kept);
    };
    if *verdict != Verdict::Ours {
        tracing::warn!(path = %path.display(), "keeping a file Ployz did not write");
        return Ok(Removal::Kept);
    }
    if selected.is_some_and(|selected| inspection.digest() != Some(selected)) {
        tracing::debug!(path = %path.display(), "keeping a file rewritten since it was selected");
        return Ok(Removal::Kept);
    }
    remove_inspected(path, *id)
}

/// Removes `path` when it still names the file inspected as `id`.
fn remove_inspected(path: &Path, id: (u64, u64)) -> io::Result<Removal> {
    if !still(path, id)? {
        return Ok(Removal::Kept);
    }
    match fs::remove_file(path) {
        Ok(()) => Ok(Removal::Removed),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(Removal::Absent),
        Err(error) => Err(error),
    }
}

/// Keeps the file at `path`, inspected as `inspection`, aside under `<name>.kept-<n>` beside
/// it, and returns where it is kept. A kept file already holding the same bytes, read whole,
/// keeps it, and `path` goes. Otherwise it takes the smallest free `<n>`, linked so nothing
/// under that name is replaced, so each distinct file adds one kept file and keeping it again
/// adds none. A name taken since the dir was read leaves the file where it is, logged. Callers
/// hold the lock their store's writers take.
///
/// # Errors
///
/// Returns an error when the dir cannot be read or the file linked or removed.
pub(crate) fn keep_aside(path: &Path, inspection: &Inspection) -> io::Result<Option<PathBuf>> {
    let Inspection::File { bytes, id, .. } = inspection else {
        return Ok(None);
    };
    let dir = path.parent().unwrap_or(Path::new("."));
    let prefix = format!(
        "{}.kept-",
        path.file_name().unwrap_or_default().to_string_lossy()
    );
    // A file past the read cap was read only in part, so it never matches a kept one.
    let whole = bytes.len() <= READ_CAP;
    let mut taken = std::collections::BTreeSet::new();
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let Some(n) = kept_index(&entry.file_name(), &prefix) else {
            continue;
        };
        taken.insert(n);
        let kept = entry.path();
        if whole
            && let Inspection::File { bytes: held, .. } = inspect(&kept, None)?
            && held == *bytes
        {
            remove_inspected(path, *id)?;
            return Ok(Some(kept));
        }
    }
    if !still(path, *id)? {
        return Ok(None);
    }
    // One of 1..=taken.len() + 1 is free.
    let n = (1..).find(|n| !taken.contains(n)).unwrap_or(1);
    let kept = dir.join(format!("{prefix}{n}"));
    match fs::hard_link(path, &kept) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            tracing::warn!(
                path = %path.display(),
                taken = %kept.display(),
                "leaving a file Ployz did not write in place; the name to keep it under was just taken"
            );
            return Ok(None);
        }
        Err(error) => return Err(error),
    }
    fs::remove_file(path)?;
    fs::File::open(dir)?.sync_all()?;
    tracing::warn!(path = %kept.display(), "kept a file Ployz did not write aside");
    Ok(Some(kept))
}

/// The `<n>` of a name [`keep_aside`] gives under `prefix`.
pub(crate) fn kept_index(name: &std::ffi::OsStr, prefix: &str) -> Option<u64> {
    name.to_str()?
        .strip_prefix(prefix)?
        .parse()
        .ok()
        .filter(|n| *n >= 1)
}

#[cfg(test)]
mod keep_aside_tests {
    use std::fs;

    use super::{Digest, READ_CAP, digest, inspect, keep_aside};

    /// Keeps `staged` aside in a dir already holding `kept`, each as `(n, bytes)`, and returns
    /// the dir's files as `(name, digest)`, sorted.
    fn keep(staged: &[u8], kept: &[(u64, &[u8])]) -> Vec<(String, Digest)> {
        let dir = tempfile::tempdir().unwrap();
        for (n, bytes) in kept {
            fs::write(dir.path().join(format!("staged.kept-{n}")), bytes).unwrap();
        }
        let path = dir.path().join("staged");
        fs::write(&path, staged).unwrap();
        keep_aside(&path, &inspect(&path, None).unwrap()).unwrap();
        let mut files: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| {
                let entry = entry.unwrap();
                let name = entry.file_name().into_string().unwrap();
                (name, digest(&fs::read(entry.path()).unwrap()))
            })
            .collect();
        files.sort();
        files
    }

    #[test]
    fn a_file_past_the_read_cap_is_kept_beside_one_sharing_what_was_read() {
        let kept = vec![b'a'; READ_CAP + 2];
        let mut staged = kept.clone();
        *staged.last_mut().unwrap() = b'b';

        assert_eq!(
            keep(&staged, &[(1, &kept)]),
            [
                ("staged.kept-1".to_owned(), digest(&kept)),
                ("staged.kept-2".to_owned(), digest(&staged)),
            ]
        );
    }

    #[test]
    fn a_kept_file_at_the_largest_index_leaves_a_smaller_one_free() {
        assert_eq!(
            keep(b"other notes", &[(u64::MAX, b"notes")]),
            [
                ("staged.kept-1".to_owned(), digest(b"other notes")),
                (format!("staged.kept-{}", u64::MAX), digest(b"notes")),
            ]
        );
    }
}
