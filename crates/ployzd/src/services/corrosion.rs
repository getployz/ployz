use std::fs::OpenOptions;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ployz_corrosion::client::Transport;
use ployz_corrosion::config as corrosion_config;
use ployz_corrosion::{CorrosionStore, SCHEMA_SQL};
use ployz_runtime_backends::runtime::labels::build_system_labels;
use ployz_runtime_backends::runtime::{
    ContainerEngine, EnsureAction, PullPolicy, RuntimeContainerSpec,
};
use ployz_store_api::{
    DeployCommit, DeployRecordUpdate, DeployRepository, DeployRevisionUpsert, DeploySnapshot,
    InstanceStatusRepository, InviteRepository, MachineRegistry, RoutingSnapshotReader,
    StoreBackend, StoreDriver, StoreRuntimeControl, SyncProbe, SyncStatus,
};
use ployz_types::Result;
use ployz_types::model::{
    DeployId, DeployRecord, InstanceId, InstanceStatusRecord, InviteRecord, MachineEvent,
    MachineId, MachineMembership, OverlayIp, RoutingState, ServiceReleaseRecord,
};
use ployz_types::spec::Namespace;
use tokio::process::{Child, Command};
use tokio::sync::{Mutex, mpsc};
use tracing::{info, warn};

const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);
const CORROSION_LOG_PATH_ENV: &str = "PLOYZ_CORROSION_LOG_PATH";
const CORROSION_RUST_LOG_ENV: &str = "PLOYZ_CORROSION_RUST_LOG";

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

pub async fn corrosion_docker(
    overlay_ip: OverlayIp,
    network_dir: &Path,
    bootstrap: &[String],
    network_id: &str,
    image: &str,
) -> std::result::Result<StoreDriver, String> {
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
    let data_volume = corrosion_data_volume_name(network_id);
    let service = DockerCorrosion::builder("ployz-corrosion", image, &data_volume)
        .cmd(vec![
            "agent".into(),
            "-c".into(),
            "/etc/corrosion/config.toml".into(),
        ])
        .volume(&format!("{config_host}:/etc/corrosion/config.toml:ro"))
        .volume(&format!("{schema_host}:/etc/corrosion/schema.sql:ro"))
        .volume(&format!("{data_volume}:/data"))
        .network_mode("container:ployz-networking")
        .build()
        .await
        .map_err(|error| format!("docker service: {error}"))?;

    let backend = Arc::new(CorrosionBackend {
        store,
        service: Arc::new(service),
    });
    Ok(StoreDriver::from_backend(
        Arc::clone(&backend) as Arc<dyn StoreBackend>,
        backend as Arc<dyn StoreRuntimeControl>,
    ))
}

pub fn corrosion_host(
    overlay_ip: OverlayIp,
    network_dir: &Path,
    bootstrap: &[String],
    network_id: &str,
) -> std::result::Result<StoreDriver, String> {
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
    let service = HostCorrosion::new(which_corrosion()?, &paths.config, &paths.dir);

    let backend = Arc::new(CorrosionBackend {
        store,
        service: Arc::new(service),
    });
    Ok(StoreDriver::from_backend(
        Arc::clone(&backend) as Arc<dyn StoreBackend>,
        backend as Arc<dyn StoreRuntimeControl>,
    ))
}

struct CorrosionBackend<S> {
    store: CorrosionStore,
    service: Arc<S>,
}

#[async_trait]
impl<S> StoreBackend for CorrosionBackend<S>
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    async fn init(&self) -> Result<()> {
        self.store.init().await
    }

    async fn list_machines(&self) -> Result<Vec<MachineMembership>> {
        self.store.list_machines().await
    }

    async fn upsert_self_machine(&self, record: &MachineMembership) -> Result<()> {
        self.store.upsert_self_machine(record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        self.store.delete_machine(id).await
    }

    async fn subscribe_machines(
        &self,
    ) -> Result<(Vec<MachineMembership>, mpsc::Receiver<MachineEvent>)> {
        self.store.subscribe_machines().await
    }

    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        self.store.create_invite(invite).await
    }

    async fn get_invite(&self, invite_id: &str) -> Result<Option<InviteRecord>> {
        self.store.get_invite(invite_id).await
    }

    async fn list_invites(&self) -> Result<Vec<InviteRecord>> {
        self.store.list_invites().await
    }

    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> Result<InviteRecord> {
        self.store
            .redeem_invite(invite_id, machine_id, now_unix_secs)
            .await
    }

    async fn revoke_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<InviteRecord> {
        self.store.revoke_invite(invite_id, now_unix_secs).await
    }

    async fn load_routing_state(&self) -> Result<RoutingState> {
        self.store.load_routing_state().await
    }

    async fn subscribe_routing_invalidations(&self) -> Result<mpsc::Receiver<()>> {
        self.store.subscribe_routing_invalidations().await
    }

    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        self.store.list_deploy_releases(namespace).await
    }

    async fn load_deploy_snapshot(&self, namespace: &Namespace) -> Result<DeploySnapshot> {
        self.store.load_deploy_snapshot(namespace).await
    }

    async fn record_service_revision(&self, command: &DeployRevisionUpsert) -> Result<()> {
        self.store.record_service_revision(command).await
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> Result<()> {
        self.store.commit_deploy(command).await
    }

    async fn update_deploy_record(&self, command: &DeployRecordUpdate) -> Result<()> {
        self.store.update_deploy_record(command).await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        self.store.get_deploy(deploy_id).await
    }

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        self.store.list_instance_status(namespace).await
    }

    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        self.store.record_instance_status(record).await
    }

    async fn remove_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        self.store.remove_instance_status(instance_id).await
    }

    async fn sync_status(&self) -> Result<SyncStatus> {
        self.store.sync_status().await
    }
}

