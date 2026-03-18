use std::fs::OpenOptions;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ployz_runtime_api::ServiceRuntime;
use ployz_runtime_backends::runtime::labels::build_system_labels;
use ployz_runtime_backends::runtime::{
    ContainerEngine, EnsureAction, PullPolicy, RuntimeContainerSpec,
};
use ployz_store_api::StoreDriver;
use ployz_types::Result;
use ployz_types::model::OverlayIp;
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::client::Transport;
use crate::config as corrosion_config;
use crate::{CorrosionStore, SCHEMA_SQL};

const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);
const CORROSION_LOG_PATH_ENV: &str = "PLOYZ_CORROSION_LOG_PATH";
const CORROSION_RUST_LOG_ENV: &str = "PLOYZ_CORROSION_RUST_LOG";

pub struct ManagedCorrosionStore {
    pub store: StoreDriver,
    pub runtime: Arc<dyn ServiceRuntime>,
}

fn which_corrosion() -> std::result::Result<PathBuf, String> {
    let candidates = ["/usr/local/bin/corrosion", "/usr/bin/corrosion"];
    for path in candidates {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(String::from(
        "corrosion binary not found (expected at /usr/local/bin/corrosion)",
    ))
}

pub async fn docker(
    overlay_ip: OverlayIp,
    network_dir: &Path,
    bootstrap: &[String],
    network_id: &str,
    image: &str,
) -> std::result::Result<ManagedCorrosionStore, String> {
    let paths = corrosion_config::Paths::new(network_dir);
    let gossip_addr = SocketAddr::new(
        IpAddr::V6(overlay_ip.0),
        corrosion_config::DEFAULT_GOSSIP_PORT,
    );
    let api_addr = SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_API_PORT);
    let config_paths = corrosion_config::Paths {
        db: PathBuf::from("/data/store.db"),
        admin: PathBuf::from("/data/admin.sock"),
        schema: PathBuf::from("/etc/corrosion/schema.sql"),
        ..paths.clone()
    };

    corrosion_config::write_config(
        &config_paths,
        &paths,
        SCHEMA_SQL,
        gossip_addr,
        api_addr,
        bootstrap,
        Some(network_id),
    )
    .map_err(|error| format!("write corrosion config: {error}"))?;

    let local_api = SocketAddr::new(
        IpAddr::V4(Ipv4Addr::LOCALHOST),
        corrosion_config::DEFAULT_API_PORT,
    );
    let store = CorrosionStore::new(
        api_addr,
        Transport::Bridge {
            local_addr: local_api,
        },
        None,
    );

    let config_host = paths.config.to_string_lossy().into_owned();
    let schema_host = paths.schema.to_string_lossy().into_owned();
    let service = DockerCorrosion::new("ployz-corrosion", image)
        .cmd(vec![
            "agent".into(),
            "-c".into(),
            "/etc/corrosion/config.toml".into(),
        ])
        .volume(&format!("{config_host}:/etc/corrosion/config.toml:ro"))
        .volume(&format!("{schema_host}:/etc/corrosion/schema.sql:ro"))
        .volume("ployz-corrosion-data:/data")
        .network_mode("container:ployz-networking")
        .build()
        .await
        .map_err(|error| format!("docker service: {error}"))?;

    Ok(ManagedCorrosionStore {
        store: StoreDriver::from_store(Arc::new(store)),
        runtime: Arc::new(service),
    })
}

pub fn host(
    overlay_ip: OverlayIp,
    network_dir: &Path,
    bootstrap: &[String],
    network_id: &str,
) -> std::result::Result<ManagedCorrosionStore, String> {
    let paths = corrosion_config::Paths::new(network_dir);
    let gossip_addr = SocketAddr::new(
        IpAddr::V6(overlay_ip.0),
        corrosion_config::DEFAULT_GOSSIP_PORT,
    );
    let api_addr = SocketAddr::new(IpAddr::V6(overlay_ip.0), corrosion_config::DEFAULT_API_PORT);

    corrosion_config::write_config(
        &paths,
        &paths,
        SCHEMA_SQL,
        gossip_addr,
        api_addr,
        bootstrap,
        Some(network_id),
    )
    .map_err(|error| format!("write corrosion config: {error}"))?;

    let store = CorrosionStore::new(api_addr, Transport::Direct, Some(paths.admin.clone()));
    let service = HostCorrosion::new(which_corrosion()?, &paths.config);

    Ok(ManagedCorrosionStore {
        store: StoreDriver::from_store(Arc::new(store)),
        runtime: Arc::new(service),
    })
}

struct HostCorrosion {
    binary: PathBuf,
    config_path: PathBuf,
    log_path: PathBuf,
    child: Mutex<Option<Child>>,
}

