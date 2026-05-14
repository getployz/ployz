use crate::daemon::{ActiveMesh, DaemonState};
use ployz_api::DaemonResponse;
use ployz_config::RuntimeTarget;
use ployz_nats::{NatsNodeRpcClient, NatsScope, NatsStore};
use ployz_store_api::StoreRuntimeControl;

impl DaemonState {
    pub(super) async fn connect_deploy_nats_store(
        &self,
        active: &ActiveMesh,
        setup_failure_code: &'static str,
    ) -> Result<NatsStore, DaemonResponse> {
        let nats_client_url = if self.runtime_target == RuntimeTarget::Docker {
            crate::services::nats::local_client_url()
        } else {
            crate::services::nats::overlay_client_url(active.config.overlay_ip)
        };
        let nats_scope =
            NatsScope::local_for_storage_participation(&active.config.storage_participation);
        let nats_store = match NatsStore::connect_with_scope(&nats_client_url, nats_scope).await {
            Ok(store) => store.with_asset_policy(active.config.storage_replicas),
            Err(error) => return Err(self.err(setup_failure_code, error.to_string())),
        };
        if let Err(error) = nats_store.start().await {
            return Err(self.err(setup_failure_code, error.to_string()));
        }
        Ok(nats_store)
    }

    pub(super) async fn deploy_participant_probe(
        &self,
        active: &ActiveMesh,
        setup_failure_code: &'static str,
    ) -> Result<crate::daemon::deploy_probe::NatsRpcProbe, DaemonResponse> {
        let nats_store = self
            .connect_deploy_nats_store(active, setup_failure_code)
            .await?;
        Ok(crate::daemon::deploy_probe::NatsRpcProbe::new(
            NatsNodeRpcClient::for_store(&nats_store),
        ))
    }
}
