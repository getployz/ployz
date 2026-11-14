use std::{
    fmt, fs, io,
    path::{Path, PathBuf},
};

use crate::MachineLayout;

/// The file beside the Machine API socket that holds the refusal of a start that
/// exited with [`crate::NETWORK_CONFLICT_EXIT_STATUS`]. Each start removes it first.
pub const STARTUP_REFUSAL_FILE: &str = "refused";

/// The text that introduces a startup refusal wherever a client reads it,
/// including the stderr of a remote `ployzd dial-stdio`. Clients of other releases stop
/// waiting on this text, so it must not change.
pub const STARTUP_REFUSED: &str = "ployzd refused to start: ";

/// Why a daemon refused its last start, as every client words it.
#[derive(Debug, PartialEq, Eq)]
pub struct Refusal(String);

impl Refusal {
    /// Whether `line` carries a startup refusal, including one a remote
    /// `ployzd dial-stdio` relayed.
    #[must_use]
    pub fn is_named_in(line: &str) -> bool {
        line.contains(STARTUP_REFUSED)
    }
}

impl fmt::Display for Refusal {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{STARTUP_REFUSED}{}", self.0)
    }
}

/// The refusal record a daemon leaves in its run directory. A refusing start
/// closes the connection that started it unanswered, so a client reads the
/// record to say why.
pub struct StartupRefusal {
    run_dir: PathBuf,
}

impl StartupRefusal {
    #[must_use]
    pub fn new(run_dir: impl Into<PathBuf>) -> Self {
        Self {
            run_dir: run_dir.into(),
        }
    }

    /// The record of the daemon whose socket is `socket`.
    #[must_use]
    pub fn beside(socket: &Path) -> Self {
        Self::new(MachineLayout::run_dir_of(socket))
    }

    fn path(&self) -> PathBuf {
        self.run_dir.join(STARTUP_REFUSAL_FILE)
    }

    /// Records `reason` for the clients a refusing start closes.
    pub fn record(&self, reason: &str) {
        let path = self.path();
        if let Err(write) = fs::write(&path, format!("{reason}\n")) {
            eprintln!(
                "WARNING: could not record the refusal in {}: {write}",
                path.display()
            );
        }
    }

    /// Removes the refusal an earlier start left. A daemon process does this
    /// first, so no failure of its start, however early, leaves an earlier
    /// refusal to be reported as its own.
    pub fn clear(&self) {
        let path = self.path();
        if let Err(error) = fs::remove_file(&path)
            && error.kind() != io::ErrorKind::NotFound
        {
            eprintln!(
                "WARNING: could not clear the last refusal in {}: {error}",
                path.display()
            );
        }
    }

    /// Why the daemon refused its last start, or `None` when it did not.
    #[must_use]
    pub fn read(&self) -> Option<Refusal> {
        let line = fs::read_to_string(self.path()).ok()?;
        let line = line.trim();
        (!line.is_empty()).then(|| Refusal(line.to_owned()))
    }

    /// `error` with the refusal appended, when there is one.
    #[must_use]
    pub fn naming(&self, error: io::Error) -> io::Error {
        match self.read() {
            Some(refusal) => io::Error::new(error.kind(), format!("{error}; {refusal}")),
            None => error,
        }
    }
}

#[cfg(all(unix, not(target_arch = "wasm32")))]
mod dial {
    use std::{cell::Cell, io, path::Path, time::Duration};

    use tokio::{net::UnixStream, time::Instant};

    use super::StartupRefusal;

    /// How long a refusal may stand beside a socket that stays down before the
    /// wait for it ends. It covers a socket restarting, which comes back before
    /// a daemon process has cleared the last refusal. Under the client's 5 s
    /// wait for an answer, so its error names the refusal.
    const REFUSED_SOCKET_GRACE: Duration = Duration::from_secs(2);

