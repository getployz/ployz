//! Replace a file in one step, so a reader or a crash finds the old contents or the new,
//! never part of either.

use std::{
    fs::{self, File},
    io::{self, Write as _},
    os::unix::fs::PermissionsExt as _,
    path::Path,
};

/// Readable and writable by the owner only, for files that hold a secret.
pub(crate) const OWNER_ONLY: u32 = 0o600;

#[derive(Debug)]
pub(crate) enum ReplaceError {
    /// `path` still holds what it held before.
    Failed(io::Error),
    /// `path` holds the new contents, but its directory wasn't flushed, so a crash may
    /// bring back the old.
    Unflushed(io::Error),
}

impl From<ReplaceError> for io::Error {
    fn from(error: ReplaceError) -> Self {
        match error {
            ReplaceError::Failed(error) | ReplaceError::Unflushed(error) => error,
        }
    }
}

/// Replace `path` with `contents` at `mode`: written to a file beside it that is never
/// readable beyond `mode`, flushed, renamed over `path`, and the rename flushed.
pub(crate) fn replace(path: &Path, contents: &[u8], mode: u32) -> Result<(), ReplaceError> {
    // A bare file name's parent is "", which names no directory to open.
    let directory = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    write(directory, path, contents, mode).map_err(ReplaceError::Failed)?;
    File::open(directory)
        .and_then(|directory| directory.sync_all())
        .map_err(ReplaceError::Unflushed)
}

fn write(directory: &Path, path: &Path, contents: &[u8], mode: u32) -> io::Result<()> {
    fs::create_dir_all(directory)?;
    let mut file = tempfile::NamedTempFile::new_in(directory)?;
    file.as_file()
        .set_permissions(fs::Permissions::from_mode(mode))?;
    file.write_all(contents)?;
    file.as_file().sync_all()?;
    file.persist(path).map_err(|error| error.error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // Nextest runs each test in its own process, so moving this one's working directory
    // reaches no other test.
    #[test]
    fn a_bare_file_name_replaces_in_the_working_directory() {
        let dir = tempfile::tempdir().unwrap();
        std::env::set_current_dir(dir.path()).unwrap();

        replace(Path::new("links.json"), b"{}", OWNER_ONLY).unwrap();

        assert_eq!(fs::read(dir.path().join("links.json")).unwrap(), b"{}");
    }
}
