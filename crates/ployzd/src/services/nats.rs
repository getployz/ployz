use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use ployz_nats::NatsScope;
use ployz_nats::NatsStore;
use ployz_nats::config::{self, CLIENT_PORT, PeerRoute, ServerConfig};
use ployz_runtime_backends::runtime::labels::build_system_labels;
use ployz_runtime_backends::runtime::{
    ContainerEngine, EnsureAction, PullPolicy, RuntimeContainerSpec,
};
use ployz_store_api::{
    AcmeChallengeSubscription, CertificateStore, CertificateSubscription, DeployCommit,
    DeployStore, ImageAvailabilityStore, InstanceStatusStore, InviteStore, MachineMembershipStore,
    MachineSubscription, PeerRttObservation, PeerRttStore, RoutingEventSubscription,
    RoutingStateStore, StoreDriver, StoreRuntimeControl, SyncProbe, SyncStatus,
};
use ployz_types::Result;
use ployz_types::error::Error;
use ployz_types::model::{
    AcmeAccountRecord, AcmeChallengeReadinessRecord, AcmeChallengeRecord, BranchEnvironmentFailure,
    BranchEnvironmentRecord, CertificateRecord, DeployId, DeployPhaseId, DeployPhaseRecord,
    DeployRecord, ImageAvailabilityRecord, ImageDigest, InstanceId, InstanceStatusRecord,
    InviteRecord, MachineId, MachineMembership, OverlayIp, PreparedDeployRecord, RoutingState,
    ServiceBranchLineageRecord, ServiceReleaseRecord, ServiceRevisionRecord, StorageParticipation,
    StorageReplicaPolicy, VolumeBranchLineageRecord, VolumeMovementRecord, VolumeRecord,
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
    storage_participation: &StorageParticipation,
    storage_replicas: StorageReplicaPolicy,
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
        storage_participation,
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

    let client_url = local_client_url();
    Ok(nats_driver(
        Arc::new(service),
        client_url,
        NatsScope::local_for_storage_participation(storage_participation),
        storage_replicas,
    ))
}

pub fn nats_host(
    overlay_ip: OverlayIp,
    network_dir: &Path,
    bootstrap: &[String],
    network_id: &str,
    storage_participation: &StorageParticipation,
    storage_replicas: StorageReplicaPolicy,
) -> std::result::Result<StoreDriver, String> {
    let paths = config::Paths::new(network_dir);
    write_node_config(
        &paths,
        &paths,
        overlay_ip,
        bootstrap,
        network_id,
        storage_participation,
    )
    .map_err(|error| format!("write nats config: {error}"))?;
    let service = HostNats::new(
        which_nats_server()?,
        paths.config.clone(),
        paths.data.clone(),
    );

    let client_url = overlay_client_url(overlay_ip);
    Ok(nats_driver(
        Arc::new(service),
        client_url,
        NatsScope::local_for_storage_participation(storage_participation),
        storage_replicas,
    ))
}

fn nats_driver<S>(
    service: Arc<S>,
    client_url: String,
    scope: NatsScope,
    storage_replicas: StorageReplicaPolicy,
) -> StoreDriver
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    let backend = Arc::new(NatsRuntime {
        service,
        client_url,
        scope,
        storage_replicas,
        store: Mutex::new(None),
    });
    StoreDriver::new(
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend.clone(),
        backend,
    )
}