    impl StartupRefusal {
        /// Whether a refusal has stood for `grace`. Every daemon process
        /// clears the refusal first, so a socket that stays down while one is
        /// recorded was most likely stopped by it: systemd's start limit trips
        /// after repeated refusals, and waiting longer would only outlast the
        /// client.
        fn stood_for(&self, grace: Duration) -> impl Fn() -> bool {
            let since = Cell::new(None);
            move || {
                if self.read().is_none() {
                    since.set(None);
                    return false;
                }
                let first = since.get().unwrap_or_else(Instant::now);
                since.set(Some(first));
                first.elapsed() >= grace
            }
        }

        /// Connects to the Machine API socket, waiting while it is missing or
        /// refusing connections unless a refusal has stood beside it past the
        /// grace, and names the refusal in the error that ends the wait.
        ///
        /// # Errors
        ///
        /// Returns a human-readable I/O error when `timeout` elapses, a refusal
        /// stands while the socket is not ready, or connect fails for a reason
        /// other than the socket not being ready.
        pub async fn connect(&self, socket: &Path, timeout: Duration) -> io::Result<UnixStream> {
            wait_until_socket_accepts(socket, timeout, self.stood_for(REFUSED_SOCKET_GRACE))
                .await
                .map_err(|error| self.naming(error))
        }
    }

