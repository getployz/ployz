use ployz_types::{Error, Result};
use ployz_types::store::StoreRuntimeControl;
use std::fs::OpenOptions;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};

const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);
const CORROSION_LOG_PATH_ENV: &str = "PLOYZ_CORROSION_LOG_PATH";
const CORROSION_RUST_LOG_ENV: &str = "PLOYZ_CORROSION_RUST_LOG";

pub struct HostCorrosion {
    binary: PathBuf,
    config_path: PathBuf,
    log_path: PathBuf,
    child: Mutex<Option<Child>>,
}

impl HostCorrosion {
    pub fn new(binary: impl Into<PathBuf>, config_path: impl Into<PathBuf>) -> Self {
        let config_path = config_path.into();
        let log_path = default_log_path(&config_path);
        Self {
            binary: binary.into(),
            config_path,
            log_path,
            child: Mutex::new(None),
        }
    }
}

fn default_log_path(config_path: &Path) -> PathBuf {
    config_path
        .parent()
        .map(|parent| parent.join("corrosion.log"))
        .unwrap_or_else(|| PathBuf::from("corrosion.log"))
}

fn configured_log_path(default_log_path: &Path) -> Option<PathBuf> {
    match std::env::var(CORROSION_LOG_PATH_ENV) {
        Ok(path) if path.is_empty() => Some(default_log_path.to_path_buf()),
        Ok(path) => Some(PathBuf::from(path)),
        Err(_) => None,
    }
}

impl StoreRuntimeControl for HostCorrosion {
    async fn start(&self) -> Result<()> {
        let mut guard = self.child.lock().await;

        if let Some(ref mut child) = *guard {
            match child.try_wait() {
                Ok(None) => {
                    info!(binary = %self.binary.display(), "corrosion already running");
                    return Ok(());
                }
                Ok(Some(status)) => {
                    warn!(%status, "corrosion exited, restarting");
                }
                Err(error) => {
                    warn!(?error, "failed to check corrosion status, restarting");
                }
            }
        }

        let mut command = Command::new(&self.binary);
        command
            .arg("agent")
            .arg("-c")
            .arg(&self.config_path)
            .stdin(Stdio::null())
            .kill_on_drop(true);

        match configured_log_path(&self.log_path) {
            Some(log_path) => {
                let log_file = OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                    .map_err(|error| {
                        Error::operation(
                            "corrosion start",
                            format!("failed to open log file {}: {error}", log_path.display()),
                        )
                    })?;
                let stdout_log = log_file.try_clone().map_err(|error| {
                    Error::operation(
                        "corrosion start",
                        format!(
                            "failed to clone log file handle {}: {error}",
                            log_path.display()
                        ),
                    )
                })?;
                command
                    .stdout(Stdio::from(stdout_log))
                    .stderr(Stdio::from(log_file));
                info!(log = %log_path.display(), "corrosion file logging enabled");
            }
            None => {
                command.stdout(Stdio::null()).stderr(Stdio::null());
            }
        }

        if let Ok(rust_log) = std::env::var(CORROSION_RUST_LOG_ENV) {
            command.env("RUST_LOG", rust_log);
        }

        let child = command.spawn().map_err(|error| {
            Error::operation(
                "corrosion start",
                format!("failed to spawn {}: {error}", self.binary.display()),
            )
        })?;

        info!(
            pid = child.id(),
            binary = %self.binary.display(),
            config = %self.config_path.display(),
            "corrosion started"
        );

        *guard = Some(child);
        Ok(())
    }

    async fn stop(&self) -> Result<()> {
        let mut guard = self.child.lock().await;
        let Some(ref mut child) = *guard else {
            return Ok(());
        };

        let pid = child.id();

        #[cfg(unix)]
        if let Some(raw_pid) = pid {
            unsafe {
                libc::kill(raw_pid as i32, libc::SIGINT);
            }
            match tokio::time::timeout(STOP_GRACE_PERIOD, child.wait()).await {
                Ok(Ok(status)) => {
                    info!(pid = raw_pid, %status, "corrosion stopped gracefully");
                    guard.take();
                    return Ok(());
                }
                Ok(Err(error)) => {
                    warn!(pid = raw_pid, ?error, "wait after SIGINT failed, force killing");
                }
                Err(_) => {
                    warn!(
                        pid = raw_pid,
                        "corrosion did not exit after SIGINT, force killing"
                    );
                }
            }
        }

        child.kill().await.map_err(|error| {
            Error::operation("corrosion stop", format!("failed to kill pid {pid:?}: {error}"))
        })?;
        let status = child.wait().await.map_err(|error| {
            Error::operation("corrosion stop", format!("failed to wait pid {pid:?}: {error}"))
        })?;

        info!(?pid, %status, "corrosion stopped (killed)");
        guard.take();
        Ok(())
    }

    async fn healthy(&self) -> bool {
        let mut guard = self.child.lock().await;
        match guard.as_mut() {
            Some(child) => matches!(child.try_wait(), Ok(None)),
            None => false,
        }
    }
}
