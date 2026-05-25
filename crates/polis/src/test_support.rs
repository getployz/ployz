use std::{
    fs::{self, File},
    io,
    net::{SocketAddr, TcpListener, UdpSocket},
    path::{Path, PathBuf},
    process::{Child, Command, ExitStatus, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use thiserror::Error;

use crate::{CorrosionStore, StoreError, StoreStatement, StoreTimeout};

static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(1);

#[derive(Debug, Error)]
pub enum CorrosionAgentError {
    #[error("corrosion setup failed: {message}")]
    Setup { message: String },

    #[error(
        "corrosion process exited before readiness: status={status}, stdout={stdout}, stderr={stderr}"
    )]
    ExitedBeforeReady {
        status: ExitStatus,
        stdout: String,
        stderr: String,
    },

    #[error("corrosion agent was not ready before deadline at {api_addr}")]
    ReadinessTimeout { api_addr: SocketAddr },

    #[error("corrosion store error during readiness: {source}")]
    Store { source: StoreError },
}

#[derive(Debug, Clone)]
pub struct CorrosionAgentConfig {
    binary_path: PathBuf,
    root_dir: PathBuf,
    api_addr: SocketAddr,
    gossip_addr: SocketAddr,
    prometheus_addr: SocketAddr,
    bootstrap: Vec<SocketAddr>,
    schema_files: Vec<CorrosionSchemaFile>,
    readiness_timeout: Duration,
    remove_root_on_drop: bool,
}

impl CorrosionAgentConfig {
    pub fn isolated() -> Result<Self, CorrosionAgentError> {
        let root_dir = unique_temp_dir()?;
        Ok(Self {
            binary_path: default_corrosion_binary_path(),
            root_dir,
            api_addr: free_tcp_addr()?,
            gossip_addr: free_udp_addr()?,
            prometheus_addr: free_tcp_addr()?,
            bootstrap: Vec::new(),
            schema_files: Vec::new(),
            readiness_timeout: Duration::from_secs(5),
            remove_root_on_drop: true,
        })
    }

    #[must_use]
    pub fn with_binary_path(mut self, binary_path: impl Into<PathBuf>) -> Self {
        self.binary_path = binary_path.into();
        self
    }

    #[must_use]
    pub fn with_bootstrap(mut self, bootstrap: Vec<SocketAddr>) -> Self {
        self.bootstrap = bootstrap;
        self
    }

    #[must_use]
    pub fn with_schema_file(
        mut self,
        name: impl Into<String>,
        contents: impl Into<String>,
    ) -> Self {
        self.schema_files.push(CorrosionSchemaFile {
            name: name.into(),
            contents: contents.into(),
        });
        self
    }

    #[must_use]
    pub fn with_readiness_timeout(mut self, readiness_timeout: Duration) -> Self {
        self.readiness_timeout = readiness_timeout;
        self
    }

    #[must_use]
    pub fn api_addr(&self) -> SocketAddr {
        self.api_addr
    }

    #[must_use]
    pub fn gossip_addr(&self) -> SocketAddr {
        self.gossip_addr
    }
}

fn default_corrosion_binary_path() -> PathBuf {
    if let Some(path) = std::env::var_os("CORROSION_BIN") {
        return PathBuf::from(path);
    }

    let repo_tools_binary =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tools/bin/corrosion");
    if repo_tools_binary.is_file() {
        return repo_tools_binary;
    }

    PathBuf::from("corrosion")
}

#[derive(Debug, Clone)]
struct CorrosionSchemaFile {
    name: String,
    contents: String,
}

#[derive(Debug)]
pub struct LocalCorrosionAgent {
    root_dir: PathBuf,
    api_addr: SocketAddr,
    gossip_addr: SocketAddr,
    child: Child,
    remove_root_on_drop: bool,
}

impl LocalCorrosionAgent {
    pub async fn start(config: CorrosionAgentConfig) -> Result<Self, CorrosionAgentError> {
        let mut last_error = None;
        let mut config = config;
        for attempt in 0..3 {
            if attempt > 0 {
                config.refresh_addresses()?;
            }
            match Self::start_once(config.clone()).await {
                Ok(agent) => return Ok(agent),
                Err(error) if error.is_retryable_startup_failure() => {
                    last_error = Some(error);
                }
                Err(error) => return Err(error),
            }
        }
        Err(last_error.unwrap_or_else(|| CorrosionAgentError::Setup {
            message: "corrosion startup retry failed without an error".to_string(),
        }))
    }

