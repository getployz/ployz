use std::{
    fs::{self, File},
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    time::{Duration, Instant},
};

use super::config::{CorrosionAgentConfig, CorrosionAgentStartMode, CorrosionRootCleanup};
use super::ownership;
use super::{
    CorrosionAdoption, CorrosionAgentError, read_log, setup_error, store_probe_statement,
    store_probe_timeout, write_config,
};
use crate::{CorrosionStore, StoreTimeout};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CorrosionShutdown {
    Graceful,
    Forced,
    Unmanaged,
    UnexpectedExit(CorrosionExit),
}

impl CorrosionShutdown {
    #[must_use]
    pub fn is_expected_shutdown(&self) -> bool {
        matches!(self, Self::Graceful | Self::Forced | Self::Unmanaged)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CorrosionExit {
    status: String,
    stdout: String,
    stderr: String,
}

impl CorrosionExit {
    fn from_logs(status: ExitStatus, root_dir: &Path) -> Self {
        Self {
            status: status.to_string(),
            stdout: read_log(&root_dir.join("stdout.log")),
            stderr: read_log(&root_dir.join("stderr.log")),
        }
    }

    #[must_use]
    pub fn status(&self) -> &str {
        &self.status
    }

    #[must_use]
    pub fn stdout(&self) -> &str {
        &self.stdout
    }

    #[must_use]
    pub fn stderr(&self) -> &str {
        &self.stderr
    }
}

impl std::fmt::Display for CorrosionExit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "status={}", self.status)
    }
}

#[derive(Debug)]
pub struct LocalCorrosionAgent {
    root_dir: PathBuf,
    api_addr: SocketAddr,
    gossip_addr: SocketAddr,
    prometheus_addr: SocketAddr,
    process: LocalCorrosionProcess,
}

#[derive(Debug)]
enum LocalCorrosionProcess {
    Managed {
        child: Child,
        root_cleanup: CorrosionRootCleanup,
    },
    Adopted,
}

impl LocalCorrosionAgent {
    pub async fn start(config: CorrosionAgentConfig) -> Result<Self, CorrosionAgentError> {
        Self::start_with_mode(config, CorrosionAgentStartMode::production()).await
    }

    pub async fn connect_existing(
        config: CorrosionAgentConfig,
    ) -> Result<Self, CorrosionAgentError> {
        ownership::wait_store_ready(config.api_addr, config.readiness_timeout).await?;
        ownership::verify_owner_marker(&config).await?;
        Ok(Self {
            root_dir: config.root_dir,
            api_addr: config.api_addr,
            gossip_addr: config.gossip_addr,
            prometheus_addr: config.prometheus_addr,
            process: LocalCorrosionProcess::Adopted,
        })
    }

    pub async fn adopt_existing(config: CorrosionAgentConfig) -> CorrosionAdoption {
        ownership::adopt_existing(config).await
    }

    pub(crate) async fn start_with_mode(
        config: CorrosionAgentConfig,
        mode: CorrosionAgentStartMode,
    ) -> Result<Self, CorrosionAgentError> {
        Self::start_once(config, mode).await
    }

