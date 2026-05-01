use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ployz_nats::NatsStore;
use ployz_nats::config::{self, CLIENT_PORT, PeerRoute, ServerConfig};
use ployz_runtime_backends::runtime::labels::build_system_labels;
use ployz_runtime_backends::runtime::{
    ContainerEngine, EnsureAction, PullPolicy, RuntimeContainerSpec,
};
use ployz_store_api::{
    AcmeChallengeSubscription, CertificateSubscription, DeployCommit, DeployRecordUpdate,
    DeployRevisionUpsert, DeploySnapshot, MachineSubscription, PeerMembershipObservation,
    PeerRttObservation, RoutingSubscription, StoreBackend, StoreDriver, StoreRuntimeControl,
    SyncStatus,
};
use ployz_types::Result;
use ployz_types::error::Error;
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeRecord, CertificateRecord, DeployId, DeployRecord, InstanceId,
    InstanceStatusRecord, InviteRecord, MachineId, MachineMembership, MachineRole, OverlayIp,
    RoutingState, ServiceReleaseRecord, VolumeRecord,
};
use ployz_types::spec::Namespace;
use tokio::io::{AsyncBufReadExt, AsyncRead, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::Mutex;
use tracing::{info, warn};

const STOP_GRACE_PERIOD: Duration = Duration::from_secs(10);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(2);
const CONNECT_RETRY_DELAY: Duration = Duration::from_millis(250);
const CONNECT_ATTEMPTS: usize = 40;

pub async fn nats_docker(
    overlay_ip: OverlayIp,
    network_dir: &Path,
    bootstrap: &[String],
    network_id: &str,
    allow_disconnected_bootstrap: bool,
    image: &str,
) -> std::result::Result<StoreDriver, String> {
    let paths = config::Paths::new(network_dir);
    let container_paths = config::Paths {
        root: PathBuf::from("/data"),
        config: PathBuf::from("/etc/nats/nats.conf"),
        data: PathBuf::from("/data/jetstream"),
    };
    write_node_config(
        &paths,
        &container_paths,
        overlay_ip,
        bootstrap,
        network_id,
        allow_disconnected_bootstrap,
    )
    .map_err(|error| format!("write nats config: {error}"))?;

    let config_host = paths.config.to_string_lossy().into_owned();
    let data_volume = nats_data_volume_name(network_id);
    let service = DockerNats::new("ployz-nats", image, &data_volume)
        .cmd(vec!["-c".into(), "/etc/nats/nats.conf".into()])
        .volume(&format!("{config_host}:/etc/nats/nats.conf:ro"))
        .volume(&format!("{data_volume}:/data"))
        .network_mode("container:ployz-networking")
        .build()
        .await
        .map_err(|error| format!("docker service: {error}"))?;

    let client_url = if allow_disconnected_bootstrap {
        local_client_url()
    } else {
        remote_storage_client_url(overlay_ip, bootstrap).unwrap_or_else(local_client_url)
    };
    Ok(nats_driver(Arc::new(service), client_url))
}

pub fn nats_host(
    overlay_ip: OverlayIp,
    network_dir: &Path,
    bootstrap: &[String],
    network_id: &str,
    allow_disconnected_bootstrap: bool,
) -> std::result::Result<StoreDriver, String> {
    let paths = config::Paths::new(network_dir);
    write_node_config(
        &paths,
        &paths,
        overlay_ip,
        bootstrap,
        network_id,
        allow_disconnected_bootstrap,
    )
    .map_err(|error| format!("write nats config: {error}"))?;
    let service = HostNats::new(
        which_nats_server()?,
        paths.config.clone(),
        paths.data.clone(),
    );

    let client_url = if allow_disconnected_bootstrap {
        overlay_client_url(overlay_ip)
    } else {
        remote_storage_client_url(overlay_ip, bootstrap)
            .unwrap_or_else(|| overlay_client_url(overlay_ip))
    };
    Ok(nats_driver(Arc::new(service), client_url))
}

fn nats_driver<S>(service: Arc<S>, client_url: String) -> StoreDriver
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    let backend = Arc::new(NatsRuntime {
        service,
        client_url,
        store: Mutex::new(None),
    });
    StoreDriver::from_backend(
        Arc::clone(&backend) as Arc<dyn StoreBackend>,
        backend as Arc<dyn StoreRuntimeControl>,
    )
}

