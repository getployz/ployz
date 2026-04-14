use super::*;
use crate::mesh::phase::PhaseEvent;
use crate::mesh::phase::TransitionError;
use ployz_store_api::SyncStatus;
use std::time::Duration;
use tracing::{info, warn};

impl Mesh {
    pub(crate) async fn wait_for_handshake(&self) -> Result<()> {
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
                "no WG handshake within 10s".to_string(),
            ))
        })?;
        info!("WG remote handshake confirmed, proceeding with store start");
        Ok(())
    }

    pub(crate) async fn wait_for_handshake_extended(&self) -> Result<()> {
        poll_until(
            Duration::from_secs(20),
            Duration::from_millis(500),
            Duration::from_secs(1),
            || async { self.network.has_remote_handshake().await },
        )
        .await
        .then_some(())
        .ok_or_else(|| {
            MeshError::Port(PortError::operation(
                "handshake wait",
                "no WG handshake after extended retry — cannot start store on broken tunnel"
                    .to_string(),
            ))
        })?;
        info!("WG remote handshake confirmed on extended retry");
        Ok(())
    }

    pub(crate) async fn wait_service_ready(&self) -> Result<()> {
        let timeout = self.service_ready_timeout;
        let ok = poll_until(
            timeout,
            Duration::from_millis(50),
            Duration::from_secs(1),
            || async { self.store_runtime.healthy().await },
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

    pub(crate) async fn wait_store_init(&self) -> Result<()> {
        let timeout = Duration::from_secs(60);
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

    pub(crate) async fn bootstrap_gate(&mut self) -> Result<()> {
        let machines = self.store.list_machines().await?;
        let has_remote_peer = machines.iter().any(|m| m.id != self.machine_id);
        if !has_remote_peer {
            self.apply(PhaseEvent::SyncComplete)?;
            return Ok(());
        }

        if self.allow_disconnected_bootstrap {
            info!("skipping bootstrap wait because disconnected bootstrap is allowed");
            self.apply(PhaseEvent::SyncComplete)?;
            return Ok(());
        }

        let interval = self.bootstrap_interval;
        let connection_timeout = self.connection_timeout;
        let sync_probe = Arc::clone(&self.sync_probe);
        let store_runtime = Arc::clone(&self.store_runtime);

        let result: std::result::Result<bool, String> =
            tokio::time::timeout(connection_timeout, async {
                let mut consecutive_errors = 0u32;
                let mut restarted = false;
                loop {
                    match sync_probe.sync_status().await {
                        Ok(SyncStatus::Disconnected) => {
                            consecutive_errors = 0;
                        }
                        Ok(_) => return Ok(true),
                        Err(e) => {
                            consecutive_errors += 1;
                            if consecutive_errors <= 3 {
                                warn!(?e, "sync probe failed during bootstrap");
                            } else if consecutive_errors == 4 && !restarted {
                                warn!(
                                    ?e,
                                    consecutive_errors,
                                    "sync probe stuck, restarting store runtime"
                                );
                                let _ = store_runtime.stop().await;
                                tokio::time::sleep(Duration::from_secs(1)).await;
                                if let Err(restart_err) = store_runtime.start().await {
                                    warn!(?restart_err, "store runtime restart failed");
                                }
                                restarted = true;
                                consecutive_errors = 0;
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
                    "corrosion gossip could not reach any remote peer within the timeout"
                        .to_string()
                }
                Err(e) => {
                    format!("corrosion API never became healthy: {e}")
                }
            };
            return Err(TransitionError::BootstrapTimeout { reason }.into());
        }

        self.apply(PhaseEvent::SyncComplete)?;
        Ok(())
    }
}

async fn poll_until<F, Fut>(
    timeout: Duration,
    initial_interval: Duration,
    max_interval: Duration,
    mut check: F,
) -> bool
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(timeout, async {
        let mut interval = initial_interval;
        loop {
            if check().await {
                return;
            }
            tokio::time::sleep(interval).await;
            interval = (interval * 2).min(max_interval);
        }
    })
    .await
    .is_ok()
}