impl HostCorrosion {
    fn new(binary: impl Into<PathBuf>, config_path: impl Into<PathBuf>) -> Self {
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

#[async_trait]
impl ServiceRuntime for HostCorrosion {
    async fn start(&self) -> std::result::Result<(), String> {
        let mut guard = self.child.lock().await;

        if let Some(child) = &mut *guard {
            match child.try_wait() {
                Ok(None) => {
                    info!(binary = %self.binary.display(), "corrosion already running");
                    return Ok(());
                }
                Ok(Some(status)) => warn!(%status, "corrosion exited, restarting"),
                Err(error) => warn!(?error, "failed to check corrosion status, restarting"),
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
                        ployz_types::Error::operation(
                            "corrosion start",
                            format!("failed to open log file {}: {error}", log_path.display()),
                        )
                        .to_string()
                    })?;
                let stdout_log = log_file.try_clone().map_err(|error| {
                    ployz_types::Error::operation(
                        "corrosion start",
                        format!(
                            "failed to clone log file handle {}: {error}",
                            log_path.display()
                        ),
                    )
                    .to_string()
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
            ployz_types::Error::operation(
                "corrosion start",
                format!("failed to spawn {}: {error}", self.binary.display()),
            )
            .to_string()
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

    async fn stop(&self) -> std::result::Result<(), String> {
        let mut guard = self.child.lock().await;
        let Some(child) = &mut *guard else {
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
                Ok(Err(error)) => warn!(
                    pid = raw_pid,
                    ?error,
                    "wait after SIGINT failed, force killing"
                ),
                Err(_) => warn!(
                    pid = raw_pid,
                    "corrosion did not exit after SIGINT, force killing"
                ),
            }
        }

        child.kill().await.map_err(|error| {
            ployz_types::Error::operation(
                "corrosion stop",
                format!("failed to kill pid {pid:?}: {error}"),
            )
            .to_string()
        })?;
        let status = child.wait().await.map_err(|error| {
            ployz_types::Error::operation(
                "corrosion stop",
                format!("failed to wait pid {pid:?}: {error}"),
            )
            .to_string()
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

struct DockerCorrosion {
    engine: ContainerEngine,
    container_name: String,
    image: String,
    cmd: Option<Vec<String>>,
    env: Vec<String>,
    volumes: Vec<String>,
    network_mode: Option<String>,
}

struct DockerCorrosionBuilder {
    container_name: String,
    image: String,
    cmd: Option<Vec<String>>,
    env: Vec<String>,
    volumes: Vec<String>,
    network_mode: Option<String>,
}

impl DockerCorrosionBuilder {
    #[must_use]
    fn cmd(mut self, cmd: Vec<String>) -> Self {
        self.cmd = Some(cmd);
        self
    }

    #[must_use]
    fn volume(mut self, spec: &str) -> Self {
        self.volumes.push(spec.to_string());
        self
    }

    #[must_use]
    fn network_mode(mut self, mode: &str) -> Self {
        self.network_mode = Some(mode.to_string());
        self
    }

    async fn build(self) -> Result<DockerCorrosion> {
        Ok(DockerCorrosion {
            engine: ContainerEngine::connect().await?,
            container_name: self.container_name,
            image: self.image,
            cmd: self.cmd,
            env: self.env,
            volumes: self.volumes,
            network_mode: self.network_mode,
        })
    }
}

impl DockerCorrosion {
    fn new(container_name: &str, image: &str) -> DockerCorrosionBuilder {
        DockerCorrosionBuilder {
            container_name: container_name.to_string(),
            image: image.to_string(),
            cmd: None,
            env: Vec::new(),
            volumes: Vec::new(),
            network_mode: None,
        }
    }

    fn to_runtime_spec(&self) -> RuntimeContainerSpec {
        let key = "system/corrosion".to_string();
        let env = self
            .env
            .iter()
            .map(|entry| match entry.split_once('=') {
                Some((key, value)) => (key.to_string(), value.to_string()),
                None => (entry.clone(), String::new()),
            })
            .collect();

        RuntimeContainerSpec {
            key: key.clone(),
            container_name: self.container_name.clone(),
            image: self.image.clone(),
            pull_policy: PullPolicy::IfNotPresent,
            cmd: self.cmd.clone(),
            entrypoint: None,
            env,
            labels: build_system_labels(&key, None),
            binds: self.volumes.clone(),
            tmpfs: std::collections::HashMap::new(),
            dns_servers: Vec::new(),
            network_mode: self.network_mode.clone(),
            port_bindings: None,
            exposed_ports: None,
            cap_add: Vec::new(),
            cap_drop: Vec::new(),
            privileged: false,
            user: None,
            restart_policy: None,
            memory_bytes: None,
            nano_cpus: None,
            sysctls: std::collections::HashMap::new(),
            stop_timeout: None,
            pid_mode: None,
        }
    }
}

#[async_trait]
impl ServiceRuntime for DockerCorrosion {
    async fn start(&self) -> std::result::Result<(), String> {
        let result = self
            .engine
            .ensure(&self.to_runtime_spec())
            .await
            .map_err(|error| error.to_string())?;

        match &result.action {
            EnsureAction::Adopted => {
                info!(name = %self.container_name, "adopted existing container")
            }
            EnsureAction::Created => {
                info!(name = %self.container_name, image = %self.image, "container started")
            }
            EnsureAction::Recreated { changed } => info!(
                name = %self.container_name,
                image = %self.image,
                changed = ?changed,
                "container recreated"
            ),
        }

        Ok(())
    }

    async fn stop(&self) -> std::result::Result<(), String> {
        self.engine
            .remove(&self.container_name, STOP_GRACE_PERIOD)
            .await
            .map_err(|error| error.to_string())
    }

    async fn healthy(&self) -> bool {
        match self.engine.inspect(&self.container_name).await {
            Ok(Some(observed)) => observed.running,
            Ok(None) => false,
            Err(_) => false,
        }
    }
}
