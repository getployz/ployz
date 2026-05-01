use std::time::Duration;

use ployz_store_api::{MachineRegistry, StoreRuntimeControl, SyncProbe, SyncStatus};
use tracing::{info, warn};

use crate::error::Error as PortError;
use crate::mesh::MeshNetwork;
use crate::mesh::driver::WireguardBackendMode;
use crate::mesh::phase::{PhaseEvent, TransitionError};

use super::{Mesh, MeshError, Result, poll_until};

impl Mesh {
    pub(super) fn apply(&mut self, event: PhaseEvent) -> Result<()> {
        let next = crate::mesh::phase::transition(self.phase, event)?;
        info!(from = %self.phase, to = %next, ?event, "phase transition");
        self.phase = next;
        Ok(())
    }

    pub async fn up(&mut self) -> Result<()> {
        self.apply(PhaseEvent::UpRequested)?;

        match self.bring_up().await {
            Ok(()) => Ok(()),
            Err(e) => {
                warn!(?e, "startup failed, tearing down");
                self.teardown().await;
                self.apply(PhaseEvent::ComponentFailed)?;
                Err(e)
            }
        }
    }

    pub(super) async fn bring_up(&mut self) -> Result<()> {
        self.network.up().await?;
        self.apply(PhaseEvent::NetworkReady)?;

        if let Some(cn) = &self.container_network {
            cn.ensure().await?;
            if self.network.mode() == WireguardBackendMode::Docker {
                cn.connect("ployz-networking", Some(cn.container_v4()))
                    .await?;
            }

            self.attach_ebpf_dataplane().await?;
        }

        let pre_start_peers: Vec<_> = self
            .seed_records
            .iter()
            .filter(|m| m.id != self.machine_id)
            .map(|m| m.wireguard_peer_spec())
            .collect();
        if !pre_start_peers.is_empty() {
            self.network.set_peers(&pre_start_peers).await?;
            self.wait_for_handshake().await?;
        }

        self.store.start().await?;
        self.wait_service_ready().await?;
        self.wait_store_init().await?;
        self.start_peer_sync_task().await?;

        self.apply(PhaseEvent::ComponentsStarted)?;
        self.bootstrap_gate().await?;
        self.spawn_background_tasks().await?;

        info!(phase = %self.phase, "mesh up");
        Ok(())
    }

    async fn wait_for_handshake(&self) -> Result<()> {
        poll_until(
            Duration::from_secs(10),
            Duration::from_millis(200),
            Duration::from_millis(200),
            || async { self.network.has_remote_handshake().await },
        )
        .await
        .then_some(())
        .ok_or_else(|| {
            MeshError::Port(PortError::operation(
                "handshake wait",
                "no WG handshake with any seeded peer within 10s -- store cannot \
                 leafnode-bridge to the hub without a working overlay path"
                    .to_string(),
            ))
        })?;
        info!("WG remote handshake confirmed, proceeding with store start");
        Ok(())
    }

    async fn wait_service_ready(&self) -> Result<()> {
        let timeout = self.service_ready_timeout;
        let ok = poll_until(
            timeout,
            Duration::from_millis(50),
            Duration::from_secs(1),
            || async { self.store.healthy().await },
        )
        .await;
        if !ok {
            return Err(MeshError::Port(PortError::operation(
                "service ready",
                format!("service did not become ready within {timeout:?}"),
            )));
        }
        Ok(())
    }

    async fn wait_store_init(&self) -> Result<()> {
        let timeout = Duration::from_secs(30);
        let init_ok = poll_until(
            timeout,
            Duration::from_millis(100),
            Duration::from_secs(2),
            || async {
                match self.store.init().await {
                    Ok(()) => true,
                    Err(e) => {
                        info!(?e, "store not ready, retrying");
                        false
                    }
                }
            },
        )
        .await;
        if !init_ok {
            return Err(MeshError::Port(PortError::operation(
                "store init",
                format!("store did not become ready within {timeout:?}"),
            )));
        }

        let query_ok = poll_until(
            timeout,
            Duration::from_millis(100),
            Duration::from_secs(2),
            || async {
                match self.store.list_machines().await {
                    Ok(_) => true,
                    Err(e) => {
                        info!(?e, "store not queryable yet, retrying");
                        false
                    }
                }
            },
        )
        .await;
        if !query_ok {
            return Err(MeshError::Port(PortError::operation(
                "store init",
                format!("store queries did not succeed within {timeout:?}"),
            )));
        }
        Ok(())
    }

    async fn bootstrap_gate(&mut self) -> Result<()> {
        let machines = self.store.list_machines().await?;
        let has_remote_peer = machines.iter().any(|m| m.id != self.machine_id);
        if !has_remote_peer {
            self.apply(PhaseEvent::SyncComplete)?;
            return Ok(());
        }

        let interval = self.bootstrap_interval;
        let connection_timeout = self.connection_timeout;
        let store = self.store.clone();

        let result: std::result::Result<bool, String> =
            tokio::time::timeout(connection_timeout, async {
                let mut consecutive_errors = 0u32;
                loop {
                    match store.sync_status().await {
                        Ok(SyncStatus::Disconnected) => {
                            consecutive_errors = 0;
                        }
                        Ok(_) => return Ok(true),
                        Err(e) => {
                            consecutive_errors += 1;
                            if consecutive_errors <= 3 {
                                warn!(?e, "sync probe failed during bootstrap");
                            } else if consecutive_errors == 4 {
                                warn!(
                                    ?e,
                                    consecutive_errors,
                                    "sync probe keeps failing"
                                );
                            }
                        }
                    }
                    tokio::time::sleep(interval).await;
                }
            })
            .await
            .unwrap_or(Ok(false));

        let connected = matches!(result, Ok(true));

        if !connected {
            let reason = match result {
                Ok(_) => {
                    "store sync could not reach any remote peer within the timeout. \
                     Try restarting the mesh on both nodes"
                        .to_string()
                }
                Err(e) => {
                    format!(
                        "store never became healthy: {e}. \
                         Try restarting the mesh on both nodes"
                    )
                }
            };
            return Err(TransitionError::BootstrapTimeout { reason }.into());
        }

        self.apply(PhaseEvent::SyncComplete)?;
        Ok(())
    }
}
