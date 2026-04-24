use std::time::Duration;

use tokio::time::timeout;
use tracing::{info, warn};

fn main() -> Result<(), ployz_gateway::GatewayError> {
    tracing_subscriber::fmt::init();
    ployz_metrics::set_build_info("ployz-gateway", env!("CARGO_PKG_VERSION"));
    let config = ployz_gateway::GatewayConfig::from_env()?;
    // Single runtime for the process. Corrosion's reqwest client pins its
    // HTTP/2 connection driver to the runtime that first used it; crossing
    // runtimes causes later requests to hang on their response futures.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("ployz-gateway-async")
        .worker_threads(2)
        .build()
        .map_err(|err| ployz_gateway::GatewayError::Runtime(err.to_string()))?;
    let store = runtime.block_on(async {
        let store =
            ployz_corrosion::CorrosionStore::connect_for_network(&config.data_dir, &config.network)
                .await
                .map_err(|err| ployz_gateway::GatewayError::Store(err.to_string()))?;
        probe_corrosion_startup(&store).await;
        Ok::<_, ployz_gateway::GatewayError>(store)
    })?;
    struct StandaloneStore(ployz_corrosion::CorrosionStore);
    impl ployz_gateway::RoutingStore for StandaloneStore {
        async fn load_routing_state(
            &self,
        ) -> Result<ployz_types::model::RoutingState, ployz_gateway::GatewayError> {
            info!("gateway store call start: load_routing_state");
            match ployz_store_api::RoutingStore::load_routing_state(&self.0).await {
                Ok(state) => {
                    info!(
                        revisions = state.revisions.len(),
                        releases = state.releases.len(),
                        instances = state.instances.len(),
                        "gateway store call complete: load_routing_state"
                    );
                    Ok(state)
                }
                Err(err) => {
                    warn!(error = %err, "gateway store call failed: load_routing_state");
                    Err(ployz_gateway::GatewayError::Store(err.to_string()))
                }
            }
        }

        async fn subscribe_routing_invalidations(
            &self,
        ) -> Result<tokio::sync::mpsc::Receiver<()>, ployz_gateway::GatewayError> {
            info!("gateway store call start: subscribe_routing_invalidations");
            match ployz_store_api::RoutingStore::subscribe_routing_invalidations(&self.0).await {
                Ok(rx) => {
                    info!("gateway store call complete: subscribe_routing_invalidations");
                    Ok(rx)
                }
                Err(err) => {
                    warn!(
                        error = %err,
                        "gateway store call failed: subscribe_routing_invalidations"
                    );
                    Err(ployz_gateway::GatewayError::Store(err.to_string()))
                }
            }
        }

        async fn list_certificates(
            &self,
        ) -> Result<Vec<ployz_types::model::CertificateRecord>, ployz_gateway::GatewayError>
        {
            info!("gateway store call start: list_certificates");
            match ployz_store_api::CertificateStore::list_certificates(&self.0).await {
                Ok(records) => {
                    info!(
                        certificates = records.len(),
                        "gateway store call complete: list_certificates"
                    );
                    Ok(records)
                }
                Err(err) => {
                    warn!(error = %err, "gateway store call failed: list_certificates");
                    Err(ployz_gateway::GatewayError::Store(err.to_string()))
                }
            }
        }

        async fn subscribe_certificates(
            &self,
        ) -> Result<
            (
                Vec<ployz_types::model::CertificateRecord>,
                tokio::sync::mpsc::Receiver<ployz_types::model::CertificateEvent>,
            ),
            ployz_gateway::GatewayError,
        > {
            info!("gateway store call start: subscribe_certificates");
            match ployz_store_api::CertificateStore::subscribe_certificates(&self.0).await {
                Ok((records, rx)) => {
                    info!(
                        certificates = records.len(),
                        "gateway store call complete: subscribe_certificates"
                    );
                    Ok((records, rx))
                }
                Err(err) => {
                    warn!(error = %err, "gateway store call failed: subscribe_certificates");
                    Err(ployz_gateway::GatewayError::Store(err.to_string()))
                }
            }
        }

        async fn list_acme_challenges(
            &self,
        ) -> Result<Vec<ployz_types::model::AcmeChallengeRecord>, ployz_gateway::GatewayError>
        {
            info!("gateway store call start: list_acme_challenges");
            match ployz_store_api::CertificateStore::list_acme_challenges(&self.0).await {
                Ok(records) => {
                    info!(
                        challenges = records.len(),
                        "gateway store call complete: list_acme_challenges"
                    );
                    Ok(records)
                }
                Err(err) => {
                    warn!(error = %err, "gateway store call failed: list_acme_challenges");
                    Err(ployz_gateway::GatewayError::Store(err.to_string()))
                }
            }
        }

        async fn subscribe_acme_challenges(
            &self,
        ) -> Result<
            (
                Vec<ployz_types::model::AcmeChallengeRecord>,
                tokio::sync::mpsc::Receiver<ployz_types::model::AcmeChallengeEvent>,
            ),
            ployz_gateway::GatewayError,
        > {
            info!("gateway store call start: subscribe_acme_challenges");
            match ployz_store_api::CertificateStore::subscribe_acme_challenges(&self.0).await {
                Ok((records, rx)) => {
                    info!(
                        challenges = records.len(),
                        "gateway store call complete: subscribe_acme_challenges"
                    );
                    Ok((records, rx))
                }
                Err(err) => {
                    warn!(
                        error = %err,
                        "gateway store call failed: subscribe_acme_challenges"
                    );
                    Err(ployz_gateway::GatewayError::Store(err.to_string()))
                }
            }
        }
    }
    ployz_gateway::run_gateway_process_on_runtime(runtime, config, StandaloneStore(store))
}

async fn probe_corrosion_startup(store: &ployz_corrosion::CorrosionStore) {
    match timeout(Duration::from_secs(5), store.client().health()).await {
        Ok(Ok(health)) => {
            info!(
                gaps = health.gaps,
                members = health.members,
                "gateway corrosion probe complete: health"
            );
        }
        Ok(Err(err)) => {
            warn!(error = %err, "gateway corrosion probe failed: health");
        }
        Err(_) => {
            warn!("gateway corrosion probe timed out: health");
        }
    }

    match timeout(
        Duration::from_secs(5),
        ployz_store_api::RoutingStore::load_routing_state(store),
    )
    .await
    {
        Ok(Ok(state)) => {
            info!(
                revisions = state.revisions.len(),
                releases = state.releases.len(),
                instances = state.instances.len(),
                "gateway corrosion probe complete: load_routing_state"
            );
        }
        Ok(Err(err)) => {
            warn!(error = %err, "gateway corrosion probe failed: load_routing_state");
        }
        Err(_) => {
            warn!("gateway corrosion probe timed out: load_routing_state");
        }
    }
}
