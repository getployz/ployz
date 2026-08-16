#[cfg(not(target_os = "linux"))]
compile_error!("ployzd supports Linux only");

use std::{
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use clap::{Parser, Subcommand};
use ployzd::{
    daemon::{ContainerMode, Daemon, DaemonConfig, Error, wait_until_socket_accepts},
    machine::DEFAULT_DATA_DIR,
};
use sd_notify::NotifyState;
use tokio::io::{AsyncWriteExt, copy, stdin, stdout};

const DEFAULT_SOCKET_PATH: &str = "/run/ployz/ployz.sock";
const DEFAULT_METRICS_ADDRESS: &str = "127.0.0.1:51090";
const DIAL_STDIO_SOCKET_TIMEOUT: Duration = Duration::from_secs(20);

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
async fn main() -> ExitCode {
    match run().await {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            ExitCode::FAILURE
        }
    }
}

async fn run() -> Result<(), Error> {
    let args = Args::parse();
    if matches!(args.command, Some(Command::Version)) {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if matches!(args.command, Some(Command::DialStdio)) {
        return dial_stdio(&args.socket).await.map_err(Error::from);
    }
    let daemon = Daemon::start(DaemonConfig {
        data_dir: args.data_dir,
        socket: args.socket,
        metrics_address: args.metrics_address,
        dns_upstreams: args.dns_upstreams,
        machine_api_address: args.machine_api_address,
        containerd_socket: args.containerd_socket,
        containers: ContainerMode::Auto,
    })
    .await?;
    notify(NotifyState::Ready);
    daemon.wait().await
}

async fn dial_stdio(path: &Path) -> io::Result<()> {
    let stream = wait_until_socket_accepts(path, DIAL_STDIO_SOCKET_TIMEOUT).await?;
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

fn notify(state: NotifyState<'_>) {
    if let Err(error) = sd_notify::notify(&[state]) {
        eprintln!("systemd notification failed: {error}");
    }
}
