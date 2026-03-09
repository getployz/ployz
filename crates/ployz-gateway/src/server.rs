use std::sync::Arc;

use async_trait::async_trait;
use pingora::prelude::*;
#[cfg(unix)]
use pingora::server::{RunArgs, ShutdownSignal, ShutdownSignalWatch};
use tokio::sync::{Mutex as AsyncMutex, oneshot};
use tracing::{info, warn};

use crate::config::{GatewayConfig, GatewayError};
use crate::proxy::GatewayApp;
use crate::snapshot::SharedSnapshot;
use crate::sync::{load_projected_snapshot_from_store, run_sync_loop};

// ---------------------------------------------------------------------------
// EmbeddedShutdownWatch
// ---------------------------------------------------------------------------

#[cfg(unix)]
pub struct EmbeddedShutdownWatch {
    receiver: AsyncMutex<Option<oneshot::Receiver<()>>>,
}

#[cfg(unix)]
impl EmbeddedShutdownWatch {
    #[must_use]
    pub fn new(receiver: oneshot::Receiver<()>) -> Self {
        Self {
            receiver: AsyncMutex::new(Some(receiver)),
        }
    }
}

#[cfg(unix)]
#[async_trait]
impl ShutdownSignalWatch for EmbeddedShutdownWatch {
    async fn recv(&self) -> ShutdownSignal {
        let mut guard = self.receiver.lock().await;
        let Some(receiver) = guard.take() else {
            return ShutdownSignal::FastShutdown;
        };
        let _ = receiver.await;
        ShutdownSignal::GracefulTerminate
    }
}

// ---------------------------------------------------------------------------
// Server bootstrap
// ---------------------------------------------------------------------------

pub fn run_server(
    opt: Opt,
    listen_addr: &str,
    threads: usize,
    shared_snapshot: SharedSnapshot,
    #[cfg(unix)] shutdown_signal: Option<Box<dyn ShutdownSignalWatch>>,
    #[cfg(not(unix))] _shutdown_signal: Option<()>,
) -> Result<(), GatewayError> {
    let mut server = Server::new(Some(opt))
        .map_err(|err| GatewayError::Runtime(format!("server init: {err}")))?;
    let Some(configuration) = Arc::get_mut(&mut server.configuration) else {
        return Err(GatewayError::Runtime(
            "server configuration was unexpectedly shared".into(),
        ));
    };
    configuration.threads = threads;
    server.bootstrap();

    let mut service =
        http_proxy_service(&server.configuration, GatewayApp::new(shared_snapshot));
    service.add_tcp(listen_addr);
    server.add_service(service);

    info!(listen = listen_addr, threads, "gateway listening");
    #[cfg(unix)]
    if let Some(shutdown_signal) = shutdown_signal {
        server.run(RunArgs { shutdown_signal });
        return Ok(());
    }
    server.run_forever()
}

// ---------------------------------------------------------------------------
// Standalone process entry point
// ---------------------------------------------------------------------------

pub fn run_gateway_process() -> Result<(), GatewayError> {
    let config = GatewayConfig::from_env()?;
    let initial_snapshot = load_initial_snapshot(&config)?;
    let shared_snapshot = SharedSnapshot::new(initial_snapshot);
    spawn_standalone_sync_thread(config.clone(), shared_snapshot.clone())?;
    let opt = Opt::parse_args();
    run_server(
        opt,
        config.listen_addr.as_str(),
        config.threads,
        shared_snapshot,
        None,
    )
}

fn load_initial_snapshot(
    config: &GatewayConfig,
) -> Result<ployz_routes::GatewaySnapshot, GatewayError> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|err| GatewayError::Runtime(err.to_string()))?;
    runtime.block_on(async {
        let store = ployz_corrosion::CorrosionStore::connect_for_network(
            &config.data_dir,
            &config.network,
        )
        .await
        .map_err(|err| GatewayError::Store(err.to_string()))?;
        load_projected_snapshot_from_store(&store).await
    })
}

fn spawn_standalone_sync_thread(
    config: GatewayConfig,
    snapshot: SharedSnapshot,
) -> Result<(), GatewayError> {
    std::thread::Builder::new()
        .name("ployz-gateway-sync".into())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(err) => {
                    warn!(?err, "failed to create gateway sync runtime");
                    return;
                }
            };
            runtime.block_on(async move {
                let store = match ployz_corrosion::CorrosionStore::connect_for_network(
                    &config.data_dir,
                    &config.network,
                )
                .await
                {
                    Ok(store) => store,
                    Err(err) => {
                        warn!(?err, "failed to connect gateway routing store");
                        return;
                    }
                };
                if let Err(err) = run_sync_loop(store, snapshot).await {
                    warn!(?err, "gateway sync loop exited");
                }
            });
        })
        .map_err(|err| GatewayError::Runtime(err.to_string()))?;
    Ok(())
}
