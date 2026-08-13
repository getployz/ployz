#[cfg(not(target_os = "linux"))]
compile_error!("ployzd supports Linux only");

use std::{
    error::Error,
    fs, io,
    net::SocketAddr,
    os::unix::fs::{FileTypeExt, MetadataExt, PermissionsExt, chown},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use clap::Parser;
use ployz_core::{LocalMachinePhase, MachineRpcServer};
use ployzd::{
    machine::{DEFAULT_DATA_DIR, StateStore},
    metrics,
    rpc::MachineService,
};
use sd_notify::NotifyState;
use tokio::{
    net::{TcpListener, UnixListener},
    signal::unix::{SignalKind, signal},
    sync::watch,
};
use tokio_stream::wrappers::UnixListenerStream;
use tonic::transport::Server;

const DEFAULT_SOCKET_PATH: &str = "/run/ployz/ployz.sock";
const DEFAULT_METRICS_ADDRESS: &str = "127.0.0.1:51090";

#[derive(Parser)]
#[command(about = "Ployz Machine daemon")]
struct Args {
    #[arg(short, long, default_value = DEFAULT_DATA_DIR)]
    data_dir: PathBuf,
    #[arg(long, default_value = DEFAULT_SOCKET_PATH)]
    socket: PathBuf,
    #[arg(long, default_value = DEFAULT_METRICS_ADDRESS)]
    metrics_address: SocketAddr,
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    let args = Args::parse();
    let state = Arc::new(Mutex::new(StateStore::open(&args.data_dir)?));
    let rpc_listener = bind_socket(&args.socket)?;
    let metrics_listener = TcpListener::bind(args.metrics_address).await?;
    let registry = metrics::registry(env!("CARGO_PKG_VERSION"))?;
    let (shutdown, shutdown_rx) = watch::channel(false);
    let (reset, mut reset_rx) = watch::channel(false);
    let service = MachineRpcServer::new(MachineService::new(Arc::clone(&state), reset));

    let rpc = Server::builder()
        .add_service(service)
        .serve_with_incoming_shutdown(
            UnixListenerStream::new(rpc_listener),
            wait_for_shutdown(shutdown_rx.clone()),
        );
    let metrics = metrics::serve(metrics_listener, registry, shutdown_rx);
    let servers = async {
        tokio::try_join!(async { rpc.await.map_err(io::Error::other) }, metrics).map(|_| ())
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

    let store = state
        .lock()
        .map_err(|_| io::Error::other("machine state lock poisoned"))?;
    if store.state().phase == LocalMachinePhase::Resetting {
        store.clear()?;
    }
    Ok(())
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

fn bind_socket(path: &Path) -> io::Result<UnixListener> {
    let parent = path
        .parent()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "socket path has no parent"))?;
    fs::create_dir_all(parent)?;
    fs::set_permissions(parent, fs::Permissions::from_mode(0o750))?;
    set_socket_group(parent)?;

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
    Ok(listener)
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
