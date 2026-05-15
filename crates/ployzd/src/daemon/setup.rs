use std::net::SocketAddr;
use std::path::PathBuf;

use thiserror::Error;
use tracing::warn;

use crate::mesh_state::bootstrap::{
    BootstrapPeerRecord, BootstrapPeerSeedTask, build_seed_records, load_bootstrap_peer_records,
    resolve_bootstrap_addrs,
};
use crate::mesh_state::network::NetworkConfig;
use ployz_cert_acme::InstantAcmeIssuerFactory;
use ployz_cert_acme_api::{
    AcmeAccountCoordinator, AcmeIssuerFactory, CertificateManagerConfig, Http01ChallengeReadiness,
    IssuanceCoordinator,
};
use ployz_config::RuntimeTarget;
use ployz_dns::DnsConfig;
use ployz_error::Error as PloyzError;
use ployz_gateway::GatewayConfig;
use ployz_model::CertificateState;
use ployz_nats::NatsLocks;
use ployz_nats::NatsStore;
use ployz_nats::config as nats_config;
use ployz_nats::{CertRenewalConsumerPolicy, NatsCertRenewalJobConsumer};
use ployz_node_runtime::RuntimeComponents;
use ployz_orchestrator::Mesh;
use ployz_orchestrator::certificates::{
    CertificateRenewalTask, LocalHttp01ChallengeReadiness, finalize_due_certificates,
    process_renewal_job,
};
use ployz_orchestrator::coordination::SubnetReservationCoordinator;
use ployz_orchestrator::mesh::wireguard::DEFAULT_LISTEN_PORT;
use ployz_runtime_api::{NoopRuntimeHandle, RuntimeHandle};
use ployz_store_api::{CertificateStore, StoreDriver, StoreRuntimeControl};
use ployz_supervision::FileHealthRecorder;
use std::sync::Arc;
use std::time::Duration;

use crate::daemon::subnet_coordination::NatsSubnetCoordinator;

use super::{ActiveMesh, DaemonState, RuntimeRestartMode};
use crate::daemon::handlers::volume::transfer_listener;
use crate::features::image::registry;
use crate::ipc::nats_listener;
use crate::runtime_profile::MeshBuildRequest;

const CERT_RENEWAL_FETCH_ERROR_BACKOFF_MIN: Duration = Duration::from_secs(1);
const CERT_RENEWAL_FETCH_ERROR_BACKOFF_MAX: Duration = Duration::from_secs(30);
const CERT_RENEWAL_JOB_RETRY_DELAY: Duration = Duration::from_secs(60);

/// Connect to the network's NATS broker and build a JetStream-KV-backed subnet
/// coordinator. Memory-runtime tests skip this and keep the in-memory
/// coordinator wired at construction.
async fn build_nats_subnet_coordinator(
    state: &DaemonState,
    config: &NetworkConfig,
) -> Result<std::sync::Arc<dyn SubnetReservationCoordinator>, StartMeshError> {
    let client_url = if state.runtime_target == RuntimeTarget::Docker {
        crate::services::nats::local_client_url()
    } else {
        crate::services::nats::overlay_client_url(config.overlay_ip)
    };
    let scope =
        ployz_nats::NatsScope::local_for_storage_participation(&config.storage_participation);
    let nats_store = NatsStore::connect_with_scope(&client_url, scope)
        .await
        .map_err(|error| StartMeshError::MeshUp(format!("nats connect for subnet coord: {error}")))?
        .with_asset_policy(config.storage_replicas);
    nats_store
        .start()
        .await
        .map_err(|error| StartMeshError::MeshUp(format!("nats start for subnet coord: {error}")))?;
    let locks = NatsLocks::new(&nats_store)
        .await
        .map_err(|error| StartMeshError::MeshUp(format!("nats locks bucket: {error}")))?;
    Ok(std::sync::Arc::new(NatsSubnetCoordinator::new(locks)))
}

async fn prepare_image_receiver_listener_for_runtime_restart(
    current_bind_addr: Option<SocketAddr>,
    new_bind_addr: Option<SocketAddr>,
    image_registry: registry::ImageRegistry,
) -> Result<Option<Box<dyn RuntimeHandle>>, String> {
    if current_bind_addr == new_bind_addr {
        return Ok(None);
    }

    let new_receiver: Box<dyn RuntimeHandle> = if let Some(bind_addr) = new_bind_addr {
        match registry::serve(bind_addr, image_registry).await {
            Ok(handle) => Box::new(handle),
            Err(error) => return Err(format!("image receiver start failed: {error}")),
        }
    } else {
        Box::new(NoopRuntimeHandle)
    };

    Ok(Some(new_receiver))
}

async fn commit_prepared_image_receiver_listener(
    runtime: &mut RuntimeComponents,
    current_bind_addr: &mut Option<SocketAddr>,
    new_bind_addr: Option<SocketAddr>,
    prepared_receiver: Option<Box<dyn RuntimeHandle>>,
) {
    let Some(prepared_receiver) = prepared_receiver else {
        return;
    };

    let previous = runtime.replace_image_receiver(prepared_receiver);
    if let Err(error) = previous.shutdown().await {
        tracing::warn!(?error, "runtime restart: image receiver stop failed");
    }
    *current_bind_addr = new_bind_addr;
}

async fn rollback_prepared_image_receiver_listener(
    prepared_receiver: Option<Box<dyn RuntimeHandle>>,
) {
    let Some(prepared_receiver) = prepared_receiver else {
        return;
    };
    if let Err(error) = prepared_receiver.shutdown().await {
        tracing::warn!(
            ?error,
            "runtime restart: prepared image receiver rollback failed"
        );
    }
}

