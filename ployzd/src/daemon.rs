//! Local Machine daemon lifecycle.

use std::{
    fs::{self, File, OpenOptions},
    io,
    net::{AddrParseError, IpAddr, SocketAddr},
    os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
    time::Duration,
};

use ployz_core::{LocalMachinePhase, MachineRpcServer, RUNTIME_WATCH_MESSAGE_SIZE_LIMIT};
use sd_notify::NotifyState;
use thiserror::Error;
use tokio::{
    net::{TcpListener, UnixListener},
    signal::unix::{SignalKind, signal},
    sync::watch,
    task::JoinHandle,
    time::Instant,
};
use tokio_stream::wrappers::{TcpListenerStream, UnixListenerStream};
use tokio_util::sync::CancellationToken;
use tonic::{service::Routes, transport::Server};

use crate::{
    certificates,
    corrosion::{
        CorrosionConfig, DEFAULT_API_ADDRESS, DEFAULT_CONTAINER_NAME, Error as CorrosionError,
        RunningCorrosion, run_machine_publisher,
    },
    dns,
    docker::{ContainerRuntime, ImageIngest, LocalDocker, MachineSpecStore, SpecStoreError},
    filesystem::set_ployz_group,
    global_reconcile::{self, global_reconcile_observation_channel},
    ingress,
    machine::{LocalMachineBody, LocalMachineStore, StoreError},
    metrics,
    network::{CORROSION_GOSSIP_PORT, MACHINE_API_PORT, NetworkError, NetworkPlane},
    proxy::MachineProxy,
    rpc::MachineService,
};

/// How the daemon attaches a Container Runtime.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ContainerMode {
    /// Connect to local Docker; on failure warn and continue without a Container Runtime.
    Auto,
    /// Do not attach a Container Runtime.
    Absent,
}

/// Inputs the daemon needs to start. Local Machine Phase decides which planes run.
pub struct DaemonConfig {
    pub data_dir: PathBuf,
    pub socket: PathBuf,
    pub metrics_address: SocketAddr,
    pub dns_upstreams: Vec<SocketAddr>,
    pub machine_api_address: Option<SocketAddr>,
    pub containerd_socket: Option<PathBuf>,
    pub containers: ContainerMode,
}

/// Running Local Machine daemon. Callers do not choose which planes start.
pub struct Daemon {
    stop: CancellationToken,
    shutdown: CancellationToken,
    store: Arc<Mutex<LocalMachineStore>>,
    corrosion: Option<RunningCorrosion>,
    ingest: Arc<ImageIngest>,
    reset_rx: watch::Receiver<bool>,
    servers: JoinHandle<io::Result<()>>,
    _socket_lock: File,
}

