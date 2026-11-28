//! Local Machine daemon lifecycle.

use std::{
    fs::File,
    io,
    net::{IpAddr, SocketAddr},
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use ployz_core::{
    CORROSION_API_ADDRESS, LocalMachinePhase, MachineLayout, VOLUME_SEND_PORT,
    startup_refusal::StartupRefusal,
};
use sd_notify::NotifyState;
use thiserror::Error;
use tokio::{
    net::{TcpListener, UnixListener},
    signal::unix::{SignalKind, signal},
    sync::watch,
    task::JoinHandle,
};
use tokio_stream::wrappers::{TcpListenerStream, UnixListenerStream};
use tokio_util::sync::CancellationToken;
use tonic::transport::Server;

use crate::{
    certificates,
    corrosion::{
        ClusterLink, CorrosionConfig, CorrosionPaths, DEFAULT_CONTAINER_NAME,
        Error as CorrosionError, LiveRuntime, RunningCorrosion, remove_retained,
        run_machine_publisher,
    },
    dns::DnsService,
    docker::{ContainerRuntime, ImageIngest, LocalDocker, MachineSpecStore, SpecStoreError},
    ingress,
    machine::{
        InterruptedReset, LocalMachineBody, LocalMachineStore, Opened, RecordOwner,
        RecordOwnerStopped, StoreError,
    },
    machine_api::{EndsOnShutdown, MachineApi},
    machine_api_socket::MachineApiSocket,
    management::{self, ManagementConfig},
    network::{CORROSION_GOSSIP_PORT, NetworkError, NetworkPlane},
    runtime_unit::UnitSupervision,
    sealed_compat::LogStore,
};

mod planes;

use planes::{Participation, optional_plane};

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
    pub dns_upstreams: Vec<SocketAddr>,
    pub units: UnitSupervision,
    pub machine_api_address: Option<SocketAddr>,
    pub containerd_socket: Option<PathBuf>,
    /// Volume plugin socket; None uses the Docker plugin socket.
    pub volume_plugin_socket: Option<PathBuf>,
    pub containers: ContainerMode,
    /// Management transport bind port and relay; defaults are production values.
    pub management: ManagementConfig,
}

