use std::{
    fs,
    net::SocketAddr,
    path::{Path, PathBuf},
    time::Duration,
};

use serde::Serialize;

use super::{CorrosionAgentError, setup_error, setup_message};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CorrosionAgentAddresses {
    api_addr: SocketAddr,
    gossip_addr: SocketAddr,
    prometheus_addr: SocketAddr,
}

impl CorrosionAgentAddresses {
    #[must_use]
    pub fn new(api_addr: SocketAddr, gossip_addr: SocketAddr, prometheus_addr: SocketAddr) -> Self {
        Self {
            api_addr,
            gossip_addr,
            prometheus_addr,
        }
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
}

#[derive(Debug, Clone)]
pub struct CorrosionAgentConfig {
    pub(crate) binary_path: PathBuf,
    pub(crate) root_dir: PathBuf,
    pub(crate) api_addr: SocketAddr,
    pub(crate) gossip_addr: SocketAddr,
    pub(crate) prometheus_addr: SocketAddr,
    pub(crate) bootstrap: Vec<SocketAddr>,
    pub(crate) readiness_timeout: Duration,
    schema_files: Vec<CorrosionSchemaFile>,
}

impl CorrosionAgentConfig {
    #[must_use]
    pub fn for_root_dir(root_dir: impl Into<PathBuf>, addresses: CorrosionAgentAddresses) -> Self {
        Self::new(root_dir.into(), addresses)
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn for_fixture_root_dir(
        root_dir: impl Into<PathBuf>,
        addresses: CorrosionAgentAddresses,
    ) -> Self {
        Self::new(root_dir.into(), addresses)
    }

    fn new(root_dir: PathBuf, addresses: CorrosionAgentAddresses) -> Self {
        Self {
            binary_path: default_corrosion_binary_path(),
            root_dir,
            api_addr: addresses.api_addr(),
            gossip_addr: addresses.gossip_addr(),
            prometheus_addr: addresses.prometheus_addr(),
            bootstrap: Vec::new(),
            readiness_timeout: Duration::from_secs(5),
            schema_files: Vec::new(),
        }
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
    pub fn with_readiness_timeout(mut self, readiness_timeout: Duration) -> Self {
        self.readiness_timeout = readiness_timeout;
        self
    }

    #[must_use]
    pub fn with_schema_file(
        mut self,
        name: impl Into<String>,
        contents: impl Into<String>,
    ) -> Self {
        self.schema_files
            .push(CorrosionSchemaFile::new(name, contents));
        self
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn refresh_addresses(&mut self, addresses: CorrosionAgentAddresses) {
        self.api_addr = addresses.api_addr();
        self.gossip_addr = addresses.gossip_addr();
        self.prometheus_addr = addresses.prometheus_addr();
    }

    #[must_use]
    pub fn binary_path(&self) -> &Path {
        &self.binary_path
    }

    #[must_use]
    pub fn root_dir(&self) -> &Path {
        &self.root_dir
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
    pub fn readiness_timeout(&self) -> Duration {
        self.readiness_timeout
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

impl CorrosionSchemaFile {
    fn new(name: impl Into<String>, contents: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            contents: contents.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) enum CorrosionAgentStartMode {
    Production,
    #[cfg(any(test, feature = "test-support"))]
    Fixture {
        root_cleanup: CorrosionRootCleanup,
    },
}

impl CorrosionAgentStartMode {
    pub(crate) fn production() -> Self {
        Self::Production
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn fixture(root_cleanup: CorrosionRootCleanup) -> Self {
        Self::Fixture { root_cleanup }
    }

    pub(crate) fn root_cleanup(&self) -> CorrosionRootCleanup {
        match self {
            Self::Production => CorrosionRootCleanup::Keep,
            #[cfg(any(test, feature = "test-support"))]
            Self::Fixture { root_cleanup } => *root_cleanup,
        }
    }
}

#[derive(Debug, Clone, Copy)]
pub(crate) enum CorrosionRootCleanup {
    Keep,
    #[cfg(any(test, feature = "test-support"))]
    RemoveOnDrop,
}

impl CorrosionRootCleanup {
    pub(crate) fn remove_on_drop(self) -> bool {
        match self {
            Self::Keep => false,
            #[cfg(any(test, feature = "test-support"))]
            Self::RemoveOnDrop => true,
        }
    }
}

pub(crate) fn write_config(
    config: &CorrosionAgentConfig,
    mode: &CorrosionAgentStartMode,
    config_path: &Path,
) -> Result<(), CorrosionAgentError> {
    let contents = toml::to_string(&CorrosionConfigFile::new(
        config,
        schema_paths(config, mode)?,
    ))
    .map_err(|error| setup_message(format!("serialize corrosion config: {error}")))?;
    fs::write(config_path, contents).map_err(setup_error)
}

#[derive(Serialize)]
struct CorrosionConfigFile {
    db: CorrosionDbConfig,
    gossip: CorrosionGossipConfig,
    api: CorrosionApiConfig,
    admin: CorrosionAdminConfig,
    telemetry: CorrosionTelemetryConfig,
    log: CorrosionLogConfig,
}

impl CorrosionConfigFile {
    fn new(config: &CorrosionAgentConfig, schema_paths: Vec<String>) -> Self {
        Self {
            db: CorrosionDbConfig {
                path: config.root_dir.join("state.db").display().to_string(),
                schema_paths,
            },
            gossip: CorrosionGossipConfig {
                addr: gossip_bind_addr(config.gossip_addr),
                external_addr: config.gossip_addr.to_string(),
                bootstrap: config.bootstrap.iter().map(SocketAddr::to_string).collect(),
                plaintext: true,
            },
            api: CorrosionApiConfig {
                addr: config.api_addr.to_string(),
            },
            admin: CorrosionAdminConfig {
                path: config.root_dir.join("admin.sock").display().to_string(),
            },
            telemetry: CorrosionTelemetryConfig {
                prometheus: CorrosionPrometheusConfig {
                    addr: config.prometheus_addr.to_string(),
                },
            },
            log: CorrosionLogConfig {
                format: "json",
                colors: false,
            },
        }
    }
}

#[derive(Serialize)]
struct CorrosionDbConfig {
    path: String,
    schema_paths: Vec<String>,
}

#[derive(Serialize)]
struct CorrosionGossipConfig {
    addr: String,
    external_addr: String,
    bootstrap: Vec<String>,
    plaintext: bool,
}

#[derive(Serialize)]
struct CorrosionApiConfig {
    addr: String,
}

#[derive(Serialize)]
struct CorrosionAdminConfig {
    path: String,
}

#[derive(Serialize)]
struct CorrosionTelemetryConfig {
    prometheus: CorrosionPrometheusConfig,
}

#[derive(Serialize)]
struct CorrosionPrometheusConfig {
    addr: String,
}

#[derive(Serialize)]
struct CorrosionLogConfig {
    format: &'static str,
    colors: bool,
}

fn gossip_bind_addr(gossip_addr: SocketAddr) -> String {
    if gossip_addr.is_ipv6() {
        format!("[::]:{}", gossip_addr.port())
    } else {
        gossip_addr.to_string()
    }
}

fn schema_paths(
    config: &CorrosionAgentConfig,
    mode: &CorrosionAgentStartMode,
) -> Result<Vec<String>, CorrosionAgentError> {
    match mode {
        CorrosionAgentStartMode::Production => write_schema_files(config),
        #[cfg(any(test, feature = "test-support"))]
        CorrosionAgentStartMode::Fixture { .. } => write_schema_files(config),
    }
}

fn write_schema_files(config: &CorrosionAgentConfig) -> Result<Vec<String>, CorrosionAgentError> {
    if config.schema_files.is_empty() {
        return Ok(Vec::new());
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
    Ok(vec![schema_dir.display().to_string()])
}
