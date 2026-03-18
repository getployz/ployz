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
    ployz_dns::run_dns_process_with_store(config, store)
}
