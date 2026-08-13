#[cfg(not(target_os = "linux"))]
compile_error!("ployzd supports Linux only");

use std::{
    error::Error,
    fs::{self, File, OpenOptions},
    io,
    net::{IpAddr, SocketAddr},
    os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt, PermissionsExt, chown},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use clap::{Parser, Subcommand};
use ployz_core::{LocalMachinePhase, MachineRpcServer};
use ployzd::{
    corrosion::{
        CorrosionConfig, DEFAULT_API_ADDRESS, DEFAULT_CONTAINER_NAME, RunningCorrosion,
        run_machine_publisher,
    },
    docker::{ContainerObserver, LocalDocker, MachineSpecStore},
    machine::{DEFAULT_DATA_DIR, LocalMachineStore},
    metrics,
    network::{CORROSION_GOSSIP_PORT, NetworkPlane},
    rpc::MachineService,
};
use sd_notify::NotifyState;
use tokio::{
    net::{TcpListener, UnixListener},
    signal::unix::{SignalKind, signal},
    sync::watch,
};
use tokio_stream::wrappers::{TcpListenerStream, UnixListenerStream};
use tonic::transport::Server;

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
}

#[derive(Subcommand)]
enum Command {
    /// Print the daemon version.
    Version,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    if matches!(args.command, Some(Command::Version)) {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    let store = Arc::new(Mutex::new(LocalMachineStore::open(&args.data_dir)?));
    let (rpc_listener, _socket_lock) = bind_socket(&args.socket)?;
    let metrics_listener = TcpListener::bind(args.metrics_address).await?;
    let mut network = {
        let record = store
            .lock()
            .map_err(|_| io::Error::other("local Machine record lock poisoned"))?
            .record()
            .clone();
        NetworkPlane::start(&record).await?
    };
    let machine_api_listeners = if let Some(network) = &network {
        let [management, gateway] = network.machine_api_addresses()?;
        Some((
            TcpListener::bind(management).await?,
            TcpListener::bind(gateway).await?,
        ))
    } else {
        None
    };
    let mut corrosion = start_corrosion(&args, &store).await?;
    let replicated_store = corrosion.as_ref().map(|running| running.store().clone());
    let specs = MachineSpecStore::open(args.data_dir.join("machine.db")).await?;
    let observer = replicated_store
        .clone()
        .map(|replicated| {
            let machine_id = store
                .lock()
                .expect("local Machine record lock was checked above")
                .record()
                .id
                .clone();
            LocalDocker::connect()
                .map(|docker| ContainerObserver::new(docker, specs, replicated, machine_id))
        })
        .transpose()?;
    let registry = metrics::registry(env!("CARGO_PKG_VERSION"))?;
    let (shutdown, shutdown_rx) = watch::channel(false);
    let (reset, mut reset_rx) = watch::channel(false);
    let service = MachineService::new(Arc::clone(&store), reset);

    let rpc = Server::builder()
        .add_service(MachineRpcServer::new(service.clone()))
        .serve_with_incoming_shutdown(
            UnixListenerStream::new(rpc_listener),
            wait_for_shutdown(shutdown_rx.clone()),
        );
    let metrics = metrics::serve(metrics_listener, registry, shutdown_rx.clone());
    let publisher = run_machine_publisher(
        replicated_store.clone(),
        Arc::clone(&store),
        shutdown_rx.clone(),
    );
    let network_rpc = async {
        if let Some((management, gateway)) = machine_api_listeners {
            let management = Server::builder()
                .add_service(MachineRpcServer::new(service.clone()))
                .serve_with_incoming_shutdown(
                    TcpListenerStream::new(management),
                    wait_for_shutdown(shutdown_rx.clone()),
                );
            let gateway = Server::builder()
                .add_service(MachineRpcServer::new(service))
                .serve_with_incoming_shutdown(
                    TcpListenerStream::new(gateway),
                    wait_for_shutdown(shutdown_rx.clone()),
                );
            tokio::try_join!(management, gateway)
                .map(|_| ())
                .map_err(io::Error::other)
        } else {
            wait_for_shutdown(shutdown_rx.clone()).await;
            Ok(())
        }
    };
    let network_runner = async {
        if let Some(network) = &mut network {
            network
                .run(replicated_store, Arc::clone(&store), shutdown_rx.clone())
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
    let servers = async {
        tokio::try_join!(
            async { rpc.await.map_err(io::Error::other) },
            metrics,
            publisher,
            network_rpc,
            network_runner,
            observer,
        )
        .map(|_| ())
    };
    tokio::pin!(servers);

    notify(NotifyState::Ready);
    tokio::select! {
        result = &mut servers => result?,
        result = shutdown_signal() => result?,
        changed = reset_rx.changed() => {
            changed?;
        }
    }
    shutdown.send_replace(true);
    notify(NotifyState::Stopping);
    // TODO(UT-098): preserve the baseline's unbounded graceful shutdown until a timeout is explicitly chosen.
    servers.await?;

    let resetting = store
        .lock()
        .map_err(|_| io::Error::other("local Machine record lock poisoned"))?
        .record()
        .phase
        == LocalMachinePhase::Resetting;
    if let Some(running) = &mut corrosion {
        if resetting {
            running.cleanup().await?;
        } else {
            running.stop().await?;
        }
    }
    if resetting {
        let store = store
            .lock()
            .map_err(|_| io::Error::other("local Machine record lock poisoned"))?;
        store.complete_reset()?;
    }
    Ok(())
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
        .start()
        .await?,
    ))
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<bool>) {
    if !*shutdown.borrow() {
        let _ = shutdown.changed().await;
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
        set_socket_group(parent)?;
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
    set_socket_group(path)?;
    Ok((listener, lock))
}

fn set_socket_group(path: &Path) -> io::Result<()> {
    let metadata = fs::metadata(path)?;
    let gid = if fs::metadata("/proc/self")?.uid() == 0 {
        group_gid("ployz").unwrap_or(0)
    } else {
        metadata.gid()
    };
    chown(path, None, Some(gid))
}

fn group_gid(name: &str) -> Option<u32> {
    fs::read_to_string("/etc/group")
        .ok()?
        .lines()
        .find_map(|line| {
            let mut fields = line.split(':');
            (fields.next()? == name)
                .then(|| fields.nth(1)?.parse().ok())
                .flatten()
        })
}

fn notify(state: NotifyState<'_>) {
    if let Err(error) = sd_notify::notify(&[state]) {
        eprintln!("systemd notification failed: {error}");
    }
}
