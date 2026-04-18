use super::*;
use crate::model::MachineStatus;
use tracing::{info, warn};

impl Mesh {
    pub(crate) fn apply(&mut self, event: PhaseEvent) -> Result<()> {
        let next = transition(self.phase, event)?;
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

    async fn bring_up(&mut self) -> Result<()> {
        self.network.up().await?;
        self.apply(PhaseEvent::NetworkReady)?;

        if let Some(cn) = &self.container_network {
            cn.ensure().await?;
            if self.network.mode() == WireguardBackendMode::Docker {
                cn.connect("ployz-networking", Some(cn.container_v4()))
                    .await?;
            }

            if let Some(dataplane_factory) = &self.dataplane_factory {
                let attached = dataplane_factory.attach(&self.network, cn).await?;
                self.wg_ifindex = attached.wg_ifindex;
                self.dataplane = Some(attached.dataplane);
            }
        }

        let pre_start_peers: Vec<_> = self
            .seed_records
            .iter()
            .filter(|m| m.id != self.machine_id)
            .cloned()
            .collect();
        if !pre_start_peers.is_empty() {
            if let Err(e) = self.network.set_peers(&pre_start_peers).await {
                warn!(?e, "pre-start peer sync failed");
            }
            if self.wait_for_handshake().await.is_err() {
                warn!("no WG handshake on first attempt, retrying with extended timeout");
                self.wait_for_handshake_extended().await?;
            }
        }

        self.store_runtime.start().await.map_err(|error| {
            MeshError::Port(PortError::operation(
                "store runtime start",
                error.to_string(),
            ))
        })?;
        self.wait_service_ready().await?;
        self.wait_store_init().await?;

        self.start_peer_sync_task().await?;

        self.apply(PhaseEvent::ComponentsStarted)?;
        self.bootstrap_gate().await?;
        self.spawn_background_tasks().await?;

        info!(phase = %self.phase, "mesh up");
        Ok(())
    }

    async fn stop_runtime(&mut self, store: RuntimeStoreMode) -> Option<MeshError> {
        let mut first_err: Option<MeshError> = None;

        self.clear_task_channels();

        if let Some(mut tasks) = self.tasks.take()
            && let Err(error) = tasks.stop().await
        {
            warn!(?error, "task stop failed during runtime stop");
            first_err.get_or_insert(error.into());
        }
        self.probe_readiness.clear_bound_families();

        if let Some(dp) = self.dataplane.take()
            && let Err(error) = dp.detach().await
        {
            warn!(?error, "ebpf detach failed during runtime stop");
        }
        self.wg_ifindex = 0;

        if store == RuntimeStoreMode::Stop
            && let Err(error) = self.store_runtime.stop().await
        {
            warn!(?error, "service stop failed during runtime stop");
            first_err.get_or_insert(MeshError::Port(PortError::operation(
                "store runtime stop",
                error.to_string(),
            )));
        }

        if let Err(error) = self.network.down().await {
            warn!(?error, "network down failed during runtime stop");
            first_err.get_or_insert(error.into());
        }

        if let Some(container_network) = &self.container_network
            && let Err(error) = container_network.remove().await
        {
            warn!(
                ?error,
                "container network remove failed during runtime stop"
            );
            first_err.get_or_insert(error.into());
        }

        first_err
    }

    async fn teardown(&mut self) {
        let _ = self.stop_runtime(RuntimeStoreMode::Stop).await;
    }

    pub async fn detach(&mut self) -> Result<()> {
        self.apply(PhaseEvent::DetachRequested)?;
        self.clear_task_channels();
        if let Some(mut tasks) = self.tasks.take() {
            tasks.stop().await?;
        }
        info!("mesh detached");
        Ok(())
    }

    pub async fn destroy(&mut self) -> Result<()> {
        self.apply(PhaseEvent::DestroyRequested)?;

        let now = crate::time::now_unix_secs();
        if self
            .update_authoritative_self_record(|record| {
                record.status = MachineStatus::Down;
                record.updated_at = now;
            })
            .await
            .is_none()
            && self.authoritative_self_record().await.is_some()
        {
            warn!(timestamp = now, "failed to set status=down on destroy");
        }

        let first_err = self.stop_runtime(RuntimeStoreMode::Stop).await;

        self.apply(PhaseEvent::TeardownComplete)?;
        info!("mesh destroyed");

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    pub async fn restart_runtime_for_subnet_change(
        &mut self,
        network: WireguardDriver,
        container_network: Option<ContainerNetwork>,
    ) -> Result<()> {
        self.apply(PhaseEvent::DestroyRequested)?;

        let stop_err = self.stop_runtime(RuntimeStoreMode::Keep).await;
        self.network = network;
        self.container_network = container_network;

        self.apply(PhaseEvent::TeardownComplete)?;
        self.apply(PhaseEvent::UpRequested)?;

        match self.bring_up().await {
            Ok(()) => {
                if let Some(error) = stop_err {
                    warn!(
                        ?error,
                        "runtime stop reported an error before subnet restart"
                    );
                }
                Ok(())
            }
            Err(error) => {
                warn!(
                    ?error,
                    "subnet runtime restart failed, tearing down runtime"
                );
                let _ = self.stop_runtime(RuntimeStoreMode::Keep).await;
                self.apply(PhaseEvent::ComponentFailed)?;
                Err(error)
            }
        }
    }
}
