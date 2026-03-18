use std::net::SocketAddr;
use std::path::PathBuf;

use thiserror::Error;
use tracing::warn;

use crate::mesh_state::bootstrap::{BootstrapInfo, build_seed_records, resolve_bootstrap_addrs};
use crate::mesh_state::network::NetworkConfig;
use ployz_config::RuntimeTarget;
use ployz_corrosion::{
    config as corrosion_config, corrosion_bootstrap_from_db, peer_records_from_db,
};
use ployz_dns::DnsConfig;
use ployz_gateway::GatewayConfig;
use ployz_orchestrator::Mesh;
use ployz_orchestrator::mesh::wireguard::DEFAULT_LISTEN_PORT;
use ployz_runtime_api::{NoopRuntimeHandle, RuntimeHandle};

#[cfg(test)]
use super::DaemonRuntimeConfig;
use super::{ActiveMesh, DaemonState};

#[derive(Debug, Clone, Copy, Default)]
pub struct MeshStartOptions {
    pub allow_disconnected_bootstrap: bool,
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
    #[error("remote control start failed on {bind}: {error}")]
    RemoteControl { bind: SocketAddr, error: String },
    #[error("gateway start failed: {0}")]
    Gateway(String),
    #[error("dns start failed: {0}")]
    Dns(String),
    #[error("active marker persist failed: {0}")]
    ActiveMarkerPersist(String),
}

struct StartPlan {
    network_dir: PathBuf,
    bootstrap: Option<BootstrapInfo>,
    bootstrap_addrs: Vec<String>,
    gateway_port: u16,
    remote_control_bind_addr: SocketAddr,
    gateway_config: GatewayConfig,
    dns_config: DnsConfig,
    overlay_network_name: Option<String>,
}

struct MeshStartTx {
    config: NetworkConfig,
    mesh: Option<Mesh>,
    remote_control: Box<dyn RuntimeHandle>,
    gateway: Box<dyn RuntimeHandle>,
    dns: Box<dyn RuntimeHandle>,
}

impl MeshStartTx {
    fn new(config: NetworkConfig) -> Self {
        Self {
            config,
            mesh: None,
            remote_control: Box::new(NoopRuntimeHandle),
            gateway: Box::new(NoopRuntimeHandle),
            dns: Box::new(NoopRuntimeHandle),
        }
    }

    /// Fatal: build mesh drivers and call `Mesh::up()`, relying on `Mesh::up()` to self-teardown on failure.
    async fn build_mesh(
        &mut self,
        state: &DaemonState,
        plan: &StartPlan,
        options: MeshStartOptions,
    ) -> Result<(), StartMeshError> {
        let exposed_tcp_ports = [plan.gateway_port];
        let db_records = peer_records_from_db(&plan.network_dir).unwrap_or_else(|error| {
            tracing::warn!(
                ?error,
                "failed to load corrosion bootstrap peers, continuing without db seeds"
            );
            Vec::new()
        });
        let components = state
            .build_runtime_mesh_components(
                self.config.overlay_ip,
                &plan.network_dir,
                &self.config.name.0,
                self.config.subnet,
                &exposed_tcp_ports,
                &plan.bootstrap_addrs,
                &self.config.id.0,
            )
            .await
            .map_err(StartMeshError::NetworkDriver)?;

        let listen_port = DEFAULT_LISTEN_PORT;
        let seed_records = build_seed_records(
            &plan.network_dir,
            &state.identity,
            &self.config,
            plan.bootstrap.as_ref(),
            listen_port,
            &db_records,
        )
        .await;

        let mut mesh = Mesh::new(
            components.network,
            components.store,
            components.store_runtime,
            components.container_network,
            state.identity.machine_id.clone(),
            listen_port,
        )
        .with_seed_records(seed_records)
        .with_disconnected_bootstrap_allowed(options.allow_disconnected_bootstrap);

        mesh.up()
            .await
            .map_err(|error| StartMeshError::MeshUp(error.to_string()))?;

        self.mesh = Some(mesh);
        Ok(())
    }

