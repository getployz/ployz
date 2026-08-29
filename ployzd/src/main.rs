#[cfg(not(target_os = "linux"))]
compile_error!("ployzd supports Linux only");

mod volume_plugin;

use std::{
    io,
    net::SocketAddr,
    path::{Path, PathBuf},
    process::ExitCode,
    time::Duration,
};

use clap::{Parser, Subcommand};
use ployz_core::DOCKER_NETWORK_CONFLICT_EXIT_STATUS;
use ployzd::{
    daemon::{ContainerMode, Daemon, DaemonConfig, Error, wait_until_socket_accepts},
    diag,
    machine::DEFAULT_DATA_DIR,
    network::NetworkError,
};
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
    /// Tracing filter. Overrides `PLOYZ_LOG`. Default: info.
    #[arg(long, value_name = "FILTER")]
    log_level: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Print the daemon version.
    Version,
    /// Bridge standard input/output to the local Machine API socket.
    #[command(hide = true)]
    DialStdio,
    /// Serve the Docker Volume plugin on its systemd socket.
    VolumePlugin,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let runtime = match tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
    {
        Ok(runtime) => runtime,
        Err(error) => {
            eprintln!("{error}");
            return ExitCode::FAILURE;
        }
    };
    let code = match runtime.block_on(run(args)) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("{error}");
            daemon_error_exit_code(&error)
        }
    };
    runtime.shutdown_timeout(Duration::from_secs(5));
    code
}

fn daemon_error_exit_code(error: &Error) -> ExitCode {
    if matches!(
        error,
        Error::Network(NetworkError::DockerNetworkConflict { .. })
    ) {
        ExitCode::from(DOCKER_NETWORK_CONFLICT_EXIT_STATUS)
    } else {
        ExitCode::FAILURE
    }
}

async fn run(args: Args) -> Result<(), Error> {
    if matches!(args.command, Some(Command::Version)) {
        println!("{}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }
    if matches!(args.command, Some(Command::DialStdio)) {
        return dial_stdio(&args.socket).await.map_err(Error::from);
    }
    diag::init(args.log_level.as_deref())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if matches!(args.command, Some(Command::VolumePlugin)) {
        let listener = volume_plugin::inherited_listener()?;
        return volume_plugin::run(listener).await.map_err(Error::from);
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_network_conflict_uses_the_dedicated_exit_status() {
        let conflict = Error::Network(NetworkError::DockerNetworkConflict {
            reason: "ownership is unproven".into(),
            expected: "expected".into(),
            observed: "observed".into(),
            recovery: "recovery",
        });

        assert_eq!(
            daemon_error_exit_code(&conflict),
            ExitCode::from(DOCKER_NETWORK_CONFLICT_EXIT_STATUS)
        );
        assert_eq!(
            daemon_error_exit_code(&Error::StorePoisoned),
            ExitCode::FAILURE
        );
    }
}
