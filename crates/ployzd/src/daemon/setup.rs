use std::net::SocketAddr;
use std::path::PathBuf;

use thiserror::Error;
use tracing::warn;

use crate::mesh_state::bootstrap::{
    BootstrapPeerRecord, BootstrapSeedCacheTask, build_seed_records, load_bootstrap_peer_records,
    resolve_bootstrap_addrs,
};
use crate::mesh_state::network::NetworkConfig;
use ployz_cert_backends::InstantAcmeIssuerFactory;
use ployz_config::RuntimeTarget;
use ployz_dns_config::DnsConfig;
use ployz_gateway_config::GatewayConfig;
use ployz_nats::NatsStore;
use ployz_nats::config as nats_config;
use ployz_nats::coord::locks::NatsLocks;
use ployz_orchestrator::Mesh;
use ployz_orchestrator::certificates::{
    CertificateManagerConfig, RenewalConfig, spawn_certificate_renewal_ticker,
};
use ployz_orchestrator::coordination::SubnetReservationCoordinator;
use ployz_orchestrator::mesh::wireguard::DEFAULT_LISTEN_PORT;
use ployz_runtime_api::{NoopRuntimeHandle, RuntimeHandle};
use ployz_store_api::StoreRuntimeControl;

use crate::daemon::subnet_coordination::NatsSubnetCoordinator;

use super::{ActiveMesh, DaemonState};
use crate::daemon::handlers::volume::transfer_listener;
use crate::ipc::nats_listener;
use crate::runtime_profile::MeshBuildRequest;

/// Connect to the network's NATS broker and build a JetStream-KV-backed subnet
/// coordinator. Memory-runtime tests skip this and keep the in-memory
/// coordinator wired at construction.
async fn build_nats_subnet_coordinator(
    state: &DaemonState,
    overlay_ip: ployz_types::model::OverlayIp,
) -> Result<std::sync::Arc<dyn SubnetReservationCoordinator>, StartMeshError> {
    let client_url = if state.runtime_target == RuntimeTarget::Docker {
        crate::services::nats::local_client_url()
    } else {
        crate::services::nats::overlay_client_url(overlay_ip)
    };
    let nats_store = NatsStore::connect(&client_url).await.map_err(|error| {
        StartMeshError::MeshUp(format!("nats connect for subnet coord: {error}"))
    })?;
    nats_store
        .start()
        .await
        .map_err(|error| StartMeshError::MeshUp(format!("nats start for subnet coord: {error}")))?;
    let locks = NatsLocks::new(&nats_store)
        .await
        .map_err(|error| StartMeshError::MeshUp(format!("nats locks bucket: {error}")))?;
    Ok(std::sync::Arc::new(NatsSubnetCoordinator::new(locks)))
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
    gateway_config: Option<GatewayConfig>,
    dns_config: Option<DnsConfig>,
}

struct MeshStartTx {
    config: NetworkConfig,
    mesh: Option<Mesh>,
    nats_control: Box<dyn RuntimeHandle>,
    zfs_transfer: Box<dyn RuntimeHandle>,
    gateway: Box<dyn RuntimeHandle>,
    dns: Box<dyn RuntimeHandle>,
}

impl MeshStartTx {
    fn new(config: NetworkConfig) -> Self {
        Self {
            config,
            mesh: None,
            nats_control: Box::new(NoopRuntimeHandle),
            zfs_transfer: Box::new(NoopRuntimeHandle),
            gateway: Box::new(NoopRuntimeHandle),
            dns: Box::new(NoopRuntimeHandle),
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
                network_id: &self.config.id.0,
                machine_role: self.config.machine_role,
            })
            .await
            .map_err(StartMeshError::NetworkDriver)?;