/// Running Local Machine daemon. Callers do not choose which planes start.
pub struct Daemon {
    stop: CancellationToken,
    shutdown: CancellationToken,
    local: RecordOwner,
    corrosion: Option<RunningCorrosion>,
    dns: DnsService,
    ingest: Arc<ImageIngest>,
    restart_requested: watch::Receiver<bool>,
    servers: JoinHandle<io::Result<()>>,
    /// Closed after the server drain even when the drain aborted the server that owns
    /// it, so peers hear the close instead of waiting out their idle timeout (#3699).
    endpoint: iroh::Endpoint,
    observe: Option<JoinHandle<()>>,
    _socket_lock: File,
    #[cfg(test)]
    builds_cleaned: watch::Receiver<bool>,
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
    Transport(#[from] tonic::transport::Error),
    #[error(transparent)]
    RecordOwner(#[from] RecordOwnerStopped),
}

impl Error {
    /// Startup refused a conflict only the operator can resolve, so starting
    /// again fails the same way until they act.
    #[must_use]
    pub fn needs_operator(&self) -> bool {
        matches!(
            self,
            Self::Network(
                NetworkError::DockerNetworkConflict { .. } | NetworkError::WireGuardConflict { .. }
            )
        )
    }
}

fn record_refusal(refusal: &StartupRefusal, error: &Error) {
    if error.needs_operator() {
        refusal.record(&ployz_core::error_chain::inline(error));
    }
}

/// Closes every connection queued on the nonblocking `listener` when startup
/// refused a conflict. Left queued, each would have `ployz.socket` start the
/// daemon again at once, only for it to refuse again until systemd's start limit.
fn close_queued_if_refused(listener: &std::os::unix::net::UnixListener, error: &Error) {
    if error.needs_operator() {
        while listener.accept().is_ok() {}
    }
}

impl Daemon {
    /// Start serving Machine RPC on the configured unix socket.
    ///
    /// Returns once the listener is held and every plane is spawned. Local Machine Phase
    /// decides which planes start, degrade, or wait for catch-up.
    ///
    /// # Errors
    ///
    /// If construction, binding, or required planes fail.
    pub async fn start(config: DaemonConfig) -> Result<Self, Error> {
        // The Observe Store outlives the daemon, so it starts before anything
        // here can fail, and the daemon does not exit before it has started.
        let observe = (config.units == UnitSupervision::Systemd).then(|| {
            let (data_dir, socket) = (config.data_dir.clone(), config.socket.clone());
            tokio::spawn(async move {
                if let Err(error) = crate::observe::unit::converge(&data_dir, &socket).await {
                    eprintln!("WARNING: the Observe Store did not start: {error}");
                }
            })
        });
        let started: Result<Self, Error> = async {
            crate::faults::check()?;
            let socket = MachineApiSocket::claim(&config.socket)?;
            let run_dir = MachineLayout::run_dir_of(&config.socket);
            crate::installer::upgrade::reconcile(&config.data_dir, &run_dir)
                .await
                .map_err(io::Error::other)?;
            let build_policy = ployz_build::HostPolicy {
                fence: Some(crate::network::fence_builder),
                ..ployz_build::HostPolicy::from_environment()
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?
            };
            Self::start_with_build_policy(config, socket, build_policy, run_dir).await
        }
        .await;
        match started {
            Ok(mut daemon) => {
                daemon.observe = observe;
                Ok(daemon)
            }
            Err(error) => {
                if let Some(observe) = observe {
                    let _ = observe.await;
                }
                Err(error)
            }
        }
    }

    async fn start_with_build_policy(
        config: DaemonConfig,
        socket: MachineApiSocket,
        build_policy: ployz_build::HostPolicy,
        run_dir: PathBuf,
    ) -> Result<Self, Error> {
        let queued = socket.listener.try_clone()?;
        let refusal = StartupRefusal::new(&run_dir);
        let started = Self::start_planes(config, socket, build_policy, run_dir).await;
        if let Err(error) = &started {
            // Recorded before the close, so a client it closes finds the line.
            record_refusal(&refusal, error);
            close_queued_if_refused(&queued, error);
        }
        started
    }

    async fn start_planes(
        config: DaemonConfig,
        socket: MachineApiSocket,
        build_policy: ployz_build::HostPolicy,
        run_dir: PathBuf,
    ) -> Result<Self, Error> {
        let store = match LocalMachineStore::open_with_admission(&config.data_dir, &run_dir)? {
            Opened::Ready(store) => store,
            Opened::Resetting(interrupted) => {
                let record = interrupted.record().clone();
                // Removing the network and Corrosion waits on Docker, which IO load slows
                // past TimeoutStartSec, and each timed-out start begins it again.
                let _extend = crate::systemd_timeout::extend_systemd_start_timeout(
                    "finishing the interrupted reset",
                );
                finish_interrupted_reset(
                    interrupted,
                    NetworkPlane::cleanup_retained(&record),
                    remove_retained(&CorrosionPaths::under(&config.data_dir, &run_dir).run_dir),
                )
                .await?
            }
        };
        let local = RecordOwner::spawn(store)?;
        let local_record = local.record();
        let local_id = local_record.id();
        let local_phase = local_record.phase();
        let local_machine = local_record.machine().cloned();
        let dns = DnsService::start(
            config.units,
            &config.data_dir,
            &run_dir,
            &config.socket,
            config.dns_upstreams.clone(),
            &local_record,
        )
        .await?;
        // Opened before the network plane, which reads the part of a member that needs a
        // new address from it.
        let specs = match config.containers {
            ContainerMode::Absent => None,
            ContainerMode::Auto => {
                Some(MachineSpecStore::open(config.data_dir.join(MachineSpecStore::FILE)).await?)
            }
        };
        let mut network =
            NetworkPlane::start(&local_record, &config.data_dir, specs.clone()).await?;
        let machine_api_listener = match config
            .machine_api_address
            .or_else(|| network.as_ref().map(NetworkPlane::machine_api_address))
        {
            Some(address) => Some(TcpListener::bind(address).await?),
            None => None,
        };
        let volume_send_listener = match network.as_ref().map(NetworkPlane::machine_api_address) {
            Some(address) => {
                Some(TcpListener::bind(SocketAddr::new(address.ip(), VOLUME_SEND_PORT)).await?)
            }
            None => None,
        };
        let containers = match specs {
            None => None,
            Some(specs) => match LocalDocker::connect() {
                Ok(docker) => Some(
                    ContainerRuntime::new(
                        docker.with_staging_ledger(config.data_dir.join("peer-pull-staging")),
                        specs,
                    )
                    .with_sealing_key(config.data_dir.clone())
                    .with_collector(config.data_dir.clone())
                    .with_log_store(LogStore::of(config.units)),
                ),
                Err(error) => {
                    eprintln!(
                        "WARNING: local Docker is unavailable: {error}",
                        error = ployz_core::error_chain::inline(&error),
                    );
                    None
                }
            },
        };
        let corrosion = start_corrosion(&config.data_dir, &run_dir, &local).await?;
        let shutdown = CancellationToken::new();
        let link = corrosion
            .as_ref()
            .map(|running| ClusterLink::start(running, local.clone(), shutdown.clone()));
        let plugin_socket = config
            .volume_plugin_socket
            .clone()
            .unwrap_or_else(|| PathBuf::from(crate::VOLUME_PLUGIN_SOCKET));
        let plugin = crate::storage::Plugin::at(plugin_socket.clone());
        let containers = match (containers, link.as_ref()) {
            (Some(runtime), Some(link)) => Some(runtime.replicating(
                link.context.replicated.clone(),
                local.clone(),
                plugin.clone(),
            )),
            (runtime, _) => runtime,
        };
        let serving_local = planes::serving_local(
            local.clone(),
            containers.clone(),
            plugin.clone(),
            link.as_ref(),
        );
        let ingest = ImageIngest::new(
            config.containerd_socket.clone(),
            containers.as_ref().map(ContainerRuntime::local_docker),
        );
        if let Some(docker) = containers.as_ref().map(ContainerRuntime::local_docker) {
            docker.sweep_staging();
        }
        let records = local.watch();
        let participation = Participation::new(records.clone(), shutdown.clone());
        let dns_service = dns.clone();
        let restart_requested = local.restart_requested();
        let certificate_data_dir = config.data_dir.clone();
        let acme_directory = certificates::directory_url();
        let ingress_data_dir = config.data_dir.clone();
        let ingress_runtime_dir = run_dir.join("ingress");
        let grants = Arc::new(management::BuildGrants::default());
        let builds = crate::build::Runner::new(build_policy, shutdown.clone())
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let running_builds = builds.running_builds();
        // Stopping workers can outlast systemd's start timeout, so it never
        // holds back READY; Builds wait for it instead.
        builds.clean_up_abandoned();
        #[cfg(test)]
        let builds_cleaned = builds.cleaned();
        builds.follow(local.watch());
        let (endpoint_chooser, endpoint_choices) = tokio::sync::mpsc::channel(8);
        let (route_publishes, published_routes) =
            tokio::sync::watch::channel(crate::ingress::PublishedRoutes::default());
        let paths = network
            .as_ref()
            .map(NetworkPlane::paths)
            .unwrap_or_default();
        let machine_api = MachineApi::builder(local.clone())
            .with_endpoint_chooser(endpoint_chooser)
            .with_paths(paths)
            .with_build_grants(Arc::clone(&grants))
            .with_builds(builds)
            .with_local(serving_local.clone())
            .with_image_ingest(Arc::clone(&ingest))
            .with_published_routes(published_routes.clone())
            .build();
        let machine_api = EndsOnShutdown::new(machine_api, shutdown.clone());

        let rpc = rpc_server().serve_with_incoming_shutdown(
            machine_api.clone(),
            UnixListenerStream::new(UnixListener::from_std(socket.listener)?),
            shutdown.clone().cancelled_owned(),
        );
        let publisher = run_machine_publisher(
            link.as_ref().map(ClusterLink::observed),
            local.clone(),
            LiveRuntime::new(
                running_builds,
                containers.as_ref().map_or_else(
                    || tokio::sync::watch::channel(None).1,
                    ContainerRuntime::pool_committable_bytes,
                ),
            ),
            shutdown.clone(),
        );
        let rediscovery = crate::machine::LocalMachine::new(local.clone());
        let rediscovery_shutdown = shutdown.clone();
        tokio::spawn(async move { rediscovery.rediscover_addresses(rediscovery_shutdown).await });
        let closes = serving_local.clone();
        tokio::spawn(async move { closes.resume_preview_closes().await });
        tokio::spawn(crate::storage::sweep_trash_hourly(
            plugin.clone(),
            shutdown.clone(),
        ));
        let management_endpoint =
            management::bind(local_record.management_secret(), &config.management)
                .await
                .map_err(io::Error::other)?;
        tracing::info!(relay = %config.management.relay, "management endpoint bound");
        let endpoint = management_endpoint.clone();
        let socket_path = config.socket.clone();
        let local_for_servers = local.clone();
        let shutdown_for_servers = shutdown.clone();
        let servers = tokio::spawn(async move {
            let local = local_for_servers;
            let shutdown = shutdown_for_servers;
            let network_rpc = async {
                tokio::try_join!(
                    optional_plane(machine_api_listener, &shutdown, |listener| async {
                        rpc_server()
                            .serve_with_incoming_shutdown(
                                machine_api.clone(),
                                TcpListenerStream::new(listener),
                                shutdown.clone().cancelled_owned(),
                            )
                            .await
                            .map_err(io::Error::other)
                    }),
                    // The send stream asks this Machine's own record and accepted rows
                    // whom it may serve.
                    optional_plane(volume_send_listener, &shutdown, |listener| {
                        crate::volume_send::serve(
                            listener,
                            (PathBuf::from("zfs"), PathBuf::from("zpool")),
                            plugin_socket.clone(),
                            serving_local.clone(),
                            shutdown.clone(),
                        )
                    }),
                    async {
                        management::serve(
                            management_endpoint,
                            // The tunnel reads a joined Machine's Cluster network from the
                            // replicated store.
                            serving_local.clone(),
                            machine_api.clone(),
                            grants,
                            shutdown.clone(),
                        )
                        .await;
                        Ok(())
                    },
                )
                .map(|_| ())
            };
            let network_runner = optional_plane(network.as_mut(), &shutdown, |network| {
                network.run(
                    link.as_ref().map(ClusterLink::observed),
                    local.clone(),
                    endpoint_choices,
                    shutdown.clone(),
                )
            });
            let observer = optional_plane(containers.clone(), &shutdown, |runtime| {
                let shutdown = shutdown.clone();
                async move {
                    runtime
                        .publish_observations(shutdown)
                        .await
                        .map_err(io::Error::other)
                }
            });
            let collector = optional_plane(containers.clone(), &shutdown, |runtime| {
                let shutdown = shutdown.clone();
                async move { runtime.collector().run(shutdown).await }
            });
            let dns = dns_service.run(records.clone(), shutdown.clone());
            let ingress = async {
                if !participation.wait().await? {
                    return Ok(());
                }
                optional_plane(
                    local_machine.clone().zip(link.clone()),
                    &shutdown,
                    |(machine, link)| {
                        ingress::run(
                            machine,
                            link,
                            ingress_data_dir,
                            ingress_runtime_dir,
                            route_publishes,
                            shutdown.clone(),
                        )
                    },
                )
                .await
            };
            let certificates = async {
                if !participation.wait().await? {
                    return Ok(());
                }
                optional_plane(link.clone(), &shutdown, |link| async {
                    certificates::run(
                        link.context.replicated,
                        certificate_data_dir,
                        acme_directory,
                        local_id,
                        shutdown.clone(),
                    )
                    .await;
                    Ok(())
                })
                .await
            };
            let boot_pass = async {
                if !participation.wait().await? {
                    return Ok(());
                }
                optional_plane(link.clone(), &shutdown, |link| async {
                    let Some(runtime) = containers.clone() else {
                        return Ok(());
                    };
                    let first_synced = runtime.first_synced();
                    let machine = serving_local.clone();
                    machine.restore_held_at_boot().await;
                    let boot = crate::boot_pass::DaemonHost {
                        machine_id: local_id,
                        machine,
                        replicated: link.context.replicated,
                        machines: link.view,
                        admin: link.context.admin,
                    };
                    tokio::select! {
                        () = shutdown.cancelled() => {}
                        () = crate::boot_pass::run(boot, first_synced, published_routes) => {}
                    }
                    Ok(())
                })
                .await
            };
            tokio::try_join!(
                async { rpc.await.map_err(io::Error::other) },
                publisher,
                network_rpc,
                network_runner,
                observer,
                collector,
                dns,
                ingress,
                certificates,
                boot_pass,
            )
            .map(|_| ())
        });
        tracing::info!(
            phase = local_phase.as_str(),
            version = env!("CARGO_PKG_VERSION"),
            "started"
        );
        tracing::debug!(socket = %socket_path.display(), "listening");
        Ok(Self {
            stop: CancellationToken::new(),
            shutdown,
            local,
            corrosion,
            dns,
            ingest,
            restart_requested,
            servers,
            endpoint,
            observe: None,
            _socket_lock: socket.lock,
            #[cfg(test)]
            builds_cleaned,
        })
    }

    /// Ask a running daemon to exit. [`wait`](Self::wait) still has to be called.
    #[cfg(test)]
    pub fn request_stop(&self) {
        self.stop.cancel();
    }

    /// Wait until a signal, `request_stop`, or restart-request
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
        let mut records = self.local.watch();
        // The record watch closes only when the owner thread stopped, which before
        // shutdown means a mutation panicked: crash out and let systemd restart us.
        let owner_stopped = async move { while records.changed().await.is_ok() {} };
        let stop = tokio::select! {
            result = &mut servers => {
                completed_servers = Some(join_servers(result));
                StopKind::Plane
            },
            _ = interrupt.recv() => StopKind::Signal("SIGINT"),
            _ = terminate.recv() => StopKind::Signal("SIGTERM"),
            () = self.stop.cancelled() => StopKind::Stop,
            () = owner_stopped => StopKind::RecordOwner,
            changed = self.restart_requested.changed() => match changed {
                Ok(()) => StopKind::Restart,
                Err(error) => {
                    errors.push(ployz_core::error_chain::inline(&error));
                    StopKind::WatchFailed("restart")
                }
            },
        };
        notify(NotifyState::Stopping);
        let resetting = self.local.record().phase() == LocalMachinePhase::Resetting;
        let reason = match stop {
            StopKind::Plane => "a plane exited".to_owned(),
            StopKind::Signal(signal) => format!("received {signal}"),
            StopKind::Stop => "stop requested".to_owned(),
            StopKind::Restart if resetting => "local Machine reset".to_owned(),
            StopKind::Restart => "restart requested".to_owned(),
            StopKind::RecordOwner => "local Machine record owner stopped".to_owned(),
            StopKind::WatchFailed(what) => format!("{what} wait failed"),
        };
        tracing::info!(reason = reason.as_str(), "shutting down");
        self.shutdown.cancel();
        // A reset drains too: the Leave that asked for it is still sending its reply, the
        // caller's only proof of the reset. The bound keeps a held connection from holding
        // the reset; CLI wait_phase has 60s to see Uninitialized after systemd restarts us.
        let (server_result, ()) = tokio::join!(
            stop_servers(completed_servers, &mut servers, SERVER_DRAIN),
            self.dns.stopping(resetting),
        );
        if let Err(error) = server_result {
            errors.push(ployz_core::error_chain::inline(&error));
        }
        if tokio::time::timeout(ENDPOINT_CLOSE, self.endpoint.close())
            .await
            .is_err()
        {
            tracing::debug!("management endpoint did not finish closing in time");
        }

        if let Err(error) = self.ingest.shutdown().await {
            errors.push(ployz_core::error_chain::inline(&error));
        }
        if resetting {
            let corrosion = self.corrosion.as_mut();
            let removal = async {
                match corrosion {
                    Some(running) => running.cleanup().await,
                    None => Ok(()),
                }
            };
            let record = self.local.record();
            if let Err(error) = finish_reset(
                &self.local,
                NetworkPlane::cleanup_retained(&record),
                removal,
            )
            .await
            {
                errors.push(ployz_core::error_chain::inline(&error));
            }
        }
        // Admitted work is detached from its RPC and outlives the server drain, so it
        // gets the same grace before the next daemon may claim the data directory.
        match crate::machine::LocalMachine::new(self.local.clone())
            .close_when_idle(SERVER_DRAIN)
            .await
        {
            Some(Ok(())) => {}
            Some(Err(error)) => errors.push(ployz_core::error_chain::inline(&error)),
            None => tracing::warn!(
                "admitted Machine work is still running; the record stays claimed until exit"
            ),
        }
        if let Some(observe) = self.observe {
            let _ = observe.await;
        }
        if errors.is_empty() {
            Ok(())
        } else {
            Err(io::Error::other(errors.join("; ")).into())
        }
    }
}

const SERVER_DRAIN: Duration = Duration::from_secs(5);
/// How long closed management connections get to tell their peers before exit.
const ENDPOINT_CLOSE: Duration = Duration::from_secs(1);

async fn stop_servers(
    completed: Option<io::Result<()>>,
    servers: &mut JoinHandle<io::Result<()>>,
    drain: Duration,
) -> io::Result<()> {
    if let Some(result) = completed {
        return result;
    }
    if let Ok(result) = tokio::time::timeout(drain, &mut *servers).await {
        return join_servers(result);
    }
    servers.abort();
    join_servers(servers.await)
}

fn join_servers(result: Result<io::Result<()>, tokio::task::JoinError>) -> io::Result<()> {
    match result {
        Ok(result) => result,
        Err(error) if error.is_cancelled() => Ok(()),
        Err(error) => Err(io::Error::other(error)),
    }
}

fn rpc_server() -> Server {
    Server::builder()
        .initial_stream_window_size(ployz_core::RPC_STREAM_WINDOW_BYTES)
        .initial_connection_window_size(ployz_core::RPC_CONNECTION_WINDOW_BYTES)
}

async fn finish_interrupted_reset(
    interrupted: InterruptedReset,
    remove_network: impl Future<Output = Result<(), NetworkError>>,
    remove_corrosion: impl Future<Output = Result<(), CorrosionError>>,
) -> Result<LocalMachineStore, Error> {
    remove_network.await?;
    remove_corrosion.await?;
    Ok(interrupted.complete()?)
}

async fn finish_reset(
    local: &RecordOwner,
    remove_network: impl Future<Output = Result<(), NetworkError>>,
    remove_corrosion: impl Future<Output = Result<(), CorrosionError>>,
) -> Result<(), Error> {
    remove_network.await?;
    remove_corrosion.await?;
    Ok(local.mutate(|store| store.complete_reset()).await??)
}

async fn start_corrosion(
    data_dir: &Path,
    run_dir: &Path,
    local: &RecordOwner,
) -> Result<Option<RunningCorrosion>, Error> {
    let record = local.record();
    let machine = match record.body() {
        LocalMachineBody::Joining { machine, .. }
        | LocalMachineBody::Participating { machine, .. } => machine,
        LocalMachineBody::Uninitialized { .. } | LocalMachineBody::Resetting { .. } => {
            return Ok(None);
        }
    };
    let paths = CorrosionPaths::under(data_dir, run_dir);
    let bootstrap = record.bootstrap().into_iter().map(|machine| {
        SocketAddr::new(
            IpAddr::V6(machine.management_address().0),
            CORROSION_GOSSIP_PORT,
        )
    });
    // MACHINE_START_WAIT covers Corrosion image pull and wait_ready via EXTEND_TIMEOUT.
    let _extend = crate::systemd_timeout::extend_systemd_start_timeout("starting Corrosion");
    Ok(Some(
        CorrosionConfig::new(
            paths.data_dir,
            paths.run_dir,
            CORROSION_API_ADDRESS,
            SocketAddr::new(
                IpAddr::V6(machine.management_address().0),
                CORROSION_GOSSIP_PORT,
            ),
            DEFAULT_CONTAINER_NAME,
        )
        .with_bootstrap(bootstrap)
        .start()
        .await?,
    ))
}

enum StopKind {
    Plane,
    Signal(&'static str),
    Stop,
    Restart,
    RecordOwner,
    WatchFailed(&'static str),
}

fn notify(state: NotifyState<'_>) {
    if let Err(error) = sd_notify::notify(&[state]) {
        eprintln!(
            "systemd notification failed: {error}",
            error = ployz_core::error_chain::inline(&error),
        );
    }
}

#[cfg(test)]
mod tests {
    use std::{
        fs, io,
        path::{Path, PathBuf},
    };

    use crate::runtime_unit::UnitSupervision;
    use ployz_core::{
        CERTIFICATE_POLICY_CAPABILITY, DESCRIBE_CONTRACT_CAPABILITY, DescribeContractRequest,
        LIST_CONTAINERS_CAPABILITY, LeaveRequest, MachineRpcClient, op,
        startup_refusal::StartupRefusal,
    };
    use tonic::transport::Endpoint;

    use super::{
        ContainerMode, CorrosionError, Daemon, DaemonConfig, Error, InterruptedReset,
        LocalMachineStore, MachineApiSocket, ManagementConfig, NetworkError, Opened, StoreError,
        close_queued_if_refused, finish_interrupted_reset, finish_reset, record_refusal,
    };
    use crate::test_dir::TestDir;

    fn test_config(root: &Path, containers: ContainerMode) -> (DaemonConfig, PathBuf) {
        fs::create_dir_all(root).unwrap();
        let socket = root.join("run/ployz.sock");
        (
            DaemonConfig {
                data_dir: root.join("data"),
                socket: socket.clone(),
                dns_upstreams: Vec::new(),
                units: UnitSupervision::External,
                machine_api_address: None,
                containerd_socket: None,
                volume_plugin_socket: None,
                containers,
                management: test_management(),
            },
            socket,
        )
    }

    /// Ephemeral management port so parallel daemons never contend for the fixed one.
    fn test_management() -> ManagementConfig {
        ManagementConfig {
            relay: crate::management::Relay::Disabled,
            port: 0,
            ..ManagementConfig::default()
        }
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
    }

    async fn leave(path: &Path) -> ployz_core::Left {
        let channel = Endpoint::from_shared(format!("unix:{}", path.display()))
            .unwrap()
            .connect()
            .await
            .unwrap();
        let response = MachineRpcClient::new(channel)
            .leave(
                op::Leave::into_request(LeaveRequest::default())
                    .encode()
                    .unwrap(),
            )
            .await
            .unwrap()
            .into_inner()
            .decode_response()
            .unwrap();
        response.decode::<op::Leave>().unwrap()
    }

    async fn reset(daemon: &Daemon) {
        daemon
            .local
            .mutate(LocalMachineStore::begin_reset)
            .await
            .unwrap()
            .unwrap();
        daemon.local.request_restart();
    }

    #[tokio::test]
    async fn startup_recovers_an_abandoned_builder_without_a_build_request() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = TestDir::new("ployzd-builder-restart");
        let (config, socket) = test_config(&root.0, ContainerMode::Absent);
        fs::set_permissions(&root.0, fs::Permissions::from_mode(0o700)).unwrap();
        let policy = ployz_build::HostPolicy {
            state_directory: root.0.clone(),
            docker: root.0.join("docker"),
            ..Default::default()
        };
        let isolated_slot = u8::MAX;
        let builder = format!("{}-{isolated_slot}", ployz_build::builder_name());
        let marker = root.0.join(format!("{builder}.lock"));
        fs::write(&marker, "termination unconfirmed\n").unwrap();
        // Docker reports the abandoned worker as one Ployz made.
        fs::write(
            &policy.docker,
            format!(
                r#"#!/bin/sh
printf '%s\n' "$*" >> cleaned
[ "$1 $2" = 'inspect --type' ] && case "$6" in buildx_buildkit_*) printf '%s\n' '{{"Id":"abandoned","Config":{{"Image":"{}","Env":["PLOYZ_BUILD_OWNER={}"]}}}}' ;; esac
exit 0
"#,
                ployz_build::BUILDKIT_IMAGE,
                rustix::process::getuid().as_raw()
            ),
        )
        .unwrap();
        fs::set_permissions(&policy.docker, fs::Permissions::from_mode(0o700)).unwrap();
        let run_dir = config.socket.parent().unwrap().to_owned();
        let claimed = MachineApiSocket::claim(&config.socket).unwrap();
        let mut daemon = Daemon::start_with_build_policy(config, claimed, policy, run_dir)
            .await
            .unwrap();
        daemon
            .builds_cleaned
            .wait_for(|cleaned| *cleaned)
            .await
            .unwrap();
        assert!(
            fs::read_to_string(root.0.join("cleaned"))
                .unwrap()
                .lines()
                .any(|line| line == "rm --force abandoned")
        );
        assert!(fs::read_to_string(marker).unwrap().is_empty());
        assert!(
            describe(&socket)
                .await
                .supports(DESCRIBE_CONTRACT_CAPABILITY)
        );
        daemon.request_stop();
        daemon.wait().await.unwrap();
    }

