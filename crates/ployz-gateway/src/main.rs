fn main() -> Result<(), ployz_gateway::GatewayError> {
    tracing_subscriber::fmt::init();
    let config = ployz_gateway::GatewayConfig::from_env()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| ployz_gateway::GatewayError::Runtime(err.to_string()))?;
    let store = runtime.block_on(async {
        ployz_corrosion::CorrosionStore::connect_for_network(&config.data_dir, &config.network)
            .await
            .map_err(|err| ployz_gateway::GatewayError::Store(err.to_string()))
    })?;
    struct StandaloneStore(ployz_corrosion::CorrosionStore);
    impl ployz_gateway::RoutingStore for StandaloneStore {
        async fn load_routing_state(
            &self,
        ) -> Result<ployz_types::model::RoutingState, ployz_gateway::GatewayError> {
            ployz_store_api::RoutingStore::load_routing_state(&self.0)
                .await
                .map_err(|err| ployz_gateway::GatewayError::Store(err.to_string()))
        }

        async fn subscribe_routing_invalidations(
            &self,
        ) -> Result<ployz_store_api::RoutingInvalidationSubscription, ployz_gateway::GatewayError>
        {
            ployz_store_api::RoutingStore::subscribe_routing_invalidations(&self.0)
                .await
                .map_err(|err| ployz_gateway::GatewayError::Store(err.to_string()))
        }
    }
    ployz_gateway::run_gateway_process_with_store(config, StandaloneStore(store))
}