        let listen_port = DEFAULT_LISTEN_PORT;
        let seed_records = build_seed_records(
            &state.identity,
            &self.config,
            state.control_target.clone(),
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

    /// Fatal: start gateway or roll back control-plane listeners plus mesh.
    async fn start_gateway(
        &mut self,
        state: &DaemonState,
        plan: &StartPlan,
    ) -> Result<(), StartMeshError> {
        let Some(config) = plan.gateway_config.clone() else {
            return Ok(());
        };
        let handle = state
            .start_runtime_gateway(config)
            .await
            .map_err(StartMeshError::Gateway)?;
        self.gateway = handle;
        Ok(())
    }

    /// Fatal: start DNS or roll back gateway, control-plane listeners, and mesh.
    async fn start_dns(
        &mut self,
        state: &DaemonState,
        plan: &StartPlan,
    ) -> Result<(), StartMeshError> {
        let Some(config) = plan.dns_config.clone() else {
            return Ok(());
        };
        let handle = state
            .start_runtime_dns(config)
            .await
            .map_err(StartMeshError::Dns)?;
        self.dns = handle;
        Ok(())
    }

    async fn start_nats_control(&mut self, state: &DaemonState) -> Result<(), StartMeshError> {
        if state.runtime_is_memory_test() {
            self.nats_control = Box::new(nats_listener::NatsListenerHandle::noop());
            return Ok(());
        }
        let Some(mesh) = self.mesh.as_ref() else {
            return Err(StartMeshError::MeshUp(
                "startup transaction missing mesh before nats control start".into(),
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
        let nats_store = NatsStore::connect(&client_url).await.map_err(|error| {
            StartMeshError::MeshUp(format!("nats connect for node rpc: {error}"))
        })?;
        nats_store
            .start()
            .await
            .map_err(|error| StartMeshError::MeshUp(format!("nats start for node rpc: {error}")))?;
        let subject = ployz_nats::subjects::node_command(&state.identity.machine_id, ">");
        let handle = nats_listener::serve(nats_store.client().clone(), subject, command_tx)
            .await
            .map_err(StartMeshError::MeshUp)?;
        let _ = mesh;
        self.nats_control = Box::new(handle);
        Ok(())
    }

    async fn start_zfs_transfer_control(
        &mut self,
        state: &DaemonState,
        plan: &StartPlan,
    ) -> Result<(), StartMeshError> {
        if state.runtime_is_memory_test() {
            self.zfs_transfer = Box::new(transfer_listener::ZfsTransferListenerHandle::noop());
            return Ok(());
        }
        let Some(zfs_root) = state.storage.zfs_root.clone() else {
            self.zfs_transfer = Box::new(transfer_listener::ZfsTransferListenerHandle::noop());
            return Ok(());
        };
        let Some(mesh) = self.mesh.as_ref() else {
            self.zfs_transfer = Box::new(transfer_listener::ZfsTransferListenerHandle::noop());
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
        self.zfs_transfer = Box::new(handle);
        Ok(())
    }

    /// Commit: publish the active mesh into daemon state.
    async fn publish_active(&mut self, state: &mut DaemonState) -> Result<(), StartMeshError> {
        let spawn_renewal_ticker = !state.runtime_is_memory_test();
        let Some(mesh_ref) = self.mesh.as_ref() else {
            return Err(StartMeshError::MeshUp(
                "startup transaction missing mesh at commit".into(),
            ));
        };

        let subnet_coord = if spawn_renewal_ticker {
            Some(build_nats_subnet_coordinator(state, self.config.overlay_ip).await?)
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
            let nats_store = NatsStore::connect(&nats_client_url)
                .await
                .map_err(|error| {
                    StartMeshError::MeshUp(format!("nats connect for cert coord: {error}"))
                })?;
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
            Some(spawn_certificate_renewal_ticker(
                store,
                issuer_factory,
                RenewalConfig::from_env(),
                coordinator,
                readiness,
                account_coordinator,
            ))
        } else {
            None
        };

        let bootstrap_seed_cache = Some(BootstrapSeedCacheTask::spawn(
            NetworkConfig::dir(&state.data_dir, &self.config.name.0),
            mesh_ref.store.clone(),
            state.identity.machine_id.clone(),
        ));

        let Some(mesh) = self.mesh.take() else {
            return Err(StartMeshError::MeshUp(
                "startup transaction missing mesh at commit".into(),
            ));
        };
        let nats_control = std::mem::replace(&mut self.nats_control, Box::new(NoopRuntimeHandle));
        let zfs_transfer = std::mem::replace(&mut self.zfs_transfer, Box::new(NoopRuntimeHandle));
        let gateway = std::mem::replace(&mut self.gateway, Box::new(NoopRuntimeHandle));
        let dns = std::mem::replace(&mut self.dns, Box::new(NoopRuntimeHandle));
        if let Some(subnet_coord) = subnet_coord {
            state.subnet_coord = subnet_coord;
        }
        state.active = Some(ActiveMesh {
            config: self.config.clone(),
            cached_subnet: self.config.subnet,
            mesh,
            nats_control,
            zfs_transfer,
            gateway,
            dns,
            certificate_renewal,
            bootstrap_seed_cache,
        });
        Ok(())
    }

    async fn rollback_startup(&mut self) {
        let dns = std::mem::replace(&mut self.dns, Box::new(NoopRuntimeHandle));
        if let Err(error) = dns.shutdown().await {
            warn!(?error, "dns rollback failed");
        }

        let gateway = std::mem::replace(&mut self.gateway, Box::new(NoopRuntimeHandle));
        if let Err(error) = gateway.shutdown().await {
            warn!(?error, "gateway rollback failed");
        }

        let nats_control = std::mem::replace(&mut self.nats_control, Box::new(NoopRuntimeHandle));
        let _ = nats_control.shutdown().await;
        let zfs_transfer = std::mem::replace(&mut self.zfs_transfer, Box::new(NoopRuntimeHandle));
        let _ = zfs_transfer.shutdown().await;

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

        let mut tx = MeshStartTx::new(net_config);
        tx.build_mesh(self, &plan).await?;

        if let Err(error) = tx.start_nats_control(self).await {
            tx.rollback_startup().await;
            return Err(error);
        }

        if let Err(error) = tx.start_zfs_transfer_control(self, &plan).await {
            tx.rollback_startup().await;
            return Err(error);
        }

        if let Err(error) = tx.start_gateway(self, &plan).await {
            tx.rollback_startup().await;
            return Err(error);
        }

        if let Err(error) = tx.start_dns(self, &plan).await {
            tx.rollback_startup().await;
            return Err(error);
        }

        if let Err(error) = tx.publish_active(self).await {
            tx.rollback_startup().await;
            return Err(error);
        }

        Ok(tx.finish())
    }

    pub async fn restart_active_runtime_from_config(
        &mut self,
        network: &str,
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
        let dns_bridge_listen_addr = self.dns_bridge_listen_addr();

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
                bootstrap: &[],
                network_id: &net_config.id.0,
                machine_role: net_config.machine_role,
            })
            .await
            .map_err(|error| format!("runtime components failed: {error}"))?;

        let new_gateway: Box<dyn RuntimeHandle> = if net_config.subnet.is_some() {
            let gateway_config = GatewayConfig::for_network(
                &self.data_dir,
                &net_config.name.0,
                self.identity.machine_id.0.clone(),
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
                self.identity.machine_id.0.clone(),
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

        let dns = std::mem::replace(&mut active.dns, Box::new(NoopRuntimeHandle));
        if let Err(error) = dns.shutdown().await {
            tracing::warn!(?error, "runtime restart: dns stop failed");
        }

        let gateway = std::mem::replace(&mut active.gateway, Box::new(NoopRuntimeHandle));
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

        active
            .mesh
            .restart_runtime_for_subnet_change(components.network, components.container_network)
            .await
            .map_err(|error| format!("mesh runtime restart failed: {error}"))?;

        let _ = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.overlay_ip = net_config.overlay_ip;
                record.subnet = net_config.subnet;
            })
            .await;
        active.config = net_config;
        active.gateway = new_gateway;
        active.dns = new_dns;
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
        let gateway_config = net_config.subnet.map(|_| {
            GatewayConfig::for_network(
                &self.data_dir,
                &net_config.name.0,
                self.identity.machine_id.0.clone(),
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
                self.identity.machine_id.0.clone(),
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
    use ployz_runtime_api::Identity;
    use ployz_types::model::{MachineId, MachineRole, NetworkName, OverlayIp, PublicKey};

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
    fn plan_mesh_start_maps_corrupt_seed_cache_to_bootstrap_resolution_failure() {
        let state = make_test_state("0.0.0.0:80");
        let config = make_network_config(&state, "alpha");
        let network_dir = state.network_dir(&config.name.0);
        fs::create_dir_all(&network_dir).expect("create network dir");
        fs::write(
            crate::mesh_state::bootstrap::bootstrap_peers_path(&network_dir),
            "{not-json",
        )
        .expect("write corrupt seed cache");

        let error = match state.plan_mesh_start(&config) {
            Ok(_) => panic!("plan should fail"),
            Err(error) => error,
        };

        assert!(matches!(error, StartMeshError::BootstrapResolve(_)));
    }

    #[test]
    fn plan_mesh_start_uses_seed_cache_and_ignores_store_data_path() {
        let state = make_test_state("0.0.0.0:80");
        let config = make_network_config(&state, "alpha");
        let network_dir = state.network_dir(&config.name.0);
        let data_path = ployz_nats::config::Paths::new(&network_dir).data;
        fs::create_dir_all(&data_path).expect("create store data path");
        let peer = BootstrapPeerRecord {
            machine_id: MachineId("peer".into()),
            public_key: PublicKey([8; 32]),
            overlay_ip: OverlayIp("fd00::8".parse().expect("valid overlay")),
            subnet: None,
            bridge_ip: None,
            role: MachineRole::StorageCandidate,
            endpoints: vec!["peer:51820".into()],
        };
        write_bootstrap_peer_records(&network_dir, std::slice::from_ref(&peer))
            .expect("write seed cache");

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
        assert!(state.active.is_some());

        teardown_active_mesh(&mut state).await;
    }

    #[tokio::test]
    async fn publish_active_failure_keeps_startup_resources_for_rollback() {
        let mut state = make_test_state("127.0.0.1:8080");
        let mut config = make_network_config(&state, "alpha");
        config.overlay_ip = OverlayIp("::1".parse().expect("valid overlay"));
        let plan = state.plan_mesh_start(&config).expect("plan should succeed");
        let mut tx = MeshStartTx::new(config);
        tx.build_mesh(&state, &plan)
            .await
            .expect("memory mesh should start before commit");
        state.runtime_profile = RuntimeProfile::from_runtime(
            RuntimeTarget::Host,
            ServiceMode::User,
            crate::BuiltInImages::load(None)
                .expect("embedded built-in images manifest should parse"),
        );

        let error = match tx.publish_active(&mut state).await {
            Ok(_) => panic!("publish_active should fail without local NATS"),
            Err(error) => error,
        };

        assert!(matches!(error, StartMeshError::MeshUp(_)));
        assert!(
            tx.mesh.is_some(),
            "startup transaction must still own mesh after publish failure"
        );
        assert!(state.active.is_none());

        tx.rollback_startup().await;
        assert!(tx.mesh.is_none());
    }

    fn make_state(
        runtime_target: RuntimeTarget,
        service_mode: ServiceMode,
        gateway_listen_addr: &str,
    ) -> DaemonState {
        let data_dir = unique_temp_dir("ployz-start-mesh");
        let identity = Identity::generate(MachineId("founder".into()), [1; 32]);

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
        let identity = Identity::generate(MachineId("founder".into()), [1; 32]);

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

        active.stop_bootstrap_seed_cache().await;
        active.mesh.destroy().await.expect("destroy mesh");
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }
}