fn write_node_config(
    host_paths: &config::Paths,
    runtime_paths: &config::Paths,
    overlay_ip: OverlayIp,
    bootstrap: &[String],
    network_id: &str,
    allow_disconnected_bootstrap: bool,
) -> std::io::Result<()> {
    let mut storage_peers: Vec<_> = bootstrap
        .iter()
        .filter_map(|peer| parse_peer_route(peer))
        .filter(|peer| peer.overlay_ip != overlay_ip.0)
        .collect();
    if allow_disconnected_bootstrap {
        storage_peers.clear();
    }
    let role = if storage_peers.is_empty() {
        MachineRole::StorageCandidate
    } else {
        MachineRole::Leaf
    };
    let server_config = ServerConfig {
        server_name: format!("ployz-{network_id}"),
        cluster_name: format!("ployz-{network_id}"),
        role,
        overlay_ip: overlay_ip.0,
        storage_peers,
        data_dir: runtime_paths.data.clone(),
    };
    config::write_config(host_paths, &server_config)
}

fn parse_peer_route(raw: &str) -> Option<PeerRoute> {
    if let Ok(SocketAddr::V6(addr)) = raw.parse::<SocketAddr>() {
        return Some(PeerRoute {
            overlay_ip: *addr.ip(),
        });
    }

    let trimmed = raw.strip_prefix('[')?.split_once(']')?.0;
    let overlay_ip = trimmed.parse::<Ipv6Addr>().ok()?;
    Some(PeerRoute { overlay_ip })
}

fn overlay_client_url(overlay_ip: OverlayIp) -> String {
    format!("nats://[{}]:{}", overlay_ip.0, CLIENT_PORT)
}

fn remote_storage_client_url(overlay_ip: OverlayIp, bootstrap: &[String]) -> Option<String> {
    bootstrap
        .iter()
        .filter_map(|peer| parse_peer_route(peer))
        .find(|peer| peer.overlay_ip != overlay_ip.0)
        .map(|peer| format!("nats://[{}]:{}", peer.overlay_ip, CLIENT_PORT))
}

fn local_client_url() -> String {
    format!("nats://{}:{}", Ipv4Addr::LOCALHOST, CLIENT_PORT)
}

fn which_nats_server() -> std::result::Result<PathBuf, String> {
    let candidates = ["/usr/local/bin/nats-server", "/usr/bin/nats-server"];
    for path in candidates {
        let candidate = PathBuf::from(path);
        if candidate.exists() {
            return Ok(candidate);
        }
    }
    Err(String::from(
        "nats-server binary not found (expected at /usr/local/bin/nats-server)",
    ))
}

struct NatsRuntime<S> {
    service: Arc<S>,
    client_url: String,
    store: Mutex<Option<Arc<NatsStore>>>,
}

impl<S> NatsRuntime<S> {
    async fn store(&self) -> Result<Arc<NatsStore>> {
        let guard = self.store.lock().await;
        let Some(store) = guard.as_ref() else {
            return Err(Error::operation(
                "nats store",
                "store accessed before NATS runtime started",
            ));
        };
        Ok(Arc::clone(store))
    }
}

#[async_trait]
impl<S> StoreRuntimeControl for NatsRuntime<S>
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    async fn start(&self) -> Result<()> {
        self.service.start().await?;

        let mut last_error = None;
        for attempt in 1..=CONNECT_ATTEMPTS {
            info!(
                attempt,
                max_attempts = CONNECT_ATTEMPTS,
                url = %self.client_url,
                "connecting to nats store"
            );
            match tokio::time::timeout(CONNECT_TIMEOUT, NatsStore::connect(&self.client_url)).await
            {
                Ok(Ok(store)) => {
                    store.start().await?;
                    *self.store.lock().await = Some(Arc::new(store));
                    return Ok(());
                }
                Ok(Err(error)) => {
                    last_error = Some(error);
                    tokio::time::sleep(CONNECT_RETRY_DELAY).await;
                }
                Err(_) => {
                    last_error = Some(Error::operation(
                        "nats_connect",
                        format!("timed out connecting to {}", self.client_url),
                    ));
                    tokio::time::sleep(CONNECT_RETRY_DELAY).await;
                }
            }
        }

        let message = last_error
            .map(|error| error.to_string())
            .unwrap_or_else(|| String::from("connection attempts exhausted"));
        Err(Error::operation("nats start", message))
    }

    async fn stop(&self) -> Result<()> {
        *self.store.lock().await = None;
        self.service.stop().await
    }

    async fn wipe_data(&self) -> Result<()> {
        *self.store.lock().await = None;
        self.service.wipe_data().await
    }

    async fn healthy(&self) -> bool {
        if !self.service.healthy().await {
            return false;
        }
        let guard = self.store.lock().await;
        match guard.as_ref() {
            Some(store) => store.healthy().await,
            None => false,
        }
    }
}

