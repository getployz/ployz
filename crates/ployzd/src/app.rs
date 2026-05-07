use std::path::Path;
use std::sync::Arc;

use ployz_config::{RuntimeTarget, ServiceMode, StorageConfig};
use ployz_metrics::{set_build_info, spawn_metrics_listener};
use ployz_runtime_api::Identity;
use tokio::sync::{RwLock, mpsc};
use tokio_util::sync::CancellationToken;

use crate::built_in_images::BuiltInImages;
use crate::daemon::handlers::RequestLane;
use crate::daemon::{ActiveMesh, DaemonState};
use crate::endpoint_maintenance::spawn_local_endpoint_maintenance;
use crate::ipc::listener::{IncomingCommand, serve};
use crate::mesh_state::network::NetworkConfig;
use crate::metrics::{
    ContainerResourceMetricsSource, DockerContainerResourceMetricsSource,
    spawn_container_resource_metrics_loop,
};
use ployz_types::model::MachineTopology;
use ployz_types::model::{
    NetworkLifecycle, NetworkLifecycleGoal, NetworkLifecycleTransition, NetworkTransitionEvidence,
};

pub fn init_tracing() {
    let _ = tracing_subscriber::fmt::try_init();
}

#[allow(clippy::too_many_arguments)]
pub async fn run_daemon(
    data_dir: &Path,
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
    socket_path: &str,
    built_in_images: BuiltInImages,
    storage: StorageConfig,
    cluster_cidr: String,
    subnet_prefix_len: u8,
    zfs_transfer_port: u16,
    gateway_listen_addr: String,
    gateway_https_listen_addr: Option<String>,
    gateway_threads: usize,
    configured_topology: Option<MachineTopology>,
    daemon_metrics_listen_addr: Option<String>,
    dns_metrics_listen_addr: Option<String>,
    gateway_metrics_listen_addr: Option<String>,
) -> Result<(), String> {
    run_daemon_inner(
        data_dir,
        runtime_target,
        service_mode,
        socket_path,
        built_in_images,
        storage,
        cluster_cidr,
        subnet_prefix_len,
        zfs_transfer_port,
        gateway_listen_addr,
        gateway_https_listen_addr,
        gateway_threads,
        configured_topology,
        daemon_metrics_listen_addr,
        dns_metrics_listen_addr,
        gateway_metrics_listen_addr,
        None,
    )
    .await
}

