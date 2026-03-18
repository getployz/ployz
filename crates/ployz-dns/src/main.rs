fn main() -> Result<(), ployz_dns::DnsError> {
    tracing_subscriber::fmt::init();
    let config = ployz_dns::DnsConfig::from_env()?;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| ployz_dns::DnsError::Runtime(err.to_string()))?;
    let store = runtime.block_on(async {
        ployz_corrosion::CorrosionStore::connect_for_network(&config.data_dir, &config.network)
            .await
            .map_err(|err| ployz_dns::DnsError::Store(err.to_string()))
    })?;
    struct StandaloneStore(ployz_corrosion::CorrosionStore);
    impl ployz_dns::DnsStore for StandaloneStore {
        async fn load_routing_state(
            &self,
        ) -> Result<ployz_types::model::RoutingState, ployz_dns::DnsError> {
            ployz_store_api::RoutingStore::load_routing_state(&self.0)
                .await
                .map_err(|err| ployz_dns::DnsError::Store(err.to_string()))
        }

        async fn subscribe_routing_invalidations(
            &self,
        ) -> Result<tokio::sync::mpsc::Receiver<()>, ployz_dns::DnsError> {
            ployz_store_api::RoutingStore::subscribe_routing_invalidations(&self.0)
                .await
                .map_err(|err| ployz_dns::DnsError::Store(err.to_string()))
        }
    }
    ployz_dns::run_dns_process_with_store(config, StandaloneStore(store))
}