#[async_trait]
impl<S> StoreBackend for NatsRuntime<S>
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    async fn init(&self) -> Result<()> {
        self.store().await?.init().await
    }

    async fn list_machines(&self) -> Result<Vec<MachineMembership>> {
        self.store().await?.list_machines().await
    }

    async fn upsert_self_machine(&self, record: &MachineMembership) -> Result<()> {
        self.store().await?.upsert_self_machine(record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        self.store().await?.delete_machine(id).await
    }

    async fn subscribe_machines(&self) -> Result<MachineSubscription> {
        self.store().await?.subscribe_machines().await
    }

    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        self.store().await?.create_invite(invite).await
    }

    async fn get_invite(&self, invite_id: &str) -> Result<Option<InviteRecord>> {
        self.store().await?.get_invite(invite_id).await
    }

    async fn list_invites(&self) -> Result<Vec<InviteRecord>> {
        self.store().await?.list_invites().await
    }

    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> Result<InviteRecord> {
        self.store()
            .await?
            .redeem_invite(invite_id, machine_id, now_unix_secs)
            .await
    }

    async fn revoke_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<InviteRecord> {
        self.store()
            .await?
            .revoke_invite(invite_id, now_unix_secs)
            .await
    }

    async fn load_routing_state(&self) -> Result<RoutingState> {
        self.store().await?.load_routing_state().await
    }

    async fn subscribe_routing_events(&self) -> Result<RoutingSubscription> {
        self.store().await?.subscribe_routing_events().await
    }

    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        self.store().await?.list_deploy_releases(namespace).await
    }

    async fn load_deploy_snapshot(&self, namespace: &Namespace) -> Result<DeploySnapshot> {
        self.store().await?.load_deploy_snapshot(namespace).await
    }

    async fn list_volumes(&self, namespace: &Namespace) -> Result<Vec<VolumeRecord>> {
        self.store().await?.list_volumes(namespace).await
    }

    async fn get_volume(
        &self,
        namespace: &Namespace,
        volume_name: &str,
    ) -> Result<Option<VolumeRecord>> {
        self.store().await?.get_volume(namespace, volume_name).await
    }

    async fn record_service_revision(&self, command: &DeployRevisionUpsert) -> Result<()> {
        self.store().await?.record_service_revision(command).await
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> Result<()> {
        self.store().await?.commit_deploy(command).await
    }

    async fn update_deploy_record(&self, command: &DeployRecordUpdate) -> Result<()> {
        self.store().await?.update_deploy_record(command).await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        self.store().await?.get_deploy(deploy_id).await
    }

    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        self.store().await?.list_instance_status(namespace).await
    }

    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        self.store().await?.record_instance_status(record).await
    }

    async fn remove_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        self.store()
            .await?
            .remove_instance_status(instance_id)
            .await
    }

    async fn get_acme_account(&self, issuer_url: &str) -> Result<Option<AcmeAccountRecord>> {
        self.store().await?.get_acme_account(issuer_url).await
    }

    async fn upsert_acme_account(&self, record: &AcmeAccountRecord) -> Result<()> {
        self.store().await?.upsert_acme_account(record).await
    }

    async fn list_certificates(&self) -> Result<Vec<CertificateRecord>> {
        self.store().await?.list_certificates().await
    }

    async fn get_certificate(&self, hostname: &str) -> Result<Option<CertificateRecord>> {
        self.store().await?.get_certificate(hostname).await
    }

    async fn upsert_certificate(&self, record: &CertificateRecord) -> Result<()> {
        self.store().await?.upsert_certificate(record).await
    }

    async fn list_acme_challenges(&self) -> Result<Vec<AcmeChallengeRecord>> {
        self.store().await?.list_acme_challenges().await
    }

    async fn upsert_acme_challenge(&self, record: &AcmeChallengeRecord) -> Result<()> {
        self.store().await?.upsert_acme_challenge(record).await
    }

    async fn delete_acme_challenge(&self, hostname: &str, token: &str) -> Result<()> {
        self.store()
            .await?
            .delete_acme_challenge(hostname, token)
            .await
    }

    async fn subscribe_certificates(&self) -> Result<CertificateSubscription> {
        self.store().await?.subscribe_certificates().await
    }

    async fn subscribe_acme_challenges(&self) -> Result<AcmeChallengeSubscription> {
        self.store().await?.subscribe_acme_challenges().await
    }

    async fn sync_status(&self) -> Result<SyncStatus> {
        self.store().await?.sync_status().await
    }

    async fn peer_rtt_observations(&self) -> Result<Vec<PeerRttObservation>> {
        self.store().await?.peer_rtt_observations().await
    }

    async fn peer_membership_observations(&self) -> Result<Vec<PeerMembershipObservation>> {
        self.store().await?.peer_membership_observations().await
    }
}

