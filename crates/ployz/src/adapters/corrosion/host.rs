use crate::error::{Error, Result};
use ployz_sdk::store::StoreRuntimeControl;
use std::path::PathBuf;
use std::process::Stdio;
use std::time::Duration;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};

const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);

pub struct HostCorrosion {
    binary: PathBuf,
    config_path: PathBuf,
    child: Mutex<Option<Child>>,
}

impl HostCorrosion {
    pub fn new(binary: impl Into<PathBuf>, config_path: impl Into<PathBuf>) -> Self {
        Self {
            binary: binary.into(),
            config_path: config_path.into(),
            child: Mutex::new(None),
        }
    }
}

impl StoreRuntimeControl for HostCorrosion {
    async fn start(&self) -> Result<()> {
        let mut guard = self.child.lock().await;

        // Already running — check if the process is still alive.
        if let Some(ref mut child) = *guard {
            match child.try_wait() {
                Ok(None) => {
                    info!(binary = %self.binary.display(), "corrosion already running");
                    return Ok(());
                }
                Ok(Some(status)) => {
                    warn!(%status, "corrosion exited, restarting");
                }
                Err(e) => {
                    warn!(?e, "failed to check corrosion status, restarting");
                }
            }
        }

        let child = Command::new(&self.binary)
            .arg("agent")
            .arg("-c")
            .arg(&self.config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|e| {
                Error::operation(
                    "corrosion start",
                    format!("failed to spawn {}: {e}", self.binary.display()),
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

        // Try graceful shutdown with SIGINT first (Unix only).
        #[cfg(unix)]
        if let Some(raw_pid) = pid {
            unsafe {
                libc::kill(raw_pid as i32, libc::SIGINT);
            }
            // Wait for graceful exit with a timeout.
            match tokio::time::timeout(STOP_GRACE_PERIOD, child.wait()).await {
                Ok(Ok(status)) => {
                    info!(pid = raw_pid, %status, "corrosion stopped gracefully");
                    guard.take();
                    return Ok(());
                }
                Ok(Err(e)) => {
                    warn!(pid = raw_pid, ?e, "wait after SIGINT failed, force killing");
                }
                Err(_) => {
                    warn!(
                        pid = raw_pid,
                        "corrosion did not exit after SIGINT, force killing"
                    );
                }
            }
        }

        // Fallback: force kill.
        child.kill().await.map_err(|e| {
            Error::operation("corrosion stop", format!("failed to kill pid {pid:?}: {e}"))
        })?;
        let status = child.wait().await.map_err(|e| {
            Error::operation("corrosion stop", format!("failed to wait pid {pid:?}: {e}"))
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
