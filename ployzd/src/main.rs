#[cfg(not(target_os = "linux"))]
compile_error!("ployzd supports Linux only");

use std::{
    error::Error,
    fs::{self, File, OpenOptions},
    io,
    net::{IpAddr, SocketAddr},
    os::unix::fs::{FileTypeExt, OpenOptionsExt, PermissionsExt},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use clap::{Parser, Subcommand};
use ployz_core::{LocalMachinePhase, MachineRpcServer};
use ployzd::{
    caddy,
    corrosion::{
        CorrosionConfig, DEFAULT_API_ADDRESS, DEFAULT_CONTAINER_NAME, RunningCorrosion,
        run_machine_publisher_with_restart,
    },
    dns,
    docker::{ContainerObserver, LocalDocker, MachineSpecStore},
    filesystem::set_ployz_group,
    machine::{DEFAULT_DATA_DIR, LocalMachineStore},
    metrics,
    network::{CORROSION_GOSSIP_PORT, MACHINE_API_PORT, NetworkPlane, machine_gateway},
    proxy::MachineProxy,
    rpc::MachineService,
};
use sd_notify::NotifyState;
use tokio::{
    io::{AsyncWriteExt, copy, stdin, stdout},
    net::{TcpListener, UnixListener},
    signal::unix::{SignalKind, signal},
    sync::watch,
};
use tokio_stream::wrappers::{TcpListenerStream, UnixListenerStream};
use tonic::{service::Routes, transport::Server};

const DEFAULT_SOCKET_PATH: &str = "/run/ployz/ployz.sock";
const DEFAULT_METRICS_ADDRESS: &str = "127.0.0.1:51090";

#[derive(Parser)]
#[command(about = "Ployz Machine daemon")]
struct Args {
    #[command(subcommand)]
    command: Option<Command>,
    #[arg(short, long, default_value = DEFAULT_DATA_DIR)]
    data_dir: PathBuf,
    #[arg(long, default_value = DEFAULT_SOCKET_PATH)]
    socket: PathBuf,
    #[arg(long, default_value = DEFAULT_METRICS_ADDRESS)]
    metrics_address: SocketAddr,
    #[arg(long = "dns-upstream", value_name = "ADDR")]
    dns_upstreams: Vec<SocketAddr>,
    #[arg(long, hide = true)]
    machine_api_address: Option<SocketAddr>,
    #[arg(long)]
    containerd_socket: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Print the daemon version.
    Version,
    /// Bridge standard input/output to the local Machine API socket.
    #[command(hide = true)]
    DialStdio,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    if matches!(args.command, Some(Command::Version)) {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if matches!(args.command, Some(Command::DialStdio)) {
        return dial_stdio(&args.socket).await.map_err(Into::into);
    }
    let store = Arc::new(Mutex::new(LocalMachineStore::open(&args.data_dir)?));
    let (rpc_listener, _socket_lock) = bind_socket(&args.socket)?;
    let metrics_listener = TcpListener::bind(args.metrics_address).await?;
    let local_record = store
        .lock()
        .map_err(|_| io::Error::other("local Machine record lock poisoned"))?
        .record()
        .clone();
    let mut network = NetworkPlane::start(&local_record).await?;
    let machine_api_listeners = if args.machine_api_address.is_none()
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
    let explicit_machine_api_listener = match args.machine_api_address {
        Some(address) => Some(TcpListener::bind(address).await?),
        None => None,
    };
    let dns_upstreams = (!args.dns_upstreams.is_empty()).then(|| args.dns_upstreams.clone());
    let specs = MachineSpecStore::open(args.data_dir.join("machine.db")).await?;
    let docker = match LocalDocker::connect() {
        Ok(docker) => Some(docker),
        Err(error) => {
            eprintln!("WARNING: local Docker is unavailable: {error}");
            None
        }
    };
    let unregistry_gateway = if local_record.phase == LocalMachinePhase::Participating {
        local_record
            .machine
            .as_ref()
            .map(|machine| machine_gateway(machine.subnet).map(|gateway| gateway.0))
            .transpose()?
    } else {
        None
    };
    let registry = metrics::registry(env!("CARGO_PKG_VERSION"))?;
    let mut corrosion = start_corrosion(&args, &store).await?;
    let replicated_store = corrosion.as_ref().map(|running| running.store().clone());
    let unregistry = match (&docker, unregistry_gateway) {
        (Some(docker), Some(gateway)) => {
            match docker
                .start_unregistry(gateway, args.containerd_socket.as_deref())
                .await
            {
                Ok(running) => running.map(Arc::new),
                Err(error) => {
                    eprintln!("WARNING: unregistry disabled: {error}");
                    None
                }
            }
        }
        _ => None,
    };
    let observer = replicated_store.clone().and_then(|replicated| {
        docker.clone().map(|docker| {
            ContainerObserver::new(
                docker,
                specs.clone(),
                replicated,
                Arc::clone(&store),
                local_record.id.clone(),
            )
        })
    });
    let (shutdown, shutdown_rx) = watch::channel(false);
    let (participating, participating_rx) =
        watch::channel(local_record.phase == LocalMachinePhase::Participating);
    let (reset, mut reset_rx) = watch::channel(false);
    let caddyfile = caddy::caddyfile_path(&args.data_dir);
    let caddy_admin_socket = args
        .socket
        .parent()
        .unwrap_or_else(|| Path::new("/run/ployz"))
        .join("caddy/admin.sock");
    let service = MachineService::with_cluster(
        Arc::clone(&store),
        reset.clone(),
        corrosion
            .as_ref()
            .map(|running| (running.store().clone(), running.admin_client())),
    )
    .with_optional_containers(docker, specs)
    .with_caddyfile(caddyfile.clone());
    let proxy = MachineProxy::new(
        Routes::new(MachineRpcServer::new(service)),
        local_record.id.clone(),
        MACHINE_API_PORT,
        replicated_store.clone(),
    );

    let rpc = Server::builder().serve_with_incoming_shutdown(
        proxy.clone(),
        UnixListenerStream::new(rpc_listener),
        wait_for_shutdown(shutdown_rx.clone()),
    );
    let metrics = metrics::serve(metrics_listener, registry, shutdown_rx.clone());
    let publisher = run_machine_publisher_with_restart(
        replicated_store.clone(),
        Arc::clone(&store),
        participating,
        reset.clone(),
        shutdown_rx.clone(),
    );
    let (management_listener, gateway_listener) = machine_api_listeners
        .map_or((None, None), |(management, gateway)| {
            (Some(management), Some(gateway))
        });
    let network_rpc = async {
        tokio::try_join!(
            serve_machine_api(
                explicit_machine_api_listener,
                proxy.clone(),
                shutdown_rx.clone()
            ),
            serve_machine_api(management_listener, proxy.clone(), shutdown_rx.clone()),
            serve_machine_api(gateway_listener, proxy.clone(), shutdown_rx.clone()),
        )
        .map(|_| ())
    };
    let network_runner = async {
        if let Some(network) = &mut network {
            network
                .run(
                    replicated_store.clone(),
                    Arc::clone(&store),
                    shutdown_rx.clone(),
                )
                .await
        } else {
            wait_for_shutdown(shutdown_rx.clone()).await;
            Ok(())
        }
    };
    let observer = async {
        match observer {
            Some(observer) => observer
                .run(shutdown_rx.clone())
                .await
                .map_err(io::Error::other),
            None => {
                wait_for_shutdown(shutdown_rx.clone()).await;
                Ok(())
            }
        }
    };
    let dns = async {
        if !wait_for_participation(participating_rx.clone(), shutdown_rx.clone()).await? {
            return Ok(());
        }
        match (local_record.machine.clone(), replicated_store.clone()) {
            (Some(machine), Some(replicated)) => {
                dns::run(machine, replicated, dns_upstreams, shutdown_rx.clone()).await
            }
            _ => {
                wait_for_shutdown(shutdown_rx.clone()).await;
                Ok(())
            }
        }
    };
    let caddy = async {
        if !wait_for_participation(participating_rx.clone(), shutdown_rx.clone()).await? {
            return Ok(());
        }
        match (local_record.machine.clone(), replicated_store.clone()) {
            (Some(machine), Some(replicated)) => {
                caddy::run(
                    machine,
                    replicated,
                    caddyfile,
                    caddy_admin_socket,
                    shutdown_rx.clone(),
                )
                .await
            }
            _ => {
                wait_for_shutdown(shutdown_rx.clone()).await;
                Ok(())
            }
        }
    };
    let unregistry_refresh = {
        let running = unregistry.clone();
        let shutdown = shutdown_rx.clone();
        async move {
            match running.as_deref() {
                Some(running) => running
                    .keep_socket_current(shutdown)
                    .await
                    .map_err(io::Error::other),
                None => {
                    wait_for_shutdown(shutdown).await;
                    Ok(())
                }
            }
        }
    };
    let servers = async {
        tokio::try_join!(
            async { rpc.await.map_err(io::Error::other) },
            metrics,
            publisher,
            network_rpc,
            network_runner,
            observer,
            dns,
            caddy,
            unregistry_refresh,
        )
        .map(|_| ())
    };
    tokio::pin!(servers);

    notify(NotifyState::Ready);
    let mut completed_servers = None;
    let trigger_error = tokio::select! {
        result = &mut servers => {
            completed_servers = Some(result);
            None
        },
        result = shutdown_signal() => result.err().map(|error| error.to_string()),
        changed = reset_rx.changed() => changed.err().map(|error| error.to_string()),
    };
    notify(NotifyState::Stopping);
    let mut errors = trigger_error.into_iter().collect::<Vec<_>>();
    let resetting = match store.lock() {
        Ok(store) => store.record().phase == LocalMachinePhase::Resetting,
        Err(_) => {
            errors.push("local Machine record lock poisoned".into());
            false
        }
    };
    shutdown.send_replace(true);
    // TODO(UT-098, UT-099): preserve both API servers' unbounded graceful shutdown
    // until a timeout is explicitly chosen.
    let server_result = match completed_servers {
        Some(result) => result,
        None => servers.await,
    };
    if let Err(error) = server_result {
        errors.push(error.to_string());
    }

    if let Some(running) = &mut corrosion {
        let result = if resetting {
            running.cleanup().await
        } else {
            running.stop().await
        };
        if let Err(error) = result {
            errors.push(error.to_string());
        }
    }
    if let Some(running) = &unregistry {
        let result = if resetting {
            running.cleanup().await
        } else {
            running.stop().await
        };
        if let Err(error) = result {
            errors.push(error.to_string());
        }
    }
    if resetting {
        match store.lock() {
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

async fn serve_machine_api(
    listener: Option<TcpListener>,
    proxy: MachineProxy,
    shutdown: watch::Receiver<bool>,
) -> io::Result<()> {
    match listener {
        Some(listener) => Server::builder()
            .serve_with_incoming_shutdown(
                proxy,
                TcpListenerStream::new(listener),
                wait_for_shutdown(shutdown),
            )
            .await
            .map_err(io::Error::other),
        None => {
            wait_for_shutdown(shutdown).await;
            Ok(())
        }
    }
}

async fn dial_stdio(path: &Path) -> io::Result<()> {
    let stream = tokio::net::UnixStream::connect(path).await?;
    let (mut socket_read, mut socket_write) = stream.into_split();
    let input = async {
        copy(&mut stdin(), &mut socket_write).await?;
        socket_write.shutdown().await
    };
    let output = async {
        copy(&mut socket_read, &mut stdout()).await?;
        stdout().flush().await
    };
    tokio::try_join!(input, output).map(|_| ())
}

async fn start_corrosion(
    args: &Args,
    store: &Arc<Mutex<LocalMachineStore>>,
) -> Result<Option<RunningCorrosion>, Box<dyn Error>> {
    let record = store
        .lock()
        .map_err(|_| io::Error::other("local Machine record lock poisoned"))?
        .record()
        .clone();
    let phase = record.phase;
    if !matches!(
        phase,
        LocalMachinePhase::Joining | LocalMachinePhase::Participating
    ) {
        return Ok(None);
    }
    let Some(machine) = record.machine else {
        return Ok(None);
    };
    let run_dir = args
        .socket
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket path has no parent"))?
        .join("corrosion");
    let bootstrap = record.bootstrap_machines.iter().map(|machine| {
        SocketAddr::new(
            IpAddr::V6(machine.management_address.0),
            CORROSION_GOSSIP_PORT,
        )
    });
    Ok(Some(
        CorrosionConfig::new(
            args.data_dir.join("corrosion"),
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

async fn wait_for_shutdown(mut shutdown: watch::Receiver<bool>) {
    if !*shutdown.borrow() {
        let _ = shutdown.changed().await;
    }
}

async fn wait_for_participation(
    mut participating: watch::Receiver<bool>,
    shutdown: watch::Receiver<bool>,
) -> io::Result<bool> {
    let mut shutdown_wait = shutdown.clone();
    tokio::select! {
        biased;
        changed = shutdown_wait.wait_for(|shutdown| *shutdown) => {
            changed.map_err(io::Error::other)?;
            Ok(false)
        }
        changed = participating.wait_for(|participating| *participating) => {
            match changed {
                Ok(_) => Ok(!*shutdown.borrow()),
                Err(_) if *shutdown.borrow() => Ok(false),
                Err(error) => Err(io::Error::other(error)),
            }
        }
    }
}

async fn shutdown_signal() -> io::Result<()> {
    let mut interrupt = signal(SignalKind::interrupt())?;
    let mut terminate = signal(SignalKind::terminate())?;
    tokio::select! {
        _ = interrupt.recv() => {}
        _ = terminate.recv() => {}
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn participation_gate_waits_for_catch_up_and_obeys_shutdown() {
        let (participating, participating_rx) = watch::channel(false);
        let (_shutdown, shutdown_rx) = watch::channel(false);
        let waiting = wait_for_participation(participating_rx, shutdown_rx);
        tokio::pin!(waiting);
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(10), &mut waiting)
                .await
                .is_err()
        );
        participating.send_replace(true);
        assert!(waiting.await.unwrap());

        let (participating, participating_rx) = watch::channel(true);
        let (shutdown, shutdown_rx) = watch::channel(false);
        shutdown.send_replace(true);
        drop(participating);
        assert!(
            !wait_for_participation(participating_rx, shutdown_rx)
                .await
                .unwrap()
        );
    }
}
