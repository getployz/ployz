//! Files and directories only their owner may use, made private as they are created, so no
//! reader sees them at the default mode. The caller names what a write must survive.

use std::{
    fs::{self, DirBuilder, File, OpenOptions},
    io::{self, Write},
    os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use thiserror::Error;

/// What a finished write has reached before it returns.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Durability {
    /// The file's contents are on disk.
    File,
    /// The file's contents are on disk and so is the directory entry that names them.
    FileAndDirectory,
}

/// Where [`replace_file`] writes before it renames over the target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TempName {
    /// `path` with its extension replaced by `tmp`, which a layout can derive and clean up. One
    /// writer owns it, so a leftover from a crash is truncated and reused.
    Sibling,
    /// `.{name}.{pid}.{n}.tmp` beside the target, new each time, so concurrent processes
    /// replacing one file never share a temporary.
    Unique,
}

/// How [`replace_file`] writes: the file's `mode`, what it must reach, and its temporary's name.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Replace {
    pub mode: u32,
    pub durability: Durability,
    pub temp: TempName,
}

#[derive(Debug, Error)]
pub enum ReplaceError {
    /// `path` still holds what it did before.
    #[error("write {}: {source}", path.display())]
    Write { path: PathBuf, source: io::Error },
    /// `path` holds the new contents but its directory entry is not known to be on disk.
    #[error("sync the directory of {}: {source}", path.display())]
    Unsynced { path: PathBuf, source: io::Error },
}

impl From<ReplaceError> for io::Error {
    fn from(error: ReplaceError) -> Self {
        match error {
            ReplaceError::Write { source, .. } | ReplaceError::Unsynced { source, .. } => source,
        }
    }
}

/// Put `contents` at `path` in one rename, so a reader sees the old file or the new one. The
/// temporary is created with `replace.mode`, and removed when the write fails after it was
/// opened and before the rename. A temporary that could not be opened is left as it was.
///
/// # Errors
/// [`ReplaceError::Write`] when the old file is still in place, [`ReplaceError::Unsynced`] when
/// only the directory sync after the rename failed.
pub fn replace_file(path: &Path, contents: &[u8], replace: Replace) -> Result<(), ReplaceError> {
    replace_through(&replace.temp.path(path), path, contents, replace)
}

fn replace_through(
    temporary: &Path,
    path: &Path,
    contents: &[u8],
    replace: Replace,
) -> Result<(), ReplaceError> {
    let written = open_temporary(temporary, replace).and_then(|mut file| {
        let written = write_temporary(&mut file, contents, replace)
            .and_then(|()| fs::rename(temporary, path));
        if written.is_err() {
            let _ = fs::remove_file(temporary);
        }
        written
    });
    if let Err(source) = written {
        return Err(ReplaceError::Write {
            path: path.to_owned(),
            source,
        });
    }
    if replace.durability == Durability::FileAndDirectory {
        sync_directory(path).map_err(|source| ReplaceError::Unsynced {
            path: path.to_owned(),
            source,
        })?;
    }
    Ok(())
}

/// Open the temporary. Only a file this returns is the caller's to remove: a failed `create_new`
/// means the name belongs to someone else.
fn open_temporary(temporary: &Path, replace: Replace) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).mode(replace.mode);
    match replace.temp {
        TempName::Sibling => options.create(true).truncate(true),
        TempName::Unique => options.create_new(true),
    };
    options.open(temporary)
}

fn write_temporary(file: &mut File, contents: &[u8], replace: Replace) -> io::Result<()> {
    file.set_permissions(fs::Permissions::from_mode(replace.mode))?;
    file.write_all(contents)?;
    file.sync_all()
}

static TEMPORARY_FILE_COUNTER: AtomicU64 = AtomicU64::new(0);

impl TempName {
    /// The temporary this name gives a write of `target`. A [`TempName::Unique`] is a new name
    /// on every call.
    #[must_use]
    pub fn path(self, target: &Path) -> PathBuf {
        match self {
            Self::Sibling => target.with_extension("tmp"),
            Self::Unique => {
                let name = target.file_name().unwrap_or_default().to_string_lossy();
                target.with_file_name(format!(
                    ".{name}.{}.{}.tmp",
                    std::process::id(),
                    TEMPORARY_FILE_COUNTER.fetch_add(1, Ordering::Relaxed)
                ))
            }
        }
    }
}

