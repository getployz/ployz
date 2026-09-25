//! The Machine API Unix socket: its claim lock and its listener.

use std::{
    fs::{self, File, OpenOptions},
    io,
    os::unix::{
        fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
        net::UnixListener,
    },
    path::Path,
};

use crate::{
    filesystem::{MACHINE_API_SOCKET_MODE, PLOYZ_DIR_MODE, set_ployz_group},
    socket_activation::inherited_unix_listener,
};

/// The claim lock on the Machine API socket path and the listener it serves.
pub(crate) struct MachineApiSocket {
    pub(crate) lock: File,
    pub(crate) listener: UnixListener,
}

impl MachineApiSocket {
    /// Claims the Machine API socket and takes its listener: the socket
    /// `ployz.socket` passed, which must be bound to `path`, or else `path`
    /// bound here (development, tests, and the local testkit run without systemd).
    ///
    /// Call it early in startup, before spawning threads or subprocesses: taking
    /// the inherited socket clears `LISTEN_FDS` and marks the fd close-on-exec.
    ///
    /// # Errors
    ///
    /// Returns an error when another daemon holds the claim, when the inherited
    /// socket is invalid or bound elsewhere, or when binding `path` fails.
    pub(crate) fn claim(path: &Path) -> io::Result<Self> {
        let lock = claim_socket(path)?;
        Ok(Self {
            lock,
            listener: machine_api_listener(path)?,
        })
    }
}

fn claim_socket(path: &Path) -> io::Result<File> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket path has no parent"))?;
    let parent_created = !parent.exists();
    fs::create_dir_all(parent)?;
    if parent_created {
        fs::set_permissions(parent, fs::Permissions::from_mode(PLOYZ_DIR_MODE))?;
        set_ployz_group(parent)?;
    }

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

/// Inherited `ployz.socket` listener bound to `path`, else a fresh bind.
fn machine_api_listener(path: &Path) -> io::Result<UnixListener> {
    let listener = match inherited_unix_listener()? {
        Some(listener) => {
            require_bound_to(&listener, path)?;
            listener
        }
        None => bind_socket(path)?,
    };
    listener.set_nonblocking(true)?;
    Ok(listener)
}

fn bind_socket(path: &Path) -> io::Result<UnixListener> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "refusing to replace a non-socket path",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(MACHINE_API_SOCKET_MODE))?;
    set_ployz_group(path)?;
    Ok(listener)
}

fn require_bound_to(listener: &UnixListener, path: &Path) -> io::Result<()> {
    let address = listener.local_addr()?;
    if address.as_pathname() != Some(path) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "systemd passed a socket bound to {address:?}, expected {}",
                path.display()
            ),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::{fs, io, os::unix::net::UnixListener};

    use super::{claim_socket, machine_api_listener, require_bound_to};
    use crate::test_dir::TestDir;

    #[tokio::test]
    async fn claimed_socket_path_refuses_connections_until_listen() {
        let root = TestDir::new("ployzd-socket-claim");
        fs::create_dir_all(root.0.join("run")).unwrap();
        let path = root.0.join("run/ployz.sock");
        let _lock = claim_socket(&path).unwrap();
        let error = tokio::net::UnixStream::connect(&path)
            .await
            .expect_err("claim must not listen");
        assert!(
            matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ),
            "{error}"
        );
        let _listener = machine_api_listener(&path).unwrap();
        tokio::net::UnixStream::connect(&path)
            .await
            .expect("listen must queue connections");
    }

    #[test]
    fn inherited_socket_must_be_bound_to_the_claimed_path() {
        let root = TestDir::new("ployzd-socket-inherited-path");
        fs::create_dir_all(&root.0).unwrap();
        let bound = root.0.join("other.sock");
        let listener = UnixListener::bind(&bound).unwrap();
        require_bound_to(&listener, &bound).unwrap();
        let expected = root.0.join("ployz.sock");
        let error = require_bound_to(&listener, &expected).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        let message = error.to_string();
        assert!(message.contains("other.sock"), "{message}");
        assert!(
            message.contains(&expected.display().to_string()),
            "{message}"
        );
    }
}