/// Failures while starting or waiting for the daemon.
#[derive(Debug, Error)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] io::Error),
    #[error(transparent)]
    Store(#[from] StoreError),
    #[error(transparent)]
    Network(#[from] NetworkError),
    #[error(transparent)]
    SpecStore(#[from] SpecStoreError),
    #[error(transparent)]
    Corrosion(#[from] CorrosionError),
    #[error(transparent)]
    Addr(#[from] AddrParseError),
    #[error(transparent)]
    Transport(#[from] tonic::transport::Error),
    #[error("local Machine record lock poisoned")]
    StorePoisoned,
}

impl Daemon {
    /// Start serving Machine RPC on the configured unix socket.
    ///
    /// Returns only once that socket accepts connections. Local Machine Phase
    /// decides which planes start, degrade, or wait for catch-up.
    ///
    /// # Errors
    ///
    /// If construction, binding, or required planes fail.
    pub async fn start(config: DaemonConfig) -> Result<Self, Error> {
        let store = Arc::new(Mutex::new(LocalMachineStore::open(&config.data_dir)?));
        let (rpc_listener, socket_lock) = bind_socket(&config.socket)?;
        let metrics_listener = TcpListener::bind(config.metrics_address).await?;
        let local_record = store
            .lock()
            .map_err(|_| Error::StorePoisoned)?
            .record()
            .clone();
        let local_id = local_record.id();
        let local_phase = local_record.phase();
        let local_machine = local_record.machine().cloned();
        let mut network = NetworkPlane::start(&local_record).await?;
        let machine_api_listeners = if config.machine_api_address.is_none()
            && let Some(network) = &network
        {
            let [management, gateway] = network.machine_api_addresses()?;
            Some((
                TcpListener::bind(management).await?,
                TcpListener::bind(gateway).await?,
            ))
        } else {
            None
        };
        let explicit_machine_api_listener = match config.machine_api_address {
            Some(address) => Some(TcpListener::bind(address).await?),
            None => None,
        };
        let dns_upstreams =
            (!config.dns_upstreams.is_empty()).then(|| config.dns_upstreams.clone());
        let containers = match config.containers {
            ContainerMode::Absent => None,
            ContainerMode::Auto => {
                let specs = MachineSpecStore::open(config.data_dir.join("machine.db")).await?;
                match LocalDocker::connect() {
                    Ok(docker) => Some(ContainerRuntime::new(docker, specs)),
                    Err(error) => {
                        eprintln!("WARNING: local Docker is unavailable: {error}");
                        None
                    }
                }
            }
        };
        let corrosion = start_corrosion(&config, &store).await?;
        if let (Some(corrosion), Some(backend)) = (
            corrosion.as_ref(),
            local_record.founding_ingress_proxy_backend(),
        ) {
            corrosion
                .store()
                .publish_founding_ingress_proxy_backend(backend)
                .await?;
        }
        let replicated_store = corrosion.as_ref().map(|running| running.store().clone());
        let admin = corrosion.as_ref().map(RunningCorrosion::admin_client);
        let containers = match (containers, replicated_store.clone()) {
            (Some(runtime), Some(replicated)) => {
                Some(runtime.replicating(replicated, Arc::clone(&store)))
            }
            (runtime, _) => runtime,
        };
        let shutdown = CancellationToken::new();
        let ingest = ImageIngest::new(
            config.containerd_socket.clone(),
            shutdown.child_token(),
            containers.as_ref().map(ContainerRuntime::local_docker),
        );
        let (participating, participating_rx) =
            watch::channel(local_phase == LocalMachinePhase::Participating);
        let (global_reconcile_publisher, global_reconcile_observations) =
            global_reconcile_observation_channel();
        let (cloud_pairing_tx, cloud_pairing_rx) = watch::channel(local_record.cloud_pairing);
        let (reset, reset_rx) = watch::channel(false);
        let certificate_data_dir = config.data_dir.clone();
        let acme_directory = certificates::directory_url();
        let ingress_data_dir = config.data_dir.clone();
        let ingress_runtime_dir = config
            .socket
            .parent()
            .unwrap_or_else(|| Path::new("/run/ployz"))
            .join("ingress");
        let service = MachineService::with_cluster(
            Arc::clone(&store),
            reset.clone(),
            corrosion
                .as_ref()
                .map(|running| (running.store().clone(), running.admin_client())),
        )
        .with_optional_containers(containers.clone())
        .with_ingress_data_dir(config.data_dir.clone())
        .with_image_ingest(Arc::clone(&ingest))
        .with_cloud_pairing(cloud_pairing_tx)
        .with_global_reconcile_observations(global_reconcile_observations);
        let proxy = MachineProxy::new(
            Routes::new(
                MachineRpcServer::new(service.clone())
                    .max_encoding_message_size(RUNTIME_WATCH_MESSAGE_SIZE_LIMIT),
            ),
            local_id,
            MACHINE_API_PORT,
            replicated_store.clone(),
        );

        let rpc = Server::builder().serve_with_incoming_shutdown(
            proxy.clone(),
            UnixListenerStream::new(rpc_listener),
            shutdown.clone().cancelled_owned(),
        );
        let metrics = metrics::serve(
            metrics_listener,
            env!("CARGO_PKG_VERSION"),
            shutdown.clone(),
        );
        let publisher = run_machine_publisher(
            replicated_store.clone(),
            Arc::clone(&store),
            participating,
            shutdown.clone(),
        );
        let (management_listener, gateway_listener) = machine_api_listeners
            .map_or((None, None), |(management, gateway)| {
                (Some(management), Some(gateway))
            });
        let socket = config.socket.clone();
        let ingress_docker = containers.as_ref().map(ContainerRuntime::local_docker);
        let store_for_servers = Arc::clone(&store);
        let shutdown_for_servers = shutdown.clone();
        let servers = tokio::spawn(async move {
            let store = store_for_servers;
            let shutdown = shutdown_for_servers;
            let network_rpc = async {
                tokio::try_join!(
                    serve_machine_api(
                        explicit_machine_api_listener,
                        proxy.clone(),
                        shutdown.clone()
                    ),
                    serve_machine_api(management_listener, proxy.clone(), shutdown.clone()),
                    serve_machine_api(gateway_listener, proxy.clone(), shutdown.clone()),
                )
                .map(|_| ())
            };
            let network_runner = async {
                if let Some(network) = &mut network {
                    network
                        .run(
                            replicated_store.clone(),
                            Arc::clone(&store),
                            shutdown.clone(),
                        )
                        .await
                } else {
                    shutdown.cancelled().await;
                    Ok(())
                }
            };
            let observer = async {
                match containers.clone() {
                    Some(runtime) => runtime
                        .publish_observations(shutdown.clone())
                        .await
                        .map_err(io::Error::other),
                    None => {
                        shutdown.cancelled().await;
                        Ok(())
                    }
                }
            };
            let dns = async {
                if !wait_for_participation(participating_rx.clone(), shutdown.clone()).await? {
                    return Ok(());
                }
                match (local_machine.clone(), replicated_store.clone(), admin) {
                    (Some(machine), Some(replicated), Some(admin)) => {
                        dns::run(machine, replicated, admin, dns_upstreams, shutdown.clone()).await
                    }
                    _ => {
                        shutdown.cancelled().await;
                        Ok(())
                    }
                }
            };
            let ingress = async {
                if !wait_for_participation(participating_rx.clone(), shutdown.clone()).await? {
                    return Ok(());
                }
                match (local_machine.clone(), replicated_store.clone()) {
                    (Some(machine), Some(replicated)) => {
                        ingress::run(
                            machine,
                            replicated,
                            ingress_data_dir,
                            ingress_runtime_dir,
                            ingress_docker,
                            shutdown.clone(),
                        )
                        .await
                    }
                    _ => {
                        shutdown.cancelled().await;
                        Ok(())
                    }
                }
            };
            let certificates = async {
                if !wait_for_participation(participating_rx.clone(), shutdown.clone()).await? {
                    return Ok(());
                }
                match replicated_store.clone() {
                    Some(replicated) => {
                        certificates::run(
                            replicated,
                            certificate_data_dir,
                            acme_directory,
                            local_id,
                            shutdown.clone(),
                        )
                        .await
                    }
                    None => {
                        shutdown.cancelled().await;
                        Ok(())
                    }
                }
            };
            let global_reconcile_local = service.local();
            let global_reconcile = async {
                match replicated_store.clone() {
                    Some(replicated) => global_reconcile::run(
                        replicated,
                        global_reconcile_local,
                        global_reconcile_publisher,
                        participating_rx.clone(),
                        shutdown.clone(),
                    )
                    .await
                    .map_err(io::Error::other),
                    None => {
                        shutdown.cancelled().await;
                        Ok(())
                    }
                }
            };
            let relay_register = async {
                if !wait_for_participation(participating_rx.clone(), shutdown.clone()).await? {
                    return Ok(());
                }
                crate::relay::run(cloud_pairing_rx, local_id, service, shutdown.clone()).await
            };
            tokio::try_join!(
                async { rpc.await.map_err(io::Error::other) },
                metrics,
                publisher,
                network_rpc,
                network_runner,
                observer,
                dns,
                ingress,
                certificates,
                global_reconcile,
                relay_register,
            )
            .map(|_| ())
        });
        if let Err(error) = wait_until_socket_accepts(&socket, Duration::from_secs(5)).await {
            shutdown.cancel();
            servers.abort();
            let _ = servers.await;
            return Err(error.into());
        }
        tracing::info!(
            phase = local_phase.as_str(),
            version = env!("CARGO_PKG_VERSION"),
            "started"
        );
        tracing::debug!(socket = %socket.display(), "listening");
        Ok(Self {
            stop: CancellationToken::new(),
            shutdown,
            store,
            corrosion,
            ingest,
            reset_rx,
            servers,
            _socket_lock: socket_lock,
        })
    }

    /// Ask a running daemon to exit. [`wait`](Self::wait) still has to be called.
    pub fn request_stop(&self) {
        self.stop.cancel();
    }

    /// Wait until a signal, [`request_stop`](Self::request_stop), or restart-request
    /// finishes shutdown or reset.
    ///
    /// systemd READY is advertised only after SIGINT/SIGTERM are catchable, so a
    /// stop that races with READY cannot hit the default terminate action.
    ///
    /// # Errors
    ///
    /// If a plane or cleanup step fails.
    pub async fn wait(mut self) -> Result<(), Error> {
        let mut interrupt = signal(SignalKind::interrupt())?;
        let mut terminate = signal(SignalKind::terminate())?;
        notify(NotifyState::Ready);
        let mut servers = self.servers;
        let mut completed_servers = None;
        let mut errors = Vec::new();
        let stop = tokio::select! {
            result = &mut servers => {
                completed_servers = Some(join_servers(result));
                StopKind::Plane
            },
            _ = interrupt.recv() => StopKind::Signal("SIGINT"),
            _ = terminate.recv() => StopKind::Signal("SIGTERM"),
            () = self.stop.cancelled() => StopKind::Stop,
            changed = self.reset_rx.changed() => match changed {
                Ok(()) => StopKind::Restart,
                Err(error) => {
                    errors.push(error.to_string());
                    StopKind::WatchFailed("restart")
                }
            },
        };
        notify(NotifyState::Stopping);
        let resetting = match self.store.lock() {
            Ok(store) => store.record().phase() == LocalMachinePhase::Resetting,
            Err(_) => {
                errors.push("local Machine record lock poisoned".into());
                false
            }
        };
        let reason = match stop {
            StopKind::Plane => "a plane exited".to_owned(),
            StopKind::Signal(signal) => format!("received {signal}"),
            StopKind::Stop => "stop requested".to_owned(),
            StopKind::Restart if resetting => "local Machine reset".to_owned(),
            StopKind::Restart => "restart requested".to_owned(),
            StopKind::WatchFailed(what) => format!("{what} wait failed"),
        };
        tracing::info!(reason = reason.as_str(), "shutting down");
        self.shutdown.cancel();
        // TODO(UT-098, UT-099): preserve both API servers' unbounded graceful shutdown
        // until a timeout is explicitly chosen.
        let server_result = match completed_servers {
            Some(result) => result,
            None => join_servers(servers.await),
        };
        if let Err(error) = server_result {
            errors.push(error.to_string());
        }

        if let Some(running) = &mut self.corrosion {
            let result = if resetting {
                running.cleanup().await
            } else {
                running.stop().await
            };
            if let Err(error) = result {
                errors.push(error.to_string());
            }
        }
        let ingest_result = if resetting {
            self.ingest.cleanup().await
        } else {
            self.ingest.stop().await
        };
        if let Err(error) = ingest_result {
            errors.push(error.to_string());
        }
        if resetting {
            match self.store.lock() {
                Ok(store) => {
                    if let Err(error) = store.complete_reset() {
                        errors.push(error.to_string());
                    }
                }
                Err(_) => errors.push("local Machine record lock poisoned".into()),
            }
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(errors.join("; ")).into())
        }
    }
}

fn join_servers(result: Result<io::Result<()>, tokio::task::JoinError>) -> io::Result<()> {
    match result {
        Ok(result) => result,
        Err(error) => Err(io::Error::other(error)),
    }
}

fn socket_not_ready(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
    )
}

/// Connect to the Machine API Unix socket, waiting while it is missing or refusing
/// connections.
///
/// # Errors
///
/// Returns a human-readable I/O error when `timeout` elapses or connect fails
/// for a reason other than the socket not being ready.
pub async fn wait_until_socket_accepts(
    path: &Path,
    timeout: Duration,
) -> io::Result<tokio::net::UnixStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match tokio::net::UnixStream::connect(path).await {
            Ok(stream) => return Ok(stream),
            Err(error) if socket_not_ready(&error) && Instant::now() < deadline => {
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
            Err(error) if socket_not_ready(&error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("Machine API socket did not become ready: {error}"),
                ));
            }
            Err(error) => {
                return Err(io::Error::new(
                    error.kind(),
                    format!("could not connect to the Machine API socket: {error}"),
                ));
            }
        }
    }
}

