use super::*;
use ployz_store_api::SyncStatus;

#[derive(Debug, Clone, Copy)]
pub struct MeshReadyStatus {
    pub ready: bool,
    pub phase: Phase,
    pub store_healthy: bool,
    pub sync_connected: bool,
    pub self_record_published: bool,
}

impl Mesh {
    pub async fn ready_status(&self) -> MeshReadyStatus {
        let phase = self.phase;
        let store_healthy = self.store_runtime.healthy().await;
        let has_remote_store_peer = self
            .store
            .list_machines()
            .await
            .map(|machines| {
                machines
                    .into_iter()
                    .any(|machine| machine.id != self.machine_id)
            })
            .unwrap_or(false);
        let has_remote_seed_peer = self
            .seed_records
            .iter()
            .any(|machine| machine.id != self.machine_id);
        let has_remote_peer = has_remote_store_peer || has_remote_seed_peer;
        let sync_connected = if has_remote_peer {
            match self.sync_probe.sync_status().await {
                Ok(SyncStatus::Disconnected) => false,
                Ok(SyncStatus::Syncing { .. }) | Ok(SyncStatus::Synced) => true,
                Err(_) => false,
            }
        } else {
            true
        };
        let self_record_published = self.authoritative_self_record().await.is_some();
        let ready =
            phase == Phase::Running && store_healthy && sync_connected && self_record_published;

        MeshReadyStatus {
            ready,
            phase,
            store_healthy,
            sync_connected,
            self_record_published,
        }
    }

    pub async fn store_healthy(&self) -> bool {
        self.store_runtime.healthy().await
    }

    pub async fn detect_endpoints(&self) -> Result<Vec<String>> {
        self.endpoint_discovery
            .detect_endpoints(self.listen_port)
            .await
            .map_err(MeshError::Runtime)
    }
}