struct HostNats {
    binary: PathBuf,
    config_path: PathBuf,
    data_dir: PathBuf,
    child: Mutex<Option<Child>>,
}

impl HostNats {
    fn new(binary: PathBuf, config_path: PathBuf, data_dir: PathBuf) -> Self {
        Self {
            binary,
            config_path,
            data_dir,
            child: Mutex::new(None),
        }
    }
}

fn log_nats_output<R>(stream: R, stream_name: &'static str)
where
    R: AsyncRead + Unpin + Send + 'static,
{
    tokio::spawn(async move {
        let mut lines = BufReader::new(stream).lines();
        loop {
            match lines.next_line().await {
                Ok(Some(line)) => info!(stream = stream_name, line = %line, "nats-server output"),
                Ok(None) => break,
                Err(error) => {
                    warn!(
                        stream = stream_name,
                        ?error,
                        "failed to read nats-server output"
                    );
                    break;
                }
            }
        }
    });
}

#[async_trait]
impl StoreRuntimeControl for HostNats {
    async fn start(&self) -> Result<()> {
        let mut guard = self.child.lock().await;
        if let Some(child) = &mut *guard {
            match child.try_wait() {
                Ok(None) => {
                    info!(binary = %self.binary.display(), "nats-server already running");
                    return Ok(());
                }
                Ok(Some(status)) => warn!(%status, "nats-server exited, restarting"),
                Err(error) => warn!(?error, "failed to check nats-server status, restarting"),
            }
        }

        let child = Command::new(&self.binary)
            .arg("-c")
            .arg(&self.config_path)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .kill_on_drop(true)
            .spawn()
            .map_err(|error| {
                Error::operation(
                    "nats start",
                    format!("failed to spawn {}: {error}", self.binary.display()),
                )
            })?;
        let mut child = child;
        if let Some(stdout) = child.stdout.take() {
            log_nats_output(stdout, "stdout");
        }
        if let Some(stderr) = child.stderr.take() {
            log_nats_output(stderr, "stderr");
        }
        info!(
            pid = child.id(),
            binary = %self.binary.display(),
            config = %self.config_path.display(),
            "nats-server started"
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
                    info!(pid = raw_pid, %status, "nats-server stopped gracefully");
                    guard.take();
                    return Ok(());
                }
                Ok(Err(error)) => warn!(pid = raw_pid, ?error, "wait after SIGINT failed"),
                Err(_) => warn!(pid = raw_pid, "nats-server did not stop after SIGINT"),
            }
        }

        child.kill().await.map_err(|error| {
            Error::operation("nats stop", format!("failed to kill pid {pid:?}: {error}"))
        })?;
        child.wait().await.map_err(|error| {
            Error::operation("nats stop", format!("failed to wait pid {pid:?}: {error}"))
        })?;
        guard.take();
        Ok(())
    }

    async fn wipe_data(&self) -> Result<()> {
        match tokio::fs::remove_dir_all(&self.data_dir).await {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(Error::operation(
                "nats wipe",
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

struct DockerNats {
    engine: ContainerEngine,
    container_name: String,
    image: String,
    cmd: Option<Vec<String>>,
    volumes: Vec<String>,
    network_mode: Option<String>,
    data_volume: String,
}

struct DockerNatsBuilder {
    container_name: String,
    image: String,
    cmd: Option<Vec<String>>,
    volumes: Vec<String>,
    network_mode: Option<String>,
    data_volume: String,
}

impl DockerNats {
    fn new(container_name: &str, image: &str, data_volume: &str) -> DockerNatsBuilder {
        DockerNatsBuilder {
            container_name: container_name.to_string(),
            image: image.to_string(),
            cmd: None,
            volumes: Vec::new(),
            network_mode: None,
            data_volume: data_volume.to_string(),
        }
    }

    fn to_runtime_spec(&self) -> RuntimeContainerSpec {
        let key = "system/nats".to_string();
        RuntimeContainerSpec {
            key: key.clone(),
            container_name: self.container_name.clone(),
            image: self.image.clone(),
            pull_policy: PullPolicy::IfNotPresent,
            cmd: self.cmd.clone(),
            labels: build_system_labels(&key, None, Some(env!("CARGO_PKG_VERSION"))),
            binds: self.volumes.clone(),
            network_mode: self.network_mode.clone(),
            ..Default::default()
        }
    }
}

impl DockerNatsBuilder {
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

    async fn build(self) -> Result<DockerNats> {
        Ok(DockerNats {
            engine: ContainerEngine::connect().await?,
            container_name: self.container_name,
            image: self.image,
            cmd: self.cmd,
            volumes: self.volumes,
            network_mode: self.network_mode,
            data_volume: self.data_volume,
        })
    }
}

#[async_trait]
impl StoreRuntimeControl for DockerNats {
    async fn start(&self) -> Result<()> {
        let result = self.engine.ensure(&self.to_runtime_spec()).await?;
        match &result.action {
            EnsureAction::Adopted => {
                info!(name = %self.container_name, "adopted existing nats container")
            }
            EnsureAction::Created => {
                info!(name = %self.container_name, image = %self.image, "nats container started")
            }
            EnsureAction::Recreated { changed } => info!(
                name = %self.container_name,
                image = %self.image,
                changed = ?changed,
                "nats container recreated"
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

fn nats_data_volume_name(network_id: &str) -> String {
    let mut suffix = String::with_capacity(network_id.len());
    for character in network_id.chars() {
        let valid = character.is_ascii_alphanumeric() || matches!(character, '_' | '.' | '-');
        if valid {
            suffix.push(character);
        } else {
            suffix.push('-');
        }
    }

    let mut name = String::from("ployz-nats-data-");
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
    use super::{OverlayIp, config, nats_data_volume_name, parse_peer_route, write_node_config};

    #[test]
    fn parses_bootstrap_peer_route_from_socket() {
        let route = parse_peer_route("[fd00::10]:6222").expect("route should parse");
        assert_eq!(route.overlay_ip.to_string(), "fd00::10");
    }

    #[test]
    fn local_bootstrap_address_does_not_enable_cluster_mode() {
        let root = std::env::temp_dir().join(format!(
            "ployz-nats-config-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let host_paths = config::Paths::new(&root);
        let runtime_paths = config::Paths::new(&root);
        let overlay_ip = OverlayIp("fd00::10".parse().expect("valid overlay"));

        write_node_config(
            &host_paths,
            &runtime_paths,
            overlay_ip,
            &["[fd00::10]:6222".into()],
            "alpha",
            false,
        )
        .expect("config should write");

        let rendered = std::fs::read_to_string(&host_paths.config).expect("config should read");
        std::fs::remove_dir_all(&root).ok();
        assert!(!rendered.contains("cluster {"));
        assert!(rendered.contains("jetstream {"));
    }

    #[test]
    fn remote_bootstrap_address_configures_leaf_node() {
        let root = std::env::temp_dir().join(format!(
            "ployz-nats-config-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let host_paths = config::Paths::new(&root);
        let runtime_paths = config::Paths::new(&root);
        let overlay_ip = OverlayIp("fd00::11".parse().expect("valid overlay"));

        write_node_config(
            &host_paths,
            &runtime_paths,
            overlay_ip,
            &["[fd00::10]:6222".into()],
            "alpha",
            false,
        )
        .expect("config should write");

        let rendered = std::fs::read_to_string(&host_paths.config).expect("config should read");
        std::fs::remove_dir_all(&root).ok();
        assert!(!rendered.contains("jetstream {"));
        assert!(rendered.contains("url: \"nats://[fd00::10]:7422\""));
    }

    #[test]
    fn disconnected_bootstrap_uses_local_standalone_storage() {
        let root = std::env::temp_dir().join(format!(
            "ployz-nats-config-test-{}-{}",
            std::process::id(),
            rand::random::<u64>()
        ));
        let host_paths = config::Paths::new(&root);
        let runtime_paths = config::Paths::new(&root);
        let overlay_ip = OverlayIp("fd00::11".parse().expect("valid overlay"));

        write_node_config(
            &host_paths,
            &runtime_paths,
            overlay_ip,
            &["[fd00::10]:6222".into()],
            "alpha",
            true,
        )
        .expect("config should write");

        let rendered = std::fs::read_to_string(&host_paths.config).expect("config should read");
        std::fs::remove_dir_all(&root).ok();
        assert!(rendered.contains("jetstream {"));
        assert!(!rendered.contains("cluster {"));
        assert!(!rendered.contains("remotes: ["));
    }

    #[test]
    fn nats_data_volume_is_namespaced_per_network() {
        assert_eq!(nats_data_volume_name("alpha"), "ployz-nats-data-alpha");
        assert_eq!(nats_data_volume_name("/mesh@a"), "ployz-nats-data-n-mesh-a");
        assert_eq!(nats_data_volume_name(""), "ployz-nats-data-mesh");
    }
}
