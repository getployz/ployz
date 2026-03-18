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
    ployz_gateway::run_gateway_process_with_store(config, store)
}