    /// Workers that take long to stop never hold back the daemon's start,
    /// and so its READY: startup cleans them up in the background.
    #[tokio::test]
    async fn startup_reports_ready_without_waiting_for_workers_to_stop() {
        use std::os::unix::fs::PermissionsExt as _;
        let root = TestDir::new("ployzd-builder-slow-stop");
        let (config, _socket) = test_config(&root.0, ContainerMode::Absent);
        fs::set_permissions(&root.0, fs::Permissions::from_mode(0o700)).unwrap();
        let policy = ployz_build::HostPolicy {
            state_directory: root.0.clone(),
            docker: root.0.join("docker"),
            ..Default::default()
        };
        let builder = ployz_build::builder_name();
        let workers = [
            format!("buildx_buildkit_{builder}0"),
            format!("buildx_buildkit_{builder}-10"),
        ];
        // Each worker takes 11s to stop while `slow` exists.
        fs::write(root.0.join("slow"), "").unwrap();
        fs::write(
            &policy.docker,
            format!(
                r#"#!/bin/sh
root=$(dirname "$0")
printf '%s\n' "$*" >> "$root/calls"
case "$1 $2" in
  'ps --all') printf '%s\n' {first} {second} ;;
  'inspect --type') case "$6" in {first}|{second}) printf '%s\n' '{{"Id":"id-'"$6"'","Config":{{"Image":"{image}","Env":["PLOYZ_BUILD_OWNER={uid}"]}}}}' ;; esac ;;
  'stop --time') waited=0; while [ -f "$root/slow" ] && [ $waited -lt 110 ]; do sleep 0.1; waited=$((waited + 1)); done ;;
esac
exit 0
"#,
                first = workers[0],
                second = workers[1],
                image = ployz_build::BUILDKIT_IMAGE,
                uid = rustix::process::getuid().as_raw()
            ),
        )
        .unwrap();
        fs::set_permissions(&policy.docker, fs::Permissions::from_mode(0o700)).unwrap();
        let run_dir = config.socket.parent().unwrap().to_owned();
        let claimed = MachineApiSocket::claim(&config.socket).unwrap();
        let started = std::time::Instant::now();
        let mut daemon = Daemon::start_with_build_policy(config, claimed, policy, run_dir)
            .await
            .unwrap();
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "start waited on the workers' stop: {:?}",
            started.elapsed()
        );
        assert!(!*daemon.builds_cleaned.borrow());
        fs::remove_file(root.0.join("slow")).unwrap();
        daemon
            .builds_cleaned
            .wait_for(|cleaned| *cleaned)
            .await
            .unwrap();
        let calls = fs::read_to_string(root.0.join("calls")).unwrap();
        assert_eq!(
            calls
                .lines()
                .filter(|call| call.starts_with("stop "))
                .count(),
            2,
            "{calls}"
        );
        for worker in &workers {
            assert!(
                calls.contains(&format!("rm --force id-{worker}")),
                "{calls}"
            );
        }
        daemon.request_stop();
        daemon.wait().await.unwrap();
    }

    #[test]
    fn a_refused_start_leaves_its_refusal_and_a_failed_one_leaves_none() {
        let root = TestDir::new("ployzd-daemon-refusal-file");
        fs::create_dir_all(&root.0).unwrap();
        let refusal = StartupRefusal::new(&root.0);

        record_refusal(&refusal, &Error::Io(io::Error::other("transient")));
        assert_eq!(refusal.read(), None);

        record_refusal(
            &refusal,
            &Error::Network(NetworkError::WireGuardConflict {
                reason: "the existing device listen port differs from the mesh port",
                recovery: "delete ployz-wg",
            }),
        );
        assert_eq!(
            refusal.read().map(|read| read.to_string()).as_deref(),
            Some(
                "ployzd refused to start: refusing to change the retained WireGuard interface: the existing device listen port differs from the mesh port; safe recovery: delete ployz-wg"
            )
        );
    }

    #[test]
    fn a_refused_start_closes_queued_connections_and_a_failed_one_keeps_them() {
        use std::{io::Read as _, os::unix::net::UnixStream, time::Duration};

        let root = TestDir::new("ployzd-daemon-refused");
        let (config, socket) = test_config(&root.0, ContainerMode::Absent);
        let claimed = MachineApiSocket::claim(&config.socket).unwrap();
        let queue = || {
            let client = UnixStream::connect(&socket).unwrap();
            client
                .set_read_timeout(Some(Duration::from_millis(200)))
                .unwrap();
            client
        };
        let refusals = [
            NetworkError::WireGuardConflict {
                reason: "unrelated identity",
                recovery: "recovery",
            },
            NetworkError::DockerNetworkConflict {
                expected: String::new(),
                observed: String::new(),
                reason: "containers are attached".to_owned(),
                recovery: "recovery",
            },
        ];

        let mut kept = queue();
        close_queued_if_refused(&claimed.listener, &Error::Io(io::Error::other("transient")));
        assert!(
            kept.read(&mut [0; 1]).is_err(),
            "a transient failure leaves the connection for the next start"
        );
        for refusal in refusals {
            let mut refused = queue();
            close_queued_if_refused(&claimed.listener, &Error::Network(refusal));
            assert_eq!(refused.read(&mut [0; 1]).unwrap(), 0);
        }
        assert_eq!(kept.read(&mut [0; 1]).unwrap(), 0);
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

    /// A daemon start runs again the Close of a preview its record left Closing. This one
    /// has no Docker, so the Close stays Closing and the daemon says so.
    #[tokio::test]
    async fn a_daemon_start_resumes_a_close_its_record_left_closing() {
        let root = TestDir::new("ployzd-daemon-resume-close");
        let (config, _) = test_config(&root.0, ContainerMode::Absent);
        let machine = LocalMachineStore::open(&config.data_dir)
            .unwrap()
            .record()
            .id();
        let home = ployz_core::PreviewHome::of(&machine);
        let preview =
            ployz_core::Namespace::parse(format!("pv-0123456789abcdef-{home}-00c0ffee")).unwrap();
        let approvals = crate::machine::local_machine::ConsumptionLog::new(&config.data_dir);
        approvals.open_preview(&preview).unwrap().unwrap();
        approvals.begin_close(&preview).unwrap().unwrap();
        let (capture, _logging) = crate::diag::test_support::capture();

        let daemon = Daemon::start(config).await.unwrap();
        let resumed = tokio::time::timeout(std::time::Duration::from_secs(10), async {
            while !capture.text().contains("preview stays closing") {
                tokio::task::yield_now().await;
            }
        })
        .await;
        daemon.request_stop();
        daemon.wait().await.unwrap();

        assert!(resumed.is_ok(), "{}", capture.text());
        assert!(
            capture.text().contains(preview.as_str()),
            "{}",
            capture.text()
        );
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
            dns_upstreams: Vec::new(),
            units: UnitSupervision::External,
            machine_api_address: None,
            containerd_socket: None,
            volume_plugin_socket: None,
            containers: ContainerMode::Absent,
            management: test_management(),
        })
        .await
        .unwrap();
        let second = Daemon::start(DaemonConfig {
            data_dir: root.0.join("data-b"),
            socket,
            dns_upstreams: Vec::new(),
            units: UnitSupervision::External,
            machine_api_address: None,
            containerd_socket: None,
            volume_plugin_socket: None,
            containers: ContainerMode::Absent,
            management: test_management(),
        })
        .await;
        assert!(second.is_err());
        first.request_stop();
        first.wait().await.unwrap();
    }

    #[tokio::test]
    async fn restart_keeps_machine_id_and_leave_leaves_an_uninitialized_machine_alone() {
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
        assert_eq!(
            leave(&socket).await,
            ployz_core::Left::Left {
                phase: ployz_core::LeftPhase::Uninitialized,
                reset_warning: None,
            }
        );
        assert_eq!(describe(&socket).await.machine_id, first.machine_id);
        daemon.request_stop();
        daemon.wait().await.unwrap();
        assert!(data_dir.exists());
    }

    #[tokio::test]
    async fn a_panicking_record_mutation_shuts_the_daemon_down() {
        let root = TestDir::new("ployzd-daemon-owner-panic");
        let (config, _socket) = test_config(&root.0, ContainerMode::Absent);
        let daemon = Daemon::start(config).await.unwrap();
        let owner = daemon.local.clone();
        let waiting = tokio::spawn(daemon.wait());

        assert!(
            owner
                .mutate(|_| -> () { panic!("store invariant violated") })
                .await
                .is_err()
        );

        let error = tokio::time::timeout(std::time::Duration::from_secs(10), waiting)
            .await
            .expect("the daemon exits after its record owner stops")
            .unwrap()
            .unwrap_err();
        assert!(
            error.to_string().contains("record owner stopped"),
            "unexpected shutdown error: {error}"
        );
    }

    #[tokio::test]
    async fn reset_completes_while_a_client_holds_the_socket() {
        let root = TestDir::new("ployzd-daemon-reset-held");
        let (config, socket) = test_config(&root.0, ContainerMode::Absent);
        let data_dir = config.data_dir.clone();
        let daemon = Daemon::start(config).await.unwrap();
        let _held = tokio::net::UnixStream::connect(&socket).await.unwrap();
        reset(&daemon).await;
        tokio::time::timeout(std::time::Duration::from_secs(10), daemon.wait())
            .await
            .expect("reset shutdown must not wait on a held Machine API connection")
            .unwrap();
        assert!(!data_dir.exists());
    }

    /// Cloud sends Leave over the management transport, and its reply is Cloud's only
    /// proof of the reset: for the last Server nothing is left to ask afterwards.
    #[tokio::test(flavor = "multi_thread")]
    async fn a_leave_over_management_gets_its_reply_through_the_reset() {
        use ployz::connect::{Connector as _, ManagementRelay, SystemConnector};

        let (_relays, relay_url, _relay) = iroh::test_utils::run_relay_server().await.unwrap();
        let root = TestDir::new("ployzd-daemon-leave-reply");
        let (mut config, _) = test_config(&root.0, ContainerMode::Absent);
        config.management = ManagementConfig {
            relay: crate::management::Relay::Only(relay_url.clone()),
            port: 0,
            relay_tls: iroh::tls::CaTlsConfig::insecure_skip_verify(),
        };
        let daemon = Daemon::start(config).await.unwrap();
        // An approved Device's own connection, the Full caller a Leave over management needs.
        let client = iroh::SecretKey::generate();
        let device = ployz_core::DeviceKey::from_bytes(*client.public().as_bytes());
        let machine = daemon
            .local
            .mutate(move |store| {
                store
                    .approve_device(device)
                    .map(|_| store.record().management_secret().public_key())
            })
            .await
            .unwrap()
            .unwrap();
        let capability = ployz_core::ManagementCapability::new(machine, client.to_bytes());
        let channel = SystemConnector::new("/ployz-missing-ssh-client")
            .with_management_relay(ManagementRelay::custom(
                relay_url,
                iroh::tls::CaTlsConfig::insecure_skip_verify(),
            ))
            .connect(
                &ployz::context::Connection::management(capability.to_secret_string()).unwrap(),
            )
            .await
            .unwrap();
        daemon
            .local
            .mutate(LocalMachineStore::begin_reset)
            .await
            .unwrap()
            .unwrap();
        let waiting = tokio::spawn(daemon.wait());

        let left = tokio::time::timeout(
            std::time::Duration::from_secs(20),
            MachineRpcClient::new(channel).leave(
                op::Leave::into_request(LeaveRequest::default())
                    .encode()
                    .unwrap(),
            ),
        )
        .await
        .expect("the Leave reply must not wait out the connection's idle timeout")
        .expect("the reset must not drop the Leave reply")
        .into_inner()
        .decode_response()
        .unwrap();
        assert_eq!(
            left.decode::<op::Leave>().unwrap(),
            ployz_core::Left::Left {
                phase: ployz_core::LeftPhase::Resetting,
                reset_warning: None,
            }
        );
        waiting.await.unwrap().unwrap();
    }

    fn interrupted_reset(data_dir: &Path) -> (ployz_core::MachineId, InterruptedReset) {
        let mut store = LocalMachineStore::open(data_dir).unwrap();
        let id = store.record().id();
        store.begin_reset().unwrap();
        drop(store);
        match LocalMachineStore::open_with_admission(data_dir, data_dir.join(".run")).unwrap() {
            Opened::Resetting(interrupted) => (id, interrupted),
            Opened::Ready(_) => panic!("a begun reset must reopen as Resetting"),
        }
    }

    #[tokio::test]
    async fn interrupted_reset_removes_corrosion_before_clearing_the_data_dir() {
        let root = TestDir::new("ployzd-interrupted-reset");
        let data_dir = root.0.join("data");
        let (old_id, interrupted) = interrupted_reset(&data_dir);
        let machine_json = data_dir.join("machine.json");

        let store = finish_interrupted_reset(
            interrupted,
            async {
                assert!(
                    machine_json.exists(),
                    "networking is removed before the record"
                );
                Ok(())
            },
            async {
                assert!(
                    machine_json.exists(),
                    "Corrosion is removed before the record"
                );
                Ok(())
            },
        )
        .await
        .unwrap();

        assert_ne!(store.record().id(), old_id);
        assert_eq!(
            store.record().phase(),
            ployz_core::LocalMachinePhase::Uninitialized
        );
    }

    /// A reset killed while it clears the data directory, as systemd does when
    /// the reset outlives the unit's stop timeout, must leave a directory the
    /// next start still recognises as a reset to finish.
    #[tokio::test]
    async fn data_dir_cleanup_that_stops_partway_keeps_the_reset_for_retry() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = TestDir::new("ployzd-partial-reset-cleanup");
        let data_dir = root.0.join("data");
        let mut store = LocalMachineStore::open(&data_dir).unwrap();
        let old_id = store.record().id();
        for n in 0..8 {
            let dir = data_dir.join(format!("state-{n}"));
            fs::create_dir(&dir).unwrap();
            fs::write(dir.join("rows"), b"rows").unwrap();
        }
        store.begin_reset().unwrap();
        drop(store);
        let Opened::Resetting(interrupted) =
            LocalMachineStore::open_with_admission(&data_dir, data_dir.join(".run")).unwrap()
        else {
            panic!("a begun reset must reopen as Resetting");
        };
        // Stop the cleanup at an entry the directory lists after the record where
        // there is one: a cleanup in listing order would already have removed the
        // record there.
        let listed: Vec<_> = fs::read_dir(&data_dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        let state = |name: &&std::ffi::OsString| name.to_string_lossy().starts_with("state-");
        let stuck = listed
            .iter()
            .skip_while(|name| *name != "machine.json")
            .find(state)
            .or_else(|| listed.iter().find(state))
            .map(|name| data_dir.join(name))
            .unwrap();
        fs::set_permissions(&stuck, fs::Permissions::from_mode(0o500)).unwrap();
        if fs::write(stuck.join("probe"), b"").is_ok() {
            eprintln!(
                "skipped: root ignores directory permissions, so nothing can stop the cleanup"
            );
            fs::set_permissions(&stuck, fs::Permissions::from_mode(0o700)).unwrap();
            return;
        }

        let result =
            finish_interrupted_reset(interrupted, async { Ok(()) }, async { Ok(()) }).await;

        assert!(
            result.is_err(),
            "the cleanup stopped at {}",
            stuck.display()
        );
        let Opened::Resetting(interrupted) =
            LocalMachineStore::open_with_admission(&data_dir, data_dir.join(".run")).unwrap()
        else {
            panic!("a partly cleared data directory must reopen as Resetting");
        };
        assert_eq!(interrupted.record().id(), old_id);
        fs::set_permissions(&stuck, fs::Permissions::from_mode(0o700)).unwrap();
        let store = finish_interrupted_reset(interrupted, async { Ok(()) }, async { Ok(()) })
            .await
            .unwrap();
        assert_ne!(store.record().id(), old_id);
        assert_eq!(
            store.record().phase(),
            ployz_core::LocalMachinePhase::Uninitialized
        );
    }

    #[tokio::test]
    async fn failed_network_removal_keeps_interrupted_reset_identity_for_retry() {
        let root = TestDir::new("ployzd-interrupted-network-reset-retry");
        let data_dir = root.0.join("data");
        let (old_id, interrupted) = interrupted_reset(&data_dir);
        let result = finish_interrupted_reset(
            interrupted,
            async {
                Err(NetworkError::WireGuardConflict {
                    reason: "unrelated identity",
                    recovery: "recovery",
                })
            },
            async { Ok(()) },
        )
        .await;
        assert!(matches!(result, Err(Error::Network(_))));
        let record: crate::machine::LocalMachineRecord =
            serde_json::from_slice(&fs::read(data_dir.join("machine.json")).unwrap()).unwrap();
        assert_eq!(record.id(), old_id);
        assert_eq!(record.phase(), ployz_core::LocalMachinePhase::Resetting);
        let Opened::Resetting(interrupted) =
            LocalMachineStore::open_with_admission(&data_dir, data_dir.join(".run")).unwrap()
        else {
            panic!("network cleanup failure must remain retryable");
        };
        let store = finish_interrupted_reset(
            interrupted,
            async {
                assert!(data_dir.join("machine.json").exists());
                Ok(())
            },
            async { Ok(()) },
        )
        .await
        .unwrap();
        assert_ne!(store.record().id(), old_id);
        assert_eq!(
            store.record().phase(),
            ployz_core::LocalMachinePhase::Uninitialized
        );
    }

    #[tokio::test]
    async fn failed_network_removal_keeps_live_reset_identity_for_retry() {
        let root = TestDir::new("ployzd-live-network-reset-retry");
        let data_dir = root.0.join("data");
        let mut store = LocalMachineStore::open(&data_dir).unwrap();
        let old_id = store.record().id();
        store.begin_reset().unwrap();
        let owner = crate::machine::RecordOwner::spawn(store).unwrap();
        let result = finish_reset(
            &owner,
            async {
                Err(NetworkError::Io(io::Error::other(
                    "Docker network still attached",
                )))
            },
            async { Ok(()) },
        )
        .await;
        assert!(matches!(result, Err(Error::Network(_))));
        let record: crate::machine::LocalMachineRecord =
            serde_json::from_slice(&fs::read(data_dir.join("machine.json")).unwrap()).unwrap();
        assert_eq!(record.id(), old_id);
        assert_eq!(record.phase(), ployz_core::LocalMachinePhase::Resetting);
    }

    #[tokio::test]
    async fn failed_corrosion_removal_keeps_the_reset_for_retry() {
        let root = TestDir::new("ployzd-interrupted-reset-retry");
        let data_dir = root.0.join("data");
        let (old_id, interrupted) = interrupted_reset(&data_dir);

        let result = finish_interrupted_reset(interrupted, async { Ok(()) }, async {
            Err(CorrosionError::Api("docker unavailable".into()))
        })
        .await;

        assert!(matches!(result, Err(Error::Corrosion(_))));
        assert!(matches!(
            LocalMachineStore::open(&data_dir),
            Err(StoreError::ResetInterrupted(_))
        ));
        let persisted: crate::machine::LocalMachineRecord =
            serde_json::from_slice(&fs::read(data_dir.join("machine.json")).unwrap()).unwrap();
        assert_eq!(persisted.id(), old_id);
        assert_eq!(persisted.phase(), ployz_core::LocalMachinePhase::Resetting);
    }

    #[tokio::test]
    async fn failed_corrosion_removal_keeps_a_live_reset_for_retry() {
        let root = TestDir::new("ployzd-live-reset-retry");
        let data_dir = root.0.join("data");
        let mut store = LocalMachineStore::open(&data_dir).unwrap();
        let old_id = store.record().id();
        store.begin_reset().unwrap();
        let owner = crate::machine::RecordOwner::spawn(store).unwrap();

        let result = finish_reset(&owner, async { Ok(()) }, async {
            Err(CorrosionError::Api("docker unavailable".into()))
        })
        .await;

        assert!(matches!(result, Err(Error::Corrosion(_))));
        let persisted: crate::machine::LocalMachineRecord =
            serde_json::from_slice(&fs::read(data_dir.join("machine.json")).unwrap()).unwrap();
        assert_eq!(persisted.id(), old_id);
        assert_eq!(persisted.phase(), ployz_core::LocalMachinePhase::Resetting);
    }
}