async fn start_nats_certificate_renewal_worker(
    store: StoreDriver,
    nats_store: NatsStore,
    issuer_factory: Arc<dyn AcmeIssuerFactory>,
    coordinator: Arc<dyn IssuanceCoordinator>,
    readiness: Arc<dyn Http01ChallengeReadiness>,
    account_coordinator: Arc<dyn AcmeAccountCoordinator>,
    health_path: PathBuf,
) -> Result<CertificateRenewalTask, StartMeshError> {
    let consumer =
        NatsCertRenewalJobConsumer::connect(&nats_store, CertRenewalConsumerPolicy::default())
            .await
            .map_err(|error| {
                StartMeshError::MeshUp(format!("nats cert renewal consumer: {error}"))
            })?;
    Ok(CertificateRenewalTask::spawn(
        "nats certificate renewal worker",
        |cancel| async move {
            let issuer = issuer_factory.create(
                Arc::new(LocalHttp01ChallengeReadiness),
                account_coordinator.clone(),
            );
            let finalization_issuer =
                issuer_factory.create(readiness.clone(), account_coordinator.clone());
            let mut fetch_error_backoff = CERT_RENEWAL_FETCH_ERROR_BACKOFF_MIN;
            let health = FileHealthRecorder::new(health_path);
            let mut last_failure_kind = None;
            record_cert_renewal_healthy(&health).await;
            loop {
                tokio::select! {
                    () = cancel.cancelled() => break,
                    next = consumer.next() => {
                        match next {
                            Ok(Some(job)) => {
                                fetch_error_backoff = CERT_RENEWAL_FETCH_ERROR_BACKOFF_MIN;
                                let hostname = job.hostname.clone();
                                let job_result = async {
                                    process_renewal_job(
                                        &store,
                                        issuer.as_ref(),
                                        coordinator.as_ref(),
                                        &hostname,
                                    ).await?;
                                    finalize_due_certificates(
                                        &store,
                                        finalization_issuer.as_ref(),
                                        coordinator.as_ref(),
                                    ).await?;
                                    renewal_job_is_complete(&store, &hostname).await
                                }.await;
                                match job_result {
                                    Ok(()) => {
                                        if let Err(error) = job.ack().await {
                                            last_failure_kind = Some(CertRenewalFailureKind::Job);
                                            record_cert_renewal_unhealthy(
                                                &health,
                                                format!("certificate renewal job ack failed for {hostname}: {error}"),
                                            ).await;
                                            tracing::warn!(?error, hostname, "certificate renewal job ack failed");
                                        } else {
                                            last_failure_kind = None;
                                            record_cert_renewal_healthy(&health).await;
                                        }
                                    }
                                    Err(error) => {
                                        last_failure_kind = Some(CertRenewalFailureKind::Job);
                                        record_cert_renewal_unhealthy(
                                            &health,
                                            format!("certificate renewal job failed for {hostname}: {error}"),
                                        ).await;
                                        tracing::warn!(?error, hostname, "certificate renewal job failed");
                                        if let Err(nak_error) = job.nak_after(Some(CERT_RENEWAL_JOB_RETRY_DELAY)).await {
                                            record_cert_renewal_unhealthy(
                                                &health,
                                                format!("certificate renewal job nak failed for {hostname}: {nak_error}"),
                                            ).await;
                                            tracing::warn!(
                                                ?nak_error,
                                                hostname,
                                                "certificate renewal job nak failed"
                                            );
                                        }
                                    }
                                }
                            }
                            Ok(None) => {
                                fetch_error_backoff = CERT_RENEWAL_FETCH_ERROR_BACKOFF_MIN;
                                if last_failure_kind != Some(CertRenewalFailureKind::Job) {
                                    last_failure_kind = None;
                                    record_cert_renewal_healthy(&health).await;
                                }
                            }
                            Err(error) => {
                                last_failure_kind = Some(CertRenewalFailureKind::Fetch);
                                record_cert_renewal_unhealthy(
                                    &health,
                                    format!("certificate renewal worker fetch failed: {error}"),
                                ).await;
                                let delay = fetch_error_backoff;
                                fetch_error_backoff = fetch_error_backoff
                                    .saturating_mul(2)
                                    .min(CERT_RENEWAL_FETCH_ERROR_BACKOFF_MAX);
                                tracing::warn!(?error, ?delay, "certificate renewal worker fetch failed");
                                tokio::select! {
                                    () = cancel.cancelled() => break,
                                    () = tokio::time::sleep(delay) => {}
                                }
                            }
                        }
                    }
                }
            }
        },
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum CertRenewalFailureKind {
    Fetch,
    Job,
}

async fn record_cert_renewal_healthy(health: &FileHealthRecorder) {
    if let Err(error) = health.force_healthy_forgetting_stale().await {
        tracing::warn!(
            path = %health.path().display(),
            %error,
            "failed to write cert renewal healthy state"
        );
    } else {
        tracing::debug!(
            path = %health.path().display(),
            "wrote cert renewal healthy state"
        );
    }
}

async fn record_cert_renewal_unhealthy(health: &FileHealthRecorder, error: impl Into<String>) {
    if let Err(error) = health
        .record_unhealthy_remembering_write_failure(error)
        .await
    {
        tracing::warn!(
            path = %health.path().display(),
            %error,
            "failed to write cert renewal stale state"
        );
    } else {
        tracing::debug!(
            path = %health.path().display(),
            "wrote cert renewal stale state"
        );
    }
}

async fn renewal_job_is_complete(store: &StoreDriver, hostname: &str) -> ployz_error::Result<()> {
    let Some(record) = store.get_certificate(hostname).await? else {
        return Ok(());
    };
    if record.state() == CertificateState::Active {
        return Ok(());
    }
    Err(PloyzError::operation(
        "certificate_renewal_incomplete",
        format!(
            "certificate {hostname} remains {} after renewal job",
            record.state()
        ),
    ))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MeshStartSummary {
    pub network_name: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum StartMeshError {
    #[error("bootstrap resolve failed: {0}")]
    BootstrapResolve(String),
    #[error("invalid gateway listen addr '{0}'")]
    GatewayListenAddr(String),
    #[error("network driver failed: {0}")]
    NetworkDriver(String),
    #[error("mesh up failed: {0}")]
    MeshUp(String),
    #[error("control plane listener start failed on {bind}: {error}")]
    ControlPlaneListener { bind: SocketAddr, error: String },
    #[error("image receiver port would overflow zfs transfer port {zfs_transfer_port}")]
    ImageReceiverPort { zfs_transfer_port: u16 },
    #[error("gateway start failed: {0}")]
    Gateway(String),
    #[error("dns start failed: {0}")]
    Dns(String),
}

struct StartPlan {
    network_dir: PathBuf,
    bootstrap_peer_records: Vec<BootstrapPeerRecord>,
    bootstrap_addrs: Vec<String>,
    gateway_ports: Vec<u16>,
    zfs_transfer_bind_addr: SocketAddr,
    image_receiver_bind_addr: SocketAddr,
    gateway_config: Option<GatewayConfig>,
    dns_config: Option<DnsConfig>,
}

struct MeshStartAttempt {
    config: NetworkConfig,
    mesh: Option<Mesh>,
    runtime: RuntimeComponents,
}

impl MeshStartAttempt {
    fn new(config: NetworkConfig) -> Self {
        Self {
            config,
            mesh: None,
            runtime: RuntimeComponents::noop(),
        }
    }

    /// Fatal: build mesh drivers and call `Mesh::up()`, relying on `Mesh::up()` to self-teardown on failure.
    async fn build_mesh(
        &mut self,
        state: &DaemonState,
        plan: &StartPlan,
    ) -> Result<(), StartMeshError> {
        let exposed_tcp_ports = match self.config.subnet {
            Some(_) => plan.gateway_ports.clone(),
            None => Vec::new(),
        };
        let components = state
            .build_runtime_mesh_components(MeshBuildRequest {
                identity: &state.identity,
                overlay_ip: self.config.overlay_ip,
                network_dir: &plan.network_dir,
                network_name: &self.config.name.0,
                subnet: self.config.subnet,
                exposed_tcp_ports: &exposed_tcp_ports,
                bootstrap: &plan.bootstrap_addrs,
                network_id: &self.config.id.as_str(),
                storage_participation: &self.config.storage_participation,
                storage_replicas: self.config.storage_replicas,
            })
            .await
            .map_err(StartMeshError::NetworkDriver)?;

        let listen_port = DEFAULT_LISTEN_PORT;
        let seed_records = build_seed_records(
            &state.identity,
            &self.config,
            listen_port,
            &plan.bootstrap_peer_records,
            state.configured_topology.as_ref(),
        )
        .await;

        let mut mesh = Mesh::new(
            components.network,
            components.store,
            components.container_network,
            state.identity.machine_id.clone(),
            listen_port,
        )
        .with_seed_records(seed_records);

        mesh.up()
            .await
            .map_err(|error| StartMeshError::MeshUp(error.to_string()))?;

        self.mesh = Some(mesh);
        Ok(())
    }

    /// Fatal: start edge runtimes or roll back control-plane listeners plus mesh.
    async fn start_edge_runtimes(
        &mut self,
        state: &DaemonState,
        plan: &StartPlan,
    ) -> Result<(), StartMeshError> {
        let gateway_config = plan.gateway_config.clone();
        let dns_config = plan.dns_config.clone();
        let gateway = async {
            let Some(config) = gateway_config else {
                return Ok(Box::new(NoopRuntimeHandle) as Box<dyn RuntimeHandle>);
            };
            state.start_runtime_gateway(config).await
        };
        let dns = async {
            let Some(config) = dns_config else {
                return Ok(Box::new(NoopRuntimeHandle) as Box<dyn RuntimeHandle>);
            };
            state.start_runtime_dns(config).await
        };

        let (gateway_result, dns_result) = tokio::join!(gateway, dns);
        match (gateway_result, dns_result) {
            (Ok(gateway), Ok(dns)) => {
                self.runtime.set_gateway(gateway);
                self.runtime.set_dns(dns);
                Ok(())
            }
            (Err(error), Ok(dns)) => {
                self.runtime.set_dns(dns);
                Err(StartMeshError::Gateway(error))
            }
            (Ok(gateway), Err(error)) => {
                self.runtime.set_gateway(gateway);
                Err(StartMeshError::Dns(error))
            }
            (Err(gateway_error), Err(dns_error)) => Err(StartMeshError::Gateway(format!(
                "{gateway_error}; dns start also failed: {dns_error}"
            ))),
        }
    }

    async fn start_nats_control(&mut self, state: &DaemonState) -> Result<(), StartMeshError> {
        if state.runtime_is_memory_test() {
            self.runtime
                .set_nats_control(Box::new(nats_listener::NatsListenerHandle::noop()));
            return Ok(());
        }
        let Some(mesh) = self.mesh.as_ref() else {
            return Err(StartMeshError::MeshUp(
                "startup attempt missing mesh before nats control start".into(),
            ));
        };
        let Some(command_tx) = state.command_tx.clone() else {
            return Err(StartMeshError::MeshUp(
                "daemon command channel unavailable".into(),
            ));
        };
        let client_url = if state.runtime_target == RuntimeTarget::Docker {
            crate::services::nats::local_client_url()
        } else {
            crate::services::nats::overlay_client_url(self.config.overlay_ip)
        };
        let scope = ployz_nats::NatsScope::local_for_storage_participation(
            &self.config.storage_participation,
        );
        let nats_store = NatsStore::connect_with_scope(&client_url, scope)
            .await
            .map_err(|error| StartMeshError::MeshUp(format!("nats connect for node rpc: {error}")))?
            .with_asset_policy(self.config.storage_replicas);
        nats_store
            .start()
            .await
            .map_err(|error| StartMeshError::MeshUp(format!("nats start for node rpc: {error}")))?;
        let (client, subject, queue_group) = nats_store
            .node_command_listener(&state.identity.machine_id)
            .into_parts();
        let handle = nats_listener::serve(
            client,
            subject,
            queue_group,
            command_tx,
            state
                .network_dir(&self.config.name.0)
                .join(nats_listener::NATS_NODE_RPC_HEALTH_FILE),
        )
        .await
        .map_err(StartMeshError::MeshUp)?;
        let _ = mesh;
        self.runtime.set_nats_control(Box::new(handle));
        Ok(())
    }

    async fn start_zfs_transfer_control(
        &mut self,
        state: &DaemonState,
        plan: &StartPlan,
    ) -> Result<(), StartMeshError> {
        if state.runtime_is_memory_test() {
            self.runtime.set_zfs_transfer(Box::new(
                transfer_listener::ZfsTransferListenerHandle::noop(),
            ));
            return Ok(());
        }
        if !state.storage.is_zfs_backend() {
            self.runtime.set_zfs_transfer(Box::new(
                transfer_listener::ZfsTransferListenerHandle::noop(),
            ));
            return Ok(());
        }
        let Some(zfs_root) = state.storage.zfs_root.clone() else {
            self.runtime.set_zfs_transfer(Box::new(
                transfer_listener::ZfsTransferListenerHandle::noop(),
            ));
            return Ok(());
        };
        let Some(mesh) = self.mesh.as_ref() else {
            self.runtime.set_zfs_transfer(Box::new(
                transfer_listener::ZfsTransferListenerHandle::noop(),
            ));
            return Ok(());
        };
        let handle = transfer_listener::serve(
            plan.zfs_transfer_bind_addr,
            zfs_root,
            state.storage.overcommit_ratio,
            mesh.store.clone(),
        )
        .await
        .map_err(|error| StartMeshError::ControlPlaneListener {
            bind: plan.zfs_transfer_bind_addr,
            error,
        })?;
        self.runtime.set_zfs_transfer(Box::new(handle));
        Ok(())
    }

    async fn start_image_receiver_control(
        &mut self,
        state: &DaemonState,
        plan: &StartPlan,
    ) -> Result<(), StartMeshError> {
        if state.runtime_is_memory_test() {
            self.runtime
                .set_image_receiver(Box::new(registry::ImageRegistryListenerHandle::noop()));
            return Ok(());
        }
        let Some(_mesh) = self.mesh.as_ref() else {
            self.runtime
                .set_image_receiver(Box::new(registry::ImageRegistryListenerHandle::noop()));
            return Ok(());
        };
        let handle = registry::serve(plan.image_receiver_bind_addr, state.image_registry.clone())
            .await
            .map_err(|error| StartMeshError::ControlPlaneListener {
                bind: plan.image_receiver_bind_addr,
                error,
            })?;
        self.runtime.set_image_receiver(Box::new(handle));
        Ok(())
    }

    /// Commit: publish the active mesh into daemon state.
    async fn publish_active(
        &mut self,
        state: &mut DaemonState,
        plan: &StartPlan,
    ) -> Result<(), StartMeshError> {
        let spawn_renewal_ticker = !state.runtime_is_memory_test();
        let Some(mesh_ref) = self.mesh.as_ref() else {
            return Err(StartMeshError::MeshUp(
                "startup attempt missing mesh at publish".into(),
            ));
        };

        let subnet_coord = if spawn_renewal_ticker {
            Some(build_nats_subnet_coordinator(state, &self.config).await?)
        } else {
            None
        };

        let certificate_renewal = if spawn_renewal_ticker {
            let store = mesh_ref.store.clone();
            let nats_client_url = if state.runtime_target == RuntimeTarget::Docker {
                crate::services::nats::local_client_url()
            } else {
                crate::services::nats::overlay_client_url(self.config.overlay_ip)
            };
            let scope = ployz_nats::NatsScope::local_for_storage_participation(
                &self.config.storage_participation,
            );
            let nats_store = NatsStore::connect_with_scope(&nats_client_url, scope)
                .await
                .map_err(|error| {
                    StartMeshError::MeshUp(format!("nats connect for cert coord: {error}"))
                })?
                .with_asset_policy(self.config.storage_replicas);
            nats_store.start().await.map_err(|error| {
                StartMeshError::MeshUp(format!("nats start for cert coord: {error}"))
            })?;
            let locks = NatsLocks::new(&nats_store)
                .await
                .map_err(|error| StartMeshError::MeshUp(format!("nats locks bucket: {error}")))?;
            let coordinator = std::sync::Arc::new(
                crate::daemon::cert_coordination::NatsIssuanceCoordinator::new(
                    locks,
                    state.identity.machine_id.clone(),
                ),
            );
            let account_coordinator = coordinator.clone();
            let readiness = std::sync::Arc::new(
                crate::daemon::cert_coordination::NatsChallengeReadiness::new(store.clone()),
            );
            let issuer_factory = std::sync::Arc::new(InstantAcmeIssuerFactory::new(
                CertificateManagerConfig::from_env(),
            ));
            Some(
                start_nats_certificate_renewal_worker(
                    store,
                    nats_store,
                    issuer_factory,
                    coordinator,
                    readiness,
                    account_coordinator,
                    state
                        .network_dir(&self.config.name.0)
                        .join(crate::daemon::cert_renewal_health::NATS_CERT_RENEWAL_HEALTH_FILE),
                )
                .await?,
            )
        } else {
            None
        };

        let bootstrap_peer_seed = Some(BootstrapPeerSeedTask::spawn(
            NetworkConfig::dir(&state.data_dir, &self.config.name.0),
            mesh_ref.store.clone(),
            state.identity.machine_id.clone(),
        ));

        let Some(mesh) = self.mesh.take() else {
            return Err(StartMeshError::MeshUp(
                "startup attempt missing mesh at publish".into(),
            ));
        };
        let runtime = std::mem::take(&mut self.runtime);
        if let Some(subnet_coord) = subnet_coord {
            state.subnet_coord = subnet_coord;
        }
        let image_receiver_bind_addr = if state.runtime_is_memory_test() {
            None
        } else {
            Some(plan.image_receiver_bind_addr)
        };
        state.active = Some(ActiveMesh {
            config: self.config.clone(),
            retained_subnet: crate::daemon::RetainedSubnet::from_running_config(self.config.subnet),
            mesh,
            runtime,
            image_receiver_bind_addr,
            certificate_renewal,
            bootstrap_peer_seed,
        });
        Ok(())
    }

    async fn rollback_startup(&mut self) {
        self.runtime.rollback_startup().await;

        if let Some(mut mesh) = self.mesh.take()
            && let Err(error) = mesh.detach().await
        {
            warn!(?error, "mesh rollback failed");
        }
    }

    fn finish(self) -> MeshStartSummary {
        MeshStartSummary {
            network_name: self.config.name.0,
        }
    }
}

impl DaemonState {
    pub async fn start_mesh_by_name(&mut self, network: &str) -> Result<MeshStartSummary, String> {
        let config_path = NetworkConfig::path(&self.data_dir, network);
        let net_config = NetworkConfig::load(&config_path)
            .map_err(|error| format!("load network config: {error}"))?;
        self.start_mesh(net_config)
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn start_mesh(
        &mut self,
        net_config: NetworkConfig,
    ) -> Result<MeshStartSummary, StartMeshError> {
        let plan = self.plan_mesh_start(&net_config)?;
        tracing::info!(
            ?self.runtime_target,
            ?self.service_mode,
            network = %net_config.name,
            "starting mesh"
        );

        let mut attempt = MeshStartAttempt::new(net_config);
        attempt.build_mesh(self, &plan).await?;

        if let Err(error) = attempt.start_nats_control(self).await {
            attempt.rollback_startup().await;
            return Err(error);
        }

        if let Err(error) = attempt.start_zfs_transfer_control(self, &plan).await {
            attempt.rollback_startup().await;
            return Err(error);
        }

        if let Err(error) = attempt.start_image_receiver_control(self, &plan).await {
            attempt.rollback_startup().await;
            return Err(error);
        }

        if let Err(error) = attempt.start_edge_runtimes(self, &plan).await {
            attempt.rollback_startup().await;
            return Err(error);
        }

        if let Err(error) = attempt.publish_active(self, &plan).await {
            attempt.rollback_startup().await;
            return Err(error);
        }

        Ok(attempt.finish())
    }

    pub async fn restart_active_runtime_from_config(
        &mut self,
        network: &str,
    ) -> Result<(), String> {
        self.restart_active_runtime_from_config_with_mode(network, RuntimeRestartMode::NetworkOnly)
            .await
    }

    pub async fn restart_active_runtime_from_config_with_mode(
        &mut self,
        network: &str,
        mode: RuntimeRestartMode,
    ) -> Result<(), String> {
        let config_path = NetworkConfig::path(&self.data_dir, network);
        let net_config = NetworkConfig::load(&config_path)
            .map_err(|error| format!("load network config: {error}"))?;
        let gateway_ports = self.gateway_ports().map_err(|error| error.to_string())?;
        let exposed_tcp_ports: Vec<u16> = match net_config.subnet {
            Some(_) => gateway_ports,
            None => Vec::new(),
        };
        let network_dir = self.network_dir(&net_config.name.0);
        let bootstrap_peer_records = load_bootstrap_peer_records(&network_dir)
            .map_err(|error| format!("load bootstrap peer records for runtime restart: {error}"))?;
        let bootstrap_addrs = resolve_bootstrap_addrs(
            &bootstrap_peer_records,
            &self.identity.machine_id,
            nats_config::ROUTE_PORT,
        );
        let dns_bridge_listen_addr = self.dns_bridge_listen_addr();
        let image_registry = self.image_registry.clone();
        let new_image_receiver_bind_addr = if self.runtime_is_memory_test() {
            None
        } else {
            Some(
                self.image_receiver_bind_addr(self.zfs_transfer_port, net_config.overlay_ip)
                    .ok_or_else(|| {
                        format!(
                            "image receiver port would overflow zfs transfer port {}",
                            self.zfs_transfer_port
                        )
                    })?,
            )
        };

        if self.active.is_none() {
            return Err("no running network".into());
        }

        let components = self
            .build_runtime_mesh_components(MeshBuildRequest {
                identity: &self.identity,
                overlay_ip: net_config.overlay_ip,
                network_dir: &network_dir,
                network_name: &net_config.name.0,
                subnet: net_config.subnet,
                exposed_tcp_ports: &exposed_tcp_ports,
                bootstrap: &bootstrap_addrs,
                network_id: &net_config.id.as_str(),
                storage_participation: &net_config.storage_participation,
                storage_replicas: net_config.storage_replicas,
            })
            .await
            .map_err(|error| format!("runtime components failed: {error}"))?;

        let new_gateway: Box<dyn RuntimeHandle> = if net_config.subnet.is_some() {
            let gateway_config = GatewayConfig::for_network(
                &self.data_dir,
                &net_config.name.0,
                self.identity.machine_id.as_str().to_string(),
                self.gateway_listen_addr.clone(),
                self.gateway_https_listen_addr.clone(),
                None,
                None,
                self.gateway_threads,
                self.gateway_metrics_listen_addr.clone(),
            );
            self.start_runtime_gateway(gateway_config)
                .await
                .map_err(|error| format!("gateway start failed: {error}"))?
        } else {
            Box::new(NoopRuntimeHandle)
        };
        let new_dns: Box<dyn RuntimeHandle> = if net_config.subnet.is_some() {
            let dns_config = DnsConfig::for_network(
                &self.data_dir,
                &net_config.name.0,
                self.identity.machine_id.as_str().to_string(),
                net_config.overlay_ip,
                dns_bridge_listen_addr,
                self.dns_metrics_listen_addr.clone(),
            );
            match self.start_runtime_dns(dns_config).await {
                Ok(handle) => handle,
                Err(error) => {
                    let gateway = new_gateway;
                    if let Err(shutdown_error) = gateway.shutdown().await {
                        tracing::warn!(
                            ?shutdown_error,
                            "runtime rollback failed after dns start error"
                        );
                    }
                    return Err(format!("dns start failed: {error}"));
                }
            }
        } else {
            Box::new(NoopRuntimeHandle)
        };

        let Some(active) = self.active.as_mut() else {
            return Err("no running network".into());
        };

        let prepared_image_receiver = match prepare_image_receiver_listener_for_runtime_restart(
            active.image_receiver_bind_addr,
            new_image_receiver_bind_addr,
            image_registry.clone(),
        )
        .await
        {
            Ok(prepared_image_receiver) => prepared_image_receiver,
            Err(error) => {
                if let Err(shutdown_error) = new_dns.shutdown().await {
                    tracing::warn!(
                        ?shutdown_error,
                        "runtime rollback failed after image receiver start error"
                    );
                }
                if let Err(shutdown_error) = new_gateway.shutdown().await {
                    tracing::warn!(
                        ?shutdown_error,
                        "runtime rollback failed after image receiver start error"
                    );
                }
                return Err(error);
            }
        };

        if let Err(error) = image_registry.revoke_all_sessions().await {
            tracing::warn!(%error, "runtime restart: image receive session cleanup failed");
        }

        let dns = active.runtime.replace_dns(Box::new(NoopRuntimeHandle));
        if let Err(error) = dns.shutdown().await {
            tracing::warn!(?error, "runtime restart: dns stop failed");
        }

        let gateway = active.runtime.replace_gateway(Box::new(NoopRuntimeHandle));
        if let Err(error) = gateway.shutdown().await {
            tracing::warn!(?error, "runtime restart: gateway stop failed");
        }

        let _ = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.overlay_ip = net_config.overlay_ip;
                record.subnet = net_config.subnet;
            })
            .await;

        let restart_result = match mode {
            RuntimeRestartMode::NetworkAndStore => active
                .mesh
                .restart_runtime_for_config_change(
                    components.network,
                    components.store,
                    components.container_network,
                )
                .await
                .map_err(|error| format!("mesh runtime restart failed: {error}")),
            RuntimeRestartMode::NetworkOnly => active
                .mesh
                .restart_runtime_for_subnet_change(components.network, components.container_network)
                .await
                .map_err(|error| format!("mesh runtime restart failed: {error}")),
        };
        if let Err(error) = restart_result {
            rollback_prepared_image_receiver_listener(prepared_image_receiver).await;
            return Err(error);
        }

        let _ = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.overlay_ip = net_config.overlay_ip;
                record.subnet = net_config.subnet;
            })
            .await;
        commit_prepared_image_receiver_listener(
            &mut active.runtime,
            &mut active.image_receiver_bind_addr,
            new_image_receiver_bind_addr,
            prepared_image_receiver,
        )
        .await;
        active.config = net_config;
        active.runtime.set_gateway(new_gateway);
        active.runtime.set_dns(new_dns);
        Ok(())
    }

    /// Fatal before startup: resolve every startup input and explicit policy value into a `StartPlan`.
    fn plan_mesh_start(&self, net_config: &NetworkConfig) -> Result<StartPlan, StartMeshError> {
        let network_dir = self.network_dir(&net_config.name.0);
        let bootstrap_peer_records =
            load_bootstrap_peer_records(&network_dir).map_err(StartMeshError::BootstrapResolve)?;
        let bootstrap_addrs = resolve_bootstrap_addrs(
            &bootstrap_peer_records,
            &self.identity.machine_id,
            nats_config::ROUTE_PORT,
        );
        let gateway_ports = self.gateway_ports()?;
        let zfs_transfer_bind_addr =
            self.zfs_transfer_bind_addr(self.zfs_transfer_port, net_config.overlay_ip);
        let Some(image_receiver_bind_addr) =
            self.image_receiver_bind_addr(self.zfs_transfer_port, net_config.overlay_ip)
        else {
            return Err(StartMeshError::ImageReceiverPort {
                zfs_transfer_port: self.zfs_transfer_port,
            });
        };
        let gateway_config = net_config.subnet.map(|_| {
            GatewayConfig::for_network(
                &self.data_dir,
                &net_config.name.0,
                self.identity.machine_id.as_str().to_string(),
                self.gateway_listen_addr.clone(),
                self.gateway_https_listen_addr.clone(),
                None,
                None,
                self.gateway_threads,
                self.gateway_metrics_listen_addr.clone(),
            )
        });
        let dns_config = net_config.subnet.map(|_| {
            DnsConfig::for_network(
                &self.data_dir,
                &net_config.name.0,
                self.identity.machine_id.as_str().to_string(),
                net_config.overlay_ip,
                self.dns_bridge_listen_addr(),
                self.dns_metrics_listen_addr.clone(),
            )
        });

        Ok(StartPlan {
            network_dir,
            bootstrap_peer_records,
            bootstrap_addrs,
            gateway_ports,
            zfs_transfer_bind_addr,
            image_receiver_bind_addr,
            gateway_config,
            dns_config,
        })
    }

    /// Returns the DNS bridge listen address for Docker runtime targets,
    /// or `None` for host-based runtimes.
    fn dns_bridge_listen_addr(&self) -> Option<String> {
        if self.runtime_target == RuntimeTarget::Docker {
            Some("0.0.0.0:53".into())
        } else {
            None
        }
    }

    fn gateway_ports(&self) -> Result<Vec<u16>, StartMeshError> {
        let mut ports = vec![Self::gateway_port(&self.gateway_listen_addr)?];
        if let Some(addr) = &self.gateway_https_listen_addr {
            ports.push(Self::gateway_port(addr)?);
        }
        ports.sort_unstable();
        ports.dedup();
        Ok(ports)
    }

    fn gateway_port(gateway_listen_addr: &str) -> Result<u16, StartMeshError> {
        let Some((_, port)) = gateway_listen_addr.rsplit_once(':') else {
            return Err(StartMeshError::GatewayListenAddr(
                gateway_listen_addr.to_string(),
            ));
        };
        port.parse::<u16>()
            .map_err(|_| StartMeshError::GatewayListenAddr(gateway_listen_addr.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::net::IpAddr;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    use super::*;
    use crate::mesh_state::bootstrap::write_bootstrap_peer_records;
    use crate::runtime_profile::RuntimeProfile;
    use ployz_config::{RuntimeTarget, ServiceMode};
    use ployz_model::{MachineId, NetworkName, OverlayIp, PublicKey};
    use ployz_runtime_api::Identity;
    use tokio::net::TcpListener;

    #[test]
    fn plan_mesh_start_uses_localhost_for_docker_zfs_transfer() {
        let state = make_state(RuntimeTarget::Docker, ServiceMode::User, "0.0.0.0:80");
        let config = make_network_config(&state, "alpha");

        let plan = state.plan_mesh_start(&config).expect("plan should succeed");

        assert_eq!(
            plan.zfs_transfer_bind_addr.ip(),
            SocketAddr::from(([127, 0, 0, 1], state.zfs_transfer_port)).ip()
        );
    }

    #[test]
    fn plan_mesh_start_uses_next_localhost_port_for_docker_image_receiver() {
        let state = make_state(RuntimeTarget::Docker, ServiceMode::User, "0.0.0.0:80");
        let config = make_network_config(&state, "alpha");

        let plan = state.plan_mesh_start(&config).expect("plan should succeed");

        assert_eq!(
            plan.image_receiver_bind_addr,
            SocketAddr::from(([127, 0, 0, 1], state.zfs_transfer_port + 1))
        );
    }

    #[test]
    fn plan_mesh_start_uses_overlay_ip_for_host_zfs_transfer() {
        let state = make_state(RuntimeTarget::Host, ServiceMode::User, "0.0.0.0:80");
        let config = make_network_config(&state, "alpha");

        let plan = state.plan_mesh_start(&config).expect("plan should succeed");

        assert_eq!(
            plan.zfs_transfer_bind_addr.ip(),
            SocketAddr::new(IpAddr::V6(config.overlay_ip.0), state.zfs_transfer_port).ip()
        );
    }

    #[test]
    fn plan_mesh_start_uses_next_overlay_port_for_host_image_receiver() {
        let state = make_state(RuntimeTarget::Host, ServiceMode::User, "0.0.0.0:80");
        let config = make_network_config(&state, "alpha");

        let plan = state.plan_mesh_start(&config).expect("plan should succeed");

        assert_eq!(
            plan.image_receiver_bind_addr,
            SocketAddr::new(IpAddr::V6(config.overlay_ip.0), state.zfs_transfer_port + 1)
        );
    }

    #[test]
    fn plan_mesh_start_rejects_image_receiver_port_overflow() {
        let mut state = make_state(RuntimeTarget::Host, ServiceMode::User, "0.0.0.0:80");
        state.zfs_transfer_port = u16::MAX;
        let config = make_network_config(&state, "alpha");

        let error = match state.plan_mesh_start(&config) {
            Ok(_) => panic!("plan should fail"),
            Err(error) => error,
        };

        assert!(matches!(error, StartMeshError::ImageReceiverPort { .. }));
    }

    #[test]
    fn plan_mesh_start_rejects_invalid_gateway_listen_addr() {
        let state = make_test_state("not-a-socket");
        let config = make_network_config(&state, "alpha");

        let error = match state.plan_mesh_start(&config) {
            Ok(_) => panic!("plan should fail"),
            Err(error) => error,
        };

        assert!(matches!(error, StartMeshError::GatewayListenAddr(_)));
    }

    #[test]
    fn plan_mesh_start_maps_corrupt_peer_seed_to_bootstrap_resolution_failure() {
        let state = make_test_state("0.0.0.0:80");
        let config = make_network_config(&state, "alpha");
        let network_dir = state.network_dir(&config.name.0);
        fs::create_dir_all(&network_dir).expect("create network dir");
        fs::write(
            crate::mesh_state::bootstrap::bootstrap_peers_path(&network_dir),
            "{not-json",
        )
        .expect("write corrupt peer seed");

        let error = match state.plan_mesh_start(&config) {
            Ok(_) => panic!("plan should fail"),
            Err(error) => error,
        };

        assert!(matches!(error, StartMeshError::BootstrapResolve(_)));
    }

    #[test]
    fn plan_mesh_start_uses_peer_seed_and_ignores_store_data_path() {
        let state = make_test_state("0.0.0.0:80");
        let config = make_network_config(&state, "alpha");
        let network_dir = state.network_dir(&config.name.0);
        let data_path = ployz_nats::config::Paths::new(&network_dir).data;
        fs::create_dir_all(&data_path).expect("create store data path");
        let peer = BootstrapPeerRecord {
            machine_id: MachineId::new("peer"),
            public_key: PublicKey([8; 32]),
            overlay_ip: OverlayIp("fd00::8".parse().expect("valid overlay")),
            subnet: None,
            bridge_ip: None,
            storage: true,
            storage_participation: ployz_model::StorageParticipation::default_authority(),
            region_role: ployz_model::RegionRole::HomeData,
            endpoints: vec!["peer:51820".into()],
        };
        write_bootstrap_peer_records(&network_dir, std::slice::from_ref(&peer))
            .expect("write peer seed");

        let plan = state.plan_mesh_start(&config).expect("plan should succeed");

        assert_eq!(plan.bootstrap_peer_records, vec![peer]);
        assert_eq!(plan.bootstrap_addrs, vec!["[fd00::8]:6222"]);
    }

    #[tokio::test]
    async fn start_mesh_returns_summary_and_publishes_active_mesh() {
        let mut state = make_test_state("127.0.0.1:8080");
        let config = make_network_config(&state, "alpha");

        let summary = state
            .start_mesh(config)
            .await
            .expect("mesh start should succeed");

        assert_eq!(summary.network_name, "alpha");
        let active = state.active.as_ref().expect("active mesh");
        assert_eq!(active.image_receiver_bind_addr, None);

        teardown_active_mesh(&mut state).await;
    }

    #[tokio::test]
    async fn publish_active_failure_keeps_startup_resources_for_rollback() {
        let mut state = make_test_state("127.0.0.1:8080");
        let mut config = make_network_config(&state, "alpha");
        config.overlay_ip = OverlayIp("::1".parse().expect("valid overlay"));
        let plan = state.plan_mesh_start(&config).expect("plan should succeed");
        let mut attempt = MeshStartAttempt::new(config);
        attempt
            .build_mesh(&state, &plan)
            .await
            .expect("memory mesh should start before commit");
        state.runtime_profile = RuntimeProfile::from_runtime(
            RuntimeTarget::Host,
            ServiceMode::User,
            crate::BuiltInImages::load(None)
                .expect("embedded built-in images manifest should parse"),
        );

        let error = match attempt.publish_active(&mut state, &plan).await {
            Ok(_) => panic!("publish_active should fail without local NATS"),
            Err(error) => error,
        };

        assert!(matches!(error, StartMeshError::MeshUp(_)));
        assert!(
            attempt.mesh.is_some(),
            "startup attempt must still own mesh after publish failure"
        );
        assert!(state.active.is_none());

        attempt.rollback_startup().await;
        assert!(attempt.mesh.is_none());
    }

    #[tokio::test]
    async fn image_receiver_prepare_failure_keeps_previous_listener() {
        let registry = registry::ImageRegistry::new(unique_temp_dir("ployz-image-registry"));
        let previous = registry::serve(SocketAddr::from(([127, 0, 0, 1], 0)), registry.clone())
            .await
            .expect("previous image receiver should bind");
        let previous_bind_addr = previous.bind_addr();
        let image_receiver: Box<dyn RuntimeHandle> = Box::new(previous);
        let occupied = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("occupy replacement receiver port");
        let occupied_addr = occupied.local_addr().expect("occupied local addr");

        let error = match prepare_image_receiver_listener_for_runtime_restart(
            Some(previous_bind_addr),
            Some(occupied_addr),
            registry,
        )
        .await
        {
            Ok(_) => panic!("occupied replacement port should fail"),
            Err(error) => error,
        };

        assert!(error.contains("image receiver start failed"));
        TcpListener::bind(previous_bind_addr)
            .await
            .expect_err("previous listener should remain active");

        image_receiver
            .shutdown()
            .await
            .expect("shutdown previous listener");
    }

    #[tokio::test]
    async fn prepared_image_receiver_is_rolled_back_without_swapping_active_listener() {
        let registry = registry::ImageRegistry::new(unique_temp_dir("ployz-image-registry"));
        let previous = registry::serve(SocketAddr::from(([127, 0, 0, 1], 0)), registry.clone())
            .await
            .expect("previous image receiver should bind");
        let previous_bind_addr = previous.bind_addr();
        let image_receiver: Box<dyn RuntimeHandle> = Box::new(previous);
        let replacement_addr = free_local_addr().await;

        let prepared = prepare_image_receiver_listener_for_runtime_restart(
            Some(previous_bind_addr),
            Some(replacement_addr),
            registry,
        )
        .await
        .expect("replacement image receiver should bind");

        TcpListener::bind(previous_bind_addr)
            .await
            .expect_err("previous listener should remain active before commit");
        TcpListener::bind(replacement_addr)
            .await
            .expect_err("prepared listener should be active before rollback");

        rollback_prepared_image_receiver_listener(prepared).await;

        TcpListener::bind(replacement_addr)
            .await
            .expect("prepared listener should be released by rollback");
        TcpListener::bind(previous_bind_addr)
            .await
            .expect_err("previous listener should remain active after rollback");

        image_receiver
            .shutdown()
            .await
            .expect("shutdown previous listener");
    }

    fn make_state(
        runtime_target: RuntimeTarget,
        service_mode: ServiceMode,
        gateway_listen_addr: &str,
    ) -> DaemonState {
        let data_dir = unique_temp_dir("ployz-start-mesh");
        let identity = Identity::generate(MachineId::new("founder"), [1; 32]);

        DaemonState::new(
            &data_dir,
            identity,
            runtime_target,
            service_mode,
            ployz_config::StorageConfig::default(),
            crate::BuiltInImages::load(None)
                .expect("embedded built-in images manifest should parse"),
            "10.210.0.0/16".into(),
            24,
            4319,
            gateway_listen_addr.into(),
            None,
            1,
            None,
            None,
            None,
        )
    }

    fn make_test_state(gateway_listen_addr: &str) -> DaemonState {
        let data_dir = unique_temp_dir("ployz-start-mesh");
        let identity = Identity::generate(MachineId::new("founder"), [1; 32]);

        DaemonState::new_for_tests(
            &data_dir,
            identity,
            "10.210.0.0/16".into(),
            24,
            4319,
            gateway_listen_addr.into(),
            None,
            1,
        )
    }

    fn make_network_config(state: &DaemonState, name: &str) -> NetworkConfig {
        NetworkConfig::new(
            NetworkName(name.into()),
            &state.identity.public_key,
            &state.cluster_cidr,
            "10.210.0.0/24".parse().expect("valid subnet"),
        )
    }

    async fn teardown_active_mesh(state: &mut DaemonState) {
        let Some(active) = state.active.as_mut() else {
            return;
        };

        active.stop_bootstrap_peer_seed().await;
        active.mesh.destroy().await.expect("destroy mesh");
    }

    async fn free_local_addr() -> SocketAddr {
        let listener = TcpListener::bind(SocketAddr::from(([127, 0, 0, 1], 0)))
            .await
            .expect("bind free local addr");
        listener.local_addr().expect("read local addr")
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }
}
