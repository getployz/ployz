//! Atomic file write for a rendered config artifact owned by one local process.
//!
//! The one remaining caller is the NATS authorization renderer, which writes
//! the `authorized-users.conf` include the `nats-server` reloads. Durable
//! control-plane state lives in the core database (`core_store`), not here.

use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
#[error("{}: {message}", path.display())]
pub(crate) struct AtomicFileWriteError {
    path: PathBuf,
    message: String,
}

pub(crate) fn write_file_atomically(
    path: &Path,
    contents: &[u8],
) -> Result<(), AtomicFileWriteError> {
    let write_error = |message: String| AtomicFileWriteError {
        path: path.to_path_buf(),
        message,
    };
    let Some(parent) = path.parent() else {
        return Err(write_error("path has no parent directory".to_owned()));
    };
    std::fs::create_dir_all(parent).map_err(|error| write_error(error.to_string()))?;
    let mut file =
        tempfile::NamedTempFile::new_in(parent).map_err(|error| write_error(error.to_string()))?;
    file.write_all(contents)
        .and_then(|()| file.as_file().sync_all())
        .map_err(|error| write_error(error.to_string()))?;
    file.persist(path)
        .map_err(|error| write_error(error.error.to_string()))?;
    sync_parent_directory(parent).map_err(|error| write_error(error.to_string()))
}

#[cfg(unix)]
fn sync_parent_directory(parent: &Path) -> Result<(), std::io::Error> {
    std::fs::File::open(parent)?.sync_all()
}

#[cfg(not(unix))]
fn sync_parent_directory(_parent: &Path) -> Result<(), std::io::Error> {
    Ok(())
}