async fn serve_machine_api(
    listener: Option<TcpListener>,
    proxy: MachineProxy,
    shutdown: CancellationToken,
) -> io::Result<()> {
    match listener {
        Some(listener) => Server::builder()
            .serve_with_incoming_shutdown(
                proxy,
                TcpListenerStream::new(listener),
                shutdown.cancelled_owned(),
            )
            .await
            .map_err(io::Error::other),
        None => {
            shutdown.cancelled().await;
            Ok(())
        }
    }
}

async fn start_corrosion(
    config: &DaemonConfig,
    store: &Arc<Mutex<LocalMachineStore>>,
) -> Result<Option<RunningCorrosion>, Error> {
    let record = store
        .lock()
        .map_err(|_| Error::StorePoisoned)?
        .record()
        .clone();
    let machine = match &record.body {
        LocalMachineBody::Joining { machine, .. }
        | LocalMachineBody::Participating { machine, .. } => machine,
        LocalMachineBody::Uninitialized { .. } | LocalMachineBody::Resetting { .. } => {
            return Ok(None);
        }
    };
    let run_dir = config
        .socket
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket path has no parent"))?
        .join("corrosion");
    let bootstrap = record.bootstrap().iter().map(|machine| {
        SocketAddr::new(
            IpAddr::V6(machine.management_address.0),
            CORROSION_GOSSIP_PORT,
        )
    });
    // TimeoutStartSec=20; EXTEND_TIMEOUT covers Corrosion image pull and wait_ready.
    let _extend = extend_systemd_start_timeout();
    Ok(Some(
        CorrosionConfig::new(
            config.data_dir.join("corrosion"),
            run_dir,
            DEFAULT_API_ADDRESS.parse()?,
            SocketAddr::new(
                IpAddr::V6(machine.management_address.0),
                CORROSION_GOSSIP_PORT,
            ),
            DEFAULT_CONTAINER_NAME,
        )
        .with_bootstrap(bootstrap)
        .start()
        .await?,
    ))
}