fn write_node_config(
    host_paths: &config::Paths,
    runtime_paths: &config::Paths,
    overlay_ip: OverlayIp,
    bootstrap: &[String],
    network_id: &str,
    storage_participation: &StorageParticipation,
) -> std::io::Result<()> {
    let storage_peers: Vec<_> = bootstrap
        .iter()
        .filter_map(|peer| parse_peer_route(peer))
        .filter(|peer| peer.overlay_ip != overlay_ip.0)
        .collect();
    let server_config = ServerConfig {
        server_name: format!(
            "ployz-{}-{}",
            network_id,
            overlay_ip.0.to_string().replace(':', "-")
        ),
        cluster_name: format!("ployz-{network_id}"),
        storage_authority: storage_participation.is_authority(),
        authority_domain: NatsScope::local_for_storage_participation(storage_participation)
            .authority_domain(),
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

pub(crate) fn overlay_client_url(overlay_ip: OverlayIp) -> String {
    format!("nats://[{}]:{}", overlay_ip.0, CLIENT_PORT)
}

pub(crate) fn local_client_url() -> String {
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
    scope: NatsScope,
    storage_replicas: StorageReplicaPolicy,
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
            match tokio::time::timeout(
                CONNECT_TIMEOUT,
                NatsStore::connect_with_scope(&self.client_url, self.scope.clone()),
            )
            .await
            {
                Ok(Ok(store)) => {
                    let store = store.with_asset_policy(self.storage_replicas);
                    match store.start().await {
                        Ok(()) => {
                            *self.store.lock().await = Some(Arc::new(store));
                            return Ok(());
                        }
                        Err(error) => {
                            last_error = Some(error);
                            tokio::time::sleep(CONNECT_RETRY_DELAY).await;
                        }
                    }
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
impl<S> MachineMembershipStore for NatsRuntime<S>
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    async fn init(&self) -> Result<()> {
        MachineMembershipStore::init(self.store().await?.as_ref()).await
    }

    async fn list_machines(&self) -> Result<Vec<MachineMembership>> {
        MachineMembershipStore::list_machines(self.store().await?.as_ref()).await
    }

    async fn upsert_self_machine(&self, record: &MachineMembership) -> Result<()> {
        MachineMembershipStore::upsert_self_machine(self.store().await?.as_ref(), record).await
    }

    async fn delete_machine(&self, id: &MachineId) -> Result<()> {
        MachineMembershipStore::delete_machine(self.store().await?.as_ref(), id).await
    }

    async fn subscribe_machines(&self) -> Result<MachineSubscription> {
        MachineMembershipStore::subscribe_machines(self.store().await?.as_ref()).await
    }
}

#[async_trait]
impl<S> InviteStore for NatsRuntime<S>
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    async fn create_invite(&self, invite: &InviteRecord) -> Result<()> {
        InviteStore::create_invite(self.store().await?.as_ref(), invite).await
    }

    async fn get_invite(&self, invite_id: &str) -> Result<Option<InviteRecord>> {
        InviteStore::get_invite(self.store().await?.as_ref(), invite_id).await
    }

    async fn list_invites(&self) -> Result<Vec<InviteRecord>> {
        InviteStore::list_invites(self.store().await?.as_ref()).await
    }

    async fn redeem_invite(
        &self,
        invite_id: &str,
        machine_id: &MachineId,
        now_unix_secs: u64,
    ) -> Result<InviteRecord> {
        InviteStore::redeem_invite(
            self.store().await?.as_ref(),
            invite_id,
            machine_id,
            now_unix_secs,
        )
        .await
    }

    async fn revoke_invite(&self, invite_id: &str, now_unix_secs: u64) -> Result<InviteRecord> {
        InviteStore::revoke_invite(self.store().await?.as_ref(), invite_id, now_unix_secs).await
    }
}

#[async_trait]
impl<S> RoutingStateStore for NatsRuntime<S>
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    async fn load_routing_state(&self) -> Result<RoutingState> {
        RoutingStateStore::load_routing_state(self.store().await?.as_ref()).await
    }

    async fn subscribe_routing_events(&self) -> Result<RoutingEventSubscription> {
        RoutingStateStore::subscribe_routing_events(self.store().await?.as_ref()).await
    }
}

#[async_trait]
impl<S> ImageAvailabilityStore for NatsRuntime<S>
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    async fn upsert_image_availability(&self, record: &ImageAvailabilityRecord) -> Result<()> {
        ImageAvailabilityStore::upsert_image_availability(self.store().await?.as_ref(), record)
            .await
    }

    async fn get_image_availability(
        &self,
        machine_id: &MachineId,
        digest: &ImageDigest,
    ) -> Result<Option<ImageAvailabilityRecord>> {
        ImageAvailabilityStore::get_image_availability(
            self.store().await?.as_ref(),
            machine_id,
            digest,
        )
        .await
    }

    async fn list_image_availability(&self) -> Result<Vec<ImageAvailabilityRecord>> {
        ImageAvailabilityStore::list_image_availability(self.store().await?.as_ref()).await
    }
}

#[async_trait]
impl<S> DeployStore for NatsRuntime<S>
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    async fn list_deploy_revisions(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceRevisionRecord>> {
        DeployStore::list_deploy_revisions(self.store().await?.as_ref(), namespace).await
    }

    async fn list_deploy_releases(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceReleaseRecord>> {
        DeployStore::list_deploy_releases(self.store().await?.as_ref(), namespace).await
    }

    async fn list_volumes(&self, namespace: &Namespace) -> Result<Vec<VolumeRecord>> {
        DeployStore::list_volumes(self.store().await?.as_ref(), namespace).await
    }

    async fn list_service_branch_lineage(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<ServiceBranchLineageRecord>> {
        DeployStore::list_service_branch_lineage(self.store().await?.as_ref(), namespace).await
    }

    async fn list_volume_movements(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<VolumeMovementRecord>> {
        DeployStore::list_volume_movements(self.store().await?.as_ref(), namespace).await
    }

    async fn list_volume_branches(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<VolumeBranchLineageRecord>> {
        DeployStore::list_volume_branches(self.store().await?.as_ref(), namespace).await
    }

    async fn get_volume(
        &self,
        namespace: &Namespace,
        volume_name: &str,
    ) -> Result<Option<VolumeRecord>> {
        DeployStore::get_volume(self.store().await?.as_ref(), namespace, volume_name).await
    }

    async fn commit_deploy(&self, command: &DeployCommit) -> Result<()> {
        DeployStore::commit_deploy(self.store().await?.as_ref(), command).await
    }

    async fn get_deploy_commit(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
    ) -> Result<Option<DeployCommit>> {
        DeployStore::get_deploy_commit(self.store().await?.as_ref(), namespace, deploy_id).await
    }

    async fn write_deploy_status(&self, deploy: &DeployRecord) -> Result<()> {
        DeployStore::write_deploy_status(self.store().await?.as_ref(), deploy).await
    }

    async fn get_deploy(&self, deploy_id: &DeployId) -> Result<Option<DeployRecord>> {
        DeployStore::get_deploy(self.store().await?.as_ref(), deploy_id).await
    }

    async fn write_prepared_deploy(&self, prepared: &PreparedDeployRecord) -> Result<()> {
        DeployStore::write_prepared_deploy(self.store().await?.as_ref(), prepared).await
    }

    async fn get_prepared_deploy(
        &self,
        prepared_deploy_id: &DeployId,
    ) -> Result<Option<PreparedDeployRecord>> {
        DeployStore::get_prepared_deploy(self.store().await?.as_ref(), prepared_deploy_id).await
    }

    async fn mark_prepared_deploy_applied(
        &self,
        prepared_deploy_id: &DeployId,
        updated_at: u64,
    ) -> Result<PreparedDeployRecord> {
        DeployStore::mark_prepared_deploy_applied(
            self.store().await?.as_ref(),
            prepared_deploy_id,
            updated_at,
        )
        .await
    }

    async fn expire_prepared_deploy(
        &self,
        prepared_deploy_id: &DeployId,
        updated_at: u64,
    ) -> Result<PreparedDeployRecord> {
        DeployStore::expire_prepared_deploy(
            self.store().await?.as_ref(),
            prepared_deploy_id,
            updated_at,
        )
        .await
    }

    async fn supersede_prepared_deploy(
        &self,
        prepared_deploy_id: &DeployId,
        updated_at: u64,
    ) -> Result<PreparedDeployRecord> {
        DeployStore::supersede_prepared_deploy(
            self.store().await?.as_ref(),
            prepared_deploy_id,
            updated_at,
        )
        .await
    }

    async fn upsert_branch_environment(&self, record: &BranchEnvironmentRecord) -> Result<()> {
        DeployStore::upsert_branch_environment(self.store().await?.as_ref(), record).await
    }

    async fn get_branch_environment(
        &self,
        target_namespace: &Namespace,
    ) -> Result<Option<BranchEnvironmentRecord>> {
        DeployStore::get_branch_environment(self.store().await?.as_ref(), target_namespace).await
    }

    async fn list_branch_environments(&self) -> Result<Vec<BranchEnvironmentRecord>> {
        DeployStore::list_branch_environments(self.store().await?.as_ref()).await
    }

    async fn mark_branch_environment_applying(
        &self,
        target_namespace: &Namespace,
        prepared_deploy_id: &DeployId,
        updated_at: u64,
    ) -> Result<BranchEnvironmentRecord> {
        DeployStore::mark_branch_environment_applying(
            self.store().await?.as_ref(),
            target_namespace,
            prepared_deploy_id,
            updated_at,
        )
        .await
    }

    async fn mark_branch_environment_active(
        &self,
        target_namespace: &Namespace,
        prepared_deploy_id: &DeployId,
        applied_deploy_id: &DeployId,
        updated_at: u64,
    ) -> Result<BranchEnvironmentRecord> {
        DeployStore::mark_branch_environment_active(
            self.store().await?.as_ref(),
            target_namespace,
            prepared_deploy_id,
            applied_deploy_id,
            updated_at,
        )
        .await
    }

    async fn mark_branch_environment_failed(
        &self,
        target_namespace: &Namespace,
        prepared_deploy_id: &DeployId,
        failure: &BranchEnvironmentFailure,
        updated_at: u64,
    ) -> Result<BranchEnvironmentRecord> {
        DeployStore::mark_branch_environment_failed(
            self.store().await?.as_ref(),
            target_namespace,
            prepared_deploy_id,
            failure,
            updated_at,
        )
        .await
    }

    async fn upsert_deploy_phase(&self, phase: &DeployPhaseRecord) -> Result<()> {
        DeployStore::upsert_deploy_phase(self.store().await?.as_ref(), phase).await
    }

    async fn get_deploy_phase(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
        phase_id: &DeployPhaseId,
    ) -> Result<Option<DeployPhaseRecord>> {
        DeployStore::get_deploy_phase(self.store().await?.as_ref(), namespace, deploy_id, phase_id)
            .await
    }

    async fn list_deploy_phases(
        &self,
        namespace: &Namespace,
        deploy_id: &DeployId,
    ) -> Result<Vec<DeployPhaseRecord>> {
        DeployStore::list_deploy_phases(self.store().await?.as_ref(), namespace, deploy_id).await
    }
}

#[async_trait]
impl<S> InstanceStatusStore for NatsRuntime<S>
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    async fn list_instance_status(
        &self,
        namespace: &Namespace,
    ) -> Result<Vec<InstanceStatusRecord>> {
        InstanceStatusStore::list_instance_status(self.store().await?.as_ref(), namespace).await
    }

    async fn record_instance_status(&self, record: &InstanceStatusRecord) -> Result<()> {
        InstanceStatusStore::record_instance_status(self.store().await?.as_ref(), record).await
    }

    async fn remove_instance_status(&self, instance_id: &InstanceId) -> Result<()> {
        InstanceStatusStore::remove_instance_status(self.store().await?.as_ref(), instance_id).await
    }
}

#[async_trait]
impl<S> CertificateStore for NatsRuntime<S>
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    async fn get_acme_account(&self, issuer_url: &str) -> Result<Option<AcmeAccountRecord>> {
        CertificateStore::get_acme_account(self.store().await?.as_ref(), issuer_url).await
    }

    async fn upsert_acme_account(&self, record: &AcmeAccountRecord) -> Result<()> {
        CertificateStore::upsert_acme_account(self.store().await?.as_ref(), record).await
    }

    async fn list_certificates(&self) -> Result<Vec<CertificateRecord>> {
        CertificateStore::list_certificates(self.store().await?.as_ref()).await
    }

    async fn get_certificate(&self, hostname: &str) -> Result<Option<CertificateRecord>> {
        CertificateStore::get_certificate(self.store().await?.as_ref(), hostname).await
    }

    async fn upsert_certificate(&self, record: &CertificateRecord) -> Result<()> {
        CertificateStore::upsert_certificate(self.store().await?.as_ref(), record).await
    }

    async fn list_acme_challenges(&self) -> Result<Vec<AcmeChallengeRecord>> {
        CertificateStore::list_acme_challenges(self.store().await?.as_ref()).await
    }

    async fn upsert_acme_challenge(&self, record: &AcmeChallengeRecord) -> Result<()> {
        CertificateStore::upsert_acme_challenge(self.store().await?.as_ref(), record).await
    }

    async fn delete_acme_challenge(&self, hostname: &str, token: &str) -> Result<()> {
        CertificateStore::delete_acme_challenge(self.store().await?.as_ref(), hostname, token).await
    }

    async fn subscribe_certificates(&self) -> Result<CertificateSubscription> {
        CertificateStore::subscribe_certificates(self.store().await?.as_ref()).await
    }

    async fn subscribe_acme_challenges(&self) -> Result<AcmeChallengeSubscription> {
        CertificateStore::subscribe_acme_challenges(self.store().await?.as_ref()).await
    }

    async fn upsert_acme_challenge_readiness(
        &self,
        record: &AcmeChallengeReadinessRecord,
    ) -> Result<()> {
        CertificateStore::upsert_acme_challenge_readiness(self.store().await?.as_ref(), record)
            .await
    }

    async fn list_acme_challenge_readiness(
        &self,
        hostname: &str,
        token: &str,
    ) -> Result<Vec<AcmeChallengeReadinessRecord>> {
        CertificateStore::list_acme_challenge_readiness(
            self.store().await?.as_ref(),
            hostname,
            token,
        )
        .await
    }
}

#[async_trait]
impl<S> SyncProbe for NatsRuntime<S>
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    async fn sync_status(&self) -> Result<SyncStatus> {
        SyncProbe::sync_status(self.store().await?.as_ref()).await
    }
}

#[async_trait]
impl<S> PeerRttStore for NatsRuntime<S>
where
    S: StoreRuntimeControl + Send + Sync + 'static,
{
    async fn peer_rtt_observations(&self) -> Result<Vec<PeerRttObservation>> {
        PeerRttStore::peer_rtt_observations(self.store().await?.as_ref()).await
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
            pull_policy: PullPolicy::IfNotPresent,
            cmd: self.cmd.clone(),
            labels: build_system_labels(&key, None, Some(env!("CARGO_PKG_VERSION"))),
            binds: self.volumes.clone(),
            network_mode: self.network_mode.clone(),
            ..RuntimeContainerSpec::new(key, self.container_name.clone(), self.image.clone())
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
            Ok(Some(observed)) => {
                observed.running == ployz_runtime_backends::runtime::Observation::Observed(true)
            }
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
    use ployz_types::model::{AuthorityId, StorageParticipation};

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
            &StorageParticipation::default_authority(),
        )
        .expect("config should write");

        let rendered = std::fs::read_to_string(&host_paths.config).expect("config should read");
        std::fs::remove_dir_all(&root).ok();
        assert!(!rendered.contains("cluster {"));
        assert!(rendered.contains("jetstream {"));
    }

    #[test]
    fn remote_bootstrap_address_configures_storage_node() {
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
            &StorageParticipation::Authority {
                authority_id: AuthorityId::new("auth-sin"),
            },
        )
        .expect("config should write");

        let rendered = std::fs::read_to_string(&host_paths.config).expect("config should read");
        std::fs::remove_dir_all(&root).ok();
        assert!(rendered.contains("jetstream {"));
        assert!(rendered.contains("domain: dom-auth-sin"));
        assert!(rendered.contains("cluster {"));
        assert!(rendered.contains("nats://[fd00::10]:6222"));
    }

    #[test]
    fn nats_data_volume_is_namespaced_per_network() {
        assert_eq!(nats_data_volume_name("alpha"), "ployz-nats-data-alpha");
        assert_eq!(nats_data_volume_name("/mesh@a"), "ployz-nats-data-n-mesh-a");
        assert_eq!(nats_data_volume_name(""), "ployz-nats-data-mesh");
    }
}
