//! Staged, durable file-system writes.
//!
//! Host Runner files land through atomic same-directory replacements. Directory
//! staging remains hand-rolled because join material needs rollback semantics.

use atomic_write_file::AtomicWriteFile;
use std::fs::{self, File, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use ployz_core::ops::FailureMessage;

/// The permissions a staged file is created with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileMode {
    Plain,
    Secret0600,
}

impl FileMode {
    fn open_staged(self, path: &Path) -> std::io::Result<File> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);
        match self {
            Self::Plain => {}
            Self::Secret0600 => {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::OpenOptionsExt;

                    options.mode(0o600);
                }
            }
        }
        options.open(path)
    }

    fn open_atomic(self, path: &Path) -> std::io::Result<AtomicWriteFile> {
        let mut options = AtomicWriteFile::options();
        match self {
            Self::Plain => {}
            Self::Secret0600 => {
                #[cfg(unix)]
                {
                    use atomic_write_file::unix::OpenOptionsExt as AtomicOpenOptionsExt;
                    use std::os::unix::fs::OpenOptionsExt as _;

                    options.mode(0o600).preserve_mode(false);
                }
            }
        }
        options.open(path)
    }
}

#[derive(Debug)]
pub enum StagedFileError {
    ClockWentBackwards {
        message: String,
    },
    CreateFailed {
        staged_path: PathBuf,
        message: String,
    },
    Exhausted {
        directory: PathBuf,
    },
}

/// A created-but-not-yet-committed file. Dropping it before `commit_to`
/// removes the staged file.
pub struct StagedFile {
    path: PathBuf,
    file: File,
    committed: bool,
}

impl StagedFile {
    pub fn create(
        directory: &Path,
        file_name: &str,
        staged_tag: &str,
        mode: FileMode,
    ) -> Result<Self, StagedFileError> {
        for attempt in 0..16 {
            let staged_path = unique_staged_path(directory, file_name, staged_tag, attempt)
                .map_err(|message| StagedFileError::ClockWentBackwards { message })?;
            match mode.open_staged(&staged_path) {
                Ok(file) => {
                    return Ok(Self {
                        path: staged_path,
                        file,
                        committed: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(StagedFileError::CreateFailed {
                        staged_path,
                        message: error.to_string(),
                    });
                }
            }
        }

        Err(StagedFileError::Exhausted {
            directory: directory.to_path_buf(),
        })
    }

    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn file(&mut self) -> &mut File {
        &mut self.file
    }

    pub fn commit_to(&mut self, path: &Path) -> std::io::Result<()> {
        fs::rename(&self.path, path)?;
        self.committed = true;
        Ok(())
    }
}

impl Drop for StagedFile {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_file(&self.path);
        }
    }
}

/// Writes `contents` durably to `directory/file_name` via a staged sibling
/// file, then syncs the directory.
pub fn write_durable_file(
    directory: &Path,
    file_name: &str,
    mode: FileMode,
    contents: &[u8],
) -> Result<(), FailureMessage> {
    let path = directory.join(file_name);
    let mut file = mode.open_atomic(&path).map_err(|error| {
        failure_message(format!(
            "failed to create atomic file {}: {error}",
            path.display()
        ))
    })?;
    file.write_all(contents).map_err(|error| {
        failure_message(format!(
            "failed to write atomic file {}: {error}",
            path.display()
        ))
    })?;
    file.commit().map_err(|error| {
        failure_message(format!(
            "failed to commit atomic file {}: {error}",
            path.display()
        ))
    })
}

/// A created-but-not-yet-committed directory. Dropping it before `commit_to`
/// removes the staged tree.
pub(crate) struct StagedDirectory {
    path: PathBuf,
    committed: bool,
}