/// Sync the directory that holds `path`, as `path` names it: a bare file name has the empty
/// parent, which cannot be opened, so a caller that means the current directory passes `./name`.
fn sync_directory(path: &Path) -> io::Result<()> {
    let parent = path.parent().ok_or_else(|| {
        io::Error::new(io::ErrorKind::InvalidInput, "path has no parent directory")
    })?;
    File::open(parent)?.sync_all()
}

/// Create the file `path` with mode `0600` and `contents`, and fail when it exists.
///
/// # Errors
/// The first step of creating, writing and syncing that failed.
pub fn create_private_file(path: &Path, contents: &[u8], durability: Durability) -> io::Result<()> {
    let mut file = OpenOptions::new()
        .create_new(true)
        .write(true)
        .mode(0o600)
        .open(path)?;
    file.write_all(contents)?;
    file.sync_all()?;
    if durability == Durability::FileAndDirectory {
        sync_directory(path)?;
    }
    Ok(())
}

/// What [`create_private_dir`] does with `path` when it is already there.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Existing {
    /// Leave its mode alone.
    Keep,
    /// Set its mode to `0700`.
    Tighten,
}

#[derive(Debug, Error)]
#[error("create private directory {}: {source}", path.display())]
pub struct DirectoryError {
    pub path: PathBuf,
    pub source: io::Error,
}

impl From<DirectoryError> for io::Error {
    fn from(error: DirectoryError) -> Self {
        error.source
    }
}