    fn socket_not_ready(error: &io::Error) -> bool {
        matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
        )
    }

    async fn wait_until_socket_accepts(
        path: &Path,
        timeout: Duration,
        refused: impl Fn() -> bool,
    ) -> io::Result<UnixStream> {
        let deadline = Instant::now() + timeout;
        loop {
            match UnixStream::connect(path).await {
                Ok(stream) => return Ok(stream),
                Err(error)
                    if socket_not_ready(&error) && Instant::now() < deadline && !refused() =>
                {
                    tokio::time::sleep(Duration::from_millis(5)).await;
                }
                Err(error) if socket_not_ready(&error) => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!(
                            "Machine API socket did not become ready: {error}",
                            error = crate::error_chain::inline(&error),
                        ),
                    ));
                }
                Err(error) => {
                    return Err(io::Error::new(
                        error.kind(),
                        format!(
                            "could not connect to the Machine API socket: {error}",
                            error = crate::error_chain::inline(&error),
                        ),
                    ));
                }
            }
        }
    }

    #[cfg(test)]
    mod tests {
        use std::{fs, io, time::Duration};

        use tokio::net::UnixListener;

        use super::{REFUSED_SOCKET_GRACE, StartupRefusal, wait_until_socket_accepts};

        const SOCKET_TIMEOUT: Duration = Duration::from_secs(20);

        fn refused_dir() -> tempfile::TempDir {
            let dir = tempfile::tempdir().unwrap();
            fs::write(dir.path().join("refused"), "refusing to change\n").unwrap();
            dir
        }

        #[tokio::test(start_paused = true)]
        async fn dial_names_a_recorded_refusal_without_waiting_out_the_socket() {
            let dir = refused_dir();
            let path = dir.path().join("ployz.sock");
            // A socket unit that hit its start limit leaves its file unanswered.
            drop(UnixListener::bind(&path).unwrap());
            let started = tokio::time::Instant::now();
            let error = StartupRefusal::beside(&path)
                .connect(&path, SOCKET_TIMEOUT)
                .await
                .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::ConnectionRefused);
            assert!(started.elapsed() < REFUSED_SOCKET_GRACE + Duration::from_millis(50));
            let message = error.to_string();
            assert!(
                message.contains("ployzd refused to start: refusing to change"),
                "{message}"
            );
        }

        #[tokio::test(start_paused = true)]
        async fn dial_reaches_a_socket_restarting_after_a_refusal() {
            let dir = refused_dir();
            let path = dir.path().join("ployz.sock");
            let listener_path = path.clone();
            let socket = tokio::spawn(async move {
                tokio::time::sleep(REFUSED_SOCKET_GRACE / 2).await;
                let listener = UnixListener::bind(&listener_path).unwrap();
                listener.accept().await.unwrap()
            });
            StartupRefusal::beside(&path)
                .connect(&path, SOCKET_TIMEOUT)
                .await
                .unwrap();
            socket.await.unwrap();
        }

        #[tokio::test(start_paused = true)]
        async fn socket_wait_connects_once_the_listener_appears() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("ployz.sock");
            let listener_path = path.clone();
            let server = tokio::spawn(async move {
                tokio::time::sleep(Duration::from_millis(30)).await;
                let listener = UnixListener::bind(&listener_path).unwrap();
                listener.accept().await.unwrap()
            });
            wait_until_socket_accepts(&path, Duration::from_secs(1), || false)
                .await
                .unwrap();
            server.await.unwrap();
        }

        #[tokio::test]
        async fn socket_wait_times_out_with_a_message_not_a_debug_struct() {
            let dir = tempfile::tempdir().unwrap();
            let path = dir.path().join("missing.sock");
            let error = wait_until_socket_accepts(&path, Duration::from_millis(20), || false)
                .await
                .unwrap_err();
            let message = error.to_string();
            assert!(message.contains("did not become ready"), "{message}");
            assert!(!message.contains("Os {"), "{message}");
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{Refusal, STARTUP_REFUSAL_FILE, STARTUP_REFUSED, StartupRefusal};

    #[test]
    fn the_refusal_file_and_its_text_are_what_other_releases_read() {
        assert_eq!(STARTUP_REFUSAL_FILE, "refused");
        assert_eq!(STARTUP_REFUSED, "ployzd refused to start: ");
        assert_eq!(crate::NETWORK_CONFLICT_EXIT_STATUS, 78);
    }

    #[test]
    fn a_refusal_file_in_the_older_format_reads_as_its_line_under_the_text() {
        let root = tempfile::tempdir().unwrap();
        let refusal = StartupRefusal::new(root.path());
        assert_eq!(refusal.read(), None);

        fs::write(root.path().join("refused"), "   \n").unwrap();
        assert_eq!(refusal.read(), None);

        fs::write(root.path().join("refused"), "refusing to change ployz-wg\n").unwrap();
        assert_eq!(
            refusal.read().map(|read| read.to_string()).as_deref(),
            Some("ployzd refused to start: refusing to change ployz-wg")
        );
    }

    #[test]
    fn a_recorded_refusal_is_one_line_and_clearing_it_twice_is_quiet() {
        let root = tempfile::tempdir().unwrap();
        let refusal = StartupRefusal::new(root.path());
        refusal.record("refusing to change ployz-wg");
        assert_eq!(
            fs::read_to_string(root.path().join("refused")).unwrap(),
            "refusing to change ployz-wg\n"
        );
        refusal.clear();
        refusal.clear();
        assert_eq!(refusal.read(), None);
    }

    #[test]
    fn a_relayed_refusal_is_named_in_a_line_and_a_plain_failure_is_not() {
        let relayed = format!("ssh: {STARTUP_REFUSED}refusing to change ployz-wg");
        assert!(Refusal::is_named_in(&relayed));
        assert!(!Refusal::is_named_in(
            "Machine API socket did not become ready"
        ));
    }

    #[test]
    fn naming_appends_the_refusal_to_the_error_and_keeps_its_kind() {
        let root = tempfile::tempdir().unwrap();
        let refusal = StartupRefusal::new(root.path());
        let down = || std::io::Error::new(std::io::ErrorKind::ConnectionRefused, "down");
        assert_eq!(refusal.naming(down()).to_string(), "down");

        refusal.record("refusing to change ployz-wg");
        let named = refusal.naming(down());
        assert_eq!(named.kind(), std::io::ErrorKind::ConnectionRefused);
        assert_eq!(
            named.to_string(),
            "down; ployzd refused to start: refusing to change ployz-wg"
        );
    }
}