    /// Fatal: start remote control or roll back the mesh.
    async fn start_remote_control(
        &mut self,
        state: &DaemonState,
        plan: &StartPlan,
    ) -> Result<(), StartMeshError> {
        let Some(mesh) = self.mesh.as_ref() else {
            return Err(StartMeshError::MeshUp(
                "startup transaction missing mesh before remote control start".into(),
            ));
        };

        let handle = state
            .start_runtime_remote_control(
                plan.remote_control_bind_addr,
                mesh.store.clone(),
                state.namespace_locks.clone(),
                state.identity.machine_id.clone(),
                plan.overlay_network_name.clone(),
                if state.runtime_target == RuntimeTarget::Docker {
                    mesh.container_dns_server()
                } else {
                    None
                },
            )
            .await
            .map_err(|error| StartMeshError::RemoteControl {
                bind: plan.remote_control_bind_addr,
                error,
            })?;

        self.remote_control = handle;
        Ok(())
    }

    /// Fatal: start gateway or roll back remote control plus mesh.
    async fn start_gateway(
        &mut self,
        state: &DaemonState,
        plan: &StartPlan,
    ) -> Result<(), StartMeshError> {
        let handle = state
            .start_runtime_gateway(plan.gateway_config.clone())
            .await
            .map_err(StartMeshError::Gateway)?;
        self.gateway = handle;
        Ok(())
    }

    /// Fatal: start DNS or roll back gateway, remote control, and mesh.
    async fn start_dns(
        &mut self,
        state: &DaemonState,
        plan: &StartPlan,
    ) -> Result<(), StartMeshError> {
        let handle = state
            .start_runtime_dns(plan.dns_config.clone())
            .await
            .map_err(StartMeshError::Dns)?;
        self.dns = handle;
        Ok(())
    }

