//! systemd socket activation.

use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::{
        fs::{FileTypeExt, MetadataExt, OpenOptionsExt},
        net::{UnixListener, UnixStream},
    },
    path::Path,
};

/// Takes the Unix listener systemd socket activation passed, if any.
///
/// # Errors
///
/// Returns an error when systemd passed more than one socket or a socket that
/// is not a Unix stream listener.
pub fn inherited_unix_listener() -> io::Result<Option<UnixListener>> {
    let mut inherited = listenfd::ListenFd::from_env();
    if inherited.len() > 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "expected at most one systemd socket, received {}",
                inherited.len()
            ),
        ));
    }
    inherited.take_unix_listener(0)
}

/// Claims `path` for this process through an exclusive lock on `<path>.lock`, held
/// while the returned file lives, so two processes never serve one socket path.
///
/// # Errors
///
/// Returns [`io::ErrorKind::AddrInUse`] when another process holds the claim, or
/// the error opening the lock file.
pub fn claim_socket_path(path: &Path) -> io::Result<File> {
    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
        if error.kind() == io::ErrorKind::WouldBlock {
            io::Error::new(
                io::ErrorKind::AddrInUse,
                "socket is owned by another daemon",
            )
        } else {
            error
        }
    })?;
    Ok(lock)
}

/// Removes the socket left at `path` when nothing serves it and this user owns it.
/// Call it while holding [`claim_socket_path`]'s claim, before binding `path`.
///
/// # Errors
///
/// Returns [`io::ErrorKind::AddrInUse`] when another process serves `path`,
/// [`io::ErrorKind::AlreadyExists`] when `path` is not a socket this user owns,
/// or the error inspecting or removing it.
pub fn remove_stale_socket(path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    if !metadata.file_type().is_socket() || metadata.uid() != fs::metadata("/proc/self")?.uid() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "refusing to replace {}: not a socket this user owns",
                path.display()
            ),
        ));
    }
    match UnixStream::connect(path) {
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("another process serves {}", path.display()),
        )),
        Err(error) if error.kind() == io::ErrorKind::ConnectionRefused => fs::remove_file(path),
        Err(error) => Err(error),
    }
}