async fn wait_for_participation(
    mut participating: watch::Receiver<bool>,
    shutdown: CancellationToken,
) -> io::Result<bool> {
    tokio::select! {
        biased;
        () = shutdown.cancelled() => Ok(false),
        changed = participating.wait_for(|participating| *participating) => {
            match changed {
                Ok(_) => Ok(!shutdown.is_cancelled()),
                Err(_) if shutdown.is_cancelled() => Ok(false),
                Err(error) => Err(io::Error::other(error)),
            }
        }
    }
}

enum StopKind {
    Plane,
    Signal(&'static str),
    Stop,
    Restart,
    WatchFailed(&'static str),
}

fn bind_socket(path: &Path) -> io::Result<(UnixListener, File)> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket path has no parent"))?;
    let parent_created = !parent.exists();
    fs::create_dir_all(parent)?;
    if parent_created {
        fs::set_permissions(parent, fs::Permissions::from_mode(0o750))?;
        set_ployz_group(parent)?;
    }

    let mut lock_path = path.as_os_str().to_owned();
    lock_path.push(".lock");
    let lock = OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .open(lock_path)?;
    fs2::FileExt::try_lock_exclusive(&lock).map_err(|error| {
        if error.kind() == io::ErrorKind::WouldBlock {
            io::Error::new(
                io::ErrorKind::AddrInUse,
                "socket is owned by another daemon",
            )
        } else {
            error
        }
    })?;

    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_socket() => fs::remove_file(path)?,
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "refusing to replace a non-socket path",
            ));
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    let listener = UnixListener::bind(path)?;
    fs::set_permissions(path, fs::Permissions::from_mode(0o660))?;
    set_ployz_group(path)?;
    Ok((listener, lock))
}