    async fn start_once(
        config: CorrosionAgentConfig,
        mode: CorrosionAgentStartMode,
    ) -> Result<Self, CorrosionAgentError> {
        fs::create_dir_all(&config.root_dir).map_err(setup_error)?;
        let config_path = config.root_dir.join("config.toml");
        write_config(&config, &config_path)?;

        let stdout_path = config.root_dir.join("stdout.log");
        let stderr_path = config.root_dir.join("stderr.log");
        let stdout = File::create(&stdout_path).map_err(setup_error)?;
        let stderr = File::create(&stderr_path).map_err(setup_error)?;
        let child = Command::new(&config.binary_path)
            .arg("-c")
            .arg(&config_path)
            .arg("agent")
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr))
            .spawn()
            .map_err(setup_error)?;

        let mut agent = Self {
            root_dir: config.root_dir.clone(),
            api_addr: config.api_addr,
            gossip_addr: config.gossip_addr,
            prometheus_addr: config.prometheus_addr,
            process: LocalCorrosionProcess::Managed {
                child,
                root_cleanup: mode.root_cleanup(),
            },
        };
        if let Err(startup) = agent
            .wait_ready(config.readiness_timeout, &stdout_path, &stderr_path)
            .await
        {
            if let Err(cleanup) = agent.shutdown(Duration::from_secs(1)).await {
                return Err(CorrosionAgentError::StartupCleanup {
                    startup: Box::new(startup),
                    cleanup: Box::new(cleanup),
                });
            }
            return Err(startup);
        }
        if let Err(startup) = ownership::record_owner_marker(&config).await {
            if let Err(cleanup) = agent.shutdown(Duration::from_secs(1)).await {
                return Err(CorrosionAgentError::StartupCleanup {
                    startup: Box::new(startup),
                    cleanup: Box::new(cleanup),
                });
            }
            return Err(startup);
        }
        Ok(agent)
    }

    #[must_use]
    pub fn api_addr(&self) -> SocketAddr {
        self.api_addr
    }

    #[must_use]
    pub fn gossip_addr(&self) -> SocketAddr {
        self.gossip_addr
    }

    #[must_use]
    pub fn prometheus_addr(&self) -> SocketAddr {
        self.prometheus_addr
    }

    #[must_use]
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn process_id(&self) -> Option<u32> {
        match &self.process {
            LocalCorrosionProcess::Managed { child, .. } => Some(child.id()),
            LocalCorrosionProcess::Adopted => None,
        }
    }

    pub fn store(&self) -> Result<CorrosionStore, CorrosionAgentError> {
        CorrosionStore::new(self.api_addr).map_err(|source| CorrosionAgentError::Store { source })
    }

    pub async fn shutdown(
        mut self,
        timeout: Duration,
    ) -> Result<CorrosionShutdown, CorrosionAgentError> {
        let LocalCorrosionProcess::Managed { child, .. } = &mut self.process else {
            return Ok(CorrosionShutdown::Unmanaged);
        };
        if let Some(status) = child.try_wait().map_err(setup_error)? {
            let exit = CorrosionExit::from_logs(status, &self.root_dir);
            return Ok(CorrosionShutdown::UnexpectedExit(exit));
        }

        if let Err(error) = terminate_child(child) {
            if child.try_wait().map_err(setup_error)?.is_some() {
                return Ok(CorrosionShutdown::Graceful);
            }
            return Err(setup_error(error));
        }
        let deadline = Instant::now() + timeout;
        while Instant::now() < deadline {
            if child.try_wait().map_err(setup_error)?.is_some() {
                return Ok(CorrosionShutdown::Graceful);
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }

        if let Err(error) = child.kill() {
            if child.try_wait().map_err(setup_error)?.is_some() {
                return Ok(CorrosionShutdown::Graceful);
            }
            return Err(setup_error(error));
        }
        let _ = child.wait().map_err(setup_error)?;
        Ok(CorrosionShutdown::Forced)
    }

    async fn wait_ready(
        &mut self,
        readiness_timeout: Duration,
        stdout_path: &Path,
        stderr_path: &Path,
    ) -> Result<(), CorrosionAgentError> {
        let deadline = Instant::now() + readiness_timeout;
        let probe_timeout: StoreTimeout = store_probe_timeout()?;
        let probe = store_probe_statement()?;

        loop {
            let LocalCorrosionProcess::Managed { child, .. } = &mut self.process else {
                return Err(CorrosionAgentError::Setup {
                    message: "corrosion child was missing during readiness".to_string(),
                });
            };

            if let Some(status) = child.try_wait().map_err(setup_error)? {
                return Err(CorrosionAgentError::ExitedBeforeReady {
                    status,
                    stdout: read_log(stdout_path),
                    stderr: read_log(stderr_path),
                });
            }

            if Instant::now() >= deadline {
                return Err(CorrosionAgentError::ReadinessTimeout {
                    api_addr: self.api_addr,
                });
            }

            if let Ok(store) = CorrosionStore::new(self.api_addr)
                && store.query(&probe, probe_timeout).await.is_ok()
            {
                return Ok(());
            }

            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

impl Drop for LocalCorrosionAgent {
    fn drop(&mut self) {
        match &mut self.process {
            LocalCorrosionProcess::Managed {
                child,
                root_cleanup,
            } => {
                if matches!(child.try_wait(), Ok(None)) {
                    let _ = child.kill();
                    let _ = child.wait();
                }
                if root_cleanup.remove_on_drop() {
                    let _ = fs::remove_dir_all(&self.root_dir);
                }
            }
            LocalCorrosionProcess::Adopted => {}
        }
    }
}

#[cfg(unix)]
fn terminate_child(child: &mut Child) -> io::Result<()> {
    let pid = child.id().try_into().map_err(|_| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!("child pid {} does not fit pid_t", child.id()),
        )
    })?;
    let result = unsafe { libc::kill(pid, libc::SIGTERM) };
    if result == 0 || io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn terminate_child(child: &mut Child) -> io::Result<()> {
    child.kill()
}
