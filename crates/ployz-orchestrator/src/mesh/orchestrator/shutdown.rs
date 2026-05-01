use crate::mesh::MeshNetwork;
use crate::mesh::container_network::ContainerNetwork;
use crate::mesh::driver::WireguardDriver;
use ployz_store_api::StoreRuntimeControl;
use tracing::{info, warn};

use super::{Mesh, MeshError, Result};

impl Mesh {
    fn clear_task_channels(&mut self) {
        self.endpoint_maintainer_tx = None;
        self.self_record_tx = None;
        self.task_cancel = None;
    }

    async fn stop_runtime(&mut self, stop_store: bool) -> Option<MeshError> {
        let mut first_err: Option<MeshError> = None;

        self.clear_task_channels();

        if let Some(mut tasks) = self.tasks.take()
            && let Err(error) = tasks.stop().await
        {
            warn!(?error, "task stop failed during runtime stop");
            first_err.get_or_insert_with(|| error.into());
        }

        if let Some(dp) = self.dataplane.take()
            && let Err(error) = dp.detach().await
        {
            warn!(?error, "ebpf detach failed during runtime stop");
        }
        self.wg_ifindex = 0;

        if stop_store && let Err(error) = self.store.stop().await {
            warn!(?error, "service stop failed during runtime stop");
            first_err.get_or_insert_with(|| error.into());
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

    pub(super) async fn teardown(&mut self) {
        let _ = self.stop_runtime(true).await;
    }

    pub async fn detach(&mut self) -> Result<()> {
        self.apply(crate::mesh::phase::PhaseEvent::DetachRequested)?;
        self.clear_task_channels();
        if let Some(mut tasks) = self.tasks.take() {
            tasks.stop().await?;
        }
        info!("mesh detached");
        Ok(())
    }

    pub async fn destroy(&mut self) -> Result<()> {
        self.apply(crate::mesh::phase::PhaseEvent::DestroyRequested)?;

        let first_err = self.stop_runtime(true).await;

        self.apply(crate::mesh::phase::PhaseEvent::TeardownComplete)?;
        info!("mesh destroyed");

        match first_err {
            Some(e) => Err(e),
            None => Ok(()),
        }
    }

    pub async fn destroy_and_wipe_store_data(&mut self) -> Result<()> {
        self.apply(crate::mesh::phase::PhaseEvent::DestroyRequested)?;

        let mut first_err = self.stop_runtime(true).await;
        if let Err(error) = self.store.wipe_data().await {
            warn!(?error, "store wipe failed during mesh destroy");
            first_err.get_or_insert_with(|| error.into());
        }

        self.apply(crate::mesh::phase::PhaseEvent::TeardownComplete)?;
        info!("mesh destroyed and store data wiped");

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
        self.apply(crate::mesh::phase::PhaseEvent::DestroyRequested)?;

        let stop_err = self.stop_runtime(false).await;
        self.network = network;
        self.container_network = container_network;

        self.apply(crate::mesh::phase::PhaseEvent::TeardownComplete)?;
        self.apply(crate::mesh::phase::PhaseEvent::UpRequested)?;

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
                let _ = self.stop_runtime(false).await;
                self.apply(crate::mesh::phase::PhaseEvent::ComponentFailed)?;
                Err(error)
            }
        }
    }
}