    async fn start_once(config: CorrosionAgentConfig) -> Result<Self, CorrosionAgentError> {
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
            root_dir: config.root_dir,
            api_addr: config.api_addr,
            gossip_addr: config.gossip_addr,
            child,
            remove_root_on_drop: config.remove_root_on_drop,
        };
        agent
            .wait_ready(config.readiness_timeout, &stdout_path, &stderr_path)
            .await?;
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
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
    }

    pub fn store(&self) -> Result<CorrosionStore, CorrosionAgentError> {
        CorrosionStore::new(self.api_addr).map_err(|source| CorrosionAgentError::Store { source })
    }

    async fn wait_ready(
        &mut self,
        readiness_timeout: Duration,
        stdout_path: &Path,
        stderr_path: &Path,
    ) -> Result<(), CorrosionAgentError> {
        let deadline = Instant::now() + readiness_timeout;
        let probe_timeout =
            StoreTimeout::seconds(1).map_err(|source| CorrosionAgentError::Store { source })?;
        let probe = StoreStatement::new("SELECT 1 AS ready")
            .map_err(|source| CorrosionAgentError::Store { source })?;

        loop {
            if let Some(status) = self.child.try_wait().map_err(setup_error)? {
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

impl CorrosionAgentConfig {
    fn refresh_addresses(&mut self) -> Result<(), CorrosionAgentError> {
        self.api_addr = free_tcp_addr()?;
        self.gossip_addr = free_udp_addr()?;
        self.prometheus_addr = free_tcp_addr()?;
        Ok(())
    }
}

impl CorrosionAgentError {
    fn is_retryable_startup_failure(&self) -> bool {
        match self {
            Self::ExitedBeforeReady { stdout, stderr, .. } => {
                let logs = format!("{stdout}\n{stderr}").to_ascii_lowercase();
                logs.contains("address already in use")
                    || logs.contains("addrinuse")
                    || logs.contains("failed to bind")
                    || logs.contains("bind")
            }
            Self::ReadinessTimeout { .. } => true,
            Self::Setup { .. } | Self::Store { .. } => false,
        }
    }
}

impl Drop for LocalCorrosionAgent {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
        if self.remove_root_on_drop {
            let _ = fs::remove_dir_all(&self.root_dir);
        }
    }
}

fn write_config(
    config: &CorrosionAgentConfig,
    config_path: &Path,
) -> Result<(), CorrosionAgentError> {
    let db_path = config.root_dir.join("state.db");
    let admin_path = config.root_dir.join("admin.sock");
    let schema_paths = write_schema_files(config)?;
    let bootstrap = config
        .bootstrap
        .iter()
        .map(|addr| format!("\"{addr}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let gossip_bind_addr = if config.gossip_addr.is_ipv6() {
        format!("[::]:{}", config.gossip_addr.port())
    } else {
        config.gossip_addr.to_string()
    };
    let contents = format!(
        r#"[db]
path = "{}"
schema_paths = [{}]

[gossip]
addr = "{}"
external_addr = "{}"
bootstrap = [{}]
plaintext = true

[api]
addr = "{}"

[admin]
path = "{}"

[telemetry]
prometheus.addr = "{}"

[log]
format = "json"
colors = false
"#,
        db_path.display(),
        schema_paths,
        gossip_bind_addr,
        config.gossip_addr,
        bootstrap,
        config.api_addr,
        admin_path.display(),
        config.prometheus_addr,
    );
    fs::write(config_path, contents).map_err(setup_error)
}

fn write_schema_files(config: &CorrosionAgentConfig) -> Result<String, CorrosionAgentError> {
    if config.schema_files.is_empty() {
        return Ok(String::new());
    }

    let schema_dir = config.root_dir.join("schema");
    fs::create_dir_all(&schema_dir).map_err(setup_error)?;
    for schema in &config.schema_files {
        let name = Path::new(&schema.name);
        let Some(file_name) = name.file_name() else {
            return Err(CorrosionAgentError::Setup {
                message: "schema file name is empty".to_string(),
            });
        };
        fs::write(schema_dir.join(file_name), &schema.contents).map_err(setup_error)?;
    }
    Ok(format!("\"{}\"", schema_dir.display()))
}

fn unique_temp_dir() -> Result<PathBuf, CorrosionAgentError> {
    let mut root = std::env::temp_dir();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|error| CorrosionAgentError::Setup {
            message: error.to_string(),
        })?
        .as_nanos();
    let next = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    root.push(format!(
        "ployz-corrosion-{}-{now}-{next}",
        std::process::id()
    ));
    Ok(root)
}

fn free_tcp_addr() -> Result<SocketAddr, CorrosionAgentError> {
    TcpListener::bind(("127.0.0.1", 0))
        .and_then(|listener| listener.local_addr())
        .map_err(setup_error)
}

fn free_udp_addr() -> Result<SocketAddr, CorrosionAgentError> {
    UdpSocket::bind("[::1]:0")
        .and_then(|socket| socket.local_addr())
        .map_err(setup_error)
}

fn setup_error(error: io::Error) -> CorrosionAgentError {
    CorrosionAgentError::Setup {
        message: error.to_string(),
    }
}

fn read_log(path: &Path) -> String {
    fs::read_to_string(path).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn missing_corrosion_binary_is_setup_error() {
        let config = CorrosionAgentConfig::isolated()
            .expect("config")
            .with_binary_path("/definitely/missing/corrosion");

        let error = LocalCorrosionAgent::start(config).await.expect_err("error");

        assert!(matches!(error, CorrosionAgentError::Setup { .. }));
    }
}
