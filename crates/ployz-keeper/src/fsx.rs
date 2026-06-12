//! Staged, durable file-system writes.
//!
//! Every keeper file lands the same way: write to a uniquely named staged
//! sibling, sync, rename into place, then sync the containing directory.

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
    fn open(self, path: &Path) -> std::io::Result<File> {
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
            match mode.open(&staged_path) {
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
    staged_tag: &str,
    mode: FileMode,
    contents: &[u8],
) -> Result<(), FailureMessage> {
    let path = directory.join(file_name);
    let mut staged =
        StagedFile::create(directory, file_name, staged_tag, mode).map_err(staged_file_failure)?;
    staged.file().write_all(contents).map_err(|error| {
        failure_message(format!(
            "failed to write staged file {}: {error}",
            staged.path().display()
        ))
    })?;
    staged.file().sync_all().map_err(|error| {
        failure_message(format!(
            "failed to sync staged file {}: {error}",
            staged.path().display()
        ))
    })?;
    staged.commit_to(&path).map_err(|error| {
        failure_message(format!(
            "failed to commit staged file {} to {}: {error}",
            staged.path().display(),
            path.display()
        ))
    })?;
    sync_directory(directory).map_err(|error| {
        failure_message(format!(
            "file {} was installed but directory sync failed for {}: {error}",
            path.display(),
            directory.display()
        ))
    })
}

fn staged_file_failure(error: StagedFileError) -> FailureMessage {
    match error {
        StagedFileError::ClockWentBackwards { message } => {
            failure_message(format!("failed to create staged file name: {message}"))
        }
        StagedFileError::CreateFailed {
            staged_path,
            message,
        } => failure_message(format!(
            "failed to create staged file {}: {message}",
            staged_path.display()
        )),
        StagedFileError::Exhausted { directory } => failure_message(format!(
            "failed to create a unique staged file in {}",
            directory.display()
        )),
    }
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
        let name = path
            .file_name()
            .and_then(|name| name.to_str())
            .expect("staged directory commit target has a UTF-8 file name");
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
    FailureMessage::try_new(message).expect("keeper generated a non-empty failure message")
}
