use std::path::Path;
use std::sync::Arc;

use ployz_config::{RuntimeTarget, ServiceMode, load_or_generate_identity};
use tokio::sync::{RwLock, mpsc};
use tokio_util::sync::CancellationToken;

use crate::built_in_images::BuiltInImages;
use crate::daemon::handlers::RequestLane;
use crate::daemon::{ActiveMesh, DaemonRuntimeConfig, DaemonState};
use crate::ipc::listener::{IncomingCommand, serve, serve_tcp};

const SUBNET_HEAL_INTERVAL: tokio::time::Duration = tokio::time::Duration::from_secs(5);

pub fn init_tracing() {
    let _ = tracing_subscriber::fmt::try_init();
}

pub fn run_gateway_process_from_env() -> Result<(), ployz_gateway::GatewayError> {
    init_tracing();
    let config = ployz_gateway::GatewayConfig::from_env()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| ployz_gateway::GatewayError::Runtime(err.to_string()))?;
    runtime.block_on(async move {
        let store =
            ployz_corrosion::CorrosionStore::connect_for_network(&config.data_dir, &config.network)
                .await
                .map_err(|err| ployz_gateway::GatewayError::Store(err.to_string()))?;
        ployz_gateway::run_gateway_with_store(config, store).await
    })?;
    Ok(())
}

pub fn run_dns_process_from_env() -> Result<(), ployz_dns::DnsError> {
    init_tracing();
    let config = ployz_dns::DnsConfig::from_env()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|err| ployz_dns::DnsError::Runtime(err.to_string()))?;
    runtime.block_on(async move {
        let store =
            ployz_corrosion::CorrosionStore::connect_for_network(&config.data_dir, &config.network)
                .await
                .map_err(|err| ployz_dns::DnsError::Store(err.to_string()))?;
        ployz_dns::run_dns_with_store(config, store).await
    })?;
    Ok(())
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
    let identity = load_or_generate_identity(&identity_path)
        .map_err(|error| format!("load or generate identity: {error}"))?;
    tracing::info!(machine_id = %identity.machine_id, "loaded identity");

    let cancel = CancellationToken::new();
    let (command_tx, mut command_rx) = mpsc::channel::<IncomingCommand>(32);

    let coordination_rpc_port = runtime.coordination_rpc_port;

    let listener_cancel = cancel.clone();
    let socket_owned = socket_path.to_owned();
    let unix_command_tx = command_tx.clone();
    let listener_handle = tokio::spawn(async move {
        if let Err(error) = serve(&socket_owned, unix_command_tx, listener_cancel).await {
            tracing::error!(?error, "socket listener failed");
        }
    });

    let tcp_addr = std::net::SocketAddr::from(([0, 0, 0, 0], coordination_rpc_port));
    let tcp_listener_cancel = cancel.clone();
    let tcp_command_tx = command_tx.clone();
    let tcp_listener_handle = tokio::spawn(async move {
        if let Err(error) = serve_tcp(tcp_addr, tcp_command_tx, tcp_listener_cancel).await {
            tracing::error!(?error, "tcp rpc listener failed");
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
    tcp_listener_handle.await.ok();
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

#[cfg(test)]
mod tests {
    use super::*;
    use ployz_api::DaemonRequest;
    use ployz_types::model::{Identity, MachineId};
    use std::time::{SystemTime, UNIX_EPOCH};
    use tokio::sync::oneshot;

    #[tokio::test]
    async fn resume_active_network_clears_marker_when_resume_fails() {
        let data_dir = unique_temp_dir("ployz-app-resume");
        let identity = Identity::generate(MachineId("founder".into()), [7; 32]);
        let state = Arc::new(RwLock::new(DaemonState::new_for_tests(
            &data_dir,
            identity,
            runtime_config(),
        )));
        {
            let state_guard = state.read().await;
            state_guard
                .write_active_marker("missing-network")
                .expect("write marker");
        }

        resume_active_network(&state).await;

        let state_guard = state.read().await;
        assert_eq!(state_guard.read_active_marker(), None);
    }

    #[tokio::test]
    async fn spawn_command_task_routes_shared_requests_through_read_handler() {
        let data_dir = unique_temp_dir("ployz-app-shared");
        let identity = Identity::generate(MachineId("founder".into()), [8; 32]);
        let state = Arc::new(RwLock::new(DaemonState::new_for_tests(
            &data_dir,
            identity,
            runtime_config(),
        )));
        let cancel = CancellationToken::new();
        let (reply, response_rx) = oneshot::channel();

        spawn_command_task(
            state,
            cancel,
            IncomingCommand {
                request: DaemonRequest::Status,
                reply,
            },
        );

        let response = response_rx.await.expect("receive daemon response");
        assert!(response.ok);
        assert_eq!(response.code, "OK");
    }

    fn runtime_config() -> DaemonRuntimeConfig {
        DaemonRuntimeConfig {
            cluster_cidr: "10.210.0.0/16".into(),
            subnet_prefix_len: 24,
            remote_control_port: 4317,
            coordination_rpc_port: 0,
            gateway_listen_addr: "127.0.0.1:8080".into(),
            gateway_threads: 1,
        }
    }

    fn unique_temp_dir(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        std::env::temp_dir().join(format!("{label}-{}-{nanos}", std::process::id()))
    }
}
