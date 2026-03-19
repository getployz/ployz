use std::path::Path;
use std::sync::Arc;

use ployz_config::{RuntimeTarget, ServiceMode};
use ployz_runtime_api::Identity;
use tokio::sync::{RwLock, mpsc};
use tokio_util::sync::CancellationToken;

use crate::built_in_images::BuiltInImages;
use crate::daemon::handlers::RequestLane;
use crate::daemon::{ActiveMesh, DaemonRuntimeConfig, DaemonState};
use crate::ipc::listener::{IncomingCommand, serve};

const SUBNET_HEAL_INTERVAL: tokio::time::Duration = tokio::time::Duration::from_secs(5);

pub fn init_tracing() {
    let _ = tracing_subscriber::fmt::try_init();
}

pub fn run_gateway_process_from_env() -> Result<(), ployz_gateway::GatewayError> {
    init_tracing();
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

pub fn run_dns_process_from_env() -> Result<(), ployz_dns::DnsError> {
    init_tracing();
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

pub async fn run_daemon(
    data_dir: &Path,
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
    socket_path: &str,
    built_in_images: BuiltInImages,
    runtime: DaemonRuntimeConfig,
) -> Result<(), String> {
    tracing::info!(?runtime_target, ?service_mode, "starting daemon");

    let identity_path = data_dir.join("identity.json");
    let identity = Identity::load_or_generate(&identity_path)
        .map_err(|error| format!("load or generate identity: {error}"))?;
    tracing::info!(machine_id = %identity.machine_id, "loaded identity");

    let cancel = CancellationToken::new();
    let (command_tx, mut command_rx) = mpsc::channel::<IncomingCommand>(32);

    let listener_cancel = cancel.clone();
    let socket_owned = socket_path.to_owned();
    let listener_handle = tokio::spawn(async move {
        if let Err(error) = serve(&socket_owned, command_tx, listener_cancel).await {
            tracing::error!(?error, "socket listener failed");
        }
    });

    let ctrl_cancel = cancel.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        tracing::info!("received ctrl-c, shutting down");
        ctrl_cancel.cancel();
        tokio::signal::ctrl_c().await.ok();
        tracing::warn!("received second ctrl-c, forcing exit");
        std::process::exit(1);
    });

    let state = Arc::new(RwLock::new(DaemonState::new(
        data_dir,
        identity,
        runtime_target,
        service_mode,
        built_in_images,
        runtime,
    )));

    resume_active_network(&state).await;
    reconcile_startup_operations(&state).await;

    tracing::info!(socket = socket_path, "daemon running");
    spawn_subnet_heal_loop(Arc::clone(&state), cancel.clone());

    loop {
        tokio::select! {
            _ = cancel.cancelled() => break,
            Some(command) = command_rx.recv() => {
                spawn_command_task(Arc::clone(&state), cancel.clone(), command);
            }
        }
    }

    shutdown_active_mesh(&state).await;
    listener_handle.await.ok();
    tracing::info!("daemon stopped");
    Ok(())
}

async fn resume_active_network(state: &Arc<RwLock<DaemonState>>) {
    let active_marker = {
        let state_guard = state.read().await;
        state_guard.read_active_marker()
    };

    if let Some(network) = active_marker {
        tracing::info!(%network, "resuming network");
        let mut state_guard = state.write().await;
        match state_guard.start_mesh_by_name(&network).await {
            Ok(_) => tracing::info!(%network, "resumed network"),
            Err(error) => {
                tracing::warn!(%error, %network, "failed to resume network");
                state_guard.clear_active_marker();
            }
        }
    }
}

async fn reconcile_startup_operations(state: &Arc<RwLock<DaemonState>>) {
    let state_guard = state.read().await;
    state_guard.reconcile_machine_operations_on_startup().await;
}

fn spawn_subnet_heal_loop(state: Arc<RwLock<DaemonState>>, cancel: CancellationToken) {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(SUBNET_HEAL_INTERVAL);
        interval.tick().await;
        loop {
            tokio::select! {
                _ = cancel.cancelled() => break,
                _ = interval.tick() => {
                    let mut state_guard = state.write().await;
                    state_guard.heal_local_subnet_conflict_if_needed().await;
                }
            }
        }
    });
}

fn spawn_command_task(
    state: Arc<RwLock<DaemonState>>,
    cancel: CancellationToken,
    command: IncomingCommand,
) {
    tokio::spawn(async move {
        let response = tokio::select! {
            _ = cancel.cancelled() => ployz_api::DaemonResponse {
                ok: false,
                code: "SHUTDOWN".into(),
                message: "daemon shutting down".into(),
                payload: None,
            },
            response = async {
                match DaemonState::request_lane(&command.request) {
                    RequestLane::Shared => {
                        let state_guard = state.read_owned().await;
                        state_guard.handle_shared(command.request).await
                    }
                    RequestLane::Exclusive => {
                        let mut state_guard = state.write_owned().await;
                        state_guard.handle_exclusive(command.request).await
                    }
                }
            } => response,
        };
        let _ = command.reply.send(response);
    });
}

async fn shutdown_active_mesh(state: &Arc<RwLock<DaemonState>>) {
    let mut state = state.write().await;
    if let Some(active) = state.active.take() {
        let ActiveMesh {
            config: _config,
            mut mesh,
            store: _store,
            remote_control,
            gateway,
            dns,
        } = active;
        let _ = dns.detach().await;
        let _ = gateway.detach().await;
        let _ = remote_control.shutdown().await;
        let _ = mesh.detach().await;
    }
}
