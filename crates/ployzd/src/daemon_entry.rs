use crate::{CliError, Result, SUBNET_HEAL_INTERVAL};
use ployz_config::{RuntimeTarget, ServiceMode};
use ployz_runtime_api::Identity;
use ployzd::BuiltInImages;
use ployzd::daemon::ActiveMesh;
use ployzd::daemon::DaemonState;
use ployzd::daemon::handlers::RequestLane;
use ployzd::ipc::listener::IncomingCommand;
use ployzd::ipc::listener::serve;
use std::path::Path;
use std::sync::Arc;
use tokio::sync::{RwLock, mpsc};
use tokio_util::sync::CancellationToken;

pub(crate) fn init_tracing() {
    let _ = tracing_subscriber::fmt::try_init();
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn cmd_run(
    data_dir: &Path,
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
    socket_path: &str,
    built_in_images: BuiltInImages,
    cluster_cidr: String,
    subnet_prefix_len: u8,
    remote_control_port: u16,
    gateway_listen_addr: String,
    gateway_threads: usize,
) -> Result<()> {
    tracing::info!(?runtime_target, ?service_mode, "starting daemon");

    let identity_path = data_dir.join("identity.json");
    let identity = Identity::load_or_generate(&identity_path)
        .map_err(|error| CliError::Identity(error.to_string()))?;
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
        cluster_cidr,
        subnet_prefix_len,
        remote_control_port,
        gateway_listen_addr,
        gateway_threads,
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