impl StagedDirectory {
    pub(crate) fn create(
        directory: &Path,
        name: &str,
        staged_tag: &str,
    ) -> Result<Self, FailureMessage> {
        for attempt in 0..16 {
            let staged_path = unique_staged_path(directory, name, staged_tag, attempt)
                .map_err(staged_name_failure)?;
            match create_private_directory(&staged_path) {
                Ok(()) => {
                    return Ok(Self {
                        path: staged_path,
                        committed: false,
                    });
                }
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => {
                    return Err(failure_message(format!(
                        "failed to create staged directory {}: {error}",
                        staged_path.display()
                    )));
                }
            }
        }

        Err(failure_message(format!(
            "failed to create a unique staged directory in {}",
            directory.display()
        )))
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn commit_to(mut self, path: &Path) -> Result<(), FailureMessage> {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return Err(failure_message(format!(
                "staged directory commit target {} must have a UTF-8 file name",
                path.display()
            )));
        };
        let backup = unique_staged_path(
            path.parent().unwrap_or_else(|| Path::new(".")),
            name,
            "previous",
            0,
        )
        .map_err(staged_name_failure)?;
        let had_previous = path.exists();
        if had_previous {
            fs::rename(path, &backup).map_err(|error| {
                failure_message(format!(
                    "failed to move previous join material directory {} to {}: {error}",
                    path.display(),
                    backup.display()
                ))
            })?;
        }

        if let Err(error) = fs::rename(&self.path, path) {
            if had_previous {
                let _ = fs::rename(&backup, path);
            }
            return Err(failure_message(format!(
                "failed to commit staged directory {} to {}: {error}",
                self.path.display(),
                path.display()
            )));
        }

        self.committed = true;
        if had_previous {
            let _ = fs::remove_dir_all(&backup);
        }
        sync_directory(path.parent().unwrap_or_else(|| Path::new("."))).map_err(|error| {
            failure_message(format!(
                "directory {} was installed but parent sync failed: {error}",
                path.display()
            ))
        })
    }
}

impl Drop for StagedDirectory {
    fn drop(&mut self) {
        if !self.committed {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

fn staged_name_failure(message: String) -> FailureMessage {
    failure_message(format!("failed to create staged file name: {message}"))
}

fn unique_staged_path(
    directory: &Path,
    file_name: &str,
    staged_tag: &str,
    attempt: u8,
) -> Result<PathBuf, String> {
    let entropy = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| error.to_string())?
        .as_nanos();
    Ok(directory.join(format!(
        ".{file_name}.{staged_tag}-{}-{entropy}-{attempt}",
        std::process::id()
    )))
}

#[cfg(unix)]
pub(crate) fn create_private_directory(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;

    fs::DirBuilder::new().mode(0o700).create(path)
}

#[cfg(not(unix))]
pub(crate) fn create_private_directory(path: &Path) -> std::io::Result<()> {
    fs::create_dir(path)
}

pub(crate) fn ensure_directory(path: &Path) -> std::io::Result<()> {
    if path.is_dir() {
        return Ok(());
    }
    create_private_directory(path)
}

#[cfg(unix)]
pub(crate) fn sync_directory(path: &Path) -> std::io::Result<()> {
    File::open(path)?.sync_all()
}

#[cfg(not(unix))]
pub(crate) fn sync_directory(_path: &Path) -> std::io::Result<()> {
    Ok(())
}

fn failure_message(message: impl Into<String>) -> FailureMessage {
    FailureMessage::try_new(message).expect("Host Runner generated a non-empty failure message")
}

#[cfg(all(test, unix))]
mod tests {
    use super::{FileMode, StagedDirectory, write_durable_file};
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    use std::os::unix::fs::PermissionsExt;

    #[test]
    fn secret_durable_file_is_written_0600() {
        let directory = tempfile::tempdir().expect("temp dir");

        write_durable_file(directory.path(), "seed", FileMode::Secret0600, b"secret")
            .expect("secret writes");

        let mode = std::fs::metadata(directory.path().join("seed"))
            .expect("secret metadata")
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600);
    }

    #[test]
    fn staged_directory_commit_rejects_non_utf8_target_name() {
        let directory = tempfile::tempdir().expect("temp dir");
        let staged = StagedDirectory::create(directory.path(), "join-material", "test")
            .expect("staged directory");
        let target = directory
            .path()
            .join(OsString::from_vec(b"join-\xFF".to_vec()));

        let error = staged
            .commit_to(&target)
            .expect_err("target name is invalid");

        assert!(error.as_str().contains("UTF-8 file name"));
        assert!(!target.exists());
    }
}