#[cfg(test)]
#[allow(clippy::too_many_arguments)]
async fn run_daemon_with_resource_metrics_source(
    data_dir: &Path,
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
    socket_path: &str,
    built_in_images: BuiltInImages,
    storage: StorageConfig,
    cluster_cidr: String,
    subnet_prefix_len: u8,
    zfs_transfer_port: u16,
    gateway_listen_addr: String,
    gateway_https_listen_addr: Option<String>,
    gateway_threads: usize,
    configured_topology: Option<MachineTopology>,
    daemon_metrics_listen_addr: Option<String>,
    dns_metrics_listen_addr: Option<String>,
    gateway_metrics_listen_addr: Option<String>,
    resource_metrics_source: Arc<dyn ContainerResourceMetricsSource>,
) -> Result<(), String> {
    run_daemon_inner(
        data_dir,
        runtime_target,
        service_mode,
        socket_path,
        built_in_images,
        storage,
        cluster_cidr,
        subnet_prefix_len,
        zfs_transfer_port,
        gateway_listen_addr,
        gateway_https_listen_addr,
        gateway_threads,
        configured_topology,
        daemon_metrics_listen_addr,
        dns_metrics_listen_addr,
        gateway_metrics_listen_addr,
        Some(resource_metrics_source),
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn run_daemon_inner(
    data_dir: &Path,
    runtime_target: RuntimeTarget,
    service_mode: ServiceMode,
    socket_path: &str,
    built_in_images: BuiltInImages,
    storage: StorageConfig,
    cluster_cidr: String,
    subnet_prefix_len: u8,
    zfs_transfer_port: u16,
    gateway_listen_addr: String,
    gateway_https_listen_addr: Option<String>,
    gateway_threads: usize,
    configured_topology: Option<MachineTopology>,
    daemon_metrics_listen_addr: Option<String>,
    dns_metrics_listen_addr: Option<String>,
    gateway_metrics_listen_addr: Option<String>,
    resource_metrics_source: Option<Arc<dyn ContainerResourceMetricsSource>>,
) -> Result<(), String> {
    tracing::info!(?runtime_target, ?service_mode, "starting daemon");
    set_build_info("ployzd", env!("CARGO_PKG_VERSION"));

    let identity_path = data_dir.join("identity.json");
    let identity = Identity::load_or_generate(&identity_path)
        .map_err(|error| format!("load or generate identity: {error}"))?;
    tracing::info!(machine_id = %identity.machine_id, "loaded identity");

    let cancel = CancellationToken::new();
    let (command_tx, mut command_rx) = mpsc::channel::<IncomingCommand>(32);

    let listener_cancel = cancel.clone();
    let socket_owned = socket_path.to_owned();
    let listener_command_tx = command_tx.clone();
    let listener_handle = tokio::spawn(async move {
        if let Err(error) = serve(&socket_owned, listener_command_tx, listener_cancel).await {
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

    let mut daemon_state = DaemonState::new(
        data_dir,
        identity,
        runtime_target,
        service_mode,
        storage,
        built_in_images,
        cluster_cidr,
        subnet_prefix_len,
        zfs_transfer_port,
        gateway_listen_addr,
        gateway_https_listen_addr,
        gateway_threads,
        configured_topology,
        dns_metrics_listen_addr,
        gateway_metrics_listen_addr,
    );
    daemon_state.command_tx = Some(command_tx.clone());
    let state = Arc::new(RwLock::new(daemon_state));
    spawn_local_endpoint_maintenance(Arc::clone(&state), cancel.clone());

    if let Some(metrics_listen_addr) = daemon_metrics_listen_addr.as_deref() {
        let metrics_addr = spawn_metrics_listener(metrics_listen_addr)
            .await
            .map_err(|error| {
                format!("start daemon metrics listener on {metrics_listen_addr}: {error}")
            })?;
        tracing::info!(listen = %metrics_addr, "daemon metrics listener running");

        let should_spawn_resource_metrics = {
            let state_guard = state.read().await;
            !state_guard.runtime_is_memory_test()
        };
        let resource_metrics_source = match resource_metrics_source {
            Some(source) => Some(source),
            None if should_spawn_resource_metrics => {
                Some(Arc::new(DockerContainerResourceMetricsSource) as Arc<_>)
            }
            None => None,
        };
        if let Some(resource_metrics_source) = resource_metrics_source {
            spawn_container_resource_metrics_loop(cancel.clone(), resource_metrics_source);
        }
    }

    resume_running_network(&state).await;
    reconcile_startup_operations(&state).await;

    tracing::info!(socket = socket_path, "daemon running");

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

async fn resume_running_network(state: &Arc<RwLock<DaemonState>>) {
    let data_dir = {
        let state_guard = state.read().await;
        state_guard.data_dir.clone()
    };
    let networks_dir = data_dir.join("networks");
    let entries = match std::fs::read_dir(&networks_dir) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
        Err(error) => {
            tracing::warn!(?error, path = %networks_dir.display(), "failed to scan network configs during startup resume");
            return;
        }
    };

    let mut running = Vec::new();
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(ToOwned::to_owned) else {
            continue;
        };
        let path = NetworkConfig::path(&data_dir, &name);
        match NetworkConfig::load(&path) {
            Ok(config) if config.lifecycle == NetworkLifecycle::Running => running.push(config),
            Ok(_) => {}
            Err(error) => {
                tracing::warn!(?error, path = %path.display(), "failed to load network config during startup resume");
            }
        }
    }

    let [config] = running.as_slice() else {
        if running.len() > 1 {
            let names = running
                .iter()
                .map(|config| config.name.0.as_str())
                .collect::<Vec<_>>();
            tracing::error!(
                ?names,
                "multiple network configs are marked running; refusing automatic resume"
            );
        }
        return;
    };

    let network = config.name.0.clone();
    tracing::info!(%network, "resuming network from persisted lifecycle");
    let mut state_guard = state.write().await;
    match state_guard.start_mesh_by_name(&network).await {
        Ok(_) => tracing::info!(%network, "resumed network"),
        Err(error) => {
            tracing::warn!(%error, %network, "failed to resume network");
            let mut stopped = config.clone();
            let stopped_network_name = stopped.name.clone();
            let _ = stopped
                .lifecycle
                .apply_transition(NetworkLifecycleTransition {
                    goal: NetworkLifecycleGoal::Stop,
                    evidence: NetworkTransitionEvidence::StartupResumeFailure {
                        network: stopped_network_name,
                    },
                    at_unix_secs: ployz_types::time::now_unix_secs(),
                });
            let path = NetworkConfig::path(&state_guard.data_dir, &network);
            if let Err(save_error) = stopped.save(&path) {
                tracing::warn!(?save_error, %network, "failed to persist stopped lifecycle after resume failure");
            }
        }
    }
}

async fn reconcile_startup_operations(state: &Arc<RwLock<DaemonState>>) {
    let state_guard = state.read().await;
    state_guard.reconcile_machine_operations_on_startup().await;
    state_guard.reconcile_zfs_transfers_on_startup().await;
}

fn spawn_command_task(
    state: Arc<RwLock<DaemonState>>,
    cancel: CancellationToken,
    mut command: IncomingCommand,
) {
    tokio::spawn(async move {
        let request_name = crate::metrics::request_name(&command.request);
        if matches!(command.request, ployz_api::DaemonRequest::RuntimeSubscribe) {
            let started_at = std::time::Instant::now();
            let Some(stream) = command.stream.take() else {
                let response = ployz_api::DaemonResponse {
                    ok: false,
                    code: "INTERNAL".into(),
                    message: "runtime stream channel missing".into(),
                    payload: None,
                };
                crate::metrics::observe_request(
                    request_name,
                    RequestLane::Shared,
                    response.ok,
                    started_at.elapsed(),
                );
                let _ = command.reply.send(response);
                return;
            };

            let opened = {
                let state_guard = state.read_owned().await;
                state_guard.open_runtime_subscription().await
            };

            let (initial, events) = match opened {
                Ok(subscription) => subscription,
                Err(response) => {
                    crate::metrics::observe_request(
                        request_name,
                        RequestLane::Shared,
                        response.ok,
                        started_at.elapsed(),
                    );
                    let _ = command.reply.send(*response);
                    return;
                }
            };

            let response = ployz_api::DaemonResponse {
                ok: true,
                code: "OK".into(),
                message: "runtime stream opened".into(),
                payload: None,
            };
            let ok = response.ok;
            if command.reply.send(response).is_err() {
                crate::metrics::observe_request(
                    request_name,
                    RequestLane::Shared,
                    false,
                    started_at.elapsed(),
                );
                return;
            }

            crate::daemon::handlers::runtime::stream_runtime_frames(
                initial, events, stream, cancel,
            )
            .await;
            crate::metrics::observe_request(
                request_name,
                RequestLane::Shared,
                ok,
                started_at.elapsed(),
            );
            return;
        }

        let lane = DaemonState::request_lane(&command.request);
        let started_at = std::time::Instant::now();
        let response_flushed = command.response_flushed.take();
        let response = tokio::select! {
            _ = cancel.cancelled() => ployz_api::DaemonResponse {
                ok: false,
                code: "SHUTDOWN".into(),
                message: "daemon shutting down".into(),
                payload: None,
            },
            response = async {
                match lane {
                    RequestLane::Shared => {
                        let state_guard = state.read_owned().await;
                        state_guard.handle_shared(command.request).await
                    }
                    RequestLane::Exclusive => {
                        let mut state_guard = state.write_owned().await;
                        state_guard
                            .handle_exclusive(command.request, response_flushed)
                            .await
                    }
                }
            } => response,
        };
        crate::metrics::observe_request(request_name, lane, response.ok, started_at.elapsed());
        let _ = command.reply.send(response);
    });
}

async fn shutdown_active_mesh(state: &Arc<RwLock<DaemonState>>) {
    let mut state = state.write().await;
    if let Some(active) = state.active.take() {
        let ActiveMesh {
            config: _config,
            cached_subnet: _cached_subnet,
            mut mesh,
            nats_control,
            zfs_transfer,
            gateway,
            dns,
            certificate_renewal,
            bootstrap_seed_cache,
        } = active;
        if let Some(task) = certificate_renewal {
            task.shutdown().await;
        }
        if let Some(task) = bootstrap_seed_cache {
            task.shutdown().await;
        }
        let _ = dns.detach().await;
        let _ = gateway.detach().await;
        let _ = zfs_transfer.shutdown().await;
        let _ = nats_control.shutdown().await;
        let _ = mesh.detach().await;
    }
}

#[cfg(test)]
mod tests {
    use super::{run_daemon, run_daemon_with_resource_metrics_source};
    use crate::BuiltInImages;
    use crate::metrics::ContainerResourceMetricsSource;
    use async_trait::async_trait;
    use ployz_api::DaemonRequest;
    use ployz_config::{RuntimeTarget, ServiceMode, StorageConfig};
    use ployz_runtime_backends::runtime::WorkloadResourceSnapshot;
    use ployz_sdk::{Transport, UnixSocketTransport};
    use std::path::PathBuf;
    use std::sync::Arc;
    use std::time::{Duration, SystemTime, UNIX_EPOCH};
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::net::{TcpStream, UnixStream};

    #[tokio::test]
    async fn daemon_metrics_listener_reports_shared_and_exclusive_requests() {
        let data_dir = unique_temp_dir("ployz-daemon-metrics");
        let socket_path = data_dir.join("ployzd.sock");
        let socket_string = socket_path
            .to_str()
            .expect("socket path should be utf-8")
            .to_string();
        let metrics_addr = free_local_addr();

        let mut daemon = tokio::spawn(async move {
            run_daemon(
                &data_dir,
                RuntimeTarget::Host,
                ServiceMode::User,
                &socket_string,
                BuiltInImages::load(None).expect("built-in images should load"),
                StorageConfig::default(),
                "10.210.0.0/16".into(),
                24,
                4319,
                "127.0.0.1:8080".into(),
                None,
                1,
                None,
                Some(metrics_addr.to_string()),
                None,
                None,
            )
            .await
        });

        wait_for_unix_socket_or_daemon_exit(&socket_path, &mut daemon).await;

        let transport = UnixSocketTransport::new(
            socket_path
                .to_str()
                .expect("socket path should be utf-8")
                .to_string(),
        );
        let status_response = transport
            .request(DaemonRequest::Status)
            .await
            .expect("status request should succeed");
        assert!(status_response.ok);

        let mesh_down_response = transport
            .request(DaemonRequest::MeshStop { force: false })
            .await
            .expect("mesh down request should return a response");
        assert!(!mesh_down_response.ok);

        let metrics = fetch_http_body(metrics_addr, "/metrics").await;
        assert!(metrics.contains(
            "ployz_daemon_requests_total{lane=\"shared\",outcome=\"ok\",request=\"status\"}"
        ));
        assert!(metrics.contains(
            "ployz_daemon_requests_total{lane=\"exclusive\",outcome=\"error\",request=\"mesh_stop\"}"
        ));
        assert!(metrics.contains(
            "ployz_daemon_request_duration_seconds_count{lane=\"shared\",outcome=\"ok\",request=\"status\"}"
        ));
        assert!(metrics.contains(
            "ployz_daemon_request_duration_seconds_count{lane=\"exclusive\",outcome=\"error\",request=\"mesh_stop\"}"
        ));

        daemon.abort();
        let _ = daemon.await;
    }

    #[tokio::test]
    async fn daemon_metrics_listener_reports_container_resource_metrics() {
        let data_dir = unique_temp_dir("ployz-daemon-rsrc");
        let socket_path = data_dir.join("ployzd.sock");
        let socket_string = socket_path
            .to_str()
            .expect("socket path should be utf-8")
            .to_string();
        let metrics_addr = free_local_addr();
        let prefix = format!(
            "resource-metrics-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("time after epoch")
                .as_nanos()
        );
        let fake_source = Arc::new(StaticResourceMetricsSource {
            snapshots: vec![WorkloadResourceSnapshot {
                namespace: format!("{prefix}-ns"),
                service: format!("{prefix}-service"),
                instance_id: format!("{prefix}-instance"),
                machine_id: format!("{prefix}-machine"),
                cpu_total_seconds: 7.5,
                memory_usage_bytes: 32 * 1024 * 1024,
                memory_limit_bytes: 64 * 1024 * 1024,
            }],
        });

        let mut daemon = tokio::spawn(async move {
            run_daemon_with_resource_metrics_source(
                &data_dir,
                RuntimeTarget::Host,
                ServiceMode::User,
                &socket_string,
                BuiltInImages::load(None).expect("built-in images should load"),
                StorageConfig::default(),
                "10.210.0.0/16".into(),
                24,
                4319,
                "127.0.0.1:8080".into(),
                None,
                1,
                None,
                Some(metrics_addr.to_string()),
                None,
                None,
                fake_source,
            )
            .await
        });

        wait_for_unix_socket_or_daemon_exit(&socket_path, &mut daemon).await;

        let transport = UnixSocketTransport::new(
            socket_path
                .to_str()
                .expect("socket path should be utf-8")
                .to_string(),
        );
        let status_response = transport
            .request(DaemonRequest::Status)
            .await
            .expect("status request should succeed");
        assert!(status_response.ok);

        let metrics = fetch_http_body(metrics_addr, "/metrics").await;
        assert!(metrics.contains(&format!(
            "ployz_container_cpu_usage_seconds_total{{instance_id=\"{prefix}-instance\",machine_id=\"{prefix}-machine\",namespace=\"{prefix}-ns\",service=\"{prefix}-service\"}} 7.5"
        )));
        assert!(metrics.contains(&format!(
            "ployz_container_memory_usage_bytes{{instance_id=\"{prefix}-instance\",machine_id=\"{prefix}-machine\",namespace=\"{prefix}-ns\",service=\"{prefix}-service\"}} 33554432"
        )));
        assert!(metrics.contains(&format!(
            "ployz_container_cpu_usage_cores{{instance_id=\"{prefix}-instance\",machine_id=\"{prefix}-machine\",namespace=\"{prefix}-ns\",service=\"{prefix}-service\"}} 0"
        )));

        daemon.abort();
        let _ = daemon.await;
    }

    async fn wait_for_unix_socket_or_daemon_exit(
        path: &PathBuf,
        daemon: &mut tokio::task::JoinHandle<Result<(), String>>,
    ) {
        for _ in 0..50 {
            if UnixStream::connect(path).await.is_ok() {
                return;
            }
            if daemon.is_finished() {
                panic!(
                    "daemon exited before socket became ready: {:?}",
                    daemon.await
                );
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out waiting for socket {}", path.display());
    }

    async fn fetch_http_body(addr: std::net::SocketAddr, path: &str) -> String {
        for _ in 0..50 {
            if let Ok(mut stream) = TcpStream::connect(addr).await {
                let request = format!(
                    "GET {path} HTTP/1.1\r\nHost: {}\r\nConnection: close\r\n\r\n",
                    addr
                );
                stream
                    .write_all(request.as_bytes())
                    .await
                    .expect("request write should succeed");
                let mut response = Vec::new();
                stream
                    .read_to_end(&mut response)
                    .await
                    .expect("response read should succeed");
                let response = String::from_utf8(response).expect("response should be utf-8");
                let Some((_, body)) = response.split_once("\r\n\r\n") else {
                    panic!("http response should contain headers");
                };
                return body.to_string();
            }
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
        panic!("timed out fetching metrics from {addr}");
    }

    fn free_local_addr() -> std::net::SocketAddr {
        std::net::TcpListener::bind("127.0.0.1:0")
            .expect("should bind ephemeral port")
            .local_addr()
            .expect("ephemeral listener should have local addr")
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("time after epoch")
            .as_nanos();
        let dir = std::env::temp_dir().join(format!("{prefix}-{nanos}"));
        std::fs::create_dir_all(&dir).expect("temp dir should be created");
        dir
    }

    struct StaticResourceMetricsSource {
        snapshots: Vec<WorkloadResourceSnapshot>,
    }

    #[async_trait]
    impl ContainerResourceMetricsSource for StaticResourceMetricsSource {
        async fn list_workload_resource_snapshots(
            &self,
        ) -> Result<Vec<WorkloadResourceSnapshot>, String> {
            Ok(self.snapshots.clone())
        }
    }
}
