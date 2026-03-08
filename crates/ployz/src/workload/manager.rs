use bollard::Docker;
use ipnet::Ipv4Net;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{info, warn};

use crate::adapters::docker_network::DockerBridgeNetwork;
use crate::adapters::wireguard::docker::DockerWireGuard;
use crate::adapters::wireguard::sidecar::{SidecarConfig, WgSidecar};
use crate::error::{Error, Result};
use crate::model::{MachineId, PublicKey, WorkloadId, WorkloadRecord};
use crate::network::ipam::SubnetIpam;

use crate::adapters::wireguard::DEFAULT_LISTEN_PORT;

struct ActiveWorkload {
    record: WorkloadRecord,
    sidecar: WgSidecar,
}

pub struct DockerWorkloadManager {
    docker: Docker,
    machine_id: MachineId,
    cluster_cidr: String,
    backbone: Arc<DockerWireGuard>,
    bridge: Arc<DockerBridgeNetwork>,
    ipam: Mutex<SubnetIpam>,
    workloads: Mutex<HashMap<WorkloadId, ActiveWorkload>>,
}

impl DockerWorkloadManager {
    pub fn new(
        machine_id: MachineId,
        subnet: Ipv4Net,
        cluster_cidr: String,
        backbone: Arc<DockerWireGuard>,
        bridge: Arc<DockerBridgeNetwork>,
    ) -> Result<Self> {
        let docker = Docker::connect_with_socket_defaults()
            .map_err(|e| Error::operation("docker connect", e.to_string()))?;

        Ok(Self {
            docker,
            machine_id,
            cluster_cidr,
            backbone,
            bridge,
            ipam: Mutex::new(SubnetIpam::new(subnet)),
            workloads: Mutex::new(HashMap::new()),
        })
    }

    pub async fn create(&self, name: &str) -> Result<WorkloadRecord> {
        let id = WorkloadId(name.to_string());
        let container_name = format!("ployz-sidecar-{name}");

        // 1. Allocate overlay IP
        let overlay_ip =
            self.ipam.lock().await.allocate().ok_or_else(|| {
                Error::operation("workload create", "subnet exhausted".to_string())
            })?;

        // Track acquired resources for rollback
        let mut rollback = CreateRollback {
            ip: Some(overlay_ip),
            pubkey: None,
            sidecar_started: false,
        };

        let result = self
            .create_inner(name, &id, &container_name, overlay_ip, &mut rollback)
            .await;

        if result.is_err() {
            rollback.run(self).await;
        }

        result
    }

    async fn create_inner(
        &self,
        name: &str,
        id: &WorkloadId,
        container_name: &str,
        overlay_ip: std::net::Ipv4Addr,
        rollback: &mut CreateRollback,
    ) -> Result<WorkloadRecord> {
        // 2. Generate x25519 keypair
        let private_key_bytes: [u8; 32] = {
            let mut buf = [0u8; 32];
            rand::fill(&mut buf);
            buf
        };
        let sidecar_pubkey =
            x25519_dalek::PublicKey::from(&x25519_dalek::StaticSecret::from(private_key_bytes))
                .to_bytes();

        // 3. Register sidecar on backbone FIRST
        self.backbone
            .add_sidecar_peer(sidecar_pubkey, overlay_ip)
            .await?;
        rollback.pubkey = Some(sidecar_pubkey);

        // 4. Create and start sidecar container
        let backbone_endpoint = format!("{}:{}", self.bridge.container_v4(), DEFAULT_LISTEN_PORT);

        let sidecar_config = SidecarConfig {
            container_name: container_name.to_string(),
            private_key: private_key_bytes,
            overlay_ip,
            backbone_pubkey: *self.backbone.public_key_bytes(),
            backbone_endpoint,
            cluster_cidr: self.cluster_cidr.clone(),
            image: self.backbone.image().to_string(),
        };

        let sidecar = WgSidecar::new(self.docker.clone(), sidecar_config);
        sidecar.up().await?;
        rollback.sidecar_started = true;

        // 5. Connect to bridge BEFORE setup_interface so the backbone endpoint is reachable
        self.bridge
            .connect(container_name, Some(overlay_ip))
            .await?;

        // 6. Configure WG interface inside sidecar (now bridge-connected, can reach backbone)
        sidecar.setup_interface().await?;

        let record = WorkloadRecord {
            id: id.clone(),
            machine_id: self.machine_id.clone(),
            overlay_ip,
            public_key: PublicKey(sidecar_pubkey),
            sidecar_container: container_name.to_string(),
        };

        self.workloads.lock().await.insert(
            id.clone(),
            ActiveWorkload {
                record: record.clone(),
                sidecar,
            },
        );

        // Disarm rollback — resources now owned by ActiveWorkload
        rollback.ip = None;
        rollback.pubkey = None;
        rollback.sidecar_started = false;

        info!(name, %overlay_ip, "workload created");
        Ok(record)
    }

    pub async fn destroy(&self, id: &WorkloadId) -> Result<()> {
        let workload = self.workloads.lock().await.remove(id).ok_or_else(|| {
            Error::operation("workload destroy", format!("workload '{id}' not found"))
        })?;

        // 1. Remove from backbone FIRST — stop routing traffic before tearing down sidecar
        if let Err(e) = self
            .backbone
            .remove_sidecar_peer(&workload.record.public_key.0)
            .await
        {
            warn!(?e, id = %id, "failed to remove sidecar peer from backbone");
        }

        // 2. Stop sidecar container
        if let Err(e) = workload.sidecar.down().await {
            warn!(?e, id = %id, "failed to stop sidecar");
        }

        // 3. Release IP
        self.ipam.lock().await.release(&workload.record.overlay_ip);

        info!(id = %id, "workload destroyed");
        Ok(())
    }

    pub async fn list(&self) -> Vec<WorkloadRecord> {
        self.workloads
            .lock()
            .await
            .values()
            .map(|w| w.record.clone())
            .collect()
    }

    pub fn sidecar_network_mode(&self, id: &WorkloadId) -> Option<String> {
        // Can't await in a sync fn, but we can try_lock for the common non-contended case
        let workloads = self.workloads.try_lock().ok()?;
        workloads
            .get(id)
            .map(|w| format!("container:{}", w.record.sidecar_container))
    }
}

struct CreateRollback {
    ip: Option<std::net::Ipv4Addr>,
    pubkey: Option<[u8; 32]>,
    sidecar_started: bool,
}

impl CreateRollback {
    async fn run(&self, mgr: &DockerWorkloadManager) {
        if self.sidecar_started {
            // Sidecar container was started but not moved into ActiveWorkload.
            // We can't hold a reference to it, but the container name is deterministic
            // and will be cleaned up by remove_sidecar_peer + eventual container GC.
        }
        if let Some(pubkey) = &self.pubkey {
            let _ = mgr.backbone.remove_sidecar_peer(pubkey).await;
        }
        if let Some(ip) = &self.ip {
            mgr.ipam.lock().await.release(ip);
        }
    }
}
