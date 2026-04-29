fn main() -> Result<(), ployz_dns::DnsError> {
    tracing_subscriber::fmt::init();
    ployz_metrics::set_build_info("ployz-dns", env!("CARGO_PKG_VERSION"));
    let config = ployz_dns::DnsConfig::from_env()?;
    // One runtime for the whole process — the Corrosion reqwest client's
    // HTTP/2 connection driver is pinned to the runtime that opens it, so
    // the store must not cross runtimes.
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .thread_name("ployz-dns-async")
        .worker_threads(2)
        .build()
        .map_err(|err| ployz_dns::DnsError::Runtime(err.to_string()))?;
    let store = runtime.block_on(async {
        ployz_corrosion::CorrosionStore::connect_for_network(&config.data_dir, &config.network)
            .await
            .map_err(|err| ployz_dns::DnsError::Store(err.to_string()))
    })?;
    struct StandaloneStore(ployz_corrosion::CorrosionStore);
    impl ployz_dns::DnsStore for StandaloneStore {
        async fn subscribe_routing_events(
            &self,
        ) -> Result<
            (
                ployz_types::model::RoutingState,
                tokio::sync::mpsc::Receiver<ployz_types::model::RoutingEvent>,
            ),
            ployz_dns::DnsError,
        > {
            ployz_store_api::RoutingSnapshotReader::subscribe_routing_events(&self.0)
                .await
                .map_err(|err| ployz_dns::DnsError::Store(err.to_string()))
        }
    }
    ployz_dns::run_dns_process_on_runtime(runtime, config, StandaloneStore(store))
}
