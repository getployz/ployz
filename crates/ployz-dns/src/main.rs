use ployz_store_api::StoreRuntimeControl;

fn main() -> Result<(), ployz_dns::DnsError> {
    tracing_subscriber::fmt::init();
    ployz_metrics::set_build_info("ployz-dns", env!("CARGO_PKG_VERSION"));
    let config = ployz_dns::DnsConfig::from_env()?;
    // One runtime for the whole process so store futures and DNS tasks share
    // one async reactor.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("ployz-dns-async")
        .worker_threads(2)
        .build()
        .map_err(|err| ployz_dns::DnsError::Runtime(err.to_string()))?;
    let store = runtime.block_on(async {
        let store = ployz_nats::NatsStore::connect_for_network(&config.data_dir, &config.network)
            .await
            .map_err(|err| ployz_dns::DnsError::Store(err.to_string()))?;
        if !store.healthy().await {
            return Err(ployz_dns::DnsError::Store("nats store unhealthy".into()));
        }
        Ok::<_, ployz_dns::DnsError>(store)
    })?;
    struct StandaloneStore(ployz_nats::NatsStore);
    impl ployz_dns::DnsStore for StandaloneStore {
        async fn subscribe_routing_events(
            &self,
        ) -> Result<ployz_store_api::RoutingEventSubscription, ployz_dns::DnsError> {
            ployz_store_api::RoutingSnapshotReader::subscribe_routing_events(&self.0)
                .await
                .map_err(|err| ployz_dns::DnsError::Store(err.to_string()))
        }
    }
    ployz_dns::run_dns_process_on_runtime(runtime, config, StandaloneStore(store))
}
