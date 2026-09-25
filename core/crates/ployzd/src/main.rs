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
use ployz_core::{
    DOCKER_NETWORK_CONFLICT_EXIT_STATUS, MachineRelease, MachineUpgradeAttemptId, StorageChoice,
};
use ployzd::{
    daemon::{ContainerMode, Daemon, DaemonConfig, Error, wait_until_socket_accepts},
    diag,
    installer::{DEFAULT_SOCKET_PATH, InstallMode, InstallRequest, Readiness},
    machine::DEFAULT_DATA_DIR,
    management::ManagementConfig,
    network::NetworkError,
};
use tokio::io::{AsyncWriteExt, copy, stdin, stdout};

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
    #[arg(long = "dns-upstream", value_name = "ADDR")]
    dns_upstreams: Vec<SocketAddr>,
    #[arg(long, hide = true)]
    machine_api_address: Option<SocketAddr>,
    /// UDP port of the management transport; tests pass 0 to avoid the fixed port.
    #[arg(long, hide = true, default_value_t = ployz_core::MANAGEMENT_PORT)]
    management_port: u16,
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
    /// Execute one accepted Machine upgrade from its transient systemd service.
    #[command(hide = true)]
    UpgradeWorker {
        #[arg(long)]
        attempt: MachineUpgradeAttemptId,
    },
    /// Install or replace this Machine's daemon release.
    Install {
        /// Release channel (stable or beta) or exact published version.
        #[arg(long, default_value = "stable")]
        version: MachineRelease,
        /// Prepare ZFS storage, or leave this Machine stateless.
        #[arg(long, default_value = "none")]
        storage: StorageChoice,
        /// Replace daemon software only; do not install Docker, OS packages, or prepare storage.
        #[arg(long)]
        software_only: bool,
        /// Write files and units but do not contact or start systemd.
        #[arg(long)]
        install_only: bool,
        /// Add this existing operator to the Ployz service group during host preparation.
        #[arg(long, value_name = "USER")]
        group_user: Option<String>,
    },
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
    let run_dir = args
        .socket
        .parent()
        .unwrap_or_else(|| Path::new("/run/ployz"))
        .to_owned();
    if let Some(Command::UpgradeWorker { attempt }) = args.command {
        return ployzd::installer::upgrade::run_worker(attempt, &args.data_dir, &run_dir)
            .await
            .map_err(io::Error::other)
            .map_err(Error::from);
    }
    if let Some(Command::Install {
        version,
        storage,
        software_only,
        install_only,
        group_user,
    }) = args.command
    {
        let request = install_request(version, storage, software_only, install_only, group_user)
            .map_err(Error::from)?;
        let outcome = ployzd::installer::install(request, &args.data_dir, &args.socket)
            .await
            .map_err(io::Error::other)?;
        match outcome.readiness {
            Readiness::InstallationOnly => {
                println!(
                    "Ployz {} installed; systemd was not started",
                    outcome.target
                );
            }
            Readiness::Running => {
                println!("Ployz {} is running and ready", outcome.target);
            }
        }
        return Ok(());
    }
    diag::init(args.log_level.as_deref())
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
    if matches!(args.command, Some(Command::VolumePlugin)) {
        let listener = volume_plugin::inherited_listener()?;
        return volume_plugin::run(listener, &args.data_dir, &run_dir)
            .await
            .map_err(Error::from);
    }
    let daemon = Daemon::start(DaemonConfig {
        data_dir: args.data_dir,
        socket: args.socket,
        dns_upstreams: args.dns_upstreams,
        machine_api_address: args.machine_api_address,
        containerd_socket: args.containerd_socket,
        containers: ContainerMode::Auto,
        management: ManagementConfig {
            port: args.management_port,
            ..ManagementConfig::default()
        },
    })
    .await?;
    daemon.wait().await
}

fn install_request(
    release: MachineRelease,
    storage: StorageChoice,
    software_only: bool,
    install_only: bool,
    group_user: Option<String>,
) -> io::Result<InstallRequest> {
    if install_only && storage != StorageChoice::None {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--install-only cannot be combined with --storage",
        ));
    }
    if install_only && group_user.is_some() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "--install-only cannot be combined with --group-user",
        ));
    }
    let mode = if install_only {
        InstallMode::InstallationOnly
    } else if software_only {
        if storage != StorageChoice::None {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--software-only cannot be combined with --storage",
            ));
        }
        if group_user.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "--software-only cannot be combined with --group-user",
            ));
        }
        InstallMode::SoftwareOnly
    } else {
        InstallMode::PrepareHost {
            storage,
            group_user,
        }
    };
    Ok(InstallRequest { release, mode })
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

    fn release(value: &str) -> MachineRelease {
        MachineRelease::parse(value).unwrap()
    }

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
            daemon_error_exit_code(&Error::RecordOwner(ployzd::machine::RecordOwnerStopped)),
            ExitCode::FAILURE
        );
    }

    #[test]
    fn install_cli_rejects_host_options_for_software_only_replacement() {
        let storage =
            install_request(release("stable"), StorageChoice::Zfs, true, false, None).unwrap_err();
        assert_eq!(storage.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            storage.to_string(),
            "--software-only cannot be combined with --storage"
        );

        let group = install_request(
            release("stable"),
            StorageChoice::None,
            true,
            false,
            Some("operator".into()),
        )
        .unwrap_err();
        assert_eq!(group.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            group.to_string(),
            "--software-only cannot be combined with --group-user"
        );
    }

    #[test]
    fn install_cli_rejects_host_options_for_installation_only() {
        let storage =
            install_request(release("stable"), StorageChoice::Zfs, false, true, None).unwrap_err();
        assert_eq!(storage.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            storage.to_string(),
            "--install-only cannot be combined with --storage"
        );

        let group = install_request(
            release("stable"),
            StorageChoice::None,
            false,
            true,
            Some("operator".into()),
        )
        .unwrap_err();
        assert_eq!(group.kind(), io::ErrorKind::InvalidInput);
        assert_eq!(
            group.to_string(),
            "--install-only cannot be combined with --group-user"
        );
    }

    #[test]
    fn install_cli_builds_one_explicit_mode() {
        let replacement =
            install_request(release("1.2.3"), StorageChoice::None, true, true, None).unwrap();
        assert!(matches!(replacement.mode, InstallMode::InstallationOnly));

        let host = install_request(
            release("1.2.3"),
            StorageChoice::Zfs,
            false,
            false,
            Some("operator".into()),
        )
        .unwrap();
        assert!(matches!(
            host.mode,
            InstallMode::PrepareHost {
                storage: StorageChoice::Zfs,
                group_user: Some(_)
            }
        ));
    }
}