    /// Commit: persist the active marker, then publish the active mesh into daemon state.
    async fn publish_active(&mut self, state: &mut DaemonState) -> Result<(), StartMeshError> {
        state
            .write_active_marker(&self.config.name.0)
            .map_err(|error| StartMeshError::ActiveMarkerPersist(error.to_string()))?;

        let Some(mesh) = self.mesh.take() else {
            return Err(StartMeshError::MeshUp(
                "startup transaction missing mesh at commit".into(),
            ));
        };
        let remote_control =
            std::mem::replace(&mut self.remote_control, Box::new(NoopRuntimeHandle));
        let gateway = std::mem::replace(&mut self.gateway, Box::new(NoopRuntimeHandle));
        let dns = std::mem::replace(&mut self.dns, Box::new(NoopRuntimeHandle));

        state.active = Some(ActiveMesh {
            config: self.config.clone(),
            mesh,
            remote_control,
            gateway,
            dns,
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

        let remote_control =
            std::mem::replace(&mut self.remote_control, Box::new(NoopRuntimeHandle));
        let _ = remote_control.shutdown().await;

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
        self.start_mesh(net_config, None, MeshStartOptions::default())
            .await
            .map_err(|error| error.to_string())
    }

    pub async fn start_mesh(
        &mut self,
        net_config: NetworkConfig,
        bootstrap: Option<BootstrapInfo>,
        options: MeshStartOptions,
    ) -> Result<MeshStartSummary, StartMeshError> {
        let plan = self.plan_mesh_start(&net_config, bootstrap, options)?;
        tracing::info!(
            ?self.runtime_target,
            ?self.service_mode,
            network = %net_config.name,
            "starting mesh"
        );

        let mut tx = MeshStartTx::new(net_config);
        tx.build_mesh(self, &plan, options).await?;

        if let Err(error) = tx.start_remote_control(self, &plan).await {
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

    pub async fn restart_active_runtime_for_subnet_heal(
        &mut self,
        network: &str,
    ) -> Result<(), String> {
        let config_path = NetworkConfig::path(&self.data_dir, network);
        let net_config = NetworkConfig::load(&config_path)
            .map_err(|error| format!("load network config: {error}"))?;
        let gateway_port =
            Self::gateway_port(&self.gateway_listen_addr).map_err(|error| error.to_string())?;
        let exposed_tcp_ports = [gateway_port];
        let network_dir = self.network_dir(&net_config.name.0);
        let dns_bridge_listen_addr = self.dns_bridge_listen_addr();

        if self.active.is_none() {
            return Err("no running network".into());
        }

        let components = self
            .build_runtime_mesh_components(
                net_config.overlay_ip,
                &network_dir,
                &net_config.name.0,
                net_config.subnet,
                &exposed_tcp_ports,
                &[],
                &net_config.id.0,
            )
            .await
            .map_err(|error| format!("runtime components failed: {error}"))?;

        let gateway_config = GatewayConfig::for_network(
            &self.data_dir,
            &net_config.name.0,
            self.gateway_listen_addr.clone(),
            self.gateway_threads,
        );
        let dns_config = DnsConfig::for_network(
            &self.data_dir,
            &net_config.name.0,
            net_config.overlay_ip,
            dns_bridge_listen_addr,
        );

        let new_gateway = self
            .start_runtime_gateway(gateway_config)
            .await
            .map_err(|error| format!("gateway start failed: {error}"))?;
        let new_dns = match self.start_runtime_dns(dns_config).await {
            Ok(handle) => handle,
            Err(error) => {
                let gateway = new_gateway;
                if let Err(shutdown_error) = gateway.shutdown().await {
                    tracing::warn!(
                        ?shutdown_error,
                        "subnet heal: gateway rollback failed after dns start error"
                    );
                }
                return Err(format!("dns start failed: {error}"));
            }
        };

        let Some(active) = self.active.as_mut() else {
            return Err("no running network".into());
        };

        let dns = std::mem::replace(&mut active.dns, Box::new(NoopRuntimeHandle));
        if let Err(error) = dns.shutdown().await {
            tracing::warn!(
                ?error,
                "subnet heal: dns stop failed during runtime restart"
            );
        }

        let gateway = std::mem::replace(&mut active.gateway, Box::new(NoopRuntimeHandle));
        if let Err(error) = gateway.shutdown().await {
            tracing::warn!(
                ?error,
                "subnet heal: gateway stop failed during runtime restart"
            );
        }

        let _ = active
            .mesh
            .update_authoritative_self_record(|record| {
                record.overlay_ip = net_config.overlay_ip;
                record.subnet = Some(net_config.subnet);
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
                record.subnet = Some(net_config.subnet);
            })
            .await;
        active.config = net_config;
        active.gateway = new_gateway;
        active.dns = new_dns;
        Ok(())
    }

    /// Fatal before startup: resolve every startup input and explicit policy value into a `StartPlan`.
    fn plan_mesh_start(
        &self,
        net_config: &NetworkConfig,
        bootstrap: Option<BootstrapInfo>,
        _options: MeshStartOptions,
    ) -> Result<StartPlan, StartMeshError> {
        let network_dir = self.network_dir(&net_config.name.0);
        let fallback_bootstrap_addrs =
            corrosion_bootstrap_from_db(&network_dir, &self.identity.machine_id)
                .map_err(StartMeshError::BootstrapResolve)?;
        let bootstrap_addrs = resolve_bootstrap_addrs(
            &bootstrap,
            corrosion_config::DEFAULT_GOSSIP_PORT,
            &fallback_bootstrap_addrs,
        )
        .map_err(StartMeshError::BootstrapResolve)?;
        let gateway_port = Self::gateway_port(&self.gateway_listen_addr)?;
        let remote_control_bind_addr =
            self.remote_control_bind_addr(self.remote_control_port, net_config.overlay_ip);
        let gateway_config = GatewayConfig::for_network(
            &self.data_dir,
            &net_config.name.0,
            self.gateway_listen_addr.clone(),
            self.gateway_threads,
        );
        let dns_config = DnsConfig::for_network(
            &self.data_dir,
            &net_config.name.0,
            net_config.overlay_ip,
            self.dns_bridge_listen_addr(),
        );

        Ok(StartPlan {
            network_dir,
            bootstrap,
            bootstrap_addrs,
            gateway_port,
            remote_control_bind_addr,
            gateway_config,
            dns_config,
            overlay_network_name: self.runtime_overlay_network_name(&net_config.name.0),
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
    use ployz_config::{RuntimeTarget, ServiceMode};
    use ployz_runtime_api::Identity;
    use ployz_types::model::{MachineId, NetworkName};

    #[test]
    fn plan_mesh_start_uses_localhost_for_docker_remote_control() {
        let state = make_state(RuntimeTarget::Docker, ServiceMode::User, "0.0.0.0:80");
        let config = make_network_config(&state, "alpha");

        let plan = state
            .plan_mesh_start(&config, None, MeshStartOptions::default())
            .expect("plan should succeed");

        assert_eq!(
            plan.remote_control_bind_addr,
            SocketAddr::from(([127, 0, 0, 1], state.remote_control_port))
        );
    }

    #[test]
    fn plan_mesh_start_uses_overlay_ip_for_host_remote_control() {
        let state = make_state(RuntimeTarget::Host, ServiceMode::User, "0.0.0.0:80");
        let config = make_network_config(&state, "alpha");

        let plan = state
            .plan_mesh_start(&config, None, MeshStartOptions::default())
            .expect("plan should succeed");

        assert_eq!(
            plan.remote_control_bind_addr,
            SocketAddr::new(IpAddr::V6(config.overlay_ip.0), state.remote_control_port)
        );
    }

    #[test]
    fn plan_mesh_start_rejects_invalid_gateway_listen_addr() {
        let state = make_test_state("not-a-socket");
        let config = make_network_config(&state, "alpha");

        let error = match state.plan_mesh_start(&config, None, MeshStartOptions::default()) {
            Ok(_) => panic!("plan should fail"),
            Err(error) => error,
        };

        assert!(matches!(error, StartMeshError::GatewayListenAddr(_)));
    }

    #[test]
    fn plan_mesh_start_maps_bootstrap_resolution_failures() {
        let state = make_test_state("0.0.0.0:80");
        let config = make_network_config(&state, "alpha");
        let network_dir = state.network_dir(&config.name.0);
        let db_path = ployz_corrosion::config::Paths::new(&network_dir).db;
        fs::create_dir_all(&db_path).expect("create invalid db path");

        let error = match state.plan_mesh_start(&config, None, MeshStartOptions::default()) {
            Ok(_) => panic!("plan should fail"),
            Err(error) => error,
        };

        assert!(matches!(error, StartMeshError::BootstrapResolve(_)));
    }

    #[tokio::test]
    async fn start_mesh_returns_summary_and_publishes_active_mesh() {
        let mut state = make_test_state("127.0.0.1:8080");
        let config = make_network_config(&state, "alpha");

        let summary = state
            .start_mesh(config, None, MeshStartOptions::default())
            .await
            .expect("mesh start should succeed");

        assert_eq!(summary.network_name, "alpha");
        assert!(state.active.is_some());

        teardown_active_mesh(&mut state).await;
    }

    #[tokio::test]
    async fn start_mesh_rolls_back_when_active_marker_persist_fails() {
        let mut state = make_test_state("127.0.0.1:8080");
        let config = make_network_config(&state, "alpha");

        fs::create_dir_all(state.active_marker_path()).expect("create marker dir");

        let error = state
            .start_mesh(config, None, MeshStartOptions::default())
            .await
            .expect_err("mesh start should fail");

        assert!(matches!(error, StartMeshError::ActiveMarkerPersist(_)));
        assert!(state.active.is_none());
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
            crate::BuiltInImages::load(None)
                .expect("embedded built-in images manifest should parse"),
            DaemonRuntimeConfig {
                cluster_cidr: "10.210.0.0/16".into(),
                subnet_prefix_len: 24,
                remote_control_port: 4317,
                gateway_listen_addr: gateway_listen_addr.into(),
                gateway_threads: 1,
            },
        )
    }

    fn make_test_state(gateway_listen_addr: &str) -> DaemonState {
        let data_dir = unique_temp_dir("ployz-start-mesh");
        let identity = Identity::generate(MachineId("founder".into()), [1; 32]);

        DaemonState::new_for_tests(
            &data_dir,
            identity,
            DaemonRuntimeConfig {
                cluster_cidr: "10.210.0.0/16".into(),
                subnet_prefix_len: 24,
                remote_control_port: 4317,
                gateway_listen_addr: gateway_listen_addr.into(),
                gateway_threads: 1,
            },
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
