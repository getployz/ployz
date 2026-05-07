use std::time::Duration;

use ployz_store_api::{RoutingSnapshotReader, StoreRuntimeControl};
use tokio::time::timeout;
use tracing::{info, warn};

fn main() -> Result<(), ployz_gateway::GatewayError> {
    tracing_subscriber::fmt::init();
    ployz_metrics::set_build_info("ployz-gateway", env!("CARGO_PKG_VERSION"));
    let config = ployz_gateway::GatewayConfig::from_env()?;
    // Single runtime for the process so store futures and server tasks share
    // one async reactor.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("ployz-gateway-async")
        .worker_threads(2)
        .build()
        .map_err(|err| ployz_gateway::GatewayError::Runtime(err.to_string()))?;
    let store = runtime.block_on(async {
        let store = ployz_nats::NatsStore::connect_for_network(&config.data_dir, &config.network)
            .await
            .map_err(|err| ployz_gateway::GatewayError::Store(err.to_string()))?;
        probe_nats_startup(&store).await;
        Ok::<_, ployz_gateway::GatewayError>(store)
    })?;
    struct StandaloneStore(ployz_nats::NatsStore);
    impl ployz_gateway::RoutingSnapshotReader for StandaloneStore {
        async fn load_routing_state(
            &self,
        ) -> Result<ployz_types::model::RoutingState, ployz_gateway::GatewayError> {
            info!("gateway store call start: load_routing_state");
            match self.0.load_routing_state().await {
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

        async fn subscribe_routing_events(
            &self,
        ) -> Result<ployz_store_api::RoutingEventSubscription, ployz_gateway::GatewayError>
        {
            info!("gateway store call start: subscribe_routing_events");
            match ployz_store_api::RoutingSnapshotReader::subscribe_routing_events(&self.0).await {
                Ok((state, rx)) => {
                    info!(
                        revisions = state.revisions.len(),
                        releases = state.releases.len(),
                        instances = state.instances.len(),
                        "gateway store call complete: subscribe_routing_events"
                    );
                    Ok((state, rx))
                }
                Err(err) => {
                    warn!(
                        error = %err,
                        "gateway store call failed: subscribe_routing_events"
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
                tokio::sync::mpsc::Receiver<ployz_store_api::CertificateSubscriptionUpdate>,
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
                tokio::sync::mpsc::Receiver<ployz_store_api::AcmeChallengeSubscriptionUpdate>,
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

        async fn upsert_acme_challenge_readiness(
            &self,
            record: &ployz_types::model::AcmeChallengeReadinessRecord,
        ) -> Result<(), ployz_gateway::GatewayError> {
            info!(
                hostname = %record.hostname,
                token = %record.token,
                machine_id = %record.machine_id,
                "gateway store call start: upsert_acme_challenge_readiness"
            );
            match ployz_store_api::CertificateStore::upsert_acme_challenge_readiness(
                &self.0, record,
            )
            .await
            {
                Ok(()) => {
                    info!(
                        hostname = %record.hostname,
                        token = %record.token,
                        machine_id = %record.machine_id,
                        "gateway store call complete: upsert_acme_challenge_readiness"
                    );
                    Ok(())
                }
                Err(err) => {
                    warn!(
                        error = %err,
                        "gateway store call failed: upsert_acme_challenge_readiness"
                    );
                    Err(ployz_gateway::GatewayError::Store(err.to_string()))
                }
            }
        }
    }
    ployz_gateway::run_gateway_process_on_runtime(runtime, config, StandaloneStore(store))
}

async fn probe_nats_startup(store: &ployz_nats::NatsStore) {
    match timeout(Duration::from_secs(5), store.healthy()).await {
        Ok(true) => info!("gateway nats probe complete: healthy"),
        Ok(false) => warn!("gateway nats probe failed: unhealthy"),
        Err(_) => warn!("gateway nats probe timed out: health"),
    };

    match timeout(Duration::from_secs(5), store.load_routing_state()).await {
        Ok(Ok(state)) => {
            info!(
                revisions = state.revisions.len(),
                releases = state.releases.len(),
                instances = state.instances.len(),
                "gateway nats probe complete: load_routing_state"
            );
        }
        Ok(Err(err)) => {
            warn!(error = %err, "gateway nats probe failed: load_routing_state");
        }
        Err(_) => {
            warn!("gateway nats probe timed out: load_routing_state");
        }
    }
}