fn notify(state: NotifyState<'_>) {
    if let Err(error) = sd_notify::notify(&[state]) {
        eprintln!("systemd notification failed: {error}");
    }
}

const SYSTEMD_START_TIMEOUT_EXTENSION: Duration = Duration::from_secs(30);
const SYSTEMD_START_TIMEOUT_EXTEND_MAX: Duration = Duration::from_secs(5 * 60);

#[must_use]
struct SystemdStartTimeoutExtend {
    task: JoinHandle<()>,
}

impl Drop for SystemdStartTimeoutExtend {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn extend_systemd_start_timeout() -> SystemdStartTimeoutExtend {
    let usec = u32::try_from(SYSTEMD_START_TIMEOUT_EXTENSION.as_micros())
        .expect("30s start-timeout extension fits u32 microseconds");
    let deadline = Instant::now() + SYSTEMD_START_TIMEOUT_EXTEND_MAX;
    SystemdStartTimeoutExtend {
        task: tokio::spawn(async move {
            loop {
                if Instant::now() >= deadline {
                    eprintln!(
                        "stopped extending the systemd start timeout: Corrosion is taking too long to start"
                    );
                    return;
                }
                if let Err(error) = sd_notify::notify(&[NotifyState::ExtendTimeoutUsec(usec)]) {
                    eprintln!("failed to extend the systemd start timeout: {error}");
                    return;
                }
                tokio::time::sleep(SYSTEMD_START_TIMEOUT_EXTENSION).await;
            }
        }),
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        net::{SocketAddr, TcpListener},
        path::{Path, PathBuf},
    };