impl<S> SyncProbe for CorrosionBackend<S>
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    async fn sync_status(&self) -> Result<SyncStatus> {
        self.store.sync_status().await
    }
}

#[async_trait]
impl<S> StoreRuntimeControl for CorrosionBackend<S>
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    async fn start(&self) -> Result<()> {
        self.service.start().await
    }

    async fn stop(&self) -> Result<()> {
        self.service.stop().await
    }

    async fn wipe_data(&self) -> Result<()> {
        self.service.wipe_data().await
    }

    async fn healthy(&self) -> bool {
        self.service.healthy().await
    }
}

struct HostCorrosion {
    binary: PathBuf,
    config_path: PathBuf,
    data_dir: PathBuf,
    log_path: PathBuf,
    child: Mutex<Option<Child>>,
}

impl HostCorrosion {
    fn new(
        binary: impl Into<PathBuf>,
        config_path: impl Into<PathBuf>,
        data_dir: impl Into<PathBuf>,
    ) -> Self {
        let config_path = config_path.into();
        let log_path = default_log_path(&config_path);
        Self {
            binary: binary.into(),
            config_path,
            data_dir: data_dir.into(),
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
impl StoreRuntimeControl for HostCorrosion {
    async fn start(&self) -> Result<()> {
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
                    })?;
                let stdout_log = log_file.try_clone().map_err(|error| {
                    ployz_types::Error::operation(
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
            ployz_types::Error::operation(
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
        })?;
        let status = child.wait().await.map_err(|error| {
            ployz_types::Error::operation(
                "corrosion stop",
                format!("failed to wait pid {pid:?}: {error}"),
            )
        })?;

        info!(?pid, %status, "corrosion stopped (killed)");
        guard.take();
        Ok(())
    }

    async fn wipe_data(&self) -> Result<()> {
        match tokio::fs::remove_dir_all(&self.data_dir).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(ployz_types::Error::operation(
                "corrosion wipe",
                format!("remove data dir {}: {error}", self.data_dir.display()),
            )),
        }
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
    data_volume: String,
}

struct DockerCorrosionBuilder {
    container_name: String,
    image: String,
    cmd: Option<Vec<String>>,
    env: Vec<String>,
    volumes: Vec<String>,
    network_mode: Option<String>,
    data_volume: String,
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
            data_volume: self.data_volume,
        })
    }
}

impl DockerCorrosion {
    fn builder(container_name: &str, image: &str, data_volume: &str) -> DockerCorrosionBuilder {
        DockerCorrosionBuilder {
            container_name: container_name.to_string(),
            image: image.to_string(),
            cmd: None,
            env: Vec::new(),
            volumes: Vec::new(),
            network_mode: None,
            data_volume: data_volume.to_string(),
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
            env,
            labels: build_system_labels(&key, None),
            binds: self.volumes.clone(),
            network_mode: self.network_mode.clone(),
            ..Default::default()
        }
    }
}

#[async_trait]
impl StoreRuntimeControl for DockerCorrosion {
    async fn start(&self) -> Result<()> {
        let result = self.engine.ensure(&self.to_runtime_spec()).await?;

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

    async fn stop(&self) -> Result<()> {
        self.engine
            .remove(&self.container_name, STOP_GRACE_PERIOD)
            .await
    }

    async fn wipe_data(&self) -> Result<()> {
        self.engine.remove_volume(&self.data_volume).await
    }

    async fn healthy(&self) -> bool {
        match self.engine.inspect(&self.container_name).await {
            Ok(Some(observed)) => observed.running,
            Ok(None) => false,
            Err(_) => false,
        }
    }
}

fn corrosion_data_volume_name(network_id: &str) -> String {
    let mut suffix = String::with_capacity(network_id.len());
    for character in network_id.chars() {
        let valid = character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-');
        if valid {
            suffix.push(character);
        } else {
            suffix.push('-');
        }
    }

    let mut name = String::from("ployz-corrosion-data-");
    if suffix.is_empty() {
        name.push_str("mesh");
        return name;
    }

    let starts_with_alnum = suffix
        .chars()
        .next()
        .is_some_and(|character| character.is_ascii_alphanumeric());
    if starts_with_alnum {
        name.push_str(&suffix);
    } else {
        name.push('n');
        name.push_str(&suffix);
    }

    name
}

#[cfg(test)]
mod tests {
    use super::corrosion_data_volume_name;

    #[test]
    fn corrosion_data_volume_is_namespaced_per_network() {
        let alpha = corrosion_data_volume_name("alpha");
        let beta = corrosion_data_volume_name("beta");

        assert_ne!(alpha, beta);
        assert_eq!(alpha, "ployz-corrosion-data-alpha");
        assert_eq!(beta, "ployz-corrosion-data-beta");
    }

    #[test]
    fn corrosion_data_volume_sanitizes_invalid_network_ids() {
        assert_eq!(
            corrosion_data_volume_name("/mesh@a"),
            "ployz-corrosion-data-n-mesh-a"
        );
        assert_eq!(corrosion_data_volume_name(""), "ployz-corrosion-data-mesh");
    }

    #[test]
    fn corrosion_data_volume_mount_and_wipe_target_same_volume() {
        let volume = corrosion_data_volume_name("alpha");
        let mount = format!("{volume}:/data");

        assert_eq!(volume, "ployz-corrosion-data-alpha");
        assert_eq!(mount, "ployz-corrosion-data-alpha:/data");
    }
}
