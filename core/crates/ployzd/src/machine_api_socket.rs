//! The Machine API Unix socket: its claim lock and its listener.

use std::{
    fs::{self, File},
    io,
    os::unix::{
        fs::{FileTypeExt, PermissionsExt},
        net::UnixListener,
    },
    path::Path,
};

use crate::{
    filesystem::{MACHINE_API_SOCKET_MODE, PLOYZ_DIR_MODE, set_ployz_group},
    socket_activation::{claim_socket_path, inherited_unix_listener},
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

    claim_socket_path(path)
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

    use super::{MachineApiSocket, claim_socket, machine_api_listener, require_bound_to};
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

    #[test]
    fn a_second_daemon_refuses_the_socket_a_running_daemon_serves() {
        let root = TestDir::new("ployzd-socket-second-daemon");
        let path = root.0.join("run/ployz.sock");
        let _running = MachineApiSocket::claim(&path).unwrap();

        let error = MachineApiSocket::claim(&path)
            .err()
            .expect("a second daemon must not take the socket");

        assert_eq!(error.kind(), io::ErrorKind::AddrInUse, "{error}");
        std::os::unix::net::UnixStream::connect(&path).expect("the running daemon still serves");
    }

    #[test]
    fn a_socket_another_process_still_serves_is_left_in_place() {
        let root = TestDir::new("ployzd-socket-live");
        fs::create_dir_all(&root.0).unwrap();
        let path = root.0.join("ployz.sock");
        let live = UnixListener::bind(&path).unwrap();

        let error = MachineApiSocket::claim(&path)
            .err()
            .expect("a served socket must not be replaced");

        assert_eq!(error.kind(), io::ErrorKind::AddrInUse, "{error}");
        let _client = std::os::unix::net::UnixStream::connect(&path).unwrap();
        live.set_nonblocking(true).unwrap();
        live.accept()
            .expect("the live listener still owns the path");
    }

    #[test]
    fn a_stale_socket_nothing_serves_is_replaced() {
        let root = TestDir::new("ployzd-socket-stale");
        fs::create_dir_all(&root.0).unwrap();
        let path = root.0.join("ployz.sock");
        drop(UnixListener::bind(&path).unwrap());

        let _claimed = MachineApiSocket::claim(&path).unwrap();

        std::os::unix::net::UnixStream::connect(&path).expect("the daemon serves the path");
    }

    #[test]
    fn a_non_socket_path_is_refused() {
        let root = TestDir::new("ployzd-socket-not-socket");
        fs::create_dir_all(&root.0).unwrap();
        let path = root.0.join("ployz.sock");
        fs::write(&path, "keep").unwrap();

        let error = MachineApiSocket::claim(&path)
            .err()
            .expect("a regular file must not be replaced");

        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists, "{error}");
        assert_eq!(fs::read_to_string(&path).unwrap(), "keep");
    }
}