    use tokio::net::UnixListener;

    use ployz_core::{
        CERTIFICATE_POLICY_CAPABILITY, DESCRIBE_CONTRACT_CAPABILITY, DescribeContractRequest,
        LIST_CONTAINERS_CAPABILITY, MachineRpcClient, ResetRequest, op,
    };
    use tonic::transport::Endpoint;

    use super::{
        ContainerMode, Daemon, DaemonConfig, wait_for_participation, wait_until_socket_accepts,
    };
    use tokio_util::sync::CancellationToken;

    struct TestDir(PathBuf);

    impl TestDir {
        fn new(prefix: &str) -> Self {
            Self(std::env::temp_dir().join(format!("{prefix}-{}", ployz_core::MachineId::random())))
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn unused_address() -> SocketAddr {
        TcpListener::bind("127.0.0.1:0")
            .unwrap()
            .local_addr()
            .unwrap()
    }

    fn test_config(root: &Path, containers: ContainerMode) -> (DaemonConfig, PathBuf) {
        fs::create_dir_all(root).unwrap();
        let socket = root.join("run/ployz.sock");
        (
            DaemonConfig {
                data_dir: root.join("data"),
                socket: socket.clone(),
                metrics_address: unused_address(),
                dns_upstreams: Vec::new(),
                machine_api_address: None,
                containerd_socket: None,
                containers,
            },
            socket,
        )
    }

    async fn describe(path: &Path) -> ployz_core::ContractDescription {
        let channel = Endpoint::from_shared(format!("unix:{}", path.display()))
            .unwrap()
            .connect()
            .await
            .unwrap();
        MachineRpcClient::new(channel)
            .describe_contract(
                op::DescribeContract::into_request(DescribeContractRequest {})
                    .encode()
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap()
            .decode::<op::DescribeContract>()
            .unwrap()
            .clone()
    }

    async fn reset(path: &Path) {
        let channel = Endpoint::from_shared(format!("unix:{}", path.display()))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let response = MachineRpcClient::new(channel)
            .reset(op::Reset::into_request(ResetRequest {}).encode().unwrap())
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap();
        response.decode::<op::Reset>().unwrap();
    }

    #[tokio::test]
    async fn absent_containers_start_without_container_capabilities() {
        let root = TestDir::new("ployzd-daemon-absent");
        let (config, socket) = test_config(&root.0, ContainerMode::Absent);
        let daemon = Daemon::start(config).await.unwrap();
        let description = describe(&socket).await;
        assert!(description.supports(DESCRIBE_CONTRACT_CAPABILITY));
        assert!(description.supports(CERTIFICATE_POLICY_CAPABILITY));
        assert!(!description.supports(LIST_CONTAINERS_CAPABILITY));
        daemon.request_stop();
        daemon.wait().await.unwrap();
    }

    #[tokio::test]
    async fn request_stop_leaves_data_dir() {
        let root = TestDir::new("ployzd-daemon-stop");
        let (config, _) = test_config(&root.0, ContainerMode::Absent);
        let data_dir = config.data_dir.clone();
        let daemon = Daemon::start(config).await.unwrap();
        daemon.request_stop();
        daemon.wait().await.unwrap();
        assert!(data_dir.exists());
    }

    #[tokio::test]
    async fn second_start_on_same_socket_fails() {
        let root = TestDir::new("ployzd-daemon-bind");
        fs::create_dir_all(&root.0).unwrap();
        let socket = root.0.join("run/ployz.sock");
        let first = Daemon::start(DaemonConfig {
            data_dir: root.0.join("data-a"),
            socket: socket.clone(),
            metrics_address: unused_address(),
            dns_upstreams: Vec::new(),
            machine_api_address: None,
            containerd_socket: None,
            containers: ContainerMode::Absent,
        })
        .await
        .unwrap();
        let second = Daemon::start(DaemonConfig {
            data_dir: root.0.join("data-b"),
            socket,
            metrics_address: unused_address(),
            dns_upstreams: Vec::new(),
            machine_api_address: None,
            containerd_socket: None,
            containers: ContainerMode::Absent,
        })
        .await;
        assert!(second.is_err());
        first.request_stop();
        first.wait().await.unwrap();
    }

    #[tokio::test]
    async fn restart_keeps_machine_id_and_reset_clears_data_dir() {
        let root = TestDir::new("ployzd-daemon-reset");
        let (config, socket) = test_config(&root.0, ContainerMode::Absent);
        let data_dir = config.data_dir.clone();
        let daemon = Daemon::start(config).await.unwrap();
        let first = describe(&socket).await;
        daemon.request_stop();
        daemon.wait().await.unwrap();

        let (config, socket) = test_config(&root.0, ContainerMode::Absent);
        let daemon = Daemon::start(config).await.unwrap();
        assert_eq!(describe(&socket).await.machine_id, first.machine_id);
        reset(&socket).await;
        daemon.wait().await.unwrap();
        assert!(!data_dir.exists());
    }

    #[tokio::test]
    async fn participation_gate_waits_for_catch_up_and_obeys_shutdown() {
        let (participating, participating_rx) = tokio::sync::watch::channel(false);
        let shutdown = CancellationToken::new();
        let waiting = wait_for_participation(participating_rx, shutdown);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        participating.send_replace(true);
        assert!(waiting.await.unwrap());

        let (participating, participating_rx) = tokio::sync::watch::channel(true);
        let shutdown = CancellationToken::new();
        shutdown.cancel();
        drop(participating);
        assert!(
            !wait_for_participation(participating_rx, shutdown)
                .await
                .unwrap()
        );
    }

    #[tokio::test]
    async fn socket_wait_connects_once_the_listener_appears() {
        let root = TestDir::new("ployzd-socket-wait-ready");
        fs::create_dir_all(&root.0).unwrap();
        let path = root.0.join("ployz.sock");
        let listener_path = path.clone();
        let server = tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(30)).await;
            let listener = UnixListener::bind(&listener_path).unwrap();
            listener.accept().await.unwrap()
        });
        wait_until_socket_accepts(&path, std::time::Duration::from_secs(1))
            .await
            .unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn socket_wait_times_out_with_a_message_not_a_debug_struct() {
        let root = TestDir::new("ployzd-socket-wait-timeout");
        fs::create_dir_all(&root.0).unwrap();
        let path = root.0.join("missing.sock");
        let error = wait_until_socket_accepts(&path, std::time::Duration::from_millis(20))
            .await
            .unwrap_err();
        let message = error.to_string();
        assert!(message.contains("did not become ready"), "{message}");
        assert!(!message.contains("Os {"), "{message}");
    }
}
