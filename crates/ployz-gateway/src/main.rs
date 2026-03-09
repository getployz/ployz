use std::path::{Path, PathBuf};

use clap::Parser;
use pingora::prelude::*;
use ployz::config::{Affordances, default_data_dir};
use ployz::NetworkConfig;
use ployz_gateway::{
    GatewayApp, GatewayConfig, SharedSnapshot, load_initial_snapshot, spawn_sync_thread,
};
use thiserror::Error;
use tracing::info;

const HTTP_LISTEN_ADDR: &str = "0.0.0.0:80";

#[derive(Parser, Debug, Clone)]
#[command(name = "ployz-gateway", about = "Ployz HTTP gateway")]
struct Cli {
    #[arg(long)]
    data_dir: Option<PathBuf>,
    #[arg(long)]
    network: Option<String>,
}

#[derive(Debug, Error)]
enum CliError {
    #[error("failed to build gateway config: {0}")]
    Config(String),
    #[error("gateway bootstrap failed: {0}")]
    Bootstrap(String),
}

fn main() -> std::result::Result<(), CliError> {
    tracing_subscriber::fmt::init();
    let cli = Cli::parse();
    let gateway_config = build_gateway_config(&cli)?;
    let initial_snapshot = load_initial_snapshot(&gateway_config)
        .map_err(|err| CliError::Bootstrap(err.to_string()))?;
    let shared_snapshot = SharedSnapshot::new(initial_snapshot);
    spawn_sync_thread(gateway_config.clone(), shared_snapshot.clone())
        .map_err(|err| CliError::Bootstrap(err.to_string()))?;

    let mut server =
        Server::new(None).map_err(|err| CliError::Bootstrap(format!("server init: {err}")))?;
    server.bootstrap();

    let mut service = http_proxy_service(&server.configuration, GatewayApp::new(shared_snapshot));
    service.add_tcp(HTTP_LISTEN_ADDR);
    server.add_service(service);

    info!(listen = HTTP_LISTEN_ADDR, "gateway listening");
    server.run_forever()
}

fn build_gateway_config(cli: &Cli) -> std::result::Result<GatewayConfig, CliError> {
    let data_dir = match &cli.data_dir {
        Some(data_dir) => data_dir.clone(),
        None => default_data_dir(&Affordances::detect()),
    };
    let network = match &cli.network {
        Some(network) => network.clone(),
        None => resolve_active_network(&data_dir)
            .ok_or_else(|| CliError::Config("no active network marker was found".into()))?,
    };
    Ok(GatewayConfig { data_dir, network })
}

fn resolve_active_network(data_dir: &Path) -> Option<String> {
    NetworkConfig::read_active_network(data_dir)
}
