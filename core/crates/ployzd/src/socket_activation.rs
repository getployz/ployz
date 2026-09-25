//! systemd socket activation.

use std::{io, os::unix::net::UnixListener};

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