/// Make `path` and every missing ancestor a directory only the owner can enter. Each is created
/// at `0700`, then set to exactly `0700` whatever the umask.
///
/// # Errors
/// The directory that could not be created or set, with the cause.
pub fn create_private_dir(path: &Path, existing: Existing) -> Result<(), DirectoryError> {
    let mut missing = Vec::new();
    let mut next = path;
    while !next.exists() {
        missing.push(next);
        next = next.parent().unwrap_or_else(|| Path::new("."));
    }
    DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(path)
        .map_err(|source| DirectoryError {
            path: path.to_owned(),
            source,
        })?;
    if existing == Existing::Tighten && !missing.contains(&path) {
        missing.push(path);
    }
    for directory in missing {
        fs::set_permissions(directory, fs::Permissions::from_mode(0o700)).map_err(|source| {
            DirectoryError {
                path: directory.to_owned(),
                source,
            }
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        os::unix::fs::{MetadataExt, PermissionsExt},
        path::Path,
    };

    use super::{
        Durability, Existing, Replace, ReplaceError, TempName, create_private_dir,
        create_private_file, replace_file, replace_through,
    };

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().mode() & 0o777
    }

    fn replace(temp: TempName) -> Replace {
        Replace {
            mode: 0o640,
            durability: Durability::FileAndDirectory,
            temp,
        }
    }

    fn names(directory: &Path) -> Vec<String> {
        let mut names: Vec<String> = fs::read_dir(directory)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().into_string().unwrap())
            .collect();
        names.sort();
        names
    }

    #[test]
    fn a_replaced_file_has_the_mode_and_contents_and_leaves_no_temporary() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        fs::write(&path, b"old").unwrap();
        for temp in [TempName::Sibling, TempName::Unique] {
            replace_file(&path, b"new", replace(temp)).unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"new");
            assert_eq!(mode(&path), 0o640);
            assert_eq!(names(directory.path()), ["state.json"]);
        }
    }

    #[test]
    fn a_sibling_temporary_left_by_a_crash_is_reused() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");
        let stale = path.with_extension("tmp");
        fs::write(&stale, b"half a write, longer than the new contents").unwrap();
        fs::set_permissions(&stale, fs::Permissions::from_mode(0o666)).unwrap();
        replace_file(&path, b"new", replace(TempName::Sibling)).unwrap();
        assert_eq!(fs::read(&path).unwrap(), b"new");
        assert_eq!(mode(&path), 0o640);
        assert_eq!(names(directory.path()), ["state.json"]);
    }

    #[test]
    fn a_unique_temporary_is_named_for_the_file_and_never_reused() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("config.yaml");
        let first = TempName::Unique.path(&path);
        let second = TempName::Unique.path(&path);
        assert_ne!(first, second);
        let name = first.file_name().unwrap().to_str().unwrap();
        assert!(
            name.starts_with(".config.yaml.") && name.ends_with(".tmp"),
            "{name}"
        );
        assert_eq!(
            TempName::Sibling.path(&path),
            directory.path().join("config.tmp")
        );
    }

    #[test]
    fn a_failed_replace_removes_its_temporary_and_keeps_the_old_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("taken");
        fs::create_dir(&path).unwrap();
        fs::write(path.join("inside"), b"kept").unwrap();
        for temp in [TempName::Sibling, TempName::Unique] {
            let error = replace_file(&path, b"new", replace(temp)).unwrap_err();
            assert!(matches!(error, ReplaceError::Write { .. }), "{error}");
            assert_eq!(names(directory.path()), ["taken"]);
        }
        assert_eq!(fs::read(path.join("inside")).unwrap(), b"kept");
    }

    #[test]
    fn a_temporary_that_cannot_be_opened_is_left_as_it_was() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.json");

        let taken = directory.path().join(".state.json.1.0.tmp");
        fs::write(&taken, b"another process's half write").unwrap();
        let error = replace_through(&taken, &path, b"new", replace(TempName::Unique)).unwrap_err();
        assert_eq!(
            std::io::Error::from(error).kind(),
            std::io::ErrorKind::AlreadyExists
        );
        assert_eq!(fs::read(&taken).unwrap(), b"another process's half write");
        assert!(!path.exists());

        let sibling = TempName::Sibling.path(&path);
        fs::create_dir(&sibling).unwrap();
        fs::write(sibling.join("inside"), b"kept").unwrap();
        replace_file(&path, b"new", replace(TempName::Sibling)).unwrap_err();
        assert_eq!(fs::read(sibling.join("inside")).unwrap(), b"kept");
        assert!(!path.exists());
    }

    #[test]
    fn a_replace_error_reads_as_its_io_error() {
        let directory = tempfile::tempdir().unwrap();
        let missing = directory.path().join("absent").join("file");
        let error = replace_file(&missing, b"x", replace(TempName::Sibling)).unwrap_err();
        let io = std::io::Error::from(error);
        assert_eq!(io.kind(), std::io::ErrorKind::NotFound);
    }

    #[test]
    fn a_private_file_is_new_and_owner_only() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("archive");
        for durability in [Durability::File, Durability::FileAndDirectory] {
            let path = path.with_extension(format!("{durability:?}"));
            create_private_file(&path, b"bytes", durability).unwrap();
            assert_eq!(fs::read(&path).unwrap(), b"bytes");
            assert_eq!(mode(&path), 0o600);
            let error = create_private_file(&path, b"again", durability).unwrap_err();
            assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
            assert_eq!(fs::read(&path).unwrap(), b"bytes");
        }
    }

    #[test]
    fn every_directory_it_creates_is_owner_only_and_an_existing_one_follows_the_choice() {
        let directory = tempfile::tempdir().unwrap();
        let top = directory.path().join("a");
        let leaf = top.join("b").join("c");
        create_private_dir(&leaf, Existing::Keep).unwrap();
        for created in [&top, &top.join("b"), &leaf] {
            assert_eq!(mode(created), 0o700, "{}", created.display());
        }

        fs::set_permissions(&leaf, fs::Permissions::from_mode(0o755)).unwrap();
        create_private_dir(&leaf, Existing::Keep).unwrap();
        assert_eq!(mode(&leaf), 0o755);
        create_private_dir(&leaf, Existing::Tighten).unwrap();
        assert_eq!(mode(&leaf), 0o700);
    }

    #[test]
    fn a_directory_that_cannot_be_created_names_itself() {
        let directory = tempfile::tempdir().unwrap();
        let file = directory.path().join("file");
        fs::write(&file, b"").unwrap();
        let error = create_private_dir(&file.join("below"), Existing::Keep).unwrap_err();
        assert_eq!(error.path, file.join("below"));
    }
}
